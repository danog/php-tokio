<?php

declare(strict_types=1);

namespace PhpTokio\Test;

use PHPUnit\Framework\Attributes\DataProvider;
use PHPUnit\Framework\Attributes\RequiresFunction;
use PHPUnit\Framework\Attributes\RequiresOperatingSystemFamily;
use PHPUnit\Framework\TestCase;

/**
 * What needs a PHP process of its own: initialization (with init(), without Revolt or with a
 * broken one, or failing), and the end of the request with futures still running.
 */
final class ShutdownTest extends TestCase
{
    private const TIMEOUT = 20.0;

    /**
     * Runs `$code` in a new PHP process with the test extension, after `$setup` (by default: the
     * autoloader, and the event loop initialized by `$init`), returning its exit code, output,
     * duration in seconds and standard error. `$revolt` declares a Revolt of its own instead of
     * the autoloaded one, in braced namespaces.
     *
     * @param list<string> $wrapper A command running the PHP command it is given.
     * @return array{int, string, float, string}
     */
    private static function runPhp(string $code, string $init = 'initWith', ?string $setup = null, string $revolt = '', array $wrapper = []): array
    {
        $extension = getenv('PHP_TOKIO_TEST_EXTENSION');
        if ($extension === false || $extension === '') {
            self::fail('Set PHP_TOKIO_TEST_EXTENSION to the path of the test extension');
        }
        $autoload = var_export(\dirname(__DIR__).'/vendor/autoload.php', true);
        $setup ??= <<<PHP
            require $autoload;
            \$stream = Native::$init();
            stream_set_blocking(\$stream, false);
            EventLoop::onReadable(\$stream, static fn () => Native::wakeup());
            PHP;
        $script = <<<PHP
            $revolt
            namespace {
            use PhpTokio\Test\Native;
            use Revolt\EventLoop;
            use function Amp\async;
            use function Amp\delay;
            $setup
            $code
            }
            PHP;
        // Files, not pipes: stream_select() can't watch pipes on Windows.
        $files = [tempnam(sys_get_temp_dir(), 'php-tokio-out'), tempnam(sys_get_temp_dir(), 'php-tokio-err')];
        try {
            $start = hrtime(true);
            $process = proc_open([...$wrapper, PHP_BINARY, '-d', "extension=$extension", '-r', $script], [1 => ['file', $files[0], 'w'], 2 => ['file', $files[1], 'w']], $pipes);
            \assert(\is_resource($process));
            while (($status = proc_get_status($process))['running']) {
                if ((hrtime(true) - $start) / 1e9 > self::TIMEOUT) {
                    proc_terminate($process);
                    proc_close($process);
                    self::fail('Timed out after '.self::TIMEOUT." s, with output:\n".file_get_contents($files[0]).file_get_contents($files[1]));
                }
                usleep(10_000);
            }
            $elapsed = (hrtime(true) - $start) / 1e9;
            proc_close($process);
            return [$status['exitcode'], file_get_contents($files[0]), $elapsed, file_get_contents($files[1])];
        } finally {
            array_map(unlink(...), $files);
        }
    }

    /**
     * Asserts a clean exit with `$output`, nothing on the standard error but the Rust panics with
     * the messages `$panics` (in that order).
     *
     * @param list<string> $panics
     */
    private static function assertRuns(string $output, string $code, string $init = 'initWith', ?string $setup = null, string $revolt = '', array $panics = []): float
    {
        [$exit, $actual, $elapsed, $errors] = self::runPhp($code, $init, $setup, $revolt);
        self::assertSame([0, $output], [$exit, $actual], $errors);
        self::assertPanics($panics, $errors);
        return $elapsed;
    }

    /** @param list<string> $panics */
    private static function assertPanics(array $panics, string $errors): void
    {
        // thread '<name>' (<id>) panicked at <file>:<line>:<column>:\n<message>\n
        $pattern = "/^thread '[^']*' \\(\\d+\\) panicked at [^\n]+:\n(.*)\n(note: run with `RUST_BACKTRACE=1`[^\n]*\n)?/m";
        preg_match_all($pattern, $errors, $matches);
        self::assertSame($panics, $matches[1], $errors);
        // Rust also starts each panic with an empty line.
        self::assertSame('', trim(preg_replace($pattern, '', $errors)), 'the standard error has nothing else');
    }

