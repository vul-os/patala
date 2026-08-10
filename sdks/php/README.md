# patala (PHP)

Two ways to reach patala from PHP, both supported, **no composer dependencies**
either way.

| mode | what it is | file |
| --- | --- | --- |
| **Direct** | `libpatala_ffi` loaded into your process with the bundled FFI extension | [`src/Ffi.php`](src/Ffi.php) |
| **Sidecar** | `patala-sidecar` as a separate process, JSON over loopback | [`src/Sidecar.php`](src/Sidecar.php) |

Both drive `MockRail`: deterministic, offline, no credentials. patala is a
payments library, so an example that moves real value is not an example.

```sh
cargo build -p patala-ffi -p patala-sidecar
php sdks/php/examples/direct_charge.php
php sdks/php/examples/sidecar_charge.php
php sdks/php/examples/fork_probe.php     # pcntl
php sdks/php/examples/fpm_probe.php      # a real php-fpm pool
```

## Which one to pick

If you have read llmux's or openrate's PHP page, this is where they tell you
the sidecar is the answer for PHP, because the Go runtime their C ABI carries
is not fork-safe and php-fpm forks by design. **That reasoning does not apply
here, and it was re-measured rather than assumed.** patala is Rust: no runtime
enters your process, no GC, no scheduler, no signal handlers, and no threads.

Measured in **real php-fpm** — `pm = static`, one worker, `opcache.preload`
loading *and charging through* `libpatala_ffi` in the master before it forks
that worker ([`examples/fpm_probe.php`](examples/fpm_probe.php)):

```
B. ffi.enable=1, same master preload — the fork measurement
  ok  the MASTER again loaded the library and charged through it before forking
    fresh handle: pid=64973 mode=fresh version=0.1.1 verify={"valid":true}
  ok  fresh handle: 0/12 requests hung in the forked worker
  ok  answered by one forked php-fpm child (pid 64973), not by this process
  ok  fresh handle: the worker charged and verified through the master's library
    inherited handle: pid=64973 mode=inherited version=0.1.1 verify={"valid":true}
  ok  inherited handle: 0/12 requests hung in the forked worker
  ok  answered by one forked php-fpm child (pid 64973), not by this process
  ok  inherited handle: the worker charged and verified through the master's library
```

Twenty-four requests through a forked worker, none hung, including on the
handle the master itself opened. llmux, in this exact shape, answers a cheap
method in 0.1 ms and then never answers a real one.

So choose on the merits:

- **Direct** when you control `php.ini` and want one fewer process: no port, no
  supervision, no loopback surface, and an 849,584-byte library (mock-only,
  release) that installs no signal handlers and starts no threads.
