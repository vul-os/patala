//go:build fiat

// The feature-gated half of the Go binding suite: `patala-fiat`'s by-name
// registry constructor and, above all, `PatalaRail.VerifyWebhook` driven
// against genuinely signed deliveries through the real compiled cdylib.
//
// This is where the WebhookStatus mapping is proven rather than asserted
// about constants. webhook_status_test.go pins the numbers; this file proves
// each number is reached by the delivery that actually means it:
//
//	Settled     <- a Stripe checkout.session.completed with payment_status "paid"
//	NotSettled  <- the same session with payment_status "unpaid"
//	Unconfirmed <- a BTCPay invoice webhook, which by design carries no
//	               settlement claim at all
//
// Every signature here is computed in Go and verified in Rust, offline: a
// webhook signature check is local by construction, so nothing dials a
// processor. No rail is ever charged or verified against a live API.
//
// Requires a cdylib built with `--features fiat-all` — `make test-fiat`.

package bindingtest

import (
	"bytes"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"

	"github.com/vul-os/patala/patala-go/bindings/patala"
)

const (
	stripeWebhookSecret = "whsec_go_binding_test"
	btcpayWebhookSecret = "btcpay-go-binding-test-secret"
	// A fixed clock, so replay-window checks are reproducible rather than
	// dependent on when the suite happens to run.
	testNowUnix = uint64(1_700_000_000)
)

// ---- helpers ---------------------------------------------------------------

func hmacSHA256Hex(secret string, payload []byte) string {
	mac := hmac.New(sha256.New, []byte(secret))
	mac.Write(payload)
	return hex.EncodeToString(mac.Sum(nil))
}

// stripeSignature reproduces Stripe's documented scheme: HMAC-SHA256 over
// "{timestamp}.{raw body}", hex, under the endpoint's whsec_ secret.
func stripeSignature(secret string, ts uint64, body []byte) string {
	payload := append([]byte(fmt.Sprintf("%d.", ts)), body...)
	return fmt.Sprintf("t=%d,v1=%s", ts, hmacSHA256Hex(secret, payload))
}

func stripeSessionBody(eventID, sessionID, reference, currency string, amountTotal uint64, paymentStatus string) []byte {
	return []byte(fmt.Sprintf(
		`{"id":%q,"type":"checkout.session.completed","data":{"object":{"id":%q,`+
			`"payment_status":%q,"amount_total":%d,"currency":%q,"client_reference_id":%q}}}`,
		eventID, sessionID, paymentStatus, amountTotal, currency, reference))
}

func stripeDelivery(t *testing.T, body []byte, now uint64) patala.WebhookDelivery {
	t.Helper()
	return patala.WebhookDelivery{
		RawBody: body,
		Headers: map[string]string{
			// Deliberately not lower-cased: header lookup is
			// case-insensitive on the Rust side, and a Go caller forwarding
			// `http.Header` gets canonical casing. If that ever regressed,
			// every genuine delivery would be rejected.
			"Stripe-Signature": stripeSignature(stripeWebhookSecret, now, body),
		},
		Query:   nil,
		NowUnix: now,
	}
}

func newStripeRail(t *testing.T) *patala.PatalaRail {
	t.Helper()
	rail, err := patala.PatalaRailNewFiat("stripe", map[string]string{
		"secret_key":      "sk_test_go_binding_test",
		"webhook_secret":  stripeWebhookSecret,
		"requires_kyc":    "true",
		"settlement_days": "2",
	})
	if err != nil {
		// Not a skip. `make test-fiat` builds the cdylib with fiat-all, so a
		// missing stripe adapter here means the build that produced these
		// bindings was wrong — silently skipping would be the false green
		// this whole file exists to remove.
		t.Fatalf("PatalaRailNewFiat(\"stripe\") failed: %v\n"+
			"these bindings were generated from a cdylib without --features fiat-stripe; "+
			"regenerate with `make FEATURES=fiat-all generate`", err)
	}
	return rail
}