    public function testInitRunsTheFuturesOnTheSharedRuntime(): void
    {
        // Tokio's default thread name, not initWith()'s.
        self::assertRuns('tokio-', 'echo substr(Native::threadName(), 0, 6);', 'init');
    }

    /** @return iterable<string, array{string}> */
    public static function initializers(): iterable
    {
        yield 'init' => ['init'];
        yield 'initWith' => ['initWith'];
    }

    #[DataProvider('initializers')]
    public function testTheRequestEndsOnceTheFuturesBorrowingFromSuspendedCallersCompleted(string $init): void
    {
        // The caller is never resumed: when PHP destroys its fiber, suspend_on() waits for the
        // future, which borrows its argument, so its runtime must keep running it.
        $elapsed = self::assertRuns("exiting\n", <<<'PHP'
            async(static fn () => Native::echo('borrowed', 500));
            delay(0.05);
            echo "exiting\n";
            PHP, $init);
        self::assertGreaterThanOrEqual(0.5, $elapsed);
    }

    #[DataProvider('initializers')]
    public function testTheRequestEndsWithoutWaitingForTheFuturesOfTrySuspendOn(string $init): void
    {
        // PHP destroying the fiber interrupts try_suspend_on() with an exit, which stays pending.
        // A graceful exit runs the finally blocks.
        $elapsed = self::assertRuns("exiting\nunwound\n", <<<'PHP'
            async(static function (): void {
                try {
                    Native::trySleep(5_000);
                } finally {
                    echo "unwound\n";
                }
            });
            delay(0.05);
            echo "exiting\n";
            PHP, $init);
        self::assertLessThan(4.0, $elapsed, 'try_suspend_on() did not wait for the future');
    }

    public function testExitInterruptsTrySuspendOn(): void
    {
        // exit() in a fiber unwinds it with an unwind exit, which stays pending too.
        self::assertRuns("exiting\n", <<<'PHP'
            async(static function (): void {
                EventLoop::delay(0.05, static function (): void {
                    echo "exiting\n";
                    exit(0);
                });
                Native::trySleep(5_000);
                echo "not reached\n";
            })->await();
            PHP);
    }

    public function testShuttingDownReleasesAnIdleRuntimeAtOnce(): void
    {
        self::assertRuns('0 1', <<<'PHP'
            Native::sleep(1);
            echo Native::stoppedThreads(), ' ';
            Native::shutdown();
            echo Native::stoppedThreads();
            PHP);
    }

    public function testShuttingDownReleasesTheRuntimeOnceItsLastFutureCompleted(): void
    {
        // The future of an interrupted try_suspend_on() keeps running after shutdown().
        self::assertRuns('0 0 1', <<<'PHP'
            $suspension = null;
            $future = async(static function () use (&$suspension) {
                $suspension = EventLoop::getSuspension();
                try {
                    Native::trySleep(500);
                } catch (Exception) {
                }
            });
            delay(0);
            $suspension->throw(new Exception);
            $future->await();
            Native::shutdown();
            echo Native::stoppedThreads(), ' ';
            usleep(50_000);
            echo Native::stoppedThreads(), ' ';
            usleep(800_000);
            echo Native::stoppedThreads();
            PHP);
    }

    public function testCallersInterruptedAfterShutdown(): void
    {
        // Their suspensions were released: suspend_on() waits for its future (whose runtime is
        // still running it), try_suspend_on() doesn't, and both rethrow.
        self::assertRuns("sleep: after shutdown\ntrySleep: after shutdown\n0\n", <<<'PHP'
            $futures = $suspensions = [];
            foreach (['sleep', 'trySleep'] as $method) {
                $futures[] = async(static function () use ($method, &$suspensions): void {
                    $suspensions[] = EventLoop::getSuspension();
                    try {
                        [Native::class, $method](500);
                    } catch (Exception $e) {
                        echo "$method: ", $e->getMessage(), "\n";
                    }
                });
            }
            delay(0);
            Native::shutdown();
            foreach ($suspensions as $suspension) {
                $suspension->throw(new Exception('after shutdown'));
            }
            Amp\Future\await($futures);
            echo Native::pending(), "\n";
            PHP);
    }

