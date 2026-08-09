// Command direct drives patala in-process from Go, through the UniFFI bindings
// in ../../../../patala-go. There is no binding in sdks/go — this is an example
// OF that one.
//
// It runs entirely against patala_core::MockRail: deterministic, offline, no
// credentials. patala is a payments library, so an example that moves real
// value is not an example.
//
// Run it with the wrapper, which generates the bindings first if they are
// missing and sets the cgo and loader flags for you:
//
//	sdks/go/examples/run.sh direct
//
// or by hand, from sdks/go/, after `make -C ../../patala-go generate`:
//
//	CGO_ENABLED=1 \
//	  CGO_LDFLAGS="-lpatala_uniffi -L../../patala-go/bindings/patala" \
//	  DYLD_LIBRARY_PATH="../../patala-go/bindings/patala:$DYLD_LIBRARY_PATH" \
//	  LD_LIBRARY_PATH="../../patala-go/bindings/patala:$LD_LIBRARY_PATH" \
//	  go run ./examples/direct
//
// THE COST OF THIS PATH IS cgo, and it is the whole reason ../sidecar exists.
// See ../../README.md, and ../../../../patala-go/README.md's "The cgo cost".
package main

import (
	"errors"
	"fmt"
	"os"
	"runtime"

	// No import alias: the generated file declares `package patala`, because
	// patala-uniffi sets that UniFFI namespace explicitly.
	"github.com/vul-os/patala/patala-go/bindings/patala"
)

var checks int

func check(cond bool, msg string) {
	checks++
	if !cond {
		fmt.Fprintf(os.Stderr, "FAILED: %s\n", msg)
		os.Exit(1)
	}
	fmt.Printf("  ok  %s\n", msg)
}

func main() {
	fmt.Printf("go %s on %s/%s, cgo linked in (this binary cannot be built with CGO_ENABLED=0)\n\n",
		runtime.Version(), runtime.GOOS, runtime.GOARCH)

	// The last argument is `failing` — flip it to true for a rail where every
	// operation fails, which is how you exercise your error path offline.
	rail := patala.PatalaRailNewMock("mock", patala.RailClassNonCustodialFinal,
		[]string{"USDC", "USD"}, 0, false)

	fmt.Println("capabilities")
	caps := rail.Capabilities()
	check(rail.Id() == "mock", fmt.Sprintf("Id() == %q", rail.Id()))
	check(caps.Class == patala.RailClassNonCustodialFinal,
		"Class is NonCustodialFinal — a wallet address and a final receipt, not a card form")
	check(!caps.HoldsFunds, "HoldsFunds is false — patala never holds funds")
	check(!caps.Reversible, "Reversible is false — there is no refund on this rail")

	fmt.Println("\npre-flight: ValidateDestination, before any money moves")
	verdict := rail.ValidateDestination("mock:wallet:alice")
	// %v prints 4, not "StructurallyValid": UniFFI lowers an enum to its
	// ordinal and the generated Go type is integer-backed with no String()
	// method. Not a bug — but never persist or compare that number, it is a
	// position in the Rust enum and reordering it changes the meaning.
	check(verdict.Status == patala.DestinationStatusStructurallyValid,
		fmt.Sprintf("a well-formed address gives StructurallyValid (printed as %v — see the comment)", verdict.Status))
	check(!verdict.IsRefusal,
		"IsRefusal is false — a field, never re-derived from Status with a switch that can fall through")
	check(verdict.HumanMustConfirm,
		"HumanMustConfirm is true even here — patala does not detect exchange-owned addresses")
	refused := rail.ValidateDestination("")
	check(refused.Status == patala.DestinationStatusMalformed && refused.IsRefusal,
		"an empty destination is a Malformed refusal — returned as a verdict, never as an error")

	fmt.Println("\nQuote -> Charge -> Verify")
	req := patala.PayRequest{
		AmountMinor: 1250,
		Currency:    "USDC",
		Destination: "mock:wallet:alice",
		Reference:   "order-1",
	}

	quote, err := rail.Quote(req)
	check(err == nil && quote.TotalMinor == uint64(1250),
		fmt.Sprintf("TotalMinor == %d, a uint64 of minor units — never a float", quote.TotalMinor))

	receipt, err := rail.Charge(req)
	check(err == nil && receipt.AmountMinor == 1250,
		fmt.Sprintf("Charge -> receipt for %d %s", receipt.AmountMinor, receipt.Currency))

	ok, err := rail.Verify(receipt)
	check(err == nil && ok, "the genuine receipt verifies true")

	tampered := receipt
	tampered.AmountMinor++
	ok, err = rail.Verify(tampered)
	check(err == nil && !ok,
		"a tampered receipt verifies (false, nil) — fail-closed, and false is DATA, not an error")

	fmt.Println("\nerrors are typed, never a panic")
	_, err = rail.Charge(patala.PayRequest{
		AmountMinor: 100, Currency: "EUR",
		Destination: "mock:wallet:alice", Reference: "order-2",
	})
	var invalid *patala.PatalaErrorInvalidRequest
	check(errors.As(err, &invalid),
		fmt.Sprintf("an unsupported currency is PatalaError.InvalidRequest: %v", err))

	fmt.Println("\nwebhooks: a rail with no push delivery says so")
	_, err = rail.VerifyWebhook(patala.WebhookDelivery{
		RawBody: []byte("{}"), Headers: map[string]string{}, NowUnix: 0,
	})
	var unsupported *patala.PatalaErrorUnsupported
	check(errors.As(err, &unsupported),
		"the mock refuses with Unsupported rather than inventing a WebhookEvent")

	fmt.Printf("\nALL %d GO DIRECT ASSERTIONS PASSED\n", checks)
}