func newBTCPayRail(t *testing.T) *patala.PatalaRail {
	t.Helper()
	rail, err := patala.PatalaRailNewFiat("btcpay", map[string]string{
		"base_url":       "https://btcpay.invalid",
		"api_key":        "go-binding-test-api-key",
		"store_id":       "store-go-binding-test",
		"webhook_secret": btcpayWebhookSecret,
	})
	if err != nil {
		t.Fatalf("PatalaRailNewFiat(\"btcpay\") failed: %v\n"+
			"these bindings were generated from a cdylib without --features fiat-btcpay; "+
			"regenerate with `make FEATURES=fiat-all generate`", err)
	}
	return rail
}

// ---- WebhookStatus, proven per variant through the real cdylib -------------

// TestStripeWebhookPaidSessionIsSettled is the only path that may produce
// WebhookStatusSettled.
func TestStripeWebhookPaidSessionIsSettled(t *testing.T) {
	rail := newStripeRail(t)
	body := stripeSessionBody("evt_go_paid", "cs_go_paid", "go-order-paid", "usd", 5000, "paid")

	event, err := rail.VerifyWebhook(stripeDelivery(t, body, testNowUnix))
	if err != nil {
		t.Fatalf("VerifyWebhook() on a genuinely signed delivery returned an error: %v", err)
	}

	if event.Status != patala.WebhookStatusSettled {
		t.Fatalf("Status = %d, want WebhookStatusSettled (%d)", event.Status, patala.WebhookStatusSettled)
	}
	if !treatAsPaid(event.Status) {
		t.Fatal("a paid Stripe session did not read as paid")
	}
	if event.RailId != "stripe" {
		t.Errorf("RailId = %q, want %q", event.RailId, "stripe")
	}
	// The dedup key must be Stripe's own event id, not the session id: two
	// different events about one session must never collapse onto one key.
	if event.EventId != "evt_go_paid" {
		t.Errorf("EventId = %q, want %q", event.EventId, "evt_go_paid")
	}
	if event.Reference != "go-order-paid" {
		t.Errorf("Reference = %q, want %q", event.Reference, "go-order-paid")
	}
	if event.AmountMinor != 5000 {
		t.Errorf("AmountMinor = %d, want 5000 (minor units, uint64)", event.AmountMinor)
	}
	if event.Currency != "USD" {
		t.Errorf("Currency = %q, want %q (normalised upper-case)", event.Currency, "USD")
	}
}

// TestStripeWebhookUnpaidSessionIsNotSettled covers the variant a naive
// implementation gets wrong: Stripe sends `checkout.session.completed` for a
// session whose `payment_status` is still `unpaid`, and that delivery carries
// the session's full `amount_total`. Nobody paid it.
func TestStripeWebhookUnpaidSessionIsNotSettled(t *testing.T) {
	rail := newStripeRail(t)
	body := stripeSessionBody("evt_go_unpaid", "cs_go_unpaid", "go-order-unpaid", "usd", 5000, "unpaid")

	event, err := rail.VerifyWebhook(stripeDelivery(t, body, testNowUnix))
	if err != nil {
		t.Fatalf("VerifyWebhook() error = %v; an unpaid session is still an AUTHENTIC delivery", err)
	}

	if event.Status != patala.WebhookStatusNotSettled {
		t.Fatalf("Status = %d, want WebhookStatusNotSettled (%d)", event.Status, patala.WebhookStatusNotSettled)
	}
	if treatAsPaid(event.Status) {
		t.Fatal("an unpaid Stripe session read as paid")
	}
	// The money fields must be cleared, so a caller that reads AmountMinor
	// without first checking Status cannot read a number nobody paid.
	if event.AmountMinor != 0 {
		t.Errorf("AmountMinor = %d, want 0 on a NotSettled event (the body carried amount_total=5000)", event.AmountMinor)
	}
	if event.Currency != "" {
		t.Errorf("Currency = %q, want empty on a NotSettled event", event.Currency)
	}
}

