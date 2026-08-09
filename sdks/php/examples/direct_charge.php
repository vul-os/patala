<?php

declare(strict_types=1);

/**
 * patala in-process from PHP, through the C ABI with the bundled FFI extension.
 *
 *   php sdks/php/examples/direct_charge.php
 *
 * Everything runs against MockRail: deterministic, offline, no credentials and
 * no network. patala is a payments library, so an example that moves real value
 * is not an example.
 *
 * Build the library first, from the workspace root:
 *
 *   cargo build -p patala-ffi
 */

require __DIR__ . '/bootstrap.php';

use Patala\Ffi;
use Patala\PatalaException;

echo 'php ', \PHP_VERSION, ' (', \PHP_SAPI, ') on ', \PHP_OS_FAMILY, "\n";
echo 'ffi.enable=', \ini_get('ffi.enable'),
     ' — "preload" still permits FFI::cdef in the CLI SAPI, which is why this runs', "\n";
echo 'threads before FFI::cdef: ', os_threads(), "\n";

$rail = new Ffi();
try {
    echo 'library: ', $rail->libraryPath(), "\n";
    echo 'patala:  ', $rail->version(), "\n";
    echo 'threads after FFI::cdef + patala_new: ', os_threads(), "\n\n";

    echo "the version probe, because a stale library earlier on the load path is silent\n";
    $rail->abiCheck($rail->version());
    check(true, 'abiCheck() against the loaded version passes');
    try {
        $rail->abiCheck('9.9.9');
        check(false, "abiCheck('9.9.9') should have thrown");
    } catch (PatalaException $e) {
        check(
            \strpos($e->getMessage(), 'mismatch') !== false,
            "abiCheck('9.9.9') throws and names both versions"
        );
    }

    echo "\ncapabilities\n";
    $caps = $rail->capabilities();
    check($rail->id() === 'mock', 'id() === ' . \var_export($rail->id(), true));
    check(
        $caps['class'] === 'NonCustodialFinal',
        "class is '{$caps['class']}' — a wallet address and a final receipt, not a card form"
    );
    check($caps['holds_funds'] === false, 'holds_funds is false — patala never holds funds');
    check($caps['reversible'] === false, 'reversible is false — there is no refund on this rail');

    echo "\npre-flight: validate-destination, before any money moves\n";
    $verdict = $rail->validateDestination('mock:wallet:alice');
    check(
        $verdict['status'] === 'StructurallyValid',
        "a well-formed address gives status '{$verdict['status']}'"
    );
    check($verdict['is_refusal'] === false, 'is_refusal is false — a field, never re-derived from status');
    check(
        $verdict['human_must_confirm'] === true,
        'human_must_confirm is true even here — patala does not detect exchange addresses'
    );
    $refused = $rail->validateDestination('');
    check(
        $refused['status'] === 'Malformed' && $refused['is_refusal'] === true,
        'an empty destination is a Malformed refusal, returned as a verdict and never thrown'
    );
    check(
        $rail->caveat() !== '',
        'caveat() is the sentence to show a human on the address form: '
            . \substr($rail->caveat(), 0, 48) . '…'
    );

    echo "\nquote -> charge -> verify\n";
    $pay = [
        'amount_minor' => 1250,
        'currency' => 'USDC',
        'destination' => 'mock:wallet:alice',
        'reference' => 'order-1',
    ];

    $quote = $rail->quote($pay);
    check(
        $quote['total_minor'] === 1250 && \is_int($quote['total_minor']),
        "total_minor === {$quote['total_minor']} and is an int — minor units, never a float"
    );

    $receipt = $rail->charge($pay);
    check(
        $receipt['amount_minor'] === 1250,
        "charge -> receipt for {$receipt['amount_minor']} {$receipt['currency']}"
    );

    check($rail->verify($receipt) === ['valid' => true], 'the genuine receipt verifies true');

    $tampered = $receipt;
    ++$tampered['amount_minor'];
    check(
        $rail->verify($tampered) === ['valid' => false],
        'a tampered receipt verifies false — fail-closed, and false is DATA, not an exception'
    );

    echo "\nerrors come back as exceptions, never as a crash in your process\n";
    try {
        $rail->charge(['currency' => 'EUR'] + $pay);
        check(false, 'charging EUR on a USDC rail should have thrown');
    } catch (PatalaException $e) {
        check(
            \strpos($e->getMessage(), 'does not support currency EUR') !== false,
            'an unsupported currency: ' . $e->getMessage()
        );
    }
    try {
        $rail->call('nope');
        check(false, 'an unknown method should have thrown');
    } catch (PatalaException $e) {
        check(
            \strpos($e->getMessage(), 'unknown method') !== false,
            'an unknown method is caught before the FFI call'
        );
    }

    echo "\nwebhooks: a rail with no push delivery says so\n";
    try {
        $rail->webhook('{}');
        check(false, 'the mock has no push delivery and should have refused');
    } catch (PatalaException $e) {
        check(
            \strpos($e->getMessage(), 'not supported') !== false,
            'the mock refuses rather than inventing an event'
        );
    }

    echo "\na closed handle is a clean error, never a segfault\n";
    $scratch = new Ffi();
    $scratch->close();
    try {
        $scratch->capabilities();
        check(false, 'a closed handle should have thrown');
    } catch (PatalaException $e) {
        check(\strpos($e->getMessage(), 'closed') !== false, 'use-after-close says so: ' . $e->getMessage());
    }
    $scratch->close();
    check(true, 'closing twice is a no-op, so cleanup paths can be idempotent');

    echo "\nthreads after the whole round trip: ", os_threads(), "   <- unchanged\n";
} finally {
    $rail->close();
}

echo "\nALL {$checks} PHP DIRECT ASSERTIONS PASSED\n";
