// The pre-flight surface, exercised from Go through the real compiled cdylib:
// `PatalaRail.ValidateDestination`, the `DestinationVerdict` record, and all
// five `DestinationStatus` variants.
//
// Why this file exists
// --------------------
// patala-core's `rail.rs` is explicit that anything not on the `PaymentRail`
// trait "is unreachable from every non-Rust consumer: the UniFFI surface and
// the sidecar both dispatch through `dyn PaymentRail`". An earlier audit of
// this workspace found webhook verification sitting outside the trait, which
// is why cackle's `PatalaFiatProvider.Webhook` returned `ErrPatalaNoWebhook`
// unconditionally — real Rust code that bought a Go consumer nothing. These
// tests are the proof that the same thing did not happen to the destination
// check: they call it from Go, on the generated bindings, and assert every
// variant and every field arrives intact.
//
// What is being defended, in order of how expensive it is to get wrong
// ---------------------------------------------------------------------
//  1. All five statuses survive the FFI boundary as five DISTINCT values. A
//     verdict that flattened to a bool at the boundary would defeat the whole
//     design — "you mistyped that", "that is a Stellar address in a Solana
//     payout", "that is a contract, not a wallet", "this looks well-formed"
//     and "this rail cannot tell you anything" are five different things to
//     say to a customer about where their refund is going.
//  2. `HumanMustConfirm` is true on EVERY verdict, including the most positive
//     one, and `ExchangeDepositCaveat` rides along with it. patala cannot tell
//     whether an address belongs to an exchange — that needs commercial
//     address-attribution data this workspace refuses to depend on — so the
//     human confirmation step is unconditional and there is no verdict that
//     lets a Go caller skip it.
//  3. `IsRefusal` crosses as DATA. It is a method on the Rust type, and a
//     method does not cross FFI; a Go consumer re-deriving it from a `switch`
//     on Status would fall through to its default for any status added later,
//     and the default a caller writes is "not a refusal". That fails OPEN, on
//     the one question that decides whether money goes to an address the rail
//     already knows is wrong.
//
// Everything here is offline. `ValidateDestination` is pure by contract — no
// network, no clock, no filesystem — so none of this needs an RPC or a
// processor, and it runs identically under `make test` and `make test-fiat`.

package bindingtest

import (
	"strings"
	"testing"

	patala "github.com/vul-os/patala/patala-go/bindings/patala"
)

// newOpaqueRail builds the offline stand-in for a rail that cannot check a
// destination at all — the shape of every fiat rail, whose `destination` is an
// opaque processor-side token meaningful only to that processor. It is the only
// way to reach DestinationStatusUnknown without compiling in a feature-gated
// real rail, which is exactly why the constructor is exported.
func newOpaqueRail(t *testing.T) *patala.PatalaRail {
	t.Helper()
	return patala.PatalaRailNewMockWithoutDestinationChecks(
		"opaque",
		patala.RailClassCustodialReversible,
		[]string{"USD"},
		0,
		false,
	)
}

// TestEveryDestinationStatusRoundTripsToGo is the central assertion of the
// binding: each of the five statuses is produced by a real call into Rust and
// arrives in Go as the right, distinct value.
func TestEveryDestinationStatusRoundTripsToGo(t *testing.T) {
	mock := newMock(t)
	opaque := newOpaqueRail(t)

	cases := []struct {
		name        string
		rail        *patala.PatalaRail
		destination string
		want        patala.DestinationStatus
		wantRefusal bool
	}{
		{
			// MockRail's synthetic "<network>:<kind>:<label>" grammar. NOT a
			// real chain address format — a real rail decodes its own alphabet,
			// length and checksum instead. The grammar exists so a consumer can
			// build and test its whole payout UI with no chain reachable.
			name: "a plain wallet on this rail's own network",
			rail: mock, destination: "mock:wallet:alice",
			want: patala.DestinationStatusStructurallyValid, wantRefusal: false,
		},
		{
			name: "a program account nobody holds a key for",
			rail: mock, destination: "mock:program:vault",
			want: patala.DestinationStatusNotAWallet, wantRefusal: true,
		},
		{
			name: "a well-formed address for a different chain",
			rail: mock, destination: "stellar:wallet:alice",
			want: patala.DestinationStatusWrongNetwork, wantRefusal: true,
		},
		{
			name: "junk a customer typed",
			rail: mock, destination: "definitely-not-an-address",
			want: patala.DestinationStatusMalformed, wantRefusal: true,
		},
		{
			// Guards fail closed: the one defect decidable with no rail
			// knowledge at all is a refusal, never a shrug.
			name: "an empty string",
			rail: mock, destination: "",
			want: patala.DestinationStatusMalformed, wantRefusal: true,
		},
		{
			name: "a rail that cannot check at all",
			rail: opaque, destination: "cus_opaque_processor_token",
			want: patala.DestinationStatusUnknown, wantRefusal: false,
		},
	}

	seen := map[patala.DestinationStatus]bool{}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			v := tc.rail.ValidateDestination(tc.destination)
			if v.Status != tc.want {
				t.Fatalf("ValidateDestination(%q).Status = %v, want %v", tc.destination, v.Status, tc.want)
			}
			if v.IsRefusal != tc.wantRefusal {
				t.Errorf("ValidateDestination(%q).IsRefusal = %v, want %v",
					tc.destination, v.IsRefusal, tc.wantRefusal)
			}
			seen[v.Status] = true
		})
	}

	// Not "the table has six rows" — "the five variants were each genuinely
	// produced by the Rust side". A binding bug that mapped everything to one
	// variant would pass every row above individually only if the expectations
	// also collapsed; this cannot pass unless five distinct values arrived.
	all := []patala.DestinationStatus{
		patala.DestinationStatusMalformed,
		patala.DestinationStatusWrongNetwork,
		patala.DestinationStatusNotAWallet,
		patala.DestinationStatusStructurallyValid,
		patala.DestinationStatusUnknown,
	}
	for _, s := range all {
		if !seen[s] {
			t.Errorf("DestinationStatus %v was never produced by a real call; it is unreachable from Go", s)
		}
	}
	if len(seen) != len(all) {
		t.Fatalf("saw %d distinct statuses, want %d — variants are collapsing at the FFI boundary", len(seen), len(all))
	}
}

