<?php

declare(strict_types=1);

/**
 * The php-fpm question, answered in real php-fpm.
 *
 *   php sdks/php/examples/fpm_probe.php
 *
 * llmux's PHP page says, of its own C ABI: "Measured in real php-fpm: a worker
 * that loaded the library in the master answers `models` in 0.1 ms and then
 * never answers `chat` at all." That is true of llmux, which is Go — the Go
 * runtime is in the process and does not survive the fork php-fpm performs to
 * make each worker.
 *
 * patala is Rust and starts no threads, so the same setup should be a
 * non-event. "Should" is not a measurement, hence this file. It:
 *
 *   1. writes a php-fpm pool with ONE static worker into a temp directory,
 *   2. preloads libpatala_ffi in the MASTER with `opcache.preload` +
 *      `FFI::load()` — the canonical production setup for `ffi.enable=preload`,
 *      and precisely the pre-fork load that breaks llmux — and opens a rail
 *      handle there too,
 *   3. starts php-fpm, which forks its worker AFTER that,
 *   4. drives real FastCGI requests at the worker over a loopback socket, with
 *      a read timeout so a hung worker is reported as hung rather than waited
 *      on forever,
 *   5. tears the whole thing down.
 *
 * Everything runs against MockRail: offline, deterministic, no credentials.
 *
 * Requires php-fpm and Zend OPcache on PATH. Build the library first:
 * `cargo build -p patala-ffi`.
 */

require __DIR__ . '/bootstrap.php';

const REQUESTS = 12;

$php_fpm = \trim((string) \shell_exec('command -v php-fpm 2>/dev/null'));
if ($php_fpm === '') {
    \fwrite(\STDERR, "php-fpm is not on PATH — cannot run this probe.\n");
    exit(2);
}

$repo = \dirname(__DIR__, 3);
$libName = \PHP_OS_FAMILY === 'Darwin' ? 'libpatala_ffi.dylib' : 'libpatala_ffi.so';
$library = \getenv('PATALA_LIBRARY') ?: null;
foreach (['debug', 'release'] as $profile) {
    if ($library === null && \is_file("{$repo}/target/{$profile}/{$libName}")) {
        $library = "{$repo}/target/{$profile}/{$libName}";
    }
}
if ($library === null) {
    \fwrite(\STDERR, "no {$libName} found — build it: cargo build -p patala-ffi\n");
    exit(2);
}

$dir = \sys_get_temp_dir() . '/patala-fpm-' . \bin2hex(\random_bytes(6));
\mkdir($dir, 0700, true);
$port = free_port();

// ---------------------------------------------------------------------------
// 1. The FFI header. `FFI::load()` needs FFI_SCOPE/FFI_LIB defines; workers
//    then reach the master's already-dlopen'd library with FFI::scope(), which
//    is the whole point of ffi.enable=preload.
// ---------------------------------------------------------------------------
\file_put_contents("{$dir}/patala.h", <<<C
    #define FFI_SCOPE "patala"
    #define FFI_LIB "{$library}"

    const char* patala_abi_version(void);
    uint64_t patala_new(const char* config_json, char** err);
    void patala_close(uint64_t h);
    char* patala_call(uint64_t h, const char* method, const char* request_json, char** err);
    void patala_free(char* p);
    C);

// ---------------------------------------------------------------------------
// 2. The preload script. Runs ONCE, in the php-fpm MASTER, before any worker
//    exists — so the library is mapped and a handle is open on the far side of
//    the fork that makes each worker.
// ---------------------------------------------------------------------------
\file_put_contents("{$dir}/preload.php", <<<PHP
    <?php
    \$ffi = FFI::load('{$dir}/patala.h');
    \$handle = \$ffi->patala_new('{"rail":"mock"}', null);
    // A charge here too, so the master has really USED the library and not just
    // mapped it — the distinction that matters for a runtime with threads.
    \$res = \$ffi->patala_call(\$handle, 'charge',
        '{"amount_minor":1250,"currency":"USDC","destination":"mock:wallet:alice","reference":"master"}',
        null);
    \$ffi->patala_free(\$res);
    file_put_contents('{$dir}/master-handle', (string) \$handle);
    PHP);