// TestBTCPayWebhookIsUnconfirmedNeverSettled is the assertion this whole file
// was written for.
//
// BTCPay's webhook body is deliberately NOT trusted for settlement: the rail
// verifies the signature, extracts only the invoice id, and reports
// Unconfirmed. That means "this delivery is genuine and says nothing about
// money" — PENDING-equivalent. A consumer must take ObjectId, find its own
// stored receipt for that invoice, and call Verify.
//
// If a future binding regeneration renumbered the enum such that this arrived
// as Settled, cackle would mark an unpaid order paid on a delivery that
// asserts nothing at all. This test is what stops that reaching a release.
func TestBTCPayWebhookIsUnconfirmedNeverSettled(t *testing.T) {
	rail := newBTCPayRail(t)
	body := []byte(`{"deliveryId":"del_go_1","webhookId":"wh_go_1",` +
		`"type":"InvoiceSettled","invoiceId":"inv_go_1","storeId":"store-go-binding-test"}`)

	delivery := patala.WebhookDelivery{
		RawBody: body,
		Headers: map[string]string{
			"BTCPay-Sig": "sha256=" + hmacSHA256Hex(btcpayWebhookSecret, body),
		},
		Query:   nil,
		NowUnix: testNowUnix,
	}

	event, err := rail.VerifyWebhook(delivery)
	if err != nil {
		t.Fatalf("VerifyWebhook() on a genuinely signed BTCPay delivery returned an error: %v", err)
	}

	if event.Status != patala.WebhookStatusUnconfirmed {
		t.Fatalf("Status = %d, want WebhookStatusUnconfirmed (%d).\n"+
			"BTCPay's webhook makes NO settlement claim; anything else here is a mis-mapping, and "+
			"WebhookStatusSettled in particular would mark an unpaid order paid.",
			event.Status, patala.WebhookStatusUnconfirmed)
	}
	if treatAsPaid(event.Status) {
		t.Fatal("an Unconfirmed webhook read as paid — this is the exact regression this test exists for")
	}
	// PENDING-equivalent means: no money asserted, and a lookup key given.
	if event.AmountMinor != 0 {
		t.Errorf("AmountMinor = %d, want 0 — an Unconfirmed event asserts no amount", event.AmountMinor)
	}
	if event.Currency != "" {
		t.Errorf("Currency = %q, want empty — an Unconfirmed event asserts no currency", event.Currency)
	}
	if event.ObjectId != "inv_go_1" {
		t.Errorf("ObjectId = %q, want %q — this is the key a consumer re-verifies with", event.ObjectId, "inv_go_1")
	}
	if event.EventId == "" {
		t.Error("EventId is empty; a consumer cannot dedup a delivery it cannot name")
	}
	if event.RailId != "btcpay" {
		t.Errorf("RailId = %q, want %q", event.RailId, "btcpay")
	}
}

