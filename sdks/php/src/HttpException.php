<?php

declare(strict_types=1);

namespace Patala;

/**
 * A non-2xx from patala-sidecar, with the parsed body kept.
 *
 * The body is kept rather than flattened into a message because on this API a
 * non-2xx is an ANSWER: `kind` is one of "invalid_request", "unknown_rail",
 * "unsupported", … and the four map to genuinely different actions.
 *
 *   400 invalid_request  your request was wrong — do not retry it unchanged
 *   404 unknown_rail     this sidecar has never heard of that rail_id
 *   501 unsupported      this rail cannot do that at all (e.g. the mock has no
 *                        push delivery) — never an invented event
 *   401                  missing/wrong token, on read-only routes too
 *
 * And note what does NOT arrive here: `{"valid": false}` is a 200, because a
 * rail's fail-closed refusal is data, not a transport failure.
 */
final class HttpException extends PatalaException
{
    /** @var int */
    private $status;

    /** @var mixed */
    private $body;

    /** @param mixed $body */
    public function __construct(int $status, $body)
    {
        $this->status = $status;
        $this->body = $body;
        $detail = \is_array($body)
            ? (($body['kind'] ?? '?') . ': ' . ($body['error'] ?? ''))
            : (string) $body;
        parent::__construct("patala-sidecar returned {$status} — {$detail}");
    }

    public function status(): int
    {
        return $this->status;
    }

    /** @return mixed */
    public function body()
    {
        return $this->body;
    }

    public function kind(): ?string
    {
        return \is_array($this->body) ? ($this->body['kind'] ?? null) : null;
    }
}
