<?php

declare(strict_types=1);

/**
 * patala from PHP over the sidecar — a separate process, no FFI at all.
 *
 *   php sdks/php/examples/sidecar_charge.php
 *
 * This spawns patala-sidecar on a free loopback port with a freshly generated
 * token, drives a full quote -> charge -> verify against MockRail using nothing
 * but streams and json_encode, and terminates it. Nothing is left running, and
 * no php.ini change is needed — which is the practical difference from
 * ../src/Ffi.php on a php-fpm box (see examples/fpm_probe.php).
 *
 * Build the server first, from the workspace root:
 *
 *   cargo build -p patala-sidecar
 */

require __DIR__ . '/bootstrap.php';

use Patala\HttpException;
use Patala\Sidecar;

echo 'php ', \PHP_VERSION, ' (', \PHP_SAPI, ") — no ext-ffi, no ext-curl, no Guzzle\n";

$sc = Sidecar::spawn();
try {
    echo 'binary:  ', (string) $sc->binary(), "\n";
    echo 'listening on ', $sc->baseUrl(), " (loopback only — the bind address is not configurable)\n\n";

    echo "capabilities\n";
    $caps = $sc->capabilities('mock');
    check(
        $caps['class'] === 'NonCustodialFinal',
        "class is '{$caps['class']}' — decide the whole UX off this, not off a provider name"
    );
    check($caps['holds_funds'] === false, 'holds_funds is false');

    echo "\npre-flight: validate-destination, before any money moves\n";
    $verdict = $sc->validateDestination('mock', 'mock:wallet:alice');
    check($verdict['status'] === 'StructurallyValid', "a well-formed address -> 200 '{$verdict['status']}'");
    check($verdict['is_refusal'] === false, 'is_refusal is false — read the body, not just the status code');
    check($verdict['human_must_confirm'] === true, 'human_must_confirm is true even on StructurallyValid');

    [$status, $refused] = $sc->try('POST', '/v1/rails/mock/validate-destination', ['destination' => '']);
    check(
        $status === 200 && $refused['status'] === 'Malformed' && $refused['is_refusal'] === true,
        'an empty destination is a well-formed REQUEST -> 200 with a Malformed refusal'
    );

    echo "\nquote -> charge -> verify\n";
    $pay = [
        'amount_minor' => 1250,
        'currency' => 'USDC',
        'destination' => 'mock:wallet:alice',
        'reference' => 'order-1',
    ];

    $quote = $sc->quote('mock', $pay);
    check(
        $quote['total_minor'] === 1250 && \is_int($quote['total_minor']),
        "total_minor === {$quote['total_minor']} and decodes as an int — minor units, never a float"
    );

    $receipt = $sc->charge('mock', $pay);
    check(
        $receipt['amount_minor'] === 1250,
        "charge -> receipt for {$receipt['amount_minor']} {$receipt['currency']}"
    );

    check($sc->verify('mock', $receipt) === ['valid' => true], 'the genuine receipt verifies {"valid": true}');

    $tampered = $receipt;
    ++$tampered['amount_minor'];
    [$status, $body] = $sc->try('POST', '/v1/rails/mock/verify', $tampered);
    check(
        $status === 200 && $body === ['valid' => false],
        'a tampered receipt is 200 {"valid": false} — fail-closed, and NOT an HTTP error'
    );

    echo "\nthe error surface, so you can tell these four apart\n";
    [$status, $body] = $sc->try('POST', '/v1/rails/mock/charge', ['currency' => 'EUR'] + $pay);
    check($status === 400, "an unsupported currency -> {$status} '{$body['kind']}'");

    [$status, $body] = $sc->try('GET', '/v1/rails/nope');
    check($status === 404, "an unknown rail_id -> {$status} '{$body['kind']}'");

    [$status, $body] = $sc->try('POST', '/v1/rails/mock/webhook', null, true, '{}');
    check(
        $status === 501,
        "the mock has no push delivery -> {$status} '{$body['kind']}', never an invented event"
    );

    [$status] = $sc->try('GET', '/v1/rails/mock', null, false);
    check($status === 401, "no Authorization header -> {$status} on a READ-ONLY route too");

    echo "\nthe throwing form, for the call sites that just want the answer\n";
    try {
        $sc->capabilities('nope');
        check(false, 'an unknown rail should have thrown');
    } catch (HttpException $e) {
        check(
            $e->status() === 404 && $e->kind() === 'unknown_rail',
            'HttpException keeps the status and the parsed body: ' . $e->getMessage()
        );
    }
} finally {
    $sc->stop();
}

echo "\nsidecar terminated; nothing left running\n";
echo "\nALL {$checks} PHP SIDECAR ASSERTIONS PASSED\n";
