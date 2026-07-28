// Package bindingtest is the Go test suite for patala's UniFFI-generated
// bindings.
//
// It exists as its own package for one reason: the generated bindings
// themselves live in ../bindings/patala/, which `make generate` deletes and
// recreates on every run (`rm -rf $(BINDINGS_DIR)`), so a `_test.go` placed
// beside them would be destroyed by the next generation. This package sits
// outside that directory, imports the generated package like any other
// consumer would, and therefore tests exactly what a downstream Go caller
// (cackle) sees.
//
// Before this package existed, `patala-go`'s only executable checks were the
// two `package main` programs under ../examples/, run as binaries via
// `go run`. `go test ./...` found no test files anywhere in the module and
// exited 0 — a make target named `test` that reported success having verified
// nothing. Everything the examples assert is asserted here too, as real
// `testing.T` tests, plus the webhook-status pinning the examples never had.
//
// The file split mirrors what each test needs from the cdylib:
//
//   - binding_test.go and webhook_status_test.go have NO build tag: they run
//     against the MockRail-only cdylib that a plain `make test` builds.
//   - fiat_webhook_test.go carries `//go:build fiat` and needs a cdylib built
//     with `--features fiat-all` (`make test-fiat`). Go has no Cargo-style
//     optional-feature mechanism, so the build tag is the Go-side equivalent —
//     the same convention ../examples/fiatroundtrip already uses.
//
// Running these requires cgo and the generated bindings; use the Makefile:
//
//	cd patala-go && make test        # MockRail surface
//	cd patala-go && make test-fiat   # + the fiat/webhook surface
package bindingtest