// TestDestinationStatusDiscriminantsArePinned pins the five constants. UniFFI
// enums cross the FFI boundary as plain integers, so reordering the Rust enum
// silently re-points every Go constant at a different variant — and here that
// would mean a `Malformed` address arriving in Go as `StructurallyValid`, i.e.
// a refusal read as a pass, with a customer's refund behind it. Same class of
// guard as TestWebhookStatusDiscriminantsArePinned.
func TestDestinationStatusDiscriminantsArePinned(t *testing.T) {
	for _, tc := range []struct {
		name string
		got  patala.DestinationStatus
		want patala.DestinationStatus
	}{
		{"Malformed", patala.DestinationStatusMalformed, 1},
		{"WrongNetwork", patala.DestinationStatusWrongNetwork, 2},
		{"NotAWallet", patala.DestinationStatusNotAWallet, 3},
		{"StructurallyValid", patala.DestinationStatusStructurallyValid, 4},
		{"Unknown", patala.DestinationStatusUnknown, 5},
	} {
		if tc.got != tc.want {
			t.Errorf("DestinationStatus%s = %d, want %d", tc.name, tc.got, tc.want)
		}
	}

	// Zero is not a valid status. A Go struct that was never populated has
	// Status == 0, so a zero value must never compare equal to a real verdict —
	// otherwise an unset field would read as whichever variant took slot zero.
	var unset patala.DestinationStatus
	for _, s := range []patala.DestinationStatus{
		patala.DestinationStatusMalformed,
		patala.DestinationStatusWrongNetwork,
		patala.DestinationStatusNotAWallet,
		patala.DestinationStatusStructurallyValid,
		patala.DestinationStatusUnknown,
	} {
		if s == unset {
			t.Errorf("DestinationStatus %d collides with the zero value; an unpopulated struct would read as a real verdict", s)
		}
	}
}

