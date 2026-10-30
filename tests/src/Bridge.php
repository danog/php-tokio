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

    public static function init(): void
    {
        if (self::$watcher !== null) {
            return;
        }
        $stream = Native::init();
        stream_set_blocking($stream, false);
        self::$watcher = EventLoop::onReadable($stream, static fn () => Native::wakeup());
    }
}