// ---------------------------------------------------------------------------
// 3. The request script, run by the forked worker.
// ---------------------------------------------------------------------------
\file_put_contents("{$dir}/worker.php", <<<'PHP'
    <?php
    header('Content-Type: text/plain');

    $ffi = FFI::scope('patala');   // the master's library, inherited by fork()
    $pay = '{"amount_minor":1250,"currency":"USDC","destination":"mock:wallet:alice","reference":"worker"}';

    function call($ffi, int $handle, string $method, ?string $req): string {
        $res = $ffi->patala_call($handle, $method, $req, null);
        if ($res === null) {
            return 'NULL';
        }
        $out = FFI::string($res);
        $ffi->patala_free($res);
        return $out;
    }

    $mode = $_GET['mode'] ?? 'fresh';
    $inherited = (int) trim(file_get_contents(__DIR__ . '/master-handle'));

    if ($mode === 'inherited') {
        // The handle the MASTER opened, used from the forked worker.
        $receipt = call($ffi, $inherited, 'charge', $pay);
        $verdict = call($ffi, $inherited, 'verify', $receipt);
    } else {
        // A handle opened in the worker, which is what patala.h tells you to do.
        $handle = $ffi->patala_new('{"rail":"mock"}', null);
        $receipt = call($ffi, $handle, 'charge', $pay);
        $verdict = call($ffi, $handle, 'verify', $receipt);
        $ffi->patala_close($handle);
    }

    printf("pid=%d mode=%s version=%s verify=%s\n",
        getmypid(), $mode, $ffi->patala_abi_version(), trim($verdict));
    PHP);

// ---------------------------------------------------------------------------
// 4. The pool: exactly one static worker, so every request lands on the same
//    forked process and a broken one cannot be hidden by a fresh sibling.
// ---------------------------------------------------------------------------
\file_put_contents("{$dir}/php-fpm.conf", <<<CONF
    [global]
    daemonize = no
    error_log = {$dir}/fpm-error.log
    pid = {$dir}/fpm.pid

    [www]
    listen = 127.0.0.1:{$port}
    pm = static
    pm.max_children = 1
    catch_workers_output = yes
    CONF);

echo 'php ', \PHP_VERSION, " / php-fpm at {$php_fpm}\n";
echo "library: {$library}\n";
echo "pool:    127.0.0.1:{$port}, pm=static, ONE worker, opcache.preload in the master\n";
echo "tmpdir:  {$dir}\n";

/**
 * Start php-fpm with one `ffi.enable` value, run the requests, tear it down.
 *
 * `opcache.preload` and `ffi.enable` are PHP_INI_SYSTEM: they are read at
 * startup, in the MASTER, so a `php_admin_value` in the pool — which is applied
 * per worker, after the fork — silently does nothing at all. `-d` is the switch
 * that reaches the master, and reaching the master is the entire point here: a
 * preload that ran per-worker would test nothing.
 *
 * @return array{0:bool,1:array<string,array{hung:int,pids:array<string,bool>,sample:string}>}
 */
function run_pool(string $phpFpm, string $dir, int $port, string $ffiEnable): array
{
    @\unlink("{$dir}/master-handle");
    $descriptors = [
        1 => ['file', "{$dir}/fpm-stdout.log", 'w'],
        2 => ['file', "{$dir}/fpm-stderr.log", 'w'],
    ];
    $proc = \proc_open([
        $phpFpm, '--nodaemonize', '--fpm-config', "{$dir}/php-fpm.conf",
        '-d', "ffi.enable={$ffiEnable}",
        '-d', 'opcache.enable=1',
        '-d', 'opcache.enable_cli=1',
        '-d', "opcache.preload={$dir}/preload.php",
    ], $descriptors, $pipes);
    if (!\is_resource($proc)) {
        \fwrite(\STDERR, "could not start php-fpm\n");
        exit(1);
    }

    $listening = false;
    for ($i = 0; $i < 100; ++$i) {
        $probe = @\stream_socket_client("tcp://127.0.0.1:{$port}", $errno, $errstr, 0.2);
        if ($probe !== false) {
            \fclose($probe);
            $listening = true;
            break;
        }
        \usleep(100_000);
    }
    if (!$listening) {
        \fwrite(\STDERR, "php-fpm never listened:\n" . (string) @\file_get_contents("{$dir}/fpm-error.log"));
        \proc_terminate($proc);
        \proc_close($proc);
        exit(1);
    }

    $preloaded = \is_file("{$dir}/master-handle");
    $results = [];
    foreach (['fresh', 'inherited'] as $mode) {
        $hung = 0;
        $pids = [];
        $sample = '';
        for ($i = 0; $i < REQUESTS; ++$i) {
            $body = fcgi_request("127.0.0.1:{$port}", "{$dir}/worker.php", "mode={$mode}", 5.0);
            if ($body === null) {
                ++$hung;
                continue;
            }
            if (\preg_match('/pid=(\d+)/', $body, $m) === 1) {
                $pids[$m[1]] = true;
            }
            if ($sample === '') {
                $sample = \trim(\preg_replace('/\s+/', ' ', \substr($body, \strpos($body, "\r\n\r\n") ?: 0)) ?? '');
            }
        }
        $results[$mode] = ['hung' => $hung, 'pids' => $pids, 'sample' => $sample];
    }

    \proc_terminate($proc);
    \proc_close($proc);

    return [$preloaded, $results];
}

