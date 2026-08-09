<?php

declare(strict_types=1);

namespace Patala;

/**
 * patala in-process, through the C ABI (`libpatala_ffi`), using PHP's bundled
 * FFI extension. No child process, no port.
 *
 *   $rail = new \Patala\Ffi();                    // offline MockRail
 *   try {
 *       $receipt = $rail->charge([
 *           'amount_minor' => 1250, 'currency' => 'USDC',
 *           'destination'  => 'mock:wallet:alice', 'reference' => 'order-1',
 *       ]);
 *       $ok = $rail->verify($receipt)['valid'];   // true
 *   } finally {
 *       $rail->close();                            // frees the handle, always
 *   }
 *
 * WHAT LOADING THIS COSTS YOU: almost nothing, and that is not the usual
 * answer. patala is Rust — no language runtime enters your process, no GC, no
 * scheduler, no signal handlers, and no threads. If you have read the
 * equivalent file in llmux or openrate, its warning about php-fpm and the
 * non-fork-safe Go runtime is true there and FALSE here; see ../README.md,
 * where it is measured against a real forking parent rather than asserted.
 *
 * The one rule that IS real: open a handle in the child. A handle's runtime
 * sits behind a mutex and fork() copies a locked mutex as locked, so a handle
 * built in the php-fpm master and used in a worker can deadlock. Build it
 * per-request, or per-worker after the fork — which is what almost all PHP
 * code does anyway.
 *
 * Requirements: ext-ffi (bundled with PHP >= 7.4, but see `ffi.enable`) and a
 * libpatala_ffi built for this platform (`cargo build -p patala-ffi`).
 */
final class Ffi
{
    /**
     * The six functions of the C ABI, transcribed from
     * patala-ffi/include/patala.h. PHP's FFI parser does not run the C
     * preprocessor, so the header itself (with its #ifndef and #include)
     * cannot be handed to FFI::cdef.
     */
    private const CDEF = <<<'C'
        const char* patala_abi_version(void);
        int patala_abi_check(const char* expected, char** err);
        uint64_t patala_new(const char* config_json, char** err);
        void patala_close(uint64_t h);
        char* patala_call(uint64_t h, const char* method, const char* request_json, char** err);
        void patala_free(char* p);
        C;

    /**
     * Every method `patala_call` dispatches. Checked here so a typo is caught
     * with the real list attached rather than after a round trip.
     */
    public const METHODS = [
        'id', 'capabilities', 'quote', 'charge', 'verify',
        'validate-destination', 'webhook', 'caveat', 'providers',
    ];

    /** @var \FFI */
    private $ffi;

    /** @var int */
    private $handle = 0;

    /** @var string */
    private $libraryPath;

    /**
     * @param array<string,mixed>|string|null $config a rail configuration
     *        document. null means the offline default: a deterministic
     *        MockRail on USDC, no credentials and no network. Unknown fields
     *        are REFUSED rather than ignored.
     * @param string|null $libraryPath override library resolution entirely.
     */
    public function __construct($config = null, ?string $libraryPath = null)
    {
        if (!\extension_loaded('FFI')) {
            throw new PatalaException(
                'the FFI extension is not loaded. It ships with PHP >= 7.4 but is ' .
                'often disabled: enable extension=ffi in php.ini. See sdks/php/README.md.'
            );
        }

        $this->libraryPath = $libraryPath ?? self::resolveLibrary();

        try {
            $this->ffi = \FFI::cdef(self::CDEF, $this->libraryPath);
        } catch (\FFI\Exception $e) {
            throw new PatalaException(
                'FFI::cdef failed for ' . $this->libraryPath . ': ' . $e->getMessage() .
                ($this->looksRestricted($e)
                    ? "\nffi.enable is 'preload' by default, which forbids FFI::cdef in " .
                      'any SAPI except CLI. Either preload a file that opens the library, ' .
                      'set ffi.enable=true, or use \\Patala\\Sidecar — which needs no ' .
                      'php.ini change at all. See sdks/php/README.md.'
                    : ''),
                0,
                $e
            );
        }

        $json = \is_string($config) || $config === null ? $config : self::encode($config);

        $err = $this->ffi->new('char*');
        // patala_new returns 0 for FAILURE — its success value is a handle,
        // and handles start at 1.
        $handle = $this->ffi->patala_new($json, \FFI::addr($err));
        if ($handle === 0) {
            throw new PatalaException('patala_new: ' . $this->takeError($err));
        }
        $this->handle = (int) $handle;
    }

