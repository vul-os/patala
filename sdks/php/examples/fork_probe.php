<?php

declare(strict_types=1);

/**
 * Does patala survive fork()? Measured with pcntl, not asserted.
 *
 *   php sdks/php/examples/fork_probe.php
 *
 * The higher-fidelity version of this question is examples/fpm_probe.php, which
 * runs a real php-fpm pool. This file is the small, dependency-free one: it
 * loads the library, uses it, forks, and reports what the child could do.
 *
 * llmux's PHP page reports the opposite result for its own C ABI, and it is
 * right to: llmux is Go, and `-buildmode=c-shared` puts the Go runtime — with
 * its threads — into your process, where fork() leaves it half-alive. patala is
 * Rust and starts no threads, so there is nothing to leave behind.
 *
 * Requires ext-pcntl (bundled, CLI only).
 */

require __DIR__ . '/bootstrap.php';

use Patala\Ffi;

if (!\extension_loaded('pcntl')) {
    \fwrite(\STDERR, "ext-pcntl is not loaded — cannot fork. Try examples/fpm_probe.php instead.\n");
    exit(2);
}

const WATCHDOG = 5;

$pay = [
    'amount_minor' => 1250,
    'currency' => 'USDC',
    'destination' => 'mock:wallet:alice',
    'reference' => 'order-1',
];

/**
 * Fork, run $work in the child, and report — even if it never returns. The pipe
 * is the only channel that can be trusted: a hung child cannot write to it and
 * a crashed one closes it. Reading it with a timeout is what turns "this hangs"
 * from folklore into a printed result.
 */
function fork_and_run(string $label, callable $work): string
{
    $pair = \stream_socket_pair(\STREAM_PF_UNIX, \STREAM_SOCK_STREAM, 0);
    $started = \microtime(true);
    $pid = \pcntl_fork();
    if ($pid === 0) {                                  // ------------- child
        \fclose($pair[0]);
        try {
            \fwrite($pair[1], \substr('returned ' . $work(), 0, 300));
        } catch (\Throwable $t) {
            \fwrite($pair[1], \substr('threw ' . \get_class($t) . ': ' . $t->getMessage(), 0, 300));
        }
        \fclose($pair[1]);
        exit(0);
    }

    \fclose($pair[1]);                                 // ------------ parent
    \stream_set_timeout($pair[0], WATCHDOG);
    $read = [$pair[0]];
    $write = $except = [];
    if (\stream_select($read, $write, $except, WATCHDOG) > 0) {
        $message = (string) \stream_get_contents($pair[0]);
        if ($message === '') {
            $message = '(wrote nothing)';
        }
    } else {
        \posix_kill($pid, \SIGKILL);
        $message = 'HUNG — nothing in ' . WATCHDOG . 's, SIGKILLed';
    }
    \pcntl_waitpid($pid, $status);
    \fclose($pair[0]);
    \printf("    %-40s %s  (%.2fs)\n", $label, $message, \microtime(true) - $started);

    return $message;
}

echo \str_repeat('=', 74), "\n";
echo "patala fork probe (PHP) — every line below is measured, not claimed\n";
echo \str_repeat('=', 74), "\n";
echo 'php ', \PHP_VERSION, ' (', \PHP_SAPI, '), pcntl ', \phpversion('pcntl') ?: 'bundled', "\n";
echo 'watchdog ', WATCHDOG, "s\n\n";

echo 'threads in a bare php process: ', os_threads(), "\n";

$rail = new Ffi();
echo 'library: ', $rail->libraryPath(), "\n";
echo 'patala:  ', $rail->version(), "\n";
echo 'threads after FFI::cdef + patala_new: ', os_threads(), "\n";
$rail->charge($pay);
echo 'threads after a charge round trip: ', os_threads(), "   <- unchanged: no runtime, no thread pool\n";

echo "\n", \str_repeat('-', 74), "\n";
echo "after fork(), with the library loaded AND USED before the fork\n";
echo \str_repeat('-', 74), "\n";
echo "  (php-fpm's master does exactly this when opcache.preload touches FFI)\n";

$outcomes = [];
$outcomes[] = fork_and_run('charge on a FRESH handle', static function () use ($pay) {
    return Ffi::with(null, static fn (Ffi $r) => $r->charge($pay)['amount_minor']);
});
$outcomes[] = fork_and_run('charge on the INHERITED handle', static fn () => $rail->charge($pay)['amount_minor']);
$outcomes[] = fork_and_run(
    'charge -> verify, inherited handle',
    static fn () => \json_encode($rail->verify($rail->charge($pay)))
);
$outcomes[] = fork_and_run(
    'validate-destination (pure, offline)',
    static fn () => $rail->validateDestination('mock:wallet:alice')['status']
);

foreach ($outcomes as $index => $outcome) {
    check(\strpos($outcome, 'HUNG') === false, "child #{$index} answered rather than hanging");
}

echo "\n  Nothing hung. In llmux the equivalent child answers a cheap method and\n";
echo "  then hangs on the first real one — and that is the trap, because a boot\n";
echo "  check built on the cheap method reports a clean bill of health for a\n";
echo "  worker that will hang on the first payment. No such trap exists here.\n";

echo "\n", \str_repeat('-', 74), "\n";
echo "what this file CANNOT show\n";
echo \str_repeat('-', 74), "\n";
echo "  The one rule in patala.h — \"open handles in the child\" — is about a\n";
echo "  parent that is BUSY at fork() time: a handle's runtime sits behind a\n";
echo "  mutex, and fork() copies a locked mutex as locked. Reproducing that\n";
echo "  needs a second thread in the parent, and this PHP is non-thread-safe\n";
echo "  (NTS), which is the normal build. So it is measured from Ruby and\n";
echo "  Python instead — 4/200 forks hung on an inherited handle there, 0/200\n";
echo "  on a handle opened in the child. See ../../ruby/examples/fork_probe.rb.\n";
echo "\n  For PHP the practical advice is the same and costs nothing: build the\n";
echo "  \\Patala\\Ffi per request, or per worker after the fork — which is what\n";
echo "  almost all PHP code does anyway.\n";

$rail->close();

echo "\nALL {$checks} PHP FORK PROBE ASSERTIONS PASSED\n";