// ---------------------------------------------------------------------------
// Configuration A: ffi.enable=preload — the setting the manual points PHP
// users at, and the one that makes an FFI library a startup concern rather
// than a per-request one.
// ---------------------------------------------------------------------------
echo "\n" . \str_repeat('-', 74) . "\nA. ffi.enable=preload\n" . \str_repeat('-', 74) . "\n";
[$preloadedA, $resultsA] = run_pool($php_fpm, $dir, $port, 'preload');
check($preloadedA, 'the MASTER ran FFI::load() and opened a rail handle before forking');

$restricted = \strpos($resultsA['fresh']['sample'], 'FFI API is restricted') !== false;
if ($restricted) {
    echo "    worker says: FFI API is restricted by \"ffi.enable\"\n";
    check(true, 'FFI::scope() THROWS in the fpm worker under ffi.enable=preload on PHP ' . \PHP_VERSION);
    echo "\n    That is a PHP finding, not a patala one, and it is worth knowing before\n"
        . "    you plan around it: the preload restriction is lifted for the CLI SAPI\n"
        . "    only, so the same code runs fine from `php script.php` and throws under\n"
        . "    php-fpm. Either set ffi.enable=1 (configuration B), or use the sidecar,\n"
        . "    which needs no php.ini change at all.\n";
} else {
    check(
        $resultsA['fresh']['hung'] === 0,
        'ffi.enable=preload: ' . $resultsA['fresh']['hung'] . '/' . REQUESTS . ' requests hung'
    );
}

// ---------------------------------------------------------------------------
// Configuration B: ffi.enable=1 — FFI usable at runtime, with the library
// still loaded and USED in the master by the same preload script. This is the
// fork measurement proper.
// ---------------------------------------------------------------------------
echo "\n" . \str_repeat('-', 74) . "\nB. ffi.enable=1, same master preload — the fork measurement\n"
    . \str_repeat('-', 74) . "\n";
[$preloadedB, $resultsB] = run_pool($php_fpm, $dir, $port, '1');
check($preloadedB, 'the MASTER again loaded the library and charged through it before forking');

foreach (['fresh', 'inherited'] as $mode) {
    $result = $resultsB[$mode];
    echo "    {$mode} handle: " . $result['sample'] . "\n";
    check(
        $result['hung'] === 0,
        "{$mode} handle: {$result['hung']}/" . REQUESTS . ' requests hung in the forked worker'
    );
    check(
        \count($result['pids']) === 1 && !isset($result['pids'][(string) \getmypid()]),
        'answered by one forked php-fpm child (pid ' . \implode(',', \array_keys($result['pids']))
            . '), not by this process'
    );
    check(
        \strpos($result['sample'], 'verify={"valid":true}') !== false,
        "{$mode} handle: the worker charged and verified through the master's library"
    );
}

foreach (\glob("{$dir}/*") ?: [] as $file) {
    @\unlink($file);
}
@\rmdir($dir);

echo <<<TEXT

    Real php-fpm, real fork, the library loaded AND USED in the master before
    the worker existed. Nothing hung — including on the handle the master
    opened, because php-fpm's master is idle at fork() time, so nothing of
    patala's was mid-flight and no mutex was copied locked. llmux, in this exact
    shape, answers a cheap method and then never answers a real one.

    Still open a handle per worker or per request, as almost all PHP already
    does. The inherited handle worked here, but patala.h's rule is about a
    parent that is BUSY at fork() time — see examples/fork_probe.php, which
    reproduces that case deliberately and does make it hang.

    TEXT;