// TestNoVerdictWaivesTheHumanConfirmation is the most important assertion in
// this file. There is no verdict — not even StructurallyValid — that a Go
// caller can read as "safe, skip the human". Exchange membership is
// undecidable without a hosted address-attribution service patala will not
// depend on, so the confirmation step is unconditional by construction.
func TestNoVerdictWaivesTheHumanConfirmation(t *testing.T) {
	mock := newMock(t)
	opaque := newOpaqueRail(t)

	type call struct {
		rail *patala.PatalaRail
		dest string
	}
	for _, c := range []call{
		{mock, "mock:wallet:alice"},
		{mock, "mock:program:vault"},
		{mock, "stellar:wallet:alice"},
		{mock, "definitely-not-an-address"},
		{mock, ""},
		{opaque, "cus_opaque_processor_token"},
	} {
		v := c.rail.ValidateDestination(c.dest)

		if !v.HumanMustConfirm {
			t.Errorf("ValidateDestination(%q) on %q: HumanMustConfirm = false; no verdict may waive it",
				c.dest, c.rail.Id())
		}

		// The caveat is not decoration: it is the sentence a UI shows the
		// person who is about to approve the payout, and it must ride on the
		// positive verdicts too — those are the ones where a caller is
		// tempted to skip it.
		if v.ExchangeDepositCaveat == "" {
			t.Errorf("ValidateDestination(%q) on %q: ExchangeDepositCaveat is empty",
				c.dest, c.rail.Id())
		}
		if !strings.Contains(v.ExchangeDepositCaveat, "exchange") {
			t.Errorf("ValidateDestination(%q) on %q: caveat does not mention exchanges: %q",
				c.dest, c.rail.Id(), v.ExchangeDepositCaveat)
		}

		// Every verdict must be explainable to a person. A refusal a caller
		// cannot put words to is nearly as unhelpful as no refusal.
		if strings.TrimSpace(v.Reason) == "" {
			t.Errorf("ValidateDestination(%q) on %q: Reason is empty; nothing to show the customer",
				c.dest, c.rail.Id())
		}

		// A verdict is only ever about the network its own rail pays on, so a
		// caller must always be able to see whose opinion it is reading.
		if v.RailId != c.rail.Id() {
			t.Errorf("ValidateDestination(%q): RailId = %q, want %q", c.dest, v.RailId, c.rail.Id())
		}
	}
}

// TestTheCaveatIsIdenticalEverywhereItAppears: the caveat on a verdict and the
// one from the standalone accessor must be the same string. A consumer shows
// the accessor's text on the "where should we send your refund?" form and the
// verdict's text on the confirmation screen; if they drifted, a customer would
// be warned about two different things.
func TestTheCaveatIsIdenticalEverywhereItAppears(t *testing.T) {
	standalone := patala.ExchangeDepositCaveat()
	if strings.TrimSpace(standalone) == "" {
		t.Fatal("ExchangeDepositCaveat() is empty; the warning a customer reads has no text")
	}

	mock := newMock(t)
	for _, dest := range []string{"mock:wallet:alice", "junk", ""} {
		if got := mock.ValidateDestination(dest).ExchangeDepositCaveat; got != standalone {
			t.Errorf("caveat on the verdict for %q differs from ExchangeDepositCaveat():\n verdict: %q\n  direct: %q",
				dest, got, standalone)
		}
	}

	// The two things the wording must actually tell a person: that patala
	// cannot determine this, and that only the wallet's owner can confirm it.
	for _, want := range []string{"exchange", "confirm"} {
		if !strings.Contains(strings.ToLower(standalone), want) {
			t.Errorf("the caveat shown to customers does not mention %q: %q", want, standalone)
		}
	}
}

// TestValidateDestinationIsPureAndOffline exercises the contract that lets a
// Go consumer call this on every keystroke of an address field, and that lets
// it work on a gate device with no uplink: same input, same verdict, no
// network, no clock.
func TestValidateDestinationIsPureAndOffline(t *testing.T) {
	// A rail configured to fail every quote/charge. If ValidateDestination
	// reached the rail's network path at all, this is where it would show.
	failing := patala.PatalaRailNewMock("mock", patala.RailClassNonCustodialFinal, []string{"USDC"}, 0, true)

	first := failing.ValidateDestination("mock:wallet:alice")
	if first.Status != patala.DestinationStatusStructurallyValid {
		t.Fatalf("ValidateDestination on a failing rail = %v; the check must not depend on the rail working",
			first.Status)
	}

	for i := 0; i < 25; i++ {
		got := failing.ValidateDestination("mock:wallet:alice")
		if got.Status != first.Status || got.Reason != first.Reason || got.IsRefusal != first.IsRefusal {
			t.Fatalf("call %d disagreed with call 0: %+v vs %+v", i, got, first)
		}
	}

	// Purity across instances too: a fresh rail with the same id must form the
	// same verdict, since nothing about a verdict may depend on hidden state.
	other := newMock(t)
	if got := other.ValidateDestination("mock:wallet:alice"); got.Status != first.Status {
		t.Errorf("a second rail with the same id gave %v, want %v", got.Status, first.Status)
	}
}

