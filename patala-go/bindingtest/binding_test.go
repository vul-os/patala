// Tests for the always-present part of the UniFFI surface: the MockRail
// constructor, the capability descriptor, quote -> charge -> verify, the
// fail-closed verify contract, and the error mapping. None of this needs a
// feature-gated cdylib, so these run under a plain `make test`.
//
// These are the assertions ../examples/roundtrip/main.go makes as a `go run`
// binary, restated as real tests so `go test` actually reports on them, plus
// the ones an example could not usefully make (a rail with no push delivery
// must report Unsupported rather than fabricate an event).

package bindingtest

import (
	"errors"
	"testing"

	// The generated Go source declares `package patala`: uniffi-bindgen-go
	// takes the package clause from the UniFFI namespace, and patala-uniffi
	// sets that namespace explicitly (see ../README.md "Which cdylib, which
	// package name?"). No import alias is needed anywhere in this module.
	"github.com/vul-os/patala/patala-go/bindings/patala"
)

// newMock builds the same offline rail ../examples/roundtrip uses.
func newMock(t *testing.T) *patala.PatalaRail {
	t.Helper()
	return patala.PatalaRailNewMock(
		"mock",
		patala.RailClassNonCustodialFinal,
		[]string{"USDC", "USD"},
		0,
		false,
	)
}

func TestMockRailIdentityAndCapabilities(t *testing.T) {
	rail := newMock(t)

	if got := rail.Id(); got != "mock" {
		t.Fatalf("Id() = %q, want %q", got, "mock")
	}

	caps := rail.Capabilities()
	if caps.RailClass != patala.RailClassNonCustodialFinal {
		t.Errorf("Capabilities().RailClass = %v, want RailClassNonCustodialFinal (%v)",
			caps.RailClass, patala.RailClassNonCustodialFinal)
	}
	// A NonCustodialFinal rail that reported either of these as true would be
	// describing a completely different settlement class to a caller choosing
	// a rail off the capability descriptor (PATALA.md §3).
	if caps.HoldsFunds {
		t.Error("Capabilities().HoldsFunds = true; a NonCustodialFinal rail must never hold funds")
	}
	if caps.Reversible {
		t.Error("Capabilities().Reversible = true; a NonCustodialFinal rail must never be reversible")
	}
	if len(caps.Currencies) != 2 || caps.Currencies[0] != "USDC" || caps.Currencies[1] != "USD" {
		t.Errorf("Capabilities().Currencies = %v, want [USDC USD] in order", caps.Currencies)
	}
}

// TestRailClassDiscriminantsArePinned pins the RailClass constants. UniFFI
// enums cross the FFI boundary as plain integers, so reordering the Rust enum
// silently re-points every Go constant at a different variant — and RailClass
// is what a caller reads to decide whether a rail is reversible.
func TestRailClassDiscriminantsArePinned(t *testing.T) {
	if patala.RailClassCustodialReversible != 1 {
		t.Errorf("RailClassCustodialReversible = %d, want 1", patala.RailClassCustodialReversible)
	}
	if patala.RailClassNonCustodialFinal != 2 {
		t.Errorf("RailClassNonCustodialFinal = %d, want 2", patala.RailClassNonCustodialFinal)
	}
	if patala.RailClassCustodialReversible == patala.RailClassNonCustodialFinal {
		t.Fatal("the two settlement classes must never compare equal")
	}
}

func TestQuoteChargeVerifyRoundTrip(t *testing.T) {
	rail := newMock(t)
	req := patala.PayRequest{
		AmountMinor: 1250,
		Currency:    "USDC",
		Destination: "dest-anything",
		Reference:   "go-test-order-1",
	}

	quote, err := rail.Quote(req)
	if err != nil {
		t.Fatalf("Quote() error = %v", err)
	}
	// uint64 minor units, never a float — a float amount is the bug this
	// type choice exists to make impossible (PATALA.md §3, §8).
	if quote.TotalMinor != 1250 {
		t.Errorf("Quote().TotalMinor = %d, want 1250", quote.TotalMinor)
	}

	receipt, err := rail.Charge(req)
	if err != nil {
		t.Fatalf("Charge() error = %v", err)
	}
	if receipt.RailId != "mock" {
		t.Errorf("Charge().RailId = %q, want %q", receipt.RailId, "mock")
	}
	if receipt.AmountMinor != 1250 {
		t.Errorf("Charge().AmountMinor = %d, want 1250", receipt.AmountMinor)
	}
	if receipt.Reference != "go-test-order-1" {
		t.Errorf("Charge().Reference = %q, want %q", receipt.Reference, "go-test-order-1")
	}
	if len(receipt.Proof) == 0 {
		t.Error("Charge().Proof is empty; verify() has nothing to re-derive from")
	}

	valid, err := rail.Verify(receipt)
	if err != nil {
		t.Fatalf("Verify() error = %v", err)
	}
	if !valid {
		t.Fatal("Verify() = false for a genuine receipt this rail just issued")
	}
}