echo "\nALL {$checks} PHP-FPM PROBE ASSERTIONS PASSED\n";

// ---------------------------------------------------------------------------
// A minimal FastCGI responder client. The protocol is four record types and a
// length-prefixed name/value encoding; a dependency for that would be sillier
// than the 50 lines it takes. The read timeout is the load-bearing part — it
// is what turns "the worker is hung" into a printed result instead of a probe
// that waits forever.
// ---------------------------------------------------------------------------

function fcgi_request(string $hostport, string $script, string $query, float $timeout): ?string
{
    $sock = @\stream_socket_client("tcp://{$hostport}", $errno, $errstr, $timeout);
    if ($sock === false) {
        return null;
    }
    \stream_set_timeout($sock, (int) $timeout, (int) (\fmod($timeout, 1) * 1e6));

    $id = 1;
    // FCGI_BEGIN_REQUEST, role = FCGI_RESPONDER (1), flags = 0 (close when done)
    $out = fcgi_record(1, $id, \pack('nCx5', 1, 0));

    $params = [
        'GATEWAY_INTERFACE' => 'FastCGI/1.0',
        'REQUEST_METHOD' => 'GET',
        'SCRIPT_FILENAME' => $script,
        'SCRIPT_NAME' => '/' . \basename($script),
        'QUERY_STRING' => $query,
        'REQUEST_URI' => '/' . \basename($script) . '?' . $query,
        'SERVER_PROTOCOL' => 'HTTP/1.1',
        'SERVER_SOFTWARE' => 'patala-fpm-probe',
        'REMOTE_ADDR' => '127.0.0.1',
        'CONTENT_LENGTH' => '0',
    ];
    $encoded = '';
    foreach ($params as $name => $value) {
        $encoded .= fcgi_pair($name, $value);
    }
    $out .= fcgi_record(4, $id, $encoded);   // FCGI_PARAMS
    $out .= fcgi_record(4, $id, '');         // end of params
    $out .= fcgi_record(5, $id, '');         // empty FCGI_STDIN
    \fwrite($sock, $out);

    $body = '';
    while (true) {
        $header = fcgi_read($sock, 8, $timeout);
        if ($header === null || \strlen($header) < 8) {
            \fclose($sock);

            return $body === '' ? null : $body;
        }
        $parsed = \unpack('Cversion/Ctype/nid/nlength/Cpadding/Creserved', $header);
        $content = $parsed['length'] > 0 ? fcgi_read($sock, $parsed['length'], $timeout) : '';
        if ($content === null) {
            \fclose($sock);

            return null;
        }
        if ($parsed['padding'] > 0) {
            fcgi_read($sock, $parsed['padding'], $timeout);
        }
        if ($parsed['type'] === 6) {          // FCGI_STDOUT
            $body .= $content;
        } elseif ($parsed['type'] === 3) {    // FCGI_END_REQUEST
            break;
        }
    }
    \fclose($sock);

    return $body;
}

function fcgi_record(int $type, int $id, string $content): string
{
    $length = \strlen($content);

    return \pack('CCnnCx', 1, $type, $id, $length, 0) . $content;
}

function fcgi_pair(string $name, string $value): string
{
    $encode = static function (int $length): string {
        return $length < 128 ? \chr($length) : \pack('N', $length | 0x8000_0000);
    };

    return $encode(\strlen($name)) . $encode(\strlen($value)) . $name . $value;
}

/** Read exactly $length bytes, or null if the peer went quiet for $timeout. */
function fcgi_read($sock, int $length, float $timeout): ?string
{
    $buffer = '';
    $deadline = \microtime(true) + $timeout;
    while (\strlen($buffer) < $length) {
        if (\microtime(true) > $deadline) {
            return null;
        }
        $chunk = @\fread($sock, $length - \strlen($buffer));
        $meta = \stream_get_meta_data($sock);
        if ($meta['timed_out']) {
            return null;
        }
        if ($chunk === false || ($chunk === '' && \feof($sock))) {
            return null;
        }
        $buffer .= $chunk;
    }

    return $buffer;
}

function free_port(): int
{
    $server = \stream_socket_server('tcp://127.0.0.1:0', $errno, $errstr);
    $name = \stream_socket_get_name($server, false);
    \fclose($server);

    return (int) \substr((string) $name, \strrpos((string) $name, ':') + 1);
}
