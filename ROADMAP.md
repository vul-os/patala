# patala — Roadmap

> A sovereign, centerless payment-rail substrate. Library + sidecar, no GUI.

This is a living, honest snapshot of direction, not a commitment or a
timeline. `PATALA.md` §4 is where the substantive deferred work is
tracked — the any-stablecoin mint generalization, an Algorand rail, and a
gateway-discovery phonebook. `CHANGELOG.md` records what has actually
shipped. This file exists only to record the one item that sits outside
both: installability, in case a surface it would apply to is ever built.

## No UI, by design — not a gap

patala is a library and a sidecar. `README.md` states this outright: "there
is no GUI," and for the sidecar specifically that absence is part of the
security story, not an omission — a token-gated loopback HTTP server with a
fixed rail registry has a much smaller attack surface than anything
browser-facing, and keys live in one hardened process rather than being
smeared across a UI. There is no roadmap item here to add one, and nothing
below should be read as one.

## If an operator/console surface is ever added

Should a future operator or console surface be built on top of the sidecar
— which is not currently planned — installability is the two separately
sized jobs the rest of the Vulos suite has found it splits into, and the
same split would apply here:

- **Installability** (a web manifest + icons so the surface can be added to
  a home screen or run as its own window) is small, mostly-assets work —
  icons would render from `brand/logo.svg` like every other icon this repo
  ships.
- **Offline support** (a service worker, cache invalidation, update and
  staleness handling) is a separate and materially bigger job, and nothing
  here commits to it.

Conditional on a surface that does not exist today; not scheduled.

---

### Publishing the language packages

Nothing is published to any registry today, and that is a decision, not an
oversight. The coordinates below are written into the manifests and reserved by
intent only — **no account holds them**, so any of them can be taken by someone
else tomorrow. That is not hypothetical: plain `llmux` was lost on both PyPI and
crates.io to unrelated projects before anyone looked, and the crates.io one is a
same-category tool at 2.4.0.

| registry | coordinate |
|---|---|
| npm | `@vul-os/patala`, `@vul-os/patala-bun` |
| JSR | `@vul-os/patala` |
| RubyGems | `vul-os-patala` |
| NuGet | `VulOs.Patala` |
| Packagist | `vul-os/patala` |
| Maven | `org.vulos:patala` |
| crates.io | not planned — `sdks/rust` is `patala-rust-examples`, an examples crate |
| Go | `github.com/vul-os/patala` — no registry, the module path is the coordinate |

Checked free across npm, JSR, PyPI, crates.io, RubyGems, NuGet, Packagist and
Hex on 2026-08-10, before being written down.

**Before anything is pushed**

1. **Claim the scopes first, publish second.** `@vul-os` has to exist as an
   organisation on npm and on JSR before a scoped package can go anywhere, and
   claiming a scope is free and reversible in a way that losing a name is not.
   NuGet ID-prefix reservation for `VulOs.*` is optional but the same logic.
2. **A release has to produce the artifacts.** Today the release workflow builds
   the binary and the C ABI bundles; it does not build a wheel, an npm tarball,
   a gem or a nupkg. Publishing without that step is publishing whatever happens
   to be in a working tree.
3. **Each package must install from a clean checkout.** Unverified here. In the
   sibling repos this was false and silently so: a hatchling `force-include` of
   a gitignored directory failed `pip install -e` on every clone, and `npm pack`
   produced a tarball of a README and a package.json with no code in it. Check
   each of patala's against a tree from `git archive HEAD` before trusting one.
**When a package does go out**, delete its registry from `UNPUBLISHED` in
`scripts/check-sdk-versions.mjs`. That list exists to refuse documentation that
tells a reader to install something that is not there; it is meant to shrink,
and the entry is what keeps the docs honest until it does.

**Order.** Go needs no registry at all — a module path is the coordinate, so it
is already "published" by tagging. Of the rest, the ones whose artifacts are
verified installable are the only candidates for a first push.

