<?php

declare(strict_types=1);

namespace Patala;

/**
 * patala from PHP over the sidecar: `patala-sidecar` as a separate process,
 * JSON over a loopback socket, no FFI in your address space at all.
 *
 *   $sc = \Patala\Sidecar::spawn();            // starts it, waits for /healthz
 *   try {
 *       $receipt = $sc->charge('mock', [
 *           'amount_minor' => 1250, 'currency' => 'USDC',
 *           'destination'  => 'mock:wallet:alice', 'reference' => 'order-1',
 *       ]);
 *       $ok = $sc->verify('mock', $receipt)['valid'];
 *   } finally {
 *       $sc->stop();
 *   }
 *
 *   // or point at one somebody else runs — the usual production shape:
 *   $sc = new \Patala\Sidecar('http://127.0.0.1:8420', getenv('PATALA_SIDECAR_TOKEN'));
 *
 * No dependencies: streams and json_encode are enough for a loopback JSON API,
 * so this package requires no Guzzle and no curl extension.
 *
 * Why choose this over \Patala\Ffi, given patala's in-process cost is unusually
 * low (see ../README.md)? KEY ISOLATION. A non-custodial rail's signing key
 * lives in whichever process calls charge; one sidecar means one process holds
 * it rather than every php-fpm worker on the box. Also: no ffi.enable in
 * php.ini, and no shared library matched to your platform.
 */
final class Sidecar
{
    /** @var string */
    private $baseUrl;

    /** @var string */
    private $token;

    /** @var resource|null the spawned process, if this instance owns one */
    private $proc;

    /** @var string|null */
    private $binary;

    public function __construct(string $baseUrl, string $token, $proc = null, ?string $binary = null)
    {
        $this->baseUrl = \rtrim($baseUrl, '/');
        $this->token = $token;
        $this->proc = $proc;
        $this->binary = $binary;
    }

    /**
     * Spawn a sidecar on a free loopback port with a freshly generated token
     * and wait for it to answer /healthz. The caller must call stop().
     */
    public static function spawn(?string $binary = null, ?int $port = null, ?string $token = null): self
    {
        $binary = $binary ?? self::resolveBinary();
        $token = $token ?? \bin2hex(\random_bytes(32));
        $port = $port ?? self::freePort();

        $descriptors = [1 => ['file', '/dev/null', 'w'], 2 => ['file', '/dev/null', 'w']];
        $proc = \proc_open(
            [$binary],
            $descriptors,
            $pipes,
            null,
            ['PATALA_SIDECAR_TOKEN' => $token, 'PATALA_SIDECAR_PORT' => (string) $port] + \getenv()
        );
        if (!\is_resource($proc)) {
            throw new PatalaException(
                "could not run '{$binary}'. Build it first: cargo build -p patala-sidecar"
            );
        }

        $sc = new self("http://127.0.0.1:{$port}", $token, $proc, $binary);
        $sc->waitUntilHealthy();

        return $sc;
    }

    public function baseUrl(): string
    {
        return $this->baseUrl;
    }

    public function token(): string
    {
        return $this->token;
    }

    public function binary(): ?string
    {
        return $this->binary;
    }

    /** /healthz is the one unauthenticated route, and it reveals only liveness. */
    public function healthy(): bool
    {
        [$status, $body] = $this->try('GET', '/healthz', null, false);

        return $status === 200 && $body === 'ok';
    }

    public function waitUntilHealthy(float $timeout = 10.0): self
    {
        $deadline = \microtime(true) + $timeout;
        while (\microtime(true) < $deadline) {
            if ($this->healthy()) {
                return $this;
            }
            if ($this->proc !== null) {
                $status = \proc_get_status($this->proc);
                if ($status !== false && $status['running'] === false) {
                    throw new PatalaException(
                        'patala-sidecar exited with ' . $status['exitcode'] . ' before answering /healthz'
                    );
                }
            }
            \usleep(50_000);
        }
        $this->stop();

        throw new PatalaException("patala-sidecar never became healthy at {$this->baseUrl}");
    }

    public function stop(): void
    {
        if (\is_resource($this->proc)) {
            \proc_terminate($this->proc);
            \proc_close($this->proc);
        }
        $this->proc = null;
    }

    public function __destruct()
    {
        $this->stop();
    }

    // ---------------------------------------------------------------- the API

    /** @return array<string,mixed> */
    public function capabilities(string $railId): array
    {
        return $this->request('GET', "/v1/rails/{$railId}");
    }

    /**
     * @param array<string,mixed> $request
     * @return array<string,mixed>
     */
    public function quote(string $railId, array $request): array
    {
        return $this->request('POST', "/v1/rails/{$railId}/quote", $request);
    }

    /**
     * @param array<string,mixed> $request
     * @return array<string,mixed>
     */
    public function charge(string $railId, array $request): array
    {
        return $this->request('POST', "/v1/rails/{$railId}/charge", $request);
    }