    public function testTheStreamMustStayOpenUntilShutdown(): void
    {
        // After shutdown(), a new event loop has a new stream.
        $autoload = var_export(\dirname(__DIR__).'/vendor/autoload.php', true);
        self::assertRuns("php-tokio's notification stream was closed before shutdown()\n1\n", <<<'PHP'
            $stream = Native::initWith();
            fclose($stream);
            try {
                Native::initWith();
            } catch (Exception $e) {
                echo $e->getMessage(), "\n";
            }
            Native::shutdown();
            $stream = Native::initWith();
            stream_set_blocking($stream, false);
            EventLoop::onReadable($stream, static fn () => Native::wakeup());
            echo Native::sleep(1), "\n";
            PHP, setup: "require $autoload;");
    }

    public function testEveryEntryPointNeedsInit(): void
    {
        $autoload = var_export(\dirname(__DIR__).'/vendor/autoload.php', true);
        self::assertRuns("0\n\nRust panic: php-tokio's EventLoop::init() must be called first\nphp-tokio's EventLoop::init() must be called first\nphp-tokio's EventLoop::init() must be called first\n", <<<'PHP'
            echo Native::pending(), "\n";
            Native::drain(1_000);
            echo "\n";
            foreach ([static fn () => Native::sleep(1), static fn () => Native::trySleep(1), Native::wakeup(...)] as $call) {
                try {
                    $call();
                } catch (Throwable $e) {
                    echo $e->getMessage(), "\n";
                }
            }
            PHP, setup: "require $autoload;", panics: ["php-tokio's EventLoop::init() must be called first"]);
    }

    /** @return iterable<string, array{string, string}> */
    public static function brokenRevolts(): iterable
    {
        yield 'no Revolt' => ['', '\Revolt\EventLoop does not exist'];
        yield 'no Suspension' => ['namespace Revolt { class EventLoop {} }', '\Revolt\EventLoop\Suspension does not exist'];
        yield 'no getSuspension()' => [
            'namespace Revolt\EventLoop { interface Suspension {} } namespace Revolt { class EventLoop {} }',
            '\Revolt\EventLoop::getSuspension does not exist',
        ];
    }

    #[DataProvider('brokenRevolts')]
    public function testInitNeedsRevolt(string $revolt, string $error): void
    {
        self::assertRuns($error, <<<'PHP'
            try {
                Native::init();
            } catch (Exception $e) {
                echo $e->getMessage();
            }
            PHP, setup: '', revolt: $revolt);
    }

    public function testInitFailsIfTheRuntimeCantBeBuilt(): void
    {
        $autoload = var_export(\dirname(__DIR__).'/vendor/autoload.php', true);
        self::assertRuns('Could not build the Tokio runtime: no runtime today', <<<'PHP'
            try {
                Native::initWithFailingRuntime();
            } catch (Exception $e) {
                echo $e->getMessage();
            }
            PHP, setup: "require $autoload;");
    }

    /** A Revolt whose getSuspension() throws, or returns a suspension that can't suspend. */
    private const FAKE_REVOLT = <<<'PHP'
        namespace Revolt\EventLoop {
            interface Suspension {}
        }
        namespace Revolt {
            final class EventLoop
            {
                public static bool $throw = false;
                public static function getSuspension(): object
                {
                    if (self::$throw) {
                        throw new \Exception('no suspension today');
                    }
                    return new class {};
                }
            }
        }
        PHP;

