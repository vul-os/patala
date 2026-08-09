// Command sidecar drives patala from Go over the loopback HTTP API, using
// nothing but net/http and encoding/json.
//
//	CGO_ENABLED=0 go run ./examples/sidecar
//
// That flag is the point. This binary is pure Go and fully static: no cgo, no
// C toolchain, no shared library to ship next to it, and `GOOS=linux GOARCH=arm64
// go build` just works. ../direct cannot do any of that. For a Go project where
// "stays a single static executable" is a hard requirement — cackle is the one
// in this suite — that trade beats native types, and patala-go's own README
// says so first rather than burying it.
//
// It spawns patala-sidecar itself on a free loopback port with a freshly
// generated token, drives a full quote -> charge -> verify against MockRail,
// and shuts it down. Nothing is left running.
//
// Build the server first, from the workspace root:
//
//	cargo build -p patala-sidecar
package main

import (
	"bytes"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"time"
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

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, format+"\n", args...)
	os.Exit(1)
}

// repoRoot walks up from this source file's directory to the workspace root.
func repoRoot() string {
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		return "."
	}
	// .../sdks/go/examples/sidecar/main.go -> .../
	return filepath.Clean(filepath.Join(filepath.Dir(file), "..", "..", "..", ".."))
}

// resolveBinary: PATALA_SIDECAR_BIN, then a workspace build, then PATH.
func resolveBinary() string {
	if env := os.Getenv("PATALA_SIDECAR_BIN"); env != "" {
		return env
	}
	for _, profile := range []string{"debug", "release"} {
		candidate := filepath.Join(repoRoot(), "target", profile, "patala-sidecar")
		if info, err := os.Stat(candidate); err == nil && !info.IsDir() {
			return candidate
		}
	}
	return "patala-sidecar"
}

func freePort() int {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		fail("could not find a free port: %v", err)
	}
	defer listener.Close()
	return listener.Addr().(*net.TCPAddr).Port
}

type sidecar struct {
	base   string
	token  string
	binary string
	cmd    *exec.Cmd
	client *http.Client
}

func start() *sidecar {
	raw := make([]byte, 32)
	if _, err := rand.Read(raw); err != nil {
		fail("could not generate a token: %v", err)
	}
	sc := &sidecar{
		token:  hex.EncodeToString(raw),
		binary: resolveBinary(),
		client: &http.Client{Timeout: 10 * time.Second},
	}
	port := freePort()
	sc.base = fmt.Sprintf("http://127.0.0.1:%d", port)

	sc.cmd = exec.Command(sc.binary)
	sc.cmd.Env = append(os.Environ(),
		"PATALA_SIDECAR_TOKEN="+sc.token,
		fmt.Sprintf("PATALA_SIDECAR_PORT=%d", port),
	)
	if err := sc.cmd.Start(); err != nil {
		fail("could not run %q: %v\nBuild it first: cargo build -p patala-sidecar", sc.binary, err)
	}

	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		resp, err := http.Get(sc.base + "/healthz") //nolint:gosec // loopback, our own child
		if err == nil {
			body, _ := io.ReadAll(resp.Body)
			resp.Body.Close()
			if string(body) == "ok" {
				return sc
			}
		}
		time.Sleep(50 * time.Millisecond)
	}
	sc.stop()
	fail("patala-sidecar never became healthy")
	return nil
}

func (s *sidecar) stop() {
	if s.cmd != nil && s.cmd.Process != nil {
		_ = s.cmd.Process.Kill()
		_, _ = s.cmd.Process.Wait()
	}
}

