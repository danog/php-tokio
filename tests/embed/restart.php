<?php

declare(strict_types=1);

// Run by restart.c once per start of PHP: a suspended caller resumed, then the event loop shut
// down at the end of the request by its resource, whose type is registered again since PHP
// started again.

use PhpTokio\Test\Native;
use Revolt\EventLoop;

use function Amp\async;

require __DIR__.'/../vendor/autoload.php';

$stream = Native::init();
stream_set_blocking($stream, false);
EventLoop::onReadable($stream, static fn () => Native::wakeup());
echo async(static fn () => Native::sleep(1))->await(), "\n";
