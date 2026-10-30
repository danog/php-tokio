<?php

declare(strict_types=1);

namespace PhpTokio\Test;

use PHPUnit\Framework\Attributes\RequiresFunction;
use PHPUnit\Framework\Attributes\RequiresOperatingSystemFamily;
use PHPUnit\Framework\TestCase;
use Revolt\EventLoop;
use Revolt\EventLoop\UncaughtThrowable;

use function Amp\async;
use function Amp\delay;
use function Amp\Future\await;

final class EventLoopTest extends TestCase
{
    public static function setUpBeforeClass(): void
    {
        if (!class_exists(Native::class)) {
            self::fail('Load the test extension: php -d extension=<path to php_tokio_test_extension> vendor/bin/phpunit');
        }
        Bridge::init();
    }

    /** Awaits the futures, returning their results by key (await() returns them in completion order). */
    private static function awaitAll(array $futures): array
    {
        $results = await($futures);
        ksort($results);
        return $results;
    }

    public function testFuturesRunConcurrently(): void
    {
        $start = hrtime(true);
        $results = self::awaitAll([
            async(Native::sleep(...), 300),
            async(Native::sleep(...), 300),
            async(Native::sleep(...), 300),
        ]);
        $elapsed = (hrtime(true) - $start) / 1e9;
        self::assertSame([300, 300, 300], $results);
        self::assertGreaterThanOrEqual(0.3, $elapsed);
        self::assertLessThan(0.6, $elapsed, 'three 300 ms futures took the time of one');
    }

    public function testEveryCallerGetsItsOwnResult(): void
    {
        $futures = [];
        for ($i = 0; $i < 1000; $i++) {
            $futures[] = async(Native::echo(...), "value $i", random_int(0, 50));
        }
        self::assertSame(array_map(static fn (int $i) => "value $i", range(0, 999)), self::awaitAll($futures));
    }

    public function testArgumentsBorrowedAcrossTheSuspensionStayValid(): void
    {
        // php_async_impl borrows the arguments for the whole suspension: churn PHP's allocator
        // meanwhile, so a dangling borrow would read reused memory.
        $value = str_repeat('borrowed ', 100_000);
        $future = async(Native::echo(...), $value, 200);
        $churn = async(static function (): void {
            for ($i = 0; $i < 20; $i++) {
                $garbage = array_map(static fn (int $n) => str_repeat((string) $n, 1000), range(0, 999));
                unset($garbage);
                Native::sleep(5);
            }
        });
        $churn->await();
        self::assertSame($value, $future->await());
    }

    public function testResultsArriveWithoutDelayWhilePhpWatchesSockets(): void
    {
        // PHP's select() emulation on Windows polls anything but sockets in 100 ms slices once it
        // also watches sockets: the notifications must come through a socket of PHP's own.
        $server = stream_socket_server('tcp://127.0.0.1:0');
        \assert($server !== false);
        $watcher = EventLoop::onReadable($server, static fn () => null);
        EventLoop::unreference($watcher);
        try {
            $start = hrtime(true);
            for ($i = 0; $i < 20; $i++) {
                self::assertSame(1, Native::sleep(1));
            }
            $elapsed = (hrtime(true) - $start) / 1e9;
            self::assertLessThan(0.5, $elapsed, sprintf('20 results of 1 ms futures took %.3f s', $elapsed));
        } finally {
            EventLoop::cancel($watcher);
            fclose($server);
        }
    }

    public function testErrorsBecomeExceptions(): void
    {
        try {
            Native::fail('it failed', 10);
            self::fail('No exception thrown');
        } catch (\Exception $e) {
            self::assertStringContainsString('it failed', $e->getMessage());
        }
    }

    public function testFuturesCompletingWhilePhpIsBusy(): void
    {
        // Many completions arrive while the PHP thread is busy, before any wakeup: more than a
        // pipe buffer holds (16 KiB by default on macOS). Windows commits every fiber's stack
        // (2 MiB by default) when the fiber starts, so fewer callers fit there.
        $count = PHP_OS_FAMILY === 'Windows' ? 2_000 : 20_000;
        $futures = [];
        for ($i = 0; $i < $count; $i++) {
            $futures[] = async(Native::sleep(...), 50);
        }
        EventLoop::queue(static fn () => usleep(500_000));
        self::assertSame(array_fill(0, $count, 50), self::awaitAll($futures));
    }

    public function testFuturesRunOnTheRuntimeFromInitWith(): void
    {
        self::assertStringStartsWith('php-tokio-test', Native::threadName());
    }

