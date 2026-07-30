//go:build fiat

// `ValidateDestination` on REAL rails, not the offline stand-in.
//
// destination_test.go proves the five verdicts round-trip to Go using
// `MockRail`'s synthetic address grammar. That is the right way to cover the
// *binding*, but it leaves the claim that matters most for a fiat consumer
// untested: that a rail whose `destination` is a processor-side token —
// a post-checkout redirect URL, a buyer's email address, or a string the rail
// never reads at all — never tells a caller that string has been vetted as
// somewhere to send money.
//
// The invariant, from `patala-fiat/src/destination.rs`'s own module docs:
//
//	"the honest ceiling for a verdict here is Unknown — 'a human must decide'
//	 — and never StructurallyValid, which on a crypto rail means 'this is a
//	 well-formed address for the network this rail pays on'. Claiming it here
//	 would tell a caller that a success_url had been vetted as somewhere to
//	 send a customer's money. It has not, and it is not."
//
// So these tests do NOT assert "a fiat rail always says Unknown" — those rails
// do perform real, citable format checks on their own field, and a wrong-field
// paste is properly a refusal. They assert the ceiling: **no fiat rail may ever
// return StructurallyValid**, whatever you feed it, and every verdict it does
// return still requires a human and still carries the caveat.
//
// Offline, like everything else here: constructing a fiat rail dials nothing,
// and ValidateDestination is pure by contract.
//
// Requires a cdylib built with `--features fiat-all` — `make test-fiat`.

package bindingtest

import (
	"strings"
	"testing"

	patala "github.com/vul-os/patala/patala-go/bindings/patala"
)

// destinationProbes spans the shapes a fiat rail's `destination` is documented
// to be, plus the shapes someone might wrongly paste into it. No fiat rail may
// answer StructurallyValid to any of them.
var destinationProbes = []struct {
	name string
	dest string
}{
	{"a well-formed https redirect URL", "https://shop.example.com/orders/1234/thanks"},
	{"a plain http redirect URL", "http://localhost:3000/return"},
	{"a buyer email address", "buyer@example.com"},
	{"an opaque processor token", "cus_opaque_processor_token"},
	{"an account-looking token", "acct_1234567890"},
	// Deliberately a perfectly good Solana address: a fiat rail must not start
	// claiming things about it just because it parses somewhere else.
	{"a real-shaped Solana address", "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM"},
	{"a real-shaped Stellar address", "GAAZI4TCR3TY5OJHCTJC2A4QSY6CJWJH5IAJTGKIN2ER7LBNVKOCCWN7"},
}

// TestFiatRailsNeverClaimAStructurallyValidDestination is the ceiling
// assertion, run across every fiat provider the cdylib was built with — not a
// hand-picked one or two.
//
// StructurallyValid means "every check this rail can make offline passed" for
// the network this rail pays on. A fiat rail pays on no network and its
// destination is not an address, so there is no input for which that answer is
// true. If one of these rails ever returns it, a consumer's payout UI would
// show a customer a green tick over a string nobody validated as a destination.
func TestFiatRailsNeverClaimAStructurallyValidDestination(t *testing.T) {
	providers := patala.PatalaFiatProviders()
	if len(providers) == 0 {
		t.Fatal("PatalaFiatProviders() is empty; this test would assert nothing")
	}

	constructed := 0
	sawUnknown := 0
	for _, provider := range providers {
		rail, err := patala.PatalaRailNewFiat(provider, minimalFiatConfig(provider))
		if err != nil {
			// A provider we cannot build here is simply not covered. The
			// constructed>0 check below is what stops that from hollowing the
			// test out entirely — this is never a silent skip.
			continue
		}
		constructed++

		reachedUnknown := false
		for _, probe := range destinationProbes {
			v := rail.ValidateDestination(probe.dest)

			if v.Status == patala.DestinationStatusStructurallyValid {
				t.Errorf("%s.ValidateDestination(%s) = StructurallyValid; a fiat rail's destination is "+
					"a processor-side token, not an address, so no input may earn that verdict",
					provider, probe.name)
			}
			if v.Status == patala.DestinationStatusUnknown {
				reachedUnknown = true
			}

			// True on every verdict from every rail, whatever the status.
			if !v.HumanMustConfirm {
				t.Errorf("%s.ValidateDestination(%s).HumanMustConfirm = false", provider, probe.name)
			}
			if !strings.Contains(v.ExchangeDepositCaveat, "exchange") {
				t.Errorf("%s.ValidateDestination(%s) does not carry the exchange caveat", provider, probe.name)
			}
			if strings.TrimSpace(v.Reason) == "" {
				t.Errorf("%s.ValidateDestination(%s) has no reason to show anyone", provider, probe.name)
			}
			if v.RailId != rail.Id() {
				t.Errorf("%s.ValidateDestination(%s).RailId = %q, want %q",
					provider, probe.name, v.RailId, rail.Id())
			}
		}

		// The ceiling has to be *reachable*, not merely never exceeded: a rail
		// that refused every one of these probes would pass the assertion above
		// while being useless. At least one probe must land on Unknown — the
		// honest "this is the right shape, but a human must decide" answer.
		if reachedUnknown {
			sawUnknown++
		} else {
			t.Errorf("%s answered no probe with Unknown; a fiat rail must have some input it "+
				"accepts as the right shape while still handing the decision to a person", provider)
		}
	}

	if constructed == 0 {
		t.Fatal("no fiat rail could be constructed; this test verified nothing")
	}
	t.Logf("checked %d probes on each of %d/%d fiat providers; %d reached the Unknown ceiling",
		len(destinationProbes), constructed, len(providers), sawUnknown)
}