    public function testAFailingGetSuspensionFailsTheCall(): void
    {
        // getSuspension()'s exception, left pending, is thrown (ext-php-rs throws nothing over it),
        // and nothing is spawned: pending() stays 0.
        self::assertRuns("no suspension today () 0\nno suspension today () 0\n", <<<'PHP'
            EventLoop::$throw = true;
            foreach ([static fn () => Native::sleep(1), static fn () => Native::trySleep(1)] as $call) {
                try {
                    $call();
                } catch (Throwable $e) {
                    echo $e->getMessage(), ' (', $e->getPrevious()?->getMessage(), ') ', Native::pending(), "\n";
                }
            }
            PHP, setup: 'Native::init();', revolt: self::FAKE_REVOLT, panics: ['Could not get the current suspension']);
    }

    public function testASuspensionThatCantSuspend(): void
    {
        // suspend_on() waits for the future; try_suspend_on() fails.
        self::assertRuns("5\nCould not suspend the caller: could not call the given function 0\n", <<<'PHP'
            echo Native::sleep(5), "\n";
            try {
                Native::trySleep(5);
            } catch (Exception $e) {
                echo $e->getMessage(), ' ', Native::pending(), "\n";
            }
            PHP, setup: 'Native::init();', revolt: self::FAKE_REVOLT);
    }

    #[RequiresOperatingSystemFamily('Linux')]
    public function testInitFailsWithoutDescriptorsOnLinux(): void
    {
        $this->assertInitFailsWithoutDescriptors();
    }

    #[RequiresOperatingSystemFamily('Darwin')]
    public function testInitFailsWithoutDescriptorsOnMacOs(): void
    {
        $this->assertInitFailsWithoutDescriptors();
    }

    /**
     * Uses descriptors up to the limit, then frees them one by one: the socket pair needs two,
     * and the stream for PHP one (initWith()'s runtime, without an I/O driver, none).
     */
    private function assertInitFailsWithoutDescriptors(): void
    {
        $autoload = var_export(\dirname(__DIR__).'/vendor/autoload.php', true);
        [$exit, $output] = self::runPhp(<<<'PHP'
            // Autoloaded now: once descriptors run out, files can't be included.
            class_exists(EventLoop::class);
            interface_exists(EventLoop\Suspension::class);
            $held = [];
            set_error_handler(static fn () => throw new Exception);
            while (true) {
                try {
                    $held[] = fopen(PHP_BINARY, 'r');
                } catch (Exception) {
                    break;
                }
            }
            restore_error_handler();
            while (true) {
                try {
                    Native::initWith();
                    echo "ok\n";
                    break;
                } catch (Exception $e) {
                    echo $e->getMessage(), "\n";
                    array_pop($held);
                }
            }
            PHP, setup: "require $autoload;", wrapper: ['sh', '-c', 'ulimit -n 64 && exec "$@"', 'sh']);
        self::assertSame(0, $exit, $output);
        $errors = [
            'Could not create the notification socket: Too many open files (os error 24)',
            // Opening the stream for PHP.
            'Too many open files (os error 24)',
        ];
        $lines = array_values(array_unique(explode("\n", trim($output))));
        self::assertSame([...$errors, 'ok'], $lines, $output);
    }

    #[RequiresFunction('pcntl_fork')]
    public function testACallPendingAcrossForkEndsTheChildWithAnErrorInsteadOfHanging(): void
    {
        // The child's copy of the fiber waits for a future that only runs in the parent.
        [$exit, $output, , $errors] = self::runPhp(<<<'PHP'
            $future = async(static fn () => Native::sleep(300));
            delay(0);
            $pid = pcntl_fork();
            if ($pid === 0) {
                exit(0);
            }
            pcntl_waitpid($pid, $status);
            echo "\nchild: ", pcntl_wexitstatus($status), "\nparent: ", $future->await(), "\n";
            PHP);
        // PHP absorbs the Error while it destroys the fiber, at the end of the child's request.
        self::assertSame([0, "\nchild: 0\nparent: 300\n"], [$exit, $output], $errors);
        self::assertPanics(['php-tokio: this call was pending when the process forked, and can only complete in the parent'], $errors);
    }
}
