<?php

namespace Reqwest;

use Revolt\EventLoop;

final class Client {
    private static ?string $id = null;
    
    public static function init(): void {
        if (self::$id !== null) {
            return;
        }

        $f = fopen("php://fd/".\Client::init(), 'r+');
        stream_set_blocking($f, false);
        self::$id = EventLoop::onReadable($f, fn () => \Client::wakeup());
    }

    public static function reference(): void{
        EventLoop::reference(self::$id);
    }
    public static function unreference(): void {
        EventLoop::unreference(self::$id);
    }

    public static function __callStatic(string $name, array $args): mixed {
        return \Client::$name(...$args);
    }
}