    /**
     * @param array<string,mixed> $receipt
     * @return array{valid:bool} `false` arrives with HTTP 200 on purpose: it is
     *         the rail's fail-closed answer, not a broken sidecar.
     */
    public function verify(string $railId, array $receipt): array
    {
        return $this->request('POST', "/v1/rails/{$railId}/verify", $receipt);
    }

    /**
     * 200 for all five verdicts, refusals included. Branch on `status` and
     * `is_refusal`; a 400 means the REQUEST was malformed and carries no
     * verdict fields at all, so a rejected request cannot be mistaken for a
     * checked address.
     *
     * @return array<string,mixed>
     */
    public function validateDestination(string $railId, string $destination): array
    {
        return $this->request('POST', "/v1/rails/{$railId}/validate-destination", [
            'destination' => $destination,
        ]);
    }

    /**
     * Forward the processor's request VERBATIM. $rawBody must be the exact
     * bytes off the wire (`file_get_contents('php://input')`), never a
     * re-encoded array — this is the one endpoint whose body is not parsed
     * JSON, because every webhook scheme signs what was actually sent.
     *
     * @param array<string,string> $headers
     * @return array<string,mixed>
     */
    public function webhook(string $railId, string $rawBody, array $headers = []): array
    {
        return $this->request('POST', "/v1/rails/{$railId}/webhook", null, true, $rawBody, $headers);
    }

    // -------------------------------------------------------------- transport

    /**
     * One call, raising on a non-2xx.
     *
     * @param array<string,mixed>|null $body
     * @param array<string,string> $headers
     * @return array<string,mixed>
     */
    public function request(
        string $method,
        string $path,
        ?array $body = null,
        bool $authed = true,
        ?string $rawBody = null,
        array $headers = []
    ): array {
        [$status, $decoded] = $this->try($method, $path, $body, $authed, $rawBody, $headers);
        if ($status < 200 || $status > 299) {
            throw new HttpException($status, $decoded);
        }
        if (!\is_array($decoded)) {
            throw new PatalaException("patala-sidecar returned a non-object body for {$path}");
        }

        return $decoded;
    }

    /**
     * The same call, returning [status, decoded body] instead of raising. Use
     * it where the status IS the thing you want to look at.
     *
     * @param array<string,mixed>|null $body
     * @param array<string,string> $headers
     * @return array{0:int,1:mixed}
     */
    public function try(
        string $method,
        string $path,
        ?array $body = null,
        bool $authed = true,
        ?string $rawBody = null,
        array $headers = []
    ): array {
        $lines = [];
        if ($authed) {
            $lines[] = 'Authorization: Bearer ' . $this->token;
        }
        foreach ($headers as $name => $value) {
            $lines[] = $name . ': ' . $value;
        }

        $content = $rawBody;
        if ($content === null && $body !== null) {
            $encoded = \json_encode($body);
            if ($encoded === false) {
                throw new PatalaException('could not encode the request: ' . \json_last_error_msg());
            }
            $content = $encoded;
            $lines[] = 'Content-Type: application/json';
        }

        $context = \stream_context_create(['http' => [
            'method' => $method,
            'header' => \implode("\r\n", $lines),
            'content' => $content ?? '',
            'timeout' => 30,
            // Without this a 4xx/5xx becomes `false` with a warning, and the
            // body — which is where `kind` and the message live — is thrown
            // away. On this API a non-2xx is an answer worth reading.
            'ignore_errors' => true,
        ]]);

        $raw = @\file_get_contents($this->baseUrl . $path, false, $context);
        if ($raw === false) {
            return [0, null];
        }

        $status = 0;
        foreach ($http_response_header ?? [] as $line) {
            if (\preg_match('#^HTTP/\S+\s+(\d{3})#', $line, $m) === 1) {
                $status = (int) $m[1];
            }
        }

        $decoded = \json_decode($raw, true);

        return [$status, $decoded === null ? $raw : $decoded];
    }

    private static function resolveBinary(): string
    {
        $env = \getenv('PATALA_SIDECAR_BIN');
        if ($env !== false && $env !== '') {
            return $env;
        }
        $repo = \dirname(__DIR__, 3); // .../sdks/php/src -> repo root
        foreach (['debug', 'release'] as $profile) {
            $candidate = $repo . '/target/' . $profile . '/patala-sidecar';
            if (\is_file($candidate)) {
                return $candidate;
            }
        }

        return 'patala-sidecar';
    }

    private static function freePort(): int
    {
        $server = \stream_socket_server('tcp://127.0.0.1:0', $errno, $errstr);
        if ($server === false) {
            throw new PatalaException("could not find a free port: {$errstr}");
        }
        $name = \stream_socket_get_name($server, false);
        \fclose($server);

        return (int) \substr((string) $name, \strrpos((string) $name, ':') + 1);
    }
}
