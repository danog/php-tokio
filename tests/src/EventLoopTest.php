<?php

declare(strict_types=1);

namespace PhpTokio\Test;

use Amp\TimeoutCancellation;
use PHPUnit\Framework\Attributes\RequiresFunction;
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
        $futures = [async(Native::sleep(...), 500), async(Native::sleep(...), 500), async(Native::sleep(...), 500)];
        // The fibers start, and suspend.
        delay(0);
        self::assertSame(3, Native::pending());
        self::awaitAll($futures);
        self::assertSame(0, Native::pending());
    }

    public function testDrainResumesTheCallersWhoseFutureCompleted(): void
    {
        $future = async(Native::sleep(...), 50);
        delay(0);
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
        delay(0);
        $start = hrtime(true);
        Native::drain(50);
        $elapsed = (hrtime(true) - $start) / 1e9;
        self::assertGreaterThanOrEqual(0.05, $elapsed);
        self::assertLessThan(0.5, $elapsed);
        self::assertSame(1, Native::pending());
        self::assertSame(1_000, $future->await());
    }

    public function testDrainCollectsTheCompletionsOfInterruptedCallers(): void
    {
        $suspension = null;
        $future = async(static function () use (&$suspension): void {
            $suspension = EventLoop::getSuspension();
            Native::trySleep(500);
        });
        delay(0);
        $suspension->throw(new \RuntimeException('interrupted'));
        try {
            $future->await();
        } catch (\RuntimeException) {
        }
        // Blocking: the completion is queued before any wakeup, and nobody waits for it.
        usleep(700_000);
        Native::drain(0);
        self::assertSame(0, Native::pending());
        self::assertSame(1, Native::sleep(1));
    }

    public function testTrySuspendOnReturnsTheInterruptingExceptionWithoutWaiting(): void
    {
        $exception = new \RuntimeException('interrupted');
        $suspension = null;
        $future = async(static function () use (&$suspension, $exception): float {
            $suspension = EventLoop::getSuspension();
            $start = hrtime(true);
            try {
                Native::trySleep(1_000);
                self::fail('No exception thrown');
            } catch (\RuntimeException $e) {
                $elapsed = (hrtime(true) - $start) / 1e9;
                // Unchanged: not wrapped in an exception of php-tokio's.
                self::assertSame($exception, $e);
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

    public function testInitializingAgainGivesTheSameStream(): void
    {
        // Not to be closed (see ShutdownTest), so not a new one.
        self::assertSame(Bridge::stream(), Native::initWith());
        self::assertSame(Bridge::stream(), Native::init());
        // The callers suspended meanwhile are still the event loop's.
        $future = async(Native::sleep(...), 500);
        delay(0);
        self::assertSame(1, Native::pending());
        self::assertSame(500, $future->await());
    }

    public function testAResumeThatThrowsDoesNotStopTheOtherResumes(): void
    {
        // Resumed by someone else, the first caller blocks in php-tokio until its future
        // completes: php-tokio's own resume() then throws, as it isn't suspended any more.
        $suspension = null;
        $first = async(static function () use (&$suspension): int {
            $suspension = EventLoop::getSuspension();
            return Native::sleep(200);
        });
        $second = async(Native::sleep(...), 200);
        delay(0);
        $errors = [];
        $handler = EventLoop::getErrorHandler();
        EventLoop::setErrorHandler(static function (\Throwable $e) use (&$errors): void {
            $errors[] = $e->getMessage();
        });
        try {
            $suspension->resume();
            self::assertSame(200, $first->await());
            // Both futures completed while the first caller blocked: the first one's completion
            // may be collected by the wakeup that resumes the second one, or by the next one.
            self::assertSame(200, $second->await(new TimeoutCancellation(2)));
            for ($i = 0; !$errors && $i < 200; $i++) {
                delay(0.01);
            }
            self::assertSame(['Must call suspend() before calling resume()'], $errors);
        } finally {
            EventLoop::setErrorHandler($handler);
        }
    }

    public function testTheStreamIsReadableExactlyWhileACompletionIsWaiting(): void
    {
        self::assertFalse(Bridge::readable(), 'nothing completed');
        self::assertSame(1, Native::sleep(1));
        self::assertFalse(Bridge::readable(), 'wakeup() consumed the notification');

        foreach ([Native::wakeup(...), static fn () => Native::drain(0)] as $consume) {
            $future = async(Native::sleep(...), 200);
            delay(0);
            // Blocking: the future completes, and nobody consumes its notification.
            usleep(400_000);
            self::assertTrue(Bridge::readable(), 'the completion was notified');
            $consume();
            self::assertFalse(Bridge::readable(), 'the notification was consumed');
            self::assertSame(200, $future->await());
        }
    }

    public function testBorrowUncheckedKeepsWhatItBorrows(): void
    {
        self::assertSame([3, 3, 3], Native::borrowedLengths('abc'));
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

    #[RequiresFunction('pcntl_fork')]
    public function testAForkedChildCanNeitherUseNorDisturbTheEventLoop(): void
    {
        // Pending in the parent across the fork (with try_suspend_on(): see ShutdownTest for
        // suspend_on()).
        $future = async(Native::trySleep(...), 200);
        delay(0);
        $pid = pcntl_fork();
        if ($pid === 0) {
            exit(self::useAfterFork());
        }
        // Blocks the PHP thread: the future completes meanwhile, and its notification must still
        // be there once the child exits, although the child called wakeup() all along.
        pcntl_waitpid($pid, $status);
        self::assertTrue(pcntl_wifexited($status), 'the child exited');
        self::assertSame(0, pcntl_wexitstatus($status), 'the child failed check '.pcntl_wexitstatus($status));
        self::assertSame(200, $future->await(new TimeoutCancellation(2)));
    }

    /**
     * In a forked child, returns 0, or the number of the first check that failed. Tokio's threads
     * aren't inherited: everything fails or does nothing.
     */
    private static function useAfterFork(): int
    {
        $rejected = static function (\Closure $call): bool {
            try {
                $call();
                return false;
            } catch (\Throwable $e) {
                return str_contains($e->getMessage(), 'fork');
            }
        };
        $start = hrtime(true);
        $checks = [
            static fn () => $rejected(Native::init(...)),
            static fn () => $rejected(Native::initWith(...)),
            static fn () => $rejected(static fn () => Native::trySleep(1)),
            static fn () => $rejected(static fn () => Native::sleep(1)),
            // The parent's caller is pending there, not here.
            static fn () => Native::pending() === 0,
            static function () use (&$start): bool {
                $start = hrtime(true);
                Native::drain(1_000);
                return (hrtime(true) - $start) / 1e9 < 0.5;
            },
            // The notification of the parent's future (due within 200 ms) stays in the shared socket.
            static function (): bool {
                $start = hrtime(true);
                while ((hrtime(true) - $start) / 1e9 < 0.4) {
                    Native::wakeup();
                    usleep(1_000);
                }
                return true;
            },
        ];
        foreach ($checks as $i => $check) {
            if (!$check()) {
                return $i + 1;
            }
        }
        return 0;
    }
}