- **Sidecar** for **key isolation** — the argument that actually matters. A
  non-custodial rail's signing key lives in whichever process calls `charge`.
  Link the library into every php-fpm worker and the key is in all of them;
  route them through one sidecar and it is in one narrowly-scoped process. See
  [`../../patala-sidecar/README.md`](../../patala-sidecar/README.md#threat-model),
  including what it does not defend against.
- **Sidecar** also when you cannot change `php.ini` — see the next section,
  which is the one genuine PHP-specific obstacle and has nothing to do with
  patala.

## `ffi.enable`, and a finding worth knowing before you plan

`ffi.enable` defaults to `preload`. The manual's recommended production shape
is: put `FFI::load()` of a header carrying `FFI_SCOPE` into an
`opcache.preload` script, and reach it from request code with `FFI::scope()`.

**On PHP 8.5.9 that does not work under php-fpm.** Measured, configuration A of
[`examples/fpm_probe.php`](examples/fpm_probe.php):

```
A. ffi.enable=preload
  ok  the MASTER ran FFI::load() and opened a rail handle before forking
    worker says: FFI API is restricted by "ffi.enable"
  ok  FFI::scope() THROWS in the fpm worker under ffi.enable=preload on PHP 8.5.9
```

The preload restriction is lifted for the **CLI SAPI only**, which is why
`php examples/direct_charge.php` runs happily with the very same `ffi.enable=preload`
in effect and an FPM worker throws. This is a PHP behaviour, not a patala one —
but it decides your options:

| you can | then |
| --- | --- |
| set `ffi.enable=1` | direct mode works in FPM, measured above |
| not touch `php.ini` | use `\Patala\Sidecar` — it needs no extension and no ini change |
| CLI / worker scripts only | direct mode already works, `preload` or not |

Note also that `ffi.enable` and `opcache.preload` are `PHP_INI_SYSTEM`: a
`php_admin_value` in a pool block is applied *per worker, after the fork* and
silently does nothing for either. The probe passes them with `-d` for exactly
that reason.

## Direct

```php
use Patala\Ffi;

$rail = new Ffi();                                  // offline MockRail
try {
    $verdict = $rail->validateDestination('mock:wallet:alice');
    if ($verdict['is_refusal']) {                   // a field — never re-derived
        throw new RuntimeException($verdict['reason']);
    }

    $receipt = $rail->charge([
        'amount_minor' => 1250, 'currency' => 'USDC',
        'destination'  => 'mock:wallet:alice', 'reference' => 'order-1',
    ]);
    $paid = $rail->verify($receipt)['valid'];       // true
} finally {
    $rail->close();
}
```

Requests and responses are the **same JSON the sidecar serves**, so moving a
call site between the two modes is a transport change, not a rewrite.

Library resolution is `PATALA_LIBRARY`, then `sdks/php/lib/`, then
`target/{debug,release}/` in a checkout, then the bare soname.

Real output, 2026-08-09, PHP 8.5.9 CLI on darwin/arm64:

```
php 8.5.9 (cli) on Darwin
ffi.enable=preload — "preload" still permits FFI::cdef in the CLI SAPI, which is why this runs
threads before FFI::cdef: 3
library: /Users/pc/code/vulos/patala/target/debug/libpatala_ffi.dylib
patala:  0.1.1
threads after FFI::cdef + patala_new: 3

the version probe, because a stale library earlier on the load path is silent
  ok  abiCheck() against the loaded version passes
  ok  abiCheck('9.9.9') throws and names both versions

capabilities
  ok  id() === 'mock'
  ok  class is 'NonCustodialFinal' — a wallet address and a final receipt, not a card form
  ok  holds_funds is false — patala never holds funds
  ok  reversible is false — there is no refund on this rail

pre-flight: validate-destination, before any money moves
  ok  a well-formed address gives status 'StructurallyValid'
  ok  is_refusal is false — a field, never re-derived from status
  ok  human_must_confirm is true even here — patala does not detect exchange addresses
  ok  an empty destination is a Malformed refusal, returned as a verdict and never thrown
  ok  caveat() is the sentence to show a human on the address form: patala cannot tell whether this address belongs …

quote -> charge -> verify
  ok  total_minor === 1250 and is an int — minor units, never a float
  ok  charge -> receipt for 1250 USDC
  ok  the genuine receipt verifies true
  ok  a tampered receipt verifies false — fail-closed, and false is DATA, not an exception

errors come back as exceptions, never as a crash in your process
  ok  an unsupported currency: patala_call(charge): patala: invalid request: rail mock does not support currency EUR
  ok  an unknown method is caught before the FFI call

webhooks: a rail with no push delivery says so
  ok  the mock refuses rather than inventing an event

a closed handle is a clean error, never a segfault
  ok  use-after-close says so: this Patala\Ffi handle is closed
  ok  closing twice is a no-op, so cleanup paths can be idempotent

threads after the whole round trip: 3   <- unchanged

ALL 20 PHP DIRECT ASSERTIONS PASSED
```

The three threads are PHP's own; the count does not move when the library is
loaded, when a handle is opened, or after a full round trip. That single
unchanging number is the whole fork story.

Two things the FFI extension makes easy to get wrong, both handled in
`src/Ffi.php` and worth copying:

- **`*err` is cleared on entry**, so `*err != NULL` after a call means *that*
  call failed and one slot is safe to reuse. Since 0.1.1 — and it reverses the
  advice this package used to give. patala wrote `*err` on failure only, so a
  reused slot still held the previous, already-freed pointer, and this README
  was where that trap was first written down. Clearing does not free what was
  there: call `patala_free` on a message before you reuse its slot.
- **`patala_free`, never PHP's memory manager and never `free()`.** Every
  non-const `char*` the library returns, results and error messages alike, is
  Rust-allocated.

There is **no streaming callback** to arrange, unlike llmux: patala has no
streaming operation, so PHP's "throwing from an FFI callback is a fatal error"
problem never arises. Six functions is the entire ABI.

## Sidecar

```php
use Patala\Sidecar;

// Production shape: point at one somebody else runs.
$sc = new Sidecar('http://127.0.0.1:8420', getenv('PATALA_SIDECAR_TOKEN'));

$receipt = $sc->charge('mock', [
    'amount_minor' => 1250, 'currency' => 'USDC',
    'destination'  => 'mock:wallet:alice', 'reference' => 'order-1',
]);
$paid = $sc->verify('mock', $receipt)['valid'];
```

`Sidecar::spawn()` starts one for you (fresh 32-byte token, free port, waits for
`/healthz`) and is what the example uses. `->try()` returns `[status, body]`
where `->request()` would throw — use it where the status *is* what you want to
inspect. `Patala\HttpException` keeps both, because a non-2xx here is an
**answer**:

```
ok  HttpException keeps the status and the parsed body:
    patala-sidecar returned 404 — unknown_rail: no rail is registered under id "nope"
```

Real output, same date:

```
php 8.5.9 (cli) — no ext-ffi, no ext-curl, no Guzzle
binary:  /Users/pc/code/vulos/patala/target/debug/patala-sidecar
listening on http://127.0.0.1:63124 (loopback only — the bind address is not configurable)

capabilities
  ok  class is 'NonCustodialFinal' — decide the whole UX off this, not off a provider name
  ok  holds_funds is false

pre-flight: validate-destination, before any money moves
  ok  a well-formed address -> 200 'StructurallyValid'
  ok  is_refusal is false — read the body, not just the status code
  ok  human_must_confirm is true even on StructurallyValid
  ok  an empty destination is a well-formed REQUEST -> 200 with a Malformed refusal

quote -> charge -> verify
  ok  total_minor === 1250 and decodes as an int — minor units, never a float
  ok  charge -> receipt for 1250 USDC
  ok  the genuine receipt verifies {"valid": true}
  ok  a tampered receipt is 200 {"valid": false} — fail-closed, and NOT an HTTP error

the error surface, so you can tell these four apart
  ok  an unsupported currency -> 400 'invalid_request'
  ok  an unknown rail_id -> 404 'unknown_rail'
  ok  the mock has no push delivery -> 501 'unsupported', never an invented event
  ok  no Authorization header -> 401 on a READ-ONLY route too

the throwing form, for the call sites that just want the answer
  ok  HttpException keeps the status and the parsed body: patala-sidecar returned 404 — unknown_rail: no rail is registered under id "nope"

sidecar terminated; nothing left running

ALL 15 PHP SIDECAR ASSERTIONS PASSED
```

One transport detail worth stealing: `stream_context_create` needs
`'ignore_errors' => true`, or a 4xx/5xx arrives as `false` with a warning and
the body — where `kind` and the message live — is thrown away.

**The sidecar's rail registry is mock-only today** — any other `rail_id` is a
`404`. That is a gap in the sidecar, not in this package.

## fork(), and the one rule that is real

`examples/fork_probe.php` (pcntl) loads the library, charges through it, then
forks:

```
threads in a bare php process: 3
threads after FFI::cdef + patala_new: 3
threads after a charge round trip: 3   <- unchanged: no runtime, no thread pool

    charge on a FRESH handle                 returned 1250  (0.01s)
    charge on the INHERITED handle           returned 1250  (0.00s)
    charge -> verify, inherited handle       returned {"valid":true}  (0.00s)
    validate-destination (pure, offline)     returned StructurallyValid  (0.00s)
```

What that file **cannot** show is the one rule `patala.h` does state: *"Handles
are not inherited usefully across a fork; open them in the child."* That rule
is about a parent that is **busy** at `fork()` time — a handle's runtime sits
behind a mutex, and `fork()` copies a locked mutex as locked. Reproducing it
needs a second thread in the parent, and a normal PHP build is non-thread-safe.
It is measured from Ruby and Python instead: **4/200** forks hung on an
inherited handle there, **0/200** on a handle opened in the child. See
[`../ruby/README.md`](../ruby/README.md).

For PHP the advice costs nothing and is what almost all PHP already does: build
the `\Patala\Ffi` per request, or per worker after the fork.

## Platforms

Built and exercised here: **darwin/arm64**, `libpatala_ffi.dylib`, from
`cargo build -p patala-ffi` on this machine. On **linux/amd64** the `.so` is
built and the C smoke test runs against it in CI's `c abi` job on
`ubuntu-latest` — so the library is known to load and answer there — but **no
PHP has ever been run against it**, and most PHP is deployed on exactly that
row. No Windows DLL exists, which is where `patala_free`'s "not your `free()`"
rule matters most.

The sidecar path needs only the `patala-sidecar` binary for your platform.

## Files

| file | mode | what it shows |
| --- | --- | --- |
| `src/Ffi.php` | direct | the six-function C ABI via `FFI::cdef`, plus `id`/`capabilities`/`quote`/`charge`/`verify`/`validateDestination`/`webhook`/`caveat`/`providers` |
| `src/Sidecar.php` | sidecar | spawn + healthz + terminate, and the HTTP API in a throwing and a `[status, body]` form |
| `src/HttpException.php`, `src/PatalaException.php` | both | what is an exception here, and what deliberately is not |
| `examples/direct_charge.php` | direct | ABI probe, capabilities, pre-flight, quote → charge → verify, tamper detection, errors, use-after-close |
| `examples/sidecar_charge.php` | sidecar | the same round trip over HTTP, plus all four error codes |
| `examples/fork_probe.php` | direct | thread counts and `fork()` with the library preloaded (pcntl) |
| `examples/fpm_probe.php` | direct | a real php-fpm pool with `opcache.preload`, driven over FastCGI, in both `ffi.enable` configurations |
