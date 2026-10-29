<?php declare(strict_types=1);

// Tests php-tokio's event loop bridge with a local HTTP server: php test_event_loop.php
// (needs the example-reqwest extension, and composer install).

use Reqwest\Client;
use Revolt\EventLoop;
use Revolt\EventLoop\UncaughtThrowable;

use function Amp\async;
use function Amp\Future\await;

require __DIR__.'/vendor/autoload.php';

if (!extension_loaded('example-reqwest')) {
    die("The example-reqwest extension is not loaded. Please load it to run this test.".PHP_EOL);
}

$router = tempnam(sys_get_temp_dir(), 'router');
file_put_contents($router, '<?php usleep((int) (($_GET["delay"] ?? 0) * 1e6)); echo "body:", $_GET["id"] ?? "";');
$port = random_int(20000, 60000);
$server = proc_open([PHP_BINARY, '-S', "127.0.0.1:$port", $router], [['pipe', 'r'], ['file', '/dev/null', 'w'], ['file', '/dev/null', 'w']], $pipes, null, ['PHP_CLI_SERVER_WORKERS' => '8']);
register_shutdown_function(static function () use ($server, $router): void {
    proc_terminate($server);
    unlink($router);
});
usleep(500_000);
$base = "http://127.0.0.1:$port";

Client::init();

$failed = false;
function check(bool $ok, string $what): void
{
    global $failed;
    $failed = $failed || !$ok;
    echo ($ok ? 'ok   ' : 'FAIL '), $what, PHP_EOL;
}
/** Futures complete in any order: compare the results by key. */
function awaitSorted(array $futures): array
{
    $results = await($futures);
    ksort($results);
    return $results;
}

$t = microtime(true);
$results = awaitSorted([
    async(fn () => Client::get("$base/?delay=1&id=a")),
    async(fn () => Client::get("$base/?delay=1&id=b")),
    async(fn () => Client::get("$base/?delay=1&id=c")),
]);
check($results === ['body:a', 'body:b', 'body:c'] && microtime(true) - $t < 1.8, 'three 1 s requests run concurrently');

$futures = [];
for ($i = 0; $i < 200; $i++) {
    $futures[] = async(fn () => Client::get("$base/?id=$i"));
}
check(awaitSorted($futures) === array_map(fn ($i) => "body:$i", range(0, 199)), '200 concurrent requests, each with its own result');

// More futures complete than the notification pipe can hold while PHP is busy: none is lost.
$futures = [];
for ($i = 0; $i < 70_000; $i++) {
    $futures[] = async(fn () => Client::sleep(100));
}
EventLoop::queue(static fn () => usleep(1_000_000));
try {
    check(awaitSorted($futures) === array_fill(0, 70_000, 100), '70000 futures completing while PHP is busy');
} catch (Throwable $e) {
    check(false, '70000 futures completing while PHP is busy: '.$e->getMessage());
}

// suspend() throws in {main} while a request is in flight: the exception propagates once the
// request completed (the future borrows the caller's arguments), and later requests work.
EventLoop::queue(static function (): never {
    throw new RuntimeException('loop error');
});
try {
    Client::get("$base/?delay=0.5&id=x");
    check(false, 'an event loop error thrown into {main} propagates');
} catch (UncaughtThrowable $e) {
    check($e->getPrevious()?->getMessage() === 'loop error', 'an event loop error thrown into {main} propagates');
}
check(Client::get("$base/?id=y") === 'body:y', 'requests work after an interrupted one');

exit($failed ? 1 : 0);