// TestEveryWebhookStatusVariantIsReachedByARealDelivery asserts the three
// tests above between them actually observed all three variants through the
// FFI. Without this, deleting one of them (or having it silently stop running
// under a build tag) would leave the pinning half-proven and nothing would
// say so.
func TestEveryWebhookStatusVariantIsReachedByARealDelivery(t *testing.T) {
	seen := map[patala.WebhookStatus]string{}

	stripe := newStripeRail(t)
	paid, err := stripe.VerifyWebhook(stripeDelivery(t,
		stripeSessionBody("evt_cov_1", "cs_cov_1", "go-order-cov-1", "usd", 1200, "paid"), testNowUnix))
	if err != nil {
		t.Fatalf("stripe paid delivery: %v", err)
	}
	seen[paid.Status] = "stripe/paid"

	unpaid, err := stripe.VerifyWebhook(stripeDelivery(t,
		stripeSessionBody("evt_cov_2", "cs_cov_2", "go-order-cov-2", "usd", 1200, "unpaid"), testNowUnix))
	if err != nil {
		t.Fatalf("stripe unpaid delivery: %v", err)
	}
	seen[unpaid.Status] = "stripe/unpaid"

	btcpayBody := []byte(`{"type":"InvoiceProcessing","invoiceId":"inv_cov_1"}`)
	btcpay, err := newBTCPayRail(t).VerifyWebhook(patala.WebhookDelivery{
		RawBody: btcpayBody,
		Headers: map[string]string{"BTCPay-Sig": "sha256=" + hmacSHA256Hex(btcpayWebhookSecret, btcpayBody)},
		NowUnix: testNowUnix,
	})
	if err != nil {
		t.Fatalf("btcpay delivery: %v", err)
	}
	seen[btcpay.Status] = "btcpay/invoice"

	for _, want := range []struct {
		status patala.WebhookStatus
		name   string
	}{
		{patala.WebhookStatusSettled, "WebhookStatusSettled"},
		{patala.WebhookStatusNotSettled, "WebhookStatusNotSettled"},
		{patala.WebhookStatusUnconfirmed, "WebhookStatusUnconfirmed"},
	} {
		if _, ok := seen[want.status]; !ok {
			t.Errorf("NOT VERIFIED: no real delivery produced %s — the mapping for that variant is unproven", want.name)
		}
	}
	if len(seen) != 3 {
		t.Fatalf("observed %d distinct WebhookStatus values across three deliveries that must differ: %v.\n"+
			"Two variants collapsing onto one number is precisely the renumbering failure this suite guards.",
			len(seen), seen)
	}
}

// ---- webhook verification fails closed ------------------------------------

func TestStripeWebhookFailsClosed(t *testing.T) {
	rail := newStripeRail(t)
	body := stripeSessionBody("evt_go_fc", "cs_go_fc", "go-order-fc", "usd", 5000, "paid")
	good := stripeDelivery(t, body, testNowUnix)

	for _, tc := range []struct {
		name  string
		build func() patala.WebhookDelivery
	}{
		{"tampered body under a valid signature", func() patala.WebhookDelivery {
			d := good
			d.RawBody = bytes.Replace(body, []byte("5000"), []byte("500000"), 1)
			return d
		}},
		{"missing signature header", func() patala.WebhookDelivery {
			d := good
			d.Headers = map[string]string{}
			return d
		}},
		{"signature from the wrong secret", func() patala.WebhookDelivery {
			d := good
			d.Headers = map[string]string{"Stripe-Signature": stripeSignature("whsec_attacker", testNowUnix, body)}
			return d
		}},
		{"replayed outside the tolerance window", func() patala.WebhookDelivery {
			// Correctly signed for its own timestamp, but an hour stale —
			// Stripe's documented 5-minute window must reject it.
			d := stripeDelivery(t, body, testNowUnix)
			d.NowUnix = testNowUnix + 3600
			return d
		}},
		{"empty body", func() patala.WebhookDelivery {
			d := good
			d.RawBody = nil
			return d
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			event, err := rail.VerifyWebhook(tc.build())
			if err == nil {
				t.Fatalf("VerifyWebhook() accepted a delivery it must reject; got event %+v", event)
			}
			if !errors.Is(err, patala.ErrPatalaErrorInvalidRequest) {
				t.Fatalf("error = %v (%T), want ErrPatalaErrorInvalidRequest", err, err)
			}
		})
	}
}