    public function testPendingCountsTheSuspendedCallers(): void
    {
        $futures = [async(Native::sleep(...), 100), async(Native::sleep(...), 100), async(Native::sleep(...), 100)];
        delay(0.02);
        self::assertSame(3, Native::pending());
        self::awaitAll($futures);
        self::assertSame(0, Native::pending());
    }

    public function testDrainResumesTheCallersWhoseFutureCompleted(): void
    {
        $future = async(Native::sleep(...), 50);
        delay(0.01);
        $start = hrtime(true);
        // Without running the event loop: drain() waits for the future itself.
        Native::drain(5_000);
        self::assertLessThan(1.0, (hrtime(true) - $start) / 1e9, 'drain() returned once the future completed');
        self::assertSame(0, Native::pending());
        self::assertSame(50, $future->await());
    }

    public function testDrainWaitsAtMostItsTimeout(): void
    {
        $future = async(Native::sleep(...), 1_000);
        delay(0.01);
        $start = hrtime(true);
        Native::drain(50);
        $elapsed = (hrtime(true) - $start) / 1e9;
        self::assertGreaterThanOrEqual(0.05, $elapsed);
        self::assertLessThan(0.5, $elapsed);
        self::assertSame(1, Native::pending());
        self::assertSame(1_000, $future->await());
    }

    public function testTrySuspendOnReturnsTheInterruptingExceptionWithoutWaiting(): void
    {
        $exception = new \RuntimeException('interrupted');
        $suspension = null;
        $future = async(static function () use (&$suspension): float {
            $suspension = EventLoop::getSuspension();
            $start = hrtime(true);
            try {
                Native::trySleep(1_000);
                self::fail('No exception thrown');
            } catch (\RuntimeException $e) {
                $elapsed = (hrtime(true) - $start) / 1e9;
                // Unchanged: not wrapped in an exception of php-tokio's.
                self::assertSame('interrupted', $e->getMessage());
                return $elapsed;
            }
        });
        delay(0.05);
        $suspension->throw($exception);
        self::assertLessThan(0.5, $future->await(), 'try_suspend_on() did not wait for the future');
        self::assertSame(0, Native::pending());
        // The detached future completes later, and resumes nobody.
        self::assertSame(1, Native::trySleep(1));
        delay(1.0);
        self::assertSame(2, Native::trySleep(2));
    }

    public function testWakeupWithoutCompletionsDoesNothing(): void
    {
        Native::wakeup();
        self::assertSame(1, Native::sleep(1));
    }

    public function testAnEventLoopErrorThrownIntoMainPropagatesOnceTheFutureCompleted(): void
    {
        // suspend() throws in {main} while a future is pending: the caller is not resumed, and
        // php-tokio returns only once the future, which may borrow its arguments, completed.
        EventLoop::queue(static function (): never {
            throw new \RuntimeException('event loop error');
        });
        $start = hrtime(true);
        try {
            Native::echo('borrowed', 300);
            self::fail('No exception thrown');
        } catch (UncaughtThrowable $e) {
            self::assertSame('event loop error', $e->getPrevious()?->getMessage());
        }
        self::assertGreaterThanOrEqual(0.3, (hrtime(true) - $start) / 1e9);
        // The next calls work, and the interrupted caller is never resumed.
        self::assertSame('after', Native::echo('after', 300));
        self::assertSame([1, 2], self::awaitAll([async(Native::sleep(...), 1), async(Native::sleep(...), 2)]));
    }

    #[RequiresOperatingSystemFamily('Linux')]
    #[RequiresFunction('pcntl_fork')]
    public function testUseAfterForkIsRejectedOnLinux(): void
    {
        $this->assertUseAfterForkIsRejected();
    }

    #[RequiresOperatingSystemFamily('Darwin')]
    #[RequiresFunction('pcntl_fork')]
    public function testUseAfterForkIsRejectedOnMacOs(): void
    {
        $this->assertUseAfterForkIsRejected();
    }

    private function assertUseAfterForkIsRejected(): void
    {
        $pid = pcntl_fork();
        if ($pid === 0) {
            // Tokio's threads aren't inherited: initializing again must fail.
            try {
                Native::init();
                $code = 1;
            } catch (\Throwable $e) {
                $code = str_contains($e->getMessage(), 'fork') ? 0 : 2;
            }
            posix_kill(getmypid(), $code === 0 ? SIGUSR1 : SIGUSR2);
            exit($code);
        }
        pcntl_waitpid($pid, $status);
        self::assertTrue(pcntl_wifsignaled($status), 'the child exited by signal');
        self::assertSame(SIGUSR1, pcntl_wtermsig($status), 'init() was rejected in the forked child');
    }
}