// TestVerifyFailsClosedOnTamperedReceipt is the single most important
// assertion on the pull path: a receipt whose amount was edited after issue
// must never verify. It is also the assertion whose failure mode is silent —
// a broken proof check returns `true` for everything and every test that only
// checks the happy path still passes.
func TestVerifyFailsClosedOnTamperedReceipt(t *testing.T) {
	rail := newMock(t)
	receipt, err := rail.Charge(patala.PayRequest{
		AmountMinor: 1250,
		Currency:    "USDC",
		Destination: "dest",
		Reference:   "go-test-order-tamper",
	})
	if err != nil {
		t.Fatalf("Charge() error = %v", err)
	}

	for _, tc := range []struct {
		name   string
		mutate func(r *patala.Receipt)
	}{
		{"amount inflated", func(r *patala.Receipt) { r.AmountMinor = 999999 }},
		{"currency swapped", func(r *patala.Receipt) { r.Currency = "EUR" }},
		{"reference swapped", func(r *patala.Receipt) { r.Reference = "someone-elses-order" }},
		{"proof truncated", func(r *patala.Receipt) { r.Proof = nil }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tampered := receipt
			tc.mutate(&tampered)
			valid, err := rail.Verify(tampered)
			// Fail-closed is expressed as data (`false`), never as an error —
			// same contract as the core trait, so a caller cannot mistake
			// "did not verify" for "the binding broke".
			if err != nil {
				t.Fatalf("Verify() on a tampered receipt returned an error (%v); the contract is (false, nil)", err)
			}
			if valid {
				t.Fatal("Verify() = true for a tampered receipt; verify is not failing closed")
			}
		})
	}
}

func TestUnsupportedCurrencyIsATypedError(t *testing.T) {
	rail := newMock(t)
	_, err := rail.Charge(patala.PayRequest{
		AmountMinor: 100,
		Currency:    "EUR",
		Destination: "dest",
		Reference:   "go-test-order-2",
	})
	if err == nil {
		t.Fatal("Charge() with an unsupported currency returned no error")
	}
	if !errors.Is(err, patala.ErrPatalaErrorInvalidRequest) {
		t.Fatalf("Charge() error = %v (%T), want ErrPatalaErrorInvalidRequest", err, err)
	}
}

func TestFailingRailSurfacesRailError(t *testing.T) {
	// The `failing` constructor flag exists so a consumer can exercise its own
	// error path; if the flag stopped reaching MockRail, this rail would
	// succeed and nothing else in the suite would notice.
	rail := patala.PatalaRailNewMock("mock", patala.RailClassNonCustodialFinal, []string{"USD"}, 0, true)
	_, err := rail.Charge(patala.PayRequest{
		AmountMinor: 100,
		Currency:    "USD",
		Destination: "dest",
		Reference:   "go-test-order-3",
	})
	if err == nil {
		t.Fatal("Charge() on a rail constructed with failing=true returned no error")
	}
	if !errors.Is(err, patala.ErrPatalaErrorRail) {
		t.Fatalf("Charge() error = %v (%T), want ErrPatalaErrorRail", err, err)
	}
}

func TestQuoteCarriesTheConfiguredFee(t *testing.T) {
	rail := patala.PatalaRailNewMock("mock", patala.RailClassNonCustodialFinal, []string{"USD"}, 75, false)
	quote, err := rail.Quote(patala.PayRequest{
		AmountMinor: 1000,
		Currency:    "USD",
		Destination: "dest",
		Reference:   "go-test-order-4",
	})
	if err != nil {
		t.Fatalf("Quote() error = %v", err)
	}
	if quote.FeeMinor != 75 {
		t.Errorf("Quote().FeeMinor = %d, want 75 (the fee_minor passed to the constructor)", quote.FeeMinor)
	}
	if quote.TotalMinor != 1075 {
		t.Errorf("Quote().TotalMinor = %d, want 1075 (amount + fee, in minor units)", quote.TotalMinor)
	}
}

// TestMockRailReportsWebhookUnsupported covers the push path's honest answer.
// MockRail has no processor and no push delivery, so `verify_webhook` must
// report Unsupported. A binding that returned a zero-valued WebhookEvent here
// instead would hand a caller an event with the zero WebhookStatus — which is
// exactly the class of bug the status pinning in webhook_status_test.go
// guards the other end of.
func TestMockRailReportsWebhookUnsupported(t *testing.T) {
	rail := newMock(t)
	_, err := rail.VerifyWebhook(patala.WebhookDelivery{
		RawBody: []byte(`{"anything":true}`),
		Headers: map[string]string{},
		Query:   nil,
		NowUnix: 1700000000,
	})
	if err == nil {
		t.Fatal("VerifyWebhook() on MockRail returned no error; a rail with no push delivery must say Unsupported, not invent an event")
	}
	if !errors.Is(err, patala.ErrPatalaErrorUnsupported) {
		t.Fatalf("VerifyWebhook() error = %v (%T), want ErrPatalaErrorUnsupported", err, err)
	}
}
