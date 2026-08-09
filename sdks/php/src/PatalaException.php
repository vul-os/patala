<?php

declare(strict_types=1);

namespace Patala;

/**
 * Anything patala refuses to do, from either mode.
 *
 * Note what is deliberately NOT an exception:
 *
 *   - `verify` answering `{"valid": false}`. That is the rail's honest,
 *     fail-closed statement that a receipt does not hold. Gate entitlement on
 *     `true` and nothing else, and never retry a `false` as though it were a
 *     transient failure — that is how an unpaid order becomes an entitlement.
 *   - `validate-destination` answering `{"status": "Unknown"}`. "I cannot
 *     check this address" is a verdict, because a caller must handle it as
 *     carefully as a refusal and an exception is too easy to swallow in a
 *     catch-all.
 */
class PatalaException extends \RuntimeException
{
}