    /**
     * Run $fn with an open rail and close it afterwards, whatever happens.
     * PHP has no `using`/`with`, so this is the closest idiomatic construct;
     * try/finally around a plain constructor is equally fine.
     *
     * @param array<string,mixed>|string|null $config
     * @return mixed whatever $fn returns
     */
    public static function with($config, callable $fn, ?string $libraryPath = null)
    {
        $rail = new self($config, $libraryPath);
        try {
            return $fn($rail);
        } finally {
            $rail->close();
        }
    }

    /**
     * The patala version the LOADED library was built from.
     *
     * Worth checking at startup: a shared library is resolved off a load path
     * you may not control, and a stale libpatala_ffi earlier on it is called
     * silently otherwise. The string is static in the library — never freed.
     */
    public function version(): string
    {
        return (string) $this->ffi->patala_abi_version();
    }

    /**
     * Throw unless the loaded library is exactly $expected. The comparison
     * lives in the library so it is not reimplemented — and forgotten — in
     * each binding.
     */
    public function abiCheck(string $expected): void
    {
        $err = $this->ffi->new('char*');
        if ($this->ffi->patala_abi_check($expected, \FFI::addr($err)) !== 0) {
            throw new PatalaException($this->takeError($err));
        }
    }

    public function libraryPath(): string
    {
        return $this->libraryPath;
    }

    /**
     * One unary call, decoded.
     *
     * @param array<string,mixed>|string|null $request
     * @return array<string,mixed>
     */
    public function call(string $method, $request = null): array
    {
        $raw = $this->callRaw($method, $request);
        $decoded = \json_decode($raw, true);
        if (!\is_array($decoded)) {
            throw new PatalaException("patala returned a non-object response to '{$method}'");
        }

        return $decoded;
    }

    /**
     * The same call, returning the response JSON verbatim — the same bytes
     * `POST /v1/rails/:id/<method>` returns from patala-sidecar.
     *
     * @param array<string,mixed>|string|null $request
     */
    public function callRaw(string $method, $request = null): string
    {
        $this->assertOpen();
        if (!\in_array($method, self::METHODS, true)) {
            throw new PatalaException(
                "unknown method '{$method}' (want one of: " . \implode(', ', self::METHODS) . ')'
            );
        }

        $json = \is_string($request) || $request === null ? $request : self::encode($request);

        // A FRESH slot per call, deliberately. patala writes *err on failure
        // only and leaves it untouched otherwise, so a reused slot still holds
        // the previous (already-freed) pointer. FFI::new zeroes its allocation.
        $err = $this->ffi->new('char*');
        $res = $this->ffi->patala_call($this->handle, $method, $json, \FFI::addr($err));

        // A NULL `char*` return arrives in PHP as null, not as a null CData.
        if ($res === null) {
            throw new PatalaException("patala_call({$method}): " . $this->takeError($err));
        }

        try {
            return \FFI::string($res);
        } finally {
            // Everything the library returns — results AND error messages — is
            // released with patala_free and nothing else. Not PHP's memory
            // manager, not free(): this is Rust's allocator.
            $this->ffi->patala_free($res);
        }
    }

    // --------------------------------------------------------- convenience

    public function id(): string
    {
        return $this->call('id')['rail_id'];
    }

    /** @return array<string,mixed> */
    public function capabilities(): array
    {
        return $this->call('capabilities');
    }

    /**
     * @param array<string,mixed> $request
     * @return array<string,mixed>
     */
    public function quote(array $request): array
    {
        return $this->call('quote', $request);
    }

    /**
     * @param array<string,mixed> $request
     * @return array<string,mixed> the Receipt — store it and hand it back to
     *         verify() later. THAT, not charge() having returned, is the
     *         entitlement check.
     */
    public function charge(array $request): array
    {
        return $this->call('charge', $request);
    }

    /**
     * @param array<string,mixed> $receipt the Receipt charge() returned
     * @return array{valid:bool} the whole document, not the bool: `false` is
     *         the rail's fail-closed answer and is data, not an error.
     */
    public function verify(array $receipt): array
    {
        return $this->call('verify', $receipt);
    }

