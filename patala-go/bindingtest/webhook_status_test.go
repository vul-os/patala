// WebhookStatus is money-critical and crosses the FFI boundary as a bare
// integer. This file pins it.
//
// Why this exists, concretely: `patala_core::WebhookStatus` has three
// variants and UniFFI lowers them to their ordinal position — Settled=1,
// NotSettled=2, Unconfirmed=3. Nothing in the generated Go names the Rust
// variant it came from at runtime; a `WebhookStatus` arriving over the FFI is
// just a `uint`. So if someone reorders the Rust enum (adds a variant at the
// top, sorts them alphabetically, splits `NotSettled`), regeneration produces
// a Go file where `WebhookStatusUnconfirmed` is a DIFFERENT number, every
// existing Go call site still compiles, and a delivery that means
// "authentic, but says nothing about money" starts arriving as the constant
// a consumer gates entitlement on.
//
// cackle consumes these bindings. `Unconfirmed` flipping to the settled
// constant marks unpaid orders as paid. That is the failure this file has to
// make impossible to ship silently, so it pins the mapping three ways:
//
//  1. The Go constants hold their exact numeric values (below).
//  2. The GENERATED SOURCE declares exactly those three variants and no
//     others — an added or removed variant fails, not just a renumbered one
//     (TestGeneratedWebhookStatusVariantsAreExactlyThree).
//  3. Real signed deliveries are driven through the real cdylib and their
//     status asserted per variant (fiat_webhook_test.go, `-tags fiat`). A
//     constant pin alone cannot catch a rail that maps its own outcome to the
//     wrong variant on the Rust side; a live round-trip can.
//
// (1) and (2) need no feature-gated cdylib and run under a plain `make test`.

package bindingtest

import (
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"testing"

	patala "github.com/vul-os/patala/patala-go/bindings/patala"
)

// generatedBindingsPath is the file `make generate` writes. It is build
// output (gitignored), so it only exists once the Makefile has run — which is
// also the only way these tests can have been compiled at all.
const generatedBindingsPath = "../bindings/patala/patala_py.go"

// treatAsPaid is the ONE decision a consumer is allowed to make from a
// WebhookStatus, stated here so the table below can assert it per variant.
// It mirrors `patala_core::WebhookEvent::is_settled` exactly: only Settled.
func treatAsPaid(s patala.WebhookStatus) bool {
	return s == patala.WebhookStatusSettled
}

// TestWebhookStatusDiscriminantsArePinned nails the wire values.
func TestWebhookStatusDiscriminantsArePinned(t *testing.T) {
	for _, tc := range []struct {
		name string
		got  patala.WebhookStatus
		want int
	}{
		{"WebhookStatusSettled", patala.WebhookStatusSettled, 1},
		{"WebhookStatusNotSettled", patala.WebhookStatusNotSettled, 2},
		{"WebhookStatusUnconfirmed", patala.WebhookStatusUnconfirmed, 3},
	} {
		if int(tc.got) != tc.want {
			t.Errorf("%s = %d, want %d — the Rust enum's variant ORDER changed; "+
				"every Go consumer's WebhookStatus comparison now means something else",
				tc.name, int(tc.got), tc.want)
		}
	}
}

// TestOnlySettledMeansPaid is the assertion cackle's correctness rests on.
func TestOnlySettledMeansPaid(t *testing.T) {
	for _, tc := range []struct {
		name string
		s    patala.WebhookStatus
		paid bool
	}{
		{"Settled", patala.WebhookStatusSettled, true},
		// The rail affirmatively established the payment did not happen.
		{"NotSettled", patala.WebhookStatusNotSettled, false},
		// The delivery is genuine but carries no settlement claim at all
		// (BTCPay, Coinbase Commerce, OpenNode, LNbits, Mollie). This is
		// PENDING-equivalent: look up your own record by ObjectId and call
		// Verify. It is NEVER paid.
		{"Unconfirmed", patala.WebhookStatusUnconfirmed, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := treatAsPaid(tc.s); got != tc.paid {
				t.Fatalf("treatAsPaid(%s) = %v, want %v", tc.name, got, tc.paid)
			}
		})
	}

	// Stated separately from the table so the specific confusion this guards
	// against is named rather than implied by a row.
	if patala.WebhookStatusUnconfirmed == patala.WebhookStatusSettled {
		t.Fatal("WebhookStatusUnconfirmed == WebhookStatusSettled: an authentic-but-says-nothing " +
			"delivery would be read as payment")
	}
	if patala.WebhookStatusUnconfirmed == patala.WebhookStatusNotSettled {
		t.Fatal("WebhookStatusUnconfirmed == WebhookStatusNotSettled: 'the rail cannot say' and " +
			"'the rail says no' must not collapse (patala_core::WebhookStatus docs)")
	}
}

