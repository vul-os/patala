# Security Policy

patala is a sovereign payment-rail library that moves value without holding funds. Security reports are taken seriously and handled with priority.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

- Preferred: [GitHub private vulnerability reporting](https://github.com/vul-os/patala/security/advisories/new) on `vul-os/patala`.
- Alternatively, email **vulosorg@gmail.com** with `[patala security]` in the subject.

You will get an acknowledgement within **72 hours** and a status update at least every **14 days** until resolution. Please allow a reasonable window to ship a fix before public disclosure.

## Scope

- **Value movement** — any path that moves, redirects or double-spends value without authorization.\n- **Receipts & provenance** — forging or tampering with signed receipts.\n- **Key & credential handling** — leaking or mishandling rail credentials or signing keys.\n- **Adapter boundaries** — a hostile rail adapter affecting a vendoring product beyond its interface.

Out of scope: vulnerabilities requiring an already-compromised host, and issues in third-party services the operator configures.

## Supported versions

Pre-1.0: only the latest release (and `main`) receives fixes.