    /**
     * The offline pre-flight check. NEVER throws for an unknown address: "I
     * cannot check this" comes back as {"status": "Unknown"}. Read
     * `is_refusal` (do not send) and `human_must_confirm` (true on EVERY
     * verdict, "StructurallyValid" included).
     *
     * @return array<string,mixed>
     */
    public function validateDestination(string $destination): array
    {
        return $this->call('validate-destination', ['destination' => $destination]);
    }

    /**
     * Authenticate an inbound processor delivery. Forward it VERBATIM — the
     * exact body bytes, the headers, the query string — because every scheme
     * signs what the processor actually sent. A body that has been through
     * json_decode/json_encode on your side is no longer that.
     *
     * @param array<string,string> $headers
     * @param array<string,string> $query
     * @return array<string,mixed> a WebhookEvent
     */
    public function webhook(
        string $body,
        array $headers = [],
        array $query = [],
        ?int $nowUnix = null,
        bool $binary = false
    ): array {
        $payload = [
            'headers' => (object) $headers,
            'query' => (object) $query,
            'now_unix' => $nowUnix ?? \time(),
        ];
        $payload[$binary ? 'body_hex' : 'body'] = $binary ? \bin2hex($body) : $body;

        return $this->call('webhook', $payload);
    }

    /**
     * The sentence to show a human on the form where a payout address is first
     * asked for. patala does not detect exchange-owned addresses, and this is
     * how it says so.
     */
    public function caveat(): string
    {
        return $this->call('caveat')['exchange_deposit_caveat'];
    }

    /**
     * Fiat provider names this build can construct. Only present when the
     * library was built with --features fiat.
     *
     * @return list<string>
     */
    public function providers(): array
    {
        return $this->call('providers')['providers'];
    }

    /** Release the rail. Idempotent, so cleanup paths can call it blindly. */
    public function close(): void
    {
        if ($this->handle !== 0) {
            $this->ffi->patala_close($this->handle);
            $this->handle = 0;
        }
    }

    /**
     * A safety net, not the mechanism. PHP destroys objects at unpredictable
     * points and not at all on a fatal error; close() in a finally block is
     * what actually guarantees the handle goes away.
     */
    public function __destruct()
    {
        $this->close();
    }

    // ------------------------------------------------------------ internals

    private function assertOpen(): void
    {
        if ($this->handle === 0) {
            throw new PatalaException('this Patala\Ffi handle is closed');
        }
    }

    /** @param \FFI\CData $err a `char*` slot */
    private function takeError($err): string
    {
        if (\FFI::isNull($err)) {
            return '(no message)';
        }
        $message = \FFI::string($err);
        $this->ffi->patala_free($err);

        return $message;
    }

    private function looksRestricted(\FFI\Exception $e): bool
    {
        return \strpos($e->getMessage(), 'ffi.enable') !== false
            || \strpos($e->getMessage(), 'FFI API is restricted') !== false;
    }

    /** @param array<string,mixed> $value */
    private static function encode(array $value): string
    {
        $json = \json_encode($value);
        if ($json === false) {
            throw new PatalaException('could not encode the request as JSON: ' . \json_last_error_msg());
        }

        return $json;
    }

    /**
     * Find libpatala_ffi:
     *   1. PATALA_LIBRARY
     *   2. lib/ inside this package
     *   3. target/{debug,release}/ in a checkout of the patala workspace
     *   4. the bare soname, letting the dynamic loader search its own paths
     */
    private static function resolveLibrary(): string
    {
        $env = \getenv('PATALA_LIBRARY');
        if ($env !== false && $env !== '') {
            return $env;
        }

        $name = self::libraryName();
        $pkg = \dirname(__DIR__);   // .../sdks/php
        $repo = \dirname($pkg, 2);  // .../sdks/php -> .../sdks -> repo root

        $candidates = [$pkg . '/lib/' . $name];
        foreach (['debug', 'release'] as $profile) {
            $candidates[] = $repo . '/target/' . $profile . '/' . $name;
        }
        foreach ($candidates as $candidate) {
            if (\is_file($candidate)) {
                return $candidate;
            }
        }

        // Let the dynamic loader try. If it fails, an FFI\Exception naming the
        // soname is more useful than a guess at a path.
        return $name;
    }

    private static function libraryName(): string
    {
        switch (\PHP_OS_FAMILY) {
            case 'Windows':
                return 'patala_ffi.dll';
            case 'Darwin':
                return 'libpatala_ffi.dylib';
            default:
                return 'libpatala_ffi.so';
        }
    }
}