// TestZeroValueWebhookStatusIsNotSettled covers Go's own footgun rather than
// UniFFI's. `var s patala.WebhookStatus` is 0, and a zero-valued WebhookEvent
// (from a struct a caller forgot to populate, or a future binding bug) must
// not read as paid. UniFFI numbers variants from 1 precisely so 0 is never a
// valid variant; this asserts that stays true.
func TestZeroValueWebhookStatusIsNotSettled(t *testing.T) {
	var zero patala.WebhookStatus
	if treatAsPaid(zero) {
		t.Fatal("the zero value of WebhookStatus reads as paid; variant numbering must start at 1")
	}
	var zeroEvent patala.WebhookEvent
	if treatAsPaid(zeroEvent.Status) {
		t.Fatal("a zero-valued WebhookEvent reads as paid")
	}
}

var webhookStatusConstRe = regexp.MustCompile(`(?m)^\s*WebhookStatus(\w+)\s+WebhookStatus\s*=\s*(\d+)\s*$`)

// TestGeneratedWebhookStatusVariantsAreExactlyThree reads the generated
// source and asserts the variant set itself, not just the values of the three
// constants this file happens to name. A regeneration that ADDS a variant
// (say `Refunded`) would leave every assertion above passing while shifting
// nothing — but it also means a status exists that no Go consumer has decided
// how to treat, and `treatAsPaid`'s exhaustiveness claim is no longer true.
// Go has no compile-time enum exhaustiveness, so this is the substitute.
func TestGeneratedWebhookStatusVariantsAreExactlyThree(t *testing.T) {
	path, err := filepath.Abs(generatedBindingsPath)
	if err != nil {
		t.Fatalf("resolving %s: %v", generatedBindingsPath, err)
	}
	src, err := os.ReadFile(path)
	if err != nil {
		// Not a skip: these tests only compile when the bindings exist, so
		// an unreadable generated file is a broken checkout, not an absent
		// optional feature.
		t.Fatalf("reading generated bindings at %s: %v\n"+
			"this file is produced by `make generate`; it must exist for the binding tests to be meaningful", path, err)
	}

	matches := webhookStatusConstRe.FindAllStringSubmatch(string(src), -1)
	got := make(map[string]string, len(matches))
	for _, m := range matches {
		got[m[1]] = m[2]
	}

	want := map[string]string{
		"Settled":     "1",
		"NotSettled":  "2",
		"Unconfirmed": "3",
	}

	if len(got) != len(want) {
		t.Fatalf("generated bindings declare %d WebhookStatus variants (%v), want exactly %d (%v).\n"+
			"A variant was added, removed or renamed in patala_core::WebhookStatus. Decide how Go "+
			"consumers must treat it (cackle gates entitlement on this), update treatAsPaid and this "+
			"table together, and re-read patala-core/src/webhook.rs's docs before doing so.",
			len(got), sortedKeys(got), len(want), sortedKeys(want))
	}
	for name, wantVal := range want {
		gotVal, ok := got[name]
		if !ok {
			t.Fatalf("generated bindings have no WebhookStatus%s constant; variants present: %v", name, sortedKeys(got))
		}
		if gotVal != wantVal {
			t.Errorf("generated WebhookStatus%s = %s, want %s (variant order changed in the Rust enum)", name, gotVal, wantVal)
		}
	}
}

func sortedKeys(m map[string]string) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}