// TestFiatRailStillRefusesAnEmptyDestination: whatever else a fiat rail checks,
// it must not let the one defect decidable with no rail knowledge at all
// through. Guards fail closed — an empty destination is undeliverable on every
// rail there is.
func TestFiatRailStillRefusesAnEmptyDestination(t *testing.T) {
	rail, err := patala.PatalaRailNewFiat("manual", map[string]string{})
	if err != nil {
		t.Fatalf("PatalaRailNewFiat(\"manual\") failed: %v; manual is always-on and never feature-gated", err)
	}

	for _, blank := range []string{"", " ", "\t\n"} {
		v := rail.ValidateDestination(blank)
		if v.Status != patala.DestinationStatusMalformed {
			t.Errorf("ValidateDestination(%q).Status = %v, want Malformed", blank, v.Status)
		}
		if !v.IsRefusal {
			t.Errorf("ValidateDestination(%q).IsRefusal = false; a blank destination must be a refusal, not a shrug", blank)
		}
	}
}

// TestFiatDestinationSurfaceIsReachableFromGo. `PatalaRail` exposes no `Refund`
// method today, so on every rail reachable from Go the pre-flight check is the
// only destination-related call there is — and it must actually be callable on
// a fiat rail, not just on MockRail.
func TestFiatDestinationSurfaceIsReachableFromGo(t *testing.T) {
	rail, err := patala.PatalaRailNewFiat("manual", map[string]string{})
	if err != nil {
		t.Fatalf("PatalaRailNewFiat(\"manual\") failed: %v", err)
	}

	// If a Refund method is ever added to the binding, this file must be
	// revisited so the docs stop saying there is none.
	var _ interface {
		ValidateDestination(string) patala.DestinationVerdict
		Charge(patala.PayRequest) (patala.Receipt, error)
	} = rail

	v := rail.ValidateDestination("some-processor-token")
	if v.ExchangeDepositCaveat == "" {
		t.Error("a fiat rail's verdict carries no caveat; the human confirming the payout has nothing to read")
	}
	if v.ExchangeDepositCaveat != patala.ExchangeDepositCaveat() {
		t.Error("a fiat rail's caveat differs from the standalone accessor's text")
	}
}

// minimalFiatConfig returns the smallest config that might construct each
// adapter. Adapters needing something else simply fail to construct and are
// counted out above — this is a best-effort spread, not a credential store.
func minimalFiatConfig(provider string) map[string]string {
	return map[string]string{
		"secret_key":     "sk_test_go_binding_test",
		"api_key":        "go-binding-test-api-key",
		"public_key":     "go-binding-test-public-key",
		"webhook_secret": "go-binding-test-webhook-secret",
		"base_url":       "https://processor.invalid",
		"store_id":       "store-go-binding-test",
		"merchant_id":    "merchant-go-binding-test",
		"account_id":     "account-go-binding-test",
	}
}
