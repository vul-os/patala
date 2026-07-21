//go:build fiat

// Command fiatroundtrip is the Go analogue of patala-py's fiat unit tests
// (patala-py/src/fiat.rs's `#[cfg(test)] mod tests`): it drives
// `patala.PatalaRailNewFiat` — the by-name registry constructor added on top
// of `patala-fiat`'s 20 processor adapters (see patala-py/README.md and
// patala-py/src/fiat.rs's module docs for why by-name+config, not one typed
// constructor per adapter) — entirely from Go, through the SAME
// UniFFI-generated bindings `./examples/roundtrip` already exercises for
// MockRail.
//
// Everything here is offline, exactly like ./examples/roundtrip:
//
//   - "manual" (patala-fiat's always-on, zero-network rail — the fiat-side
//     equivalent of patala_core::MockRail) is charged and verified for real,
//     proving the full by-name-provider -> config-map -> PaymentRail plumbing
//     actually works end to end through cgo.
//   - "stripe" is only ever CONSTRUCTED (never charged/verified, which would
//     dial Stripe's real API) to prove a feature-gated processor adapter's
//     config map -> typed Config -> real Rail path works and its
//     capabilities/class come through correctly — the same
//     construction-only precedent patala-py's own Rust tests set for
//     Solana/Stellar/Hyperswitch and for stripe/paypal/btcpay in fiat.rs.
//
// This binary requires a cdylib built WITH the fiat feature, e.g.:
//
//	cargo build -p patala-py --features fiat-all
//
// then regenerate bindings (see ../../README.md) before running:
//
//	CGO_ENABLED=1 \
//	  CGO_LDFLAGS="-lpatala_py -Lbindings/patala" \
//	  DYLD_LIBRARY_PATH="bindings/patala:$DYLD_LIBRARY_PATH" \
//	  LD_LIBRARY_PATH="bindings/patala:$LD_LIBRARY_PATH" \
//	  go run ./examples/fiatroundtrip
//
// or simply `make run-example-fiat` from patala-go/ (builds patala-py with
// `--features fiat-all` first).
package main

import (
	"fmt"
	"os"
	"sort"
	"strings"

	// See ../roundtrip/main.go's identical comment: the generated file's
	// `package` clause is fixed to `patala_py` regardless of the output
	// directory name, hence the import alias.
	patala "github.com/vul-os/patala/patala-go/bindings/patala"
)

func must[T any](v T, err error) T {
	if err != nil {
		fmt.Fprintf(os.Stderr, "FAILED: %v\n", err)
		os.Exit(1)
	}
	return v
}

func assert(cond bool, msg string) {
	if !cond {
		fmt.Fprintf(os.Stderr, "FAILED: %s\n", msg)
		os.Exit(1)
	}
}

func main() {
	// ---- discover what this build actually compiled in -------------------
	providers := patala.PatalaFiatProviders()
	sort.Strings(providers)
	assert(contains(providers, "manual"), "\"manual\" must always be listed")
	fmt.Printf("fiat providers compiled into this build: %v\n", providers)

	// ---- "manual": a genuine, fully offline charge -> verify round trip --
	manualRail := must(patala.PatalaRailNewFiat("MANUAL", map[string]string{}))
	assert(manualRail.Id() == "manual", fmt.Sprintf("unexpected rail id: %q", manualRail.Id()))

	manualCaps := manualRail.Capabilities()
	assert(!manualCaps.HoldsFunds, "manual has no processor -- nothing custodies anything")
	fmt.Printf("manual capabilities OK: class=%v holds_funds=%v\n", manualCaps.Class, manualCaps.HoldsFunds)

	req := patala.PayRequest{
		AmountMinor: 1_500,
		Currency:    "ZAR",
		Destination: "buyer@example.org",
		Reference:   "go-fiat-order-1",
	}
	receipt := must(manualRail.Charge(req))
	// Honest pending/settled lifecycle (PATALA.md §3, §8, patala-fiat's own
	// `manual` module docs): a manual "bank transfer" charge never settles
	// money by itself -- only a direct-Rust caller of ManualRail's own
	// inherent `mark_paid` (NOT part of the PaymentRail trait, so
	// unreachable through this generic by-name FFI surface) can do that. So
	// `AmountMinor` stays 0 and `Verify` honestly reports `false` until then
	// -- this is the contract working correctly, not a bug.
	assert(receipt.AmountMinor == 0, "a fresh manual charge has not settled any money yet")
	settled := must(manualRail.Verify(receipt))
	assert(!settled, "an unconfirmed manual instruction must never report settled")
	fmt.Println("manual charge/verify OK: honestly pending (amount_minor=0, verify=false) until a human confirms it")

	// ---- unknown provider name: a typed error, never a panic/silent fallback --
	_, err := patala.PatalaRailNewFiat("not-a-real-processor", map[string]string{})
	assert(err != nil, "expected an error for an unrecognised provider name")
	assert(
		strings.Contains(err.Error(), "InvalidRequest"),
		fmt.Sprintf("expected an InvalidRequest-shaped error, got: %v", err),
	)
	fmt.Printf("unknown-provider error mapping OK: %v\n", err)

	// ---- "stripe": construction-only, proving a feature-gated processor's
	// config map -> typed Config -> real Rail path works (never charge/
	// verify, which would dial Stripe's real API -- see module docs above
	// and patala-py's own construction-only precedent for Solana/Stellar/
	// Hyperswitch/Stripe/PayPal/BTCPay). Skipped gracefully if this
	// particular cdylib was not built with `fiat-stripe`.
	stripeRail, err := patala.PatalaRailNewFiat("stripe", map[string]string{
		"secret_key":      "sk_test_go_example",
		"webhook_secret":  "whsec_go_example",
		"requires_kyc":    "true",
		"settlement_days": "2",
	})
	if err != nil && strings.Contains(err.Error(), "without --features fiat-stripe") {
		fmt.Println("stripe SKIPPED: this cdylib was not built with --features fiat-stripe")
	} else {
		if err != nil {
			fmt.Fprintf(os.Stderr, "FAILED: %v\n", err)
			os.Exit(1)
		}
		stripeCaps := stripeRail.Capabilities()
		assert(stripeRail.Id() == "stripe", fmt.Sprintf("unexpected rail id: %q", stripeRail.Id()))
		assert(stripeCaps.Class == patala.RailClassCustodialReversible, "stripe must be CustodialReversible")
		assert(stripeCaps.HoldsFunds, "stripe (the PROCESSOR) custodies funds in flight -- never patala's")
		fmt.Printf("stripe construction-only OK: class=%v holds_funds=%v (never charged/verified -- no live network)\n", stripeCaps.Class, stripeCaps.HoldsFunds)
	}

	fmt.Println("\nALL GO FIAT ROUNDTRIP ASSERTIONS PASSED")
}

func contains(haystack []string, needle string) bool {
	for _, s := range haystack {
		if s == needle {
			return true
		}
	}
	return false
}
