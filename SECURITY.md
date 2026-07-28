# Security Policy

patala is a sovereign payment-rail library that moves value without holding funds. Security reports are taken seriously and handled with priority.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

- Preferred: [GitHub private vulnerability reporting](https://github.com/vul-os/patala/security/advisories/new) on `vul-os/patala`.
- Alternatively, email **vulosorg@gmail.com** with `[patala security]` in the subject.

You will get an acknowledgement within **72 hours** and a status update at least every **14 days** until resolution. Please allow a reasonable window to ship a fix before public disclosure.

## Scope

- **Value movement** — any path that moves, redirects or double-spends value without authorization.
- **Receipts & provenance** — forging or tampering with signed receipts.
- **Key & credential handling** — leaking or mishandling rail credentials or signing keys.
- **Adapter boundaries** — a hostile rail adapter affecting a vendoring product beyond its interface.

Out of scope: vulnerabilities requiring an already-compromised host, and issues in third-party services the operator configures.

## Release artifacts: there are none

patala **publishes no artifacts today**, so there is nothing here to checksum, sign or
verify. Stated explicitly because "no signing" and "signing that was skipped" look the same
from the outside, and only one of them is a finding:

- no release workflow (`.github/workflows/` contains `ci.yml` only) and no tags;
- no crate is published to crates.io — `patala-py` and `patala-sidecar` set
  `publish = false`, and nothing publishes the rest;
- no container image, no npm package, no prebuilt binary, no install script;
- `patala-go` is an in-repo Go module (`github.com/vul-os/patala/patala-go`). If it is ever
  consumed through the module proxy, its integrity comes from Go's own `go.sum` and the
  checksum database, not from anything this repo would emit.

Consumers vendor the source or use a path dependency. When patala does start publishing —
a tagged release, a crates.io push, or a binary — that release must carry a `SHA256SUMS`
manifest covering every asset plus a build-provenance attestation, and a user-facing
`verify.sh` that fails closed, matching the pattern used across the suite. Adding the
publishing step without that is the regression to watch for.

## Supported versions

Pre-1.0: only the latest release (and `main`) receives fixes.
