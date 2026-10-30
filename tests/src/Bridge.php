<?php

declare(strict_types=1);

namespace PhpTokio\Test;

use Revolt\EventLoop;

/**
 * Watches php-tokio's notification socket, as an extension's PHP side does.
 */
final class Bridge
{
    private static ?string $watcher = null;
    /** @var resource|null */
    private static $stream = null;

    public static function init(): void
    {
        if (self::$watcher !== null) {
            return;
        }
        // A runtime of the event loop's own (one worker, named php-tokio-test): ShutdownTest
        // covers the shared one of init().
        $stream = Native::initWith();
        stream_set_blocking($stream, false);
        self::$stream = $stream;
        self::$watcher = EventLoop::onReadable($stream, static fn () => Native::wakeup());
    }

    /** Whether the notification stream is readable now. */
    public static function readable(): bool
    {
        $read = [self::$stream];
        $write = $except = null;
        return stream_select($read, $write, $except, 0) === 1;
    }
}
