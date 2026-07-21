// Command roundtrip is the Go analogue of patala-py/examples/smoke_test.py:
// it drives a full quote -> charge -> verify round trip against
// patala_core::MockRail, entirely offline, entirely from Go, through the
// UniFFI-generated bindings in ../../bindings/patala (see ../../README.md
// for how those bindings get there — they are generated, not checked in).
//
// Run (from patala-go/, after `make generate` — see README.md):
//
//	CGO_ENABLED=1 \
//	  CGO_LDFLAGS="-lpatala_py -Lbindings/patala" \
//	  DYLD_LIBRARY_PATH="bindings/patala:$DYLD_LIBRARY_PATH" \
//	  LD_LIBRARY_PATH="bindings/patala:$LD_LIBRARY_PATH" \
//	  go run ./examples/roundtrip
//
// or simply `make run-example` from patala-go/.
package main

import (
	"errors"
	"fmt"
	"os"

	// The generated Go source still declares `package patala_py` (see
	// README.md "Which cdylib, which package name?" — uniffi-bindgen-go
	// v0.5.0's `package_name` config only renames the output directory, not
	// the `package` clause itself, which is fixed to the crate's UniFFI
	// namespace baked in by patala-py's own `uniffi::setup_scaffolding!()`
	// call, which this package does not touch). Aliasing the import to
	// `patala` keeps the Go call sites reading naturally regardless.
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
	rail := patala.PatalaRailNewMock(
		"mock",
		patala.RailClassNonCustodialFinal,
		[]string{"USDC", "USD"},
		0,
		false,
	)

	assert(rail.Id() == "mock", fmt.Sprintf("unexpected rail id: %q", rail.Id()))

	caps := rail.Capabilities()
	assert(caps.Class == patala.RailClassNonCustodialFinal, "capabilities.Class must round-trip through the FFI boundary")
	assert(!caps.HoldsFunds, "a NonCustodialFinal rail must not hold funds")
	assert(!caps.Reversible, "a NonCustodialFinal rail must not be reversible")
	fmt.Printf("capabilities OK: class=%v currencies=%v holds_funds=%v\n", caps.Class, caps.Currencies, caps.HoldsFunds)

	req := patala.PayRequest{
		AmountMinor: 1_250,
		Currency:    "USDC",
		Destination: "dest-anything",
		Reference:   "go-roundtrip-order-1",
	}

	quote := must(rail.Quote(req))
	assert(quote.TotalMinor == 1_250, fmt.Sprintf("unexpected total_minor: %d", quote.TotalMinor))
	fmt.Printf("quote OK: total_minor=%d (uint64, not float)\n", quote.TotalMinor)

	receipt := must(rail.Charge(req))
	assert(receipt.AmountMinor == 1_250, "receipt.AmountMinor mismatch")
	assert(receipt.RailId == "mock", "receipt.RailId mismatch")
	fmt.Printf("charge OK: receipt rail_id=%q amount_minor=%d\n", receipt.RailId, receipt.AmountMinor)

	valid := must(rail.Verify(receipt))
	assert(valid, "a genuine receipt must verify")
	fmt.Println("verify OK: genuine receipt verified true")

	// Fail-closed: a tampered receipt must never verify (PATALA.md §3, §8).
	tampered := receipt
	tampered.AmountMinor = 999_999
	tamperedValid := must(rail.Verify(tampered))
	assert(!tamperedValid, "a tampered receipt must never verify")
	fmt.Println("verify OK: tampered receipt verified false (fail-closed)")

	// An unsupported currency surfaces as a typed error, not a crash/panic.
	_, err := rail.Charge(patala.PayRequest{
		AmountMinor: 100,
		Currency:    "EUR",
		Destination: "dest",
		Reference:   "go-roundtrip-order-2",
	})
	assert(err != nil, "expected an error for an unsupported currency")
	assert(
		errors.Is(err, patala.ErrPatalaErrorInvalidRequest),
		fmt.Sprintf("expected ErrPatalaErrorInvalidRequest, got %T: %v", err, err),
	)
	fmt.Printf("error mapping OK: unsupported currency raised %v\n", err)

	fmt.Println("\nALL GO ROUNDTRIP ASSERTIONS PASSED")
}
