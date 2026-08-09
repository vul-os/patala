<?php

declare(strict_types=1);

/**
 * A four-line PSR-4 autoloader for `Patala\`, so the examples run straight from
 * a checkout with no `composer install`. Real consumers get the same mapping
 * from composer.json's autoload section.
 */
\spl_autoload_register(static function (string $class): void {
    if (\strpos($class, 'Patala\\') !== 0) {
        return;
    }
    $path = \dirname(__DIR__) . '/src/' . \str_replace('\\', '/', \substr($class, 7)) . '.php';
    if (\is_file($path)) {
        require $path;
    }
});

/** @var int $checks */
$checks = 0;

function check(bool $condition, string $message): void
{
    global $checks;
    ++$checks;
    if (!$condition) {
        \fwrite(\STDERR, "FAILED: {$message}\n");
        exit(1);
    }
    echo "  ok  {$message}\n";
}

/**
 * Threads in this process, counted by the OS. PHP has no thread API to ask, and
 * the question is what the native library started anyway.
 */
function os_threads(): int
{
    $out = @\shell_exec('ps -M ' . \getmypid() . ' 2>/dev/null');

    return $out === null ? -1 : \max(\substr_count($out, "\n") - 1, 0);
}