// do performs one request. A 4xx/5xx comes back as a status plus a body rather
// than an error: on this API a non-2xx is an answer worth reading, not a
// transport failure. Pass authed=false to omit the bearer token.
func (s *sidecar) do(method, path string, body any, authed bool) (int, []byte) {
	var reader io.Reader
	if body != nil {
		encoded, err := json.Marshal(body)
		if err != nil {
			fail("could not encode the request: %v", err)
		}
		reader = bytes.NewReader(encoded)
	}
	req, err := http.NewRequest(method, s.base+path, reader)
	if err != nil {
		fail("could not build the request: %v", err)
	}
	if authed {
		req.Header.Set("Authorization", "Bearer "+s.token)
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := s.client.Do(req)
	if err != nil {
		fail("%s %s: %v", method, path, err)
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	return resp.StatusCode, raw
}

func decode[T any](raw []byte) T {
	var out T
	if err := json.Unmarshal(raw, &out); err != nil {
		fail("could not decode %q: %v", string(raw), err)
	}
	return out
}

type capabilities struct {
	Class      string   `json:"class"`
	HoldsFunds bool     `json:"holds_funds"`
	Reversible bool     `json:"reversible"`
	Currencies []string `json:"currencies"`
}

type verdict struct {
	Status           string `json:"status"`
	IsRefusal        bool   `json:"is_refusal"`
	HumanMustConfirm bool   `json:"human_must_confirm"`
}

type quote struct {
	TotalMinor uint64 `json:"total_minor"`
}

// receipt keeps `proof` as json.RawMessage on purpose: a receipt is an opaque
// token to hand back to /verify unchanged, and re-encoding a field you have
// modelled loosely is how a genuine receipt stops verifying.
type receipt struct {
	RailID        string          `json:"rail_id"`
	AmountMinor   uint64          `json:"amount_minor"`
	Currency      string          `json:"currency"`
	Reference     string          `json:"reference"`
	Proof         json.RawMessage `json:"proof"`
	SettledAtUnix int64           `json:"settled_at_unix"`
}

type apiError struct {
	Error string `json:"error"`
	Kind  string `json:"kind"`
}

func main() {
	sc := start()
	defer sc.stop()

	fmt.Printf("binary:   %s\n", sc.binary)
	fmt.Printf("listening on %s (loopback only — the bind address is not configurable)\n", sc.base)
	fmt.Printf("go %s on %s/%s, CGO_ENABLED is irrelevant here\n\n",
		runtime.Version(), runtime.GOOS, runtime.GOARCH)

	fmt.Println("capabilities")
	status, raw := sc.do(http.MethodGet, "/v1/rails/mock", nil, true)
	check(status == 200, fmt.Sprintf("GET /v1/rails/mock -> %d", status))
	caps := decode[capabilities](raw)
	check(caps.Class == "NonCustodialFinal",
		fmt.Sprintf("class is %q — decide the whole UX off this, not off a provider name", caps.Class))
	check(!caps.HoldsFunds, "holds_funds is false")

	fmt.Println("\npre-flight: validate-destination, before any money moves")
	status, raw = sc.do(http.MethodPost, "/v1/rails/mock/validate-destination",
		map[string]string{"destination": "mock:wallet:alice"}, true)
	good := decode[verdict](raw)
	check(status == 200 && good.Status == "StructurallyValid",
		fmt.Sprintf("a well-formed address -> %d %q", status, good.Status))
	check(!good.IsRefusal, "is_refusal is false — read the body, not just the status code")
	check(good.HumanMustConfirm, "human_must_confirm is true even on StructurallyValid")

	status, raw = sc.do(http.MethodPost, "/v1/rails/mock/validate-destination",
		map[string]string{"destination": ""}, true)
	bad := decode[verdict](raw)
	check(status == 200 && bad.Status == "Malformed" && bad.IsRefusal,
		"an empty destination is a well-formed REQUEST -> 200 with a Malformed refusal")

	fmt.Println("\nquote -> charge -> verify")
	pay := map[string]any{
		"amount_minor": 1250, "currency": "USDC",
		"destination": "mock:wallet:alice", "reference": "order-1",
	}
	status, raw = sc.do(http.MethodPost, "/v1/rails/mock/quote", pay, true)
	q := decode[quote](raw)
	check(status == 200 && q.TotalMinor == 1250,
		fmt.Sprintf("total_minor == %d, decoded into a uint64 — minor units, never a float", q.TotalMinor))

	status, raw = sc.do(http.MethodPost, "/v1/rails/mock/charge", pay, true)
	rcpt := decode[receipt](raw)
	check(status == 200 && rcpt.AmountMinor == 1250,
		fmt.Sprintf("charge -> receipt for %d %s", rcpt.AmountMinor, rcpt.Currency))

	status, raw = sc.do(http.MethodPost, "/v1/rails/mock/verify", rcpt, true)
	valid := decode[struct {
		Valid bool `json:"valid"`
	}](raw)
	check(status == 200 && valid.Valid, "the genuine receipt verifies {\"valid\": true}")

	tampered := rcpt
	tampered.AmountMinor++
	status, raw = sc.do(http.MethodPost, "/v1/rails/mock/verify", tampered, true)
	valid = decode[struct {
		Valid bool `json:"valid"`
	}](raw)
	check(status == 200 && !valid.Valid,
		"a tampered receipt is 200 {\"valid\": false} — fail-closed, and NOT an HTTP error")

	fmt.Println("\nthe error surface, so you can tell these four apart")
	badPay := map[string]any{
		"amount_minor": 1250, "currency": "EUR",
		"destination": "mock:wallet:alice", "reference": "order-1",
	}
	status, raw = sc.do(http.MethodPost, "/v1/rails/mock/charge", badPay, true)
	check(status == 400, fmt.Sprintf("an unsupported currency -> %d %q", status, decode[apiError](raw).Kind))

	status, raw = sc.do(http.MethodGet, "/v1/rails/nope", nil, true)
	check(status == 404, fmt.Sprintf("an unknown rail_id -> %d %q", status, decode[apiError](raw).Kind))

	status, raw = sc.do(http.MethodPost, "/v1/rails/mock/webhook", map[string]any{}, true)
	check(status == 501, fmt.Sprintf("the mock has no push delivery -> %d %q, never an invented event",
		status, decode[apiError](raw).Kind))

	status, _ = sc.do(http.MethodGet, "/v1/rails/mock", nil, false)
	check(status == 401, fmt.Sprintf("no Authorization header -> %d on a READ-ONLY route too", status))

	sc.stop()
	fmt.Println("\nsidecar terminated; nothing left running")
	fmt.Printf("\nALL %d GO SIDECAR ASSERTIONS PASSED\n", checks)
}
