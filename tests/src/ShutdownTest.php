<?php

declare(strict_types=1);

namespace PhpTokio\Test;

use PHPUnit\Framework\TestCase;

/**
 * What happens in a process of its own: initialization with init(), and the end of the request
 * with futures still running.
 */
final class ShutdownTest extends TestCase
{
    /**
     * Runs `$code` in a new PHP process with the test extension and the event loop initialized
     * by `$init` (init or initWith), returning its exit code, output and duration in seconds.
     *
     * @return array{int, string, float}
     */
    private static function runPhp(string $init, string $code): array
    {
        $extension = getenv('PHP_TOKIO_TEST_EXTENSION');
        if ($extension === false || $extension === '') {
            self::fail('Set PHP_TOKIO_TEST_EXTENSION to the path of the test extension');
        }
        $autoload = var_export(\dirname(__DIR__).'/vendor/autoload.php', true);
        $script = <<<PHP
            require $autoload;
            use PhpTokio\Test\Native;
            use Revolt\EventLoop;
            use function Amp\async;
            use function Amp\delay;
            \$stream = Native::$init();
            stream_set_blocking(\$stream, false);
            EventLoop::onReadable(\$stream, static fn () => Native::wakeup());
            $code
            PHP;
        $start = hrtime(true);
        $process = proc_open([PHP_BINARY, '-n', '-d', "extension=$extension", '-r', $script], [1 => ['pipe', 'w'], 2 => ['redirect', 1]], $pipes);
        \assert(\is_resource($process));
        $output = stream_get_contents($pipes[1]);
        fclose($pipes[1]);
        $code = proc_close($process);
        return [$code, (string) $output, (hrtime(true) - $start) / 1e9];
    }

    public function testInitRunsTheFuturesOnTheSharedRuntime(): void
    {
        [$code, $output] = self::runPhp('init', 'echo Native::threadName();');
        self::assertSame(0, $code);
        // Tokio's default name, not the one of initWith()'s runtime.
        self::assertStringStartsWith('tokio-', $output);
    }

    /** @return iterable<string, array{string}> */
    public static function initializers(): iterable
    {
        yield 'init' => ['init'];
        yield 'initWith' => ['initWith'];
    }

    #[\PHPUnit\Framework\Attributes\DataProvider('initializers')]
    public function testTheRequestEndsOnceTheFuturesBorrowingFromSuspendedCallersCompleted(string $init): void
    {
        // The caller is never resumed: when PHP destroys its fiber, suspend_on() waits for the
        // future, which borrows its argument, so its runtime must keep running it.
        [$code, $output, $elapsed] = self::runPhp($init, <<<'PHP'
            async(static fn () => Native::echo('borrowed', 500));
            delay(0.05);
            echo "exiting\n";
            PHP);
        self::assertSame([0, "exiting\n"], [$code, $output]);
        self::assertGreaterThanOrEqual(0.5, $elapsed);
    }

    #[\PHPUnit\Framework\Attributes\DataProvider('initializers')]
    public function testTheRequestEndsWithoutWaitingForTheFuturesOfTrySuspendOn(string $init): void
    {
        [$code, $output, $elapsed] = self::runPhp($init, <<<'PHP'
            async(static fn () => Native::trySleep(5_000));
            delay(0.05);
            echo "exiting\n";
            PHP);
        self::assertSame([0, "exiting\n"], [$code, $output]);
        self::assertLessThan(4.0, $elapsed, 'try_suspend_on() did not wait for the future');
    }
}
