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