func TestBTCPayWebhookFailsClosed(t *testing.T) {
	rail := newBTCPayRail(t)
	body := []byte(`{"type":"InvoiceSettled","invoiceId":"inv_go_fc"}`)
	sig := "sha256=" + hmacSHA256Hex(btcpayWebhookSecret, body)

	for _, tc := range []struct {
		name    string
		body    []byte
		sigHdr  string
		present bool
	}{
		{"tampered body", []byte(`{"type":"InvoiceSettled","invoiceId":"inv_attacker"}`), sig, true},
		{"wrong secret", body, "sha256=" + hmacSHA256Hex("not-the-secret", body), true},
		{"unprefixed signature", body, hmacSHA256Hex(btcpayWebhookSecret, body), true},
		{"missing header", body, "", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			headers := map[string]string{}
			if tc.present {
				headers["BTCPay-Sig"] = tc.sigHdr
			}
			event, err := rail.VerifyWebhook(patala.WebhookDelivery{
				RawBody: tc.body,
				Headers: headers,
				NowUnix: testNowUnix,
			})
			if err == nil {
				t.Fatalf("VerifyWebhook() accepted a forged BTCPay delivery; got event %+v", event)
			}
		})
	}
}

// TestManualRailReportsWebhookUnsupported: `manual` has no processor, so
// there is no signature to check and no honest event to return.
func TestManualRailReportsWebhookUnsupported(t *testing.T) {
	rail, err := patala.PatalaRailNewFiat("manual", map[string]string{})
	if err != nil {
		t.Fatalf("PatalaRailNewFiat(\"manual\") failed: %v (manual is always compiled in)", err)
	}
	if _, err := rail.VerifyWebhook(patala.WebhookDelivery{
		RawBody: []byte(`{}`),
		Headers: map[string]string{},
		NowUnix: testNowUnix,
	}); err == nil {
		t.Fatal("VerifyWebhook() on `manual` returned no error; a rail with no processor must report Unsupported")
	} else if !errors.Is(err, patala.ErrPatalaErrorUnsupported) {
		t.Fatalf("error = %v (%T), want ErrPatalaErrorUnsupported", err, err)
	}
}

// ---- the by-name registry surface -----------------------------------------

// TestFiatProviderCoverage is the coverage-count assertion for this half of
// the suite: the cdylib these bindings came from must expose `manual` plus
// exactly one provider per patala-fiat/src/<name>/ module directory.
//
// scripts/check-features.sh already checks the three Cargo manifests agree.
// This checks the thing that actually shipped: the COMPILED cdylib. A
// processor that exists in patala-fiat, is listed in fiat-all, and still fails
// to reach the Go binding would pass that script and fail here.
func TestFiatProviderCoverage(t *testing.T) {
	providers := patala.PatalaFiatProviders()
	got := make(map[string]bool, len(providers))
	for _, p := range providers {
		got[p] = true
	}

	if !got["manual"] {
		t.Errorf("PatalaFiatProviders() omits \"manual\", which is always-on and never feature-gated; got %v", providers)
	}

	srcDir, err := filepath.Abs("../../patala-fiat/src")
	if err != nil {
		t.Fatalf("resolving patala-fiat/src: %v", err)
	}
	entries, err := os.ReadDir(srcDir)
	if err != nil {
		// In-repo path; absent means a broken checkout, not an optional
		// feature. Failing here is deliberate — skipping would let the
		// count assertion below silently disappear.
		t.Fatalf("reading %s: %v\nthis test cross-checks the compiled cdylib against patala-fiat's module directories", srcDir, err)
	}
	var wantProcessors []string
	for _, e := range entries {
		if e.IsDir() {
			wantProcessors = append(wantProcessors, e.Name())
		}
	}
	sort.Strings(wantProcessors)
	if len(wantProcessors) == 0 {
		t.Fatalf("found no processor directories under %s; the cross-check below would assert nothing", srcDir)
	}

	var missing []string
	for _, p := range wantProcessors {
		if !got[p] {
			missing = append(missing, p)
		}
	}
	if len(missing) > 0 {
		t.Errorf("NOT REACHABLE FROM GO: %v.\n"+
			"patala-fiat/src/ has %d processor directories; the cdylib these bindings were generated from "+
			"exposes %d providers (%v). Rebuild with `make FEATURES=fiat-all generate`, and if that does "+
			"not fix it the adapter is missing from patala-py's fiat-all feature.",
			missing, len(wantProcessors), len(providers), providers)
	}

	// manual + one per processor directory, and nothing extra.
	if want := len(wantProcessors) + 1; len(providers) != want {
		t.Errorf("PatalaFiatProviders() returned %d providers, want %d (manual + %d processors): %v",
			len(providers), want, len(wantProcessors), providers)
	}
}