// TestRefusalsAreExactlyTheThreeEstablishedDefects pins the classification a Go
// caller gates on, so it cannot drift into either failure mode: a refusal read
// as passable, or Unknown read as a refusal (which would make the honest "a
// human must decide" answer indistinguishable from "this address is wrong").
func TestRefusalsAreExactlyTheThreeEstablishedDefects(t *testing.T) {
	mock := newMock(t)
	opaque := newOpaqueRail(t)

	refusals := map[string]*patala.PatalaRail{
		"junk":                 mock,
		"mock:program:vault":   mock,
		"stellar:wallet:alice": mock,
	}
	for dest, rail := range refusals {
		if v := rail.ValidateDestination(dest); !v.IsRefusal {
			t.Errorf("ValidateDestination(%q).IsRefusal = false; status %v is an established defect", dest, v.Status)
		}
	}

	// Neither of these is a refusal — and neither is a green light. They are
	// also not interchangeable with each other, which is the distinction a
	// caller most often flattens.
	valid := mock.ValidateDestination("mock:wallet:alice")
	unknown := opaque.ValidateDestination("cus_opaque_processor_token")
	if valid.IsRefusal {
		t.Error("StructurallyValid must not be a refusal")
	}
	if unknown.IsRefusal {
		t.Error("Unknown must not be a refusal: nothing was established, which is not the same as a defect")
	}
	if valid.Status == unknown.Status {
		t.Fatal("StructurallyValid and Unknown collapsed to the same status; 'checked and clean' and 'could not check' are not the same answer")
	}
}

// TestCompensatingPaymentFlowEndToEndFromGo walks the whole documented payout
// flow through the binding, in the order a merchant backend would: the customer
// supplies an address, it is validated offline, a refusal stops the flow, and
// only a non-refused address reaches `Charge` — with its OWN reference, because
// the payout is a second, independent payment and not a reversal of the first.
//
// This is the flow docs/compensating-payments.md describes. Having it as a test
// means the documented sequence is one that actually compiles and runs against
// the real bindings, rather than prose nobody executed.
func TestCompensatingPaymentFlowEndToEndFromGo(t *testing.T) {
	rail := newMock(t)

	// 1. The original payment, from the customer to the merchant.
	original, err := rail.Charge(patala.PayRequest{
		AmountMinor: 2500,
		Currency:    "USDC",
		Destination: "mock:wallet:merchant",
		Reference:   "go-order-refundable",
	})
	if err != nil {
		t.Fatalf("Charge() error = %v", err)
	}

	// 2. The customer asks for their money back. The merchant asks the CUSTOMER
	//    for an address — never reusing a sender address, which is very often
	//    an exchange withdrawal address the customer cannot be credited on.
	//    Suppose they first paste something wrong:
	wrong := rail.ValidateDestination("stellar:wallet:customer")
	if !wrong.IsRefusal {
		t.Fatal("a wrong-network address must be refused before any money moves")
	}
	// A refusal stops here. It is not a warning to click past, and the caller
	// has a sentence to put on screen.
	if strings.TrimSpace(wrong.Reason) == "" {
		t.Error("a refusal must come with a reason the customer can act on")
	}

	// 3. They correct it. The best answer available is StructurallyValid — the
	//    absence of a decidable defect, NOT "safe".
	good := rail.ValidateDestination("mock:wallet:customer")
	if good.IsRefusal {
		t.Fatalf("ValidateDestination refused a well-formed address: %s", good.Reason)
	}
	if !good.HumanMustConfirm {
		t.Fatal("the payout must not be approvable without a human confirming the address")
	}
	// 4. A human is shown good.Reason AND good.ExchangeDepositCaveat, and
	//    confirms they control the wallet. That step is a person, not code —
	//    this stands in for it.
	humanConfirmed := good.ExchangeDepositCaveat != "" && good.HumanMustConfirm

	if !humanConfirmed {
		t.Fatal("nothing to show the human; the confirmation step cannot be performed")
	}

	// 5. The compensating payment: an ordinary Charge, in the opposite
	//    direction, with its OWN reference. Not a refund — this rail is
	//    NonCustodialFinal and finality is the point.
	payout, err := rail.Charge(patala.PayRequest{
		AmountMinor: original.AmountMinor,
		Currency:    original.Currency,
		Destination: "mock:wallet:customer",
		Reference:   "go-order-refundable-payout", // its own idempotency key
	})
	if err != nil {
		t.Fatalf("the compensating Charge() failed: %v", err)
	}
	if payout.Reference == original.Reference {
		t.Error("the payout reused the original reference; it is a separate payment and needs its own")
	}

	// The payout's receipt — not the original — is the proof the payout
	// happened, and it verifies on its own.
	valid, err := rail.Verify(payout)
	if err != nil {
		t.Fatalf("Verify(payout) error = %v", err)
	}
	if !valid {
		t.Fatal("the payout receipt does not verify; there is no proof the customer was paid")
	}

	// And the original receipt is untouched: a compensating payment changes
	// nothing about the payment it compensates for.
	stillValid, err := rail.Verify(original)
	if err != nil || !stillValid {
		t.Fatalf("the original receipt stopped verifying after the payout (valid=%v, err=%v); a compensating payment must not alter the original",
			stillValid, err)
	}
}