func TestFiatUnknownProviderIsATypedError(t *testing.T) {
	_, err := patala.PatalaRailNewFiat("not-a-real-processor", map[string]string{})
	if err == nil {
		t.Fatal("PatalaRailNewFiat() with an unknown provider returned no error; an unrecognised name must never fall back to some default rail")
	}
	if !errors.Is(err, patala.ErrPatalaErrorInvalidRequest) {
		t.Fatalf("error = %v (%T), want ErrPatalaErrorInvalidRequest", err, err)
	}
}

func TestFiatProviderNameIsCaseInsensitive(t *testing.T) {
	rail, err := patala.PatalaRailNewFiat("MANUAL", map[string]string{})
	if err != nil {
		t.Fatalf("PatalaRailNewFiat(\"MANUAL\") failed: %v", err)
	}
	if rail.Id() != "manual" {
		t.Errorf("Id() = %q, want %q — the rail id must be the canonical name, not the caller's casing", rail.Id(), "manual")
	}
}

// TestStripeConstructionOnlyCapabilities constructs (never charges, never
// verifies — either would dial Stripe's real API) a feature-gated processor
// adapter to prove the config map -> typed Config -> real Rail path and that
// its class crosses the FFI intact. Getting this wrong the other way —
// reporting a custodial processor as NonCustodialFinal — would tell a caller
// the payment is irreversible when it is not.
func TestStripeConstructionOnlyCapabilities(t *testing.T) {
	rail := newStripeRail(t)
	if rail.Id() != "stripe" {
		t.Errorf("Id() = %q, want %q", rail.Id(), "stripe")
	}
	caps := rail.Capabilities()
	if caps.Class != patala.RailClassCustodialReversible {
		t.Errorf("Capabilities().Class = %d, want RailClassCustodialReversible (%d)", caps.Class, patala.RailClassCustodialReversible)
	}
	if !caps.HoldsFunds {
		t.Error("Capabilities().HoldsFunds = false; Stripe (the processor) custodies funds in flight")
	}
	if !caps.Reversible {
		t.Error("Capabilities().Reversible = false; a card processor supports chargebacks/refunds")
	}
	if !caps.RequiresKyc {
		t.Error("Capabilities().RequiresKyc = false, but requires_kyc=\"true\" was passed in the config map")
	}
}

// TestFiatConfigRejectsNonNumericValues: every config value is a string
// (UniFFI HashMap<String,String>), so a numeric field is parsed on the Rust
// side and a bad value must be a typed error rather than a silent default.
func TestFiatConfigRejectsNonNumericValues(t *testing.T) {
	_, err := patala.PatalaRailNewFiat("btcpay", map[string]string{
		"base_url":           "https://btcpay.invalid",
		"api_key":            "k",
		"store_id":           "s",
		"webhook_secret":     "w",
		"settlement_seconds": "not-a-number",
	})
	if err == nil {
		t.Fatal("PatalaRailNewFiat() accepted settlement_seconds=\"not-a-number\"; a bad numeric config value must not silently default")
	}
	if !errors.Is(err, patala.ErrPatalaErrorInvalidRequest) {
		t.Fatalf("error = %v (%T), want ErrPatalaErrorInvalidRequest", err, err)
	}
	if !strings.Contains(err.Error(), "settlement_seconds") {
		t.Errorf("error %q does not name the offending config key", err.Error())
	}
}
