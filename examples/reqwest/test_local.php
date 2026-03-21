<?php declare(strict_types=1);

/**
 * Local test suite for php-tokio / example-reqwest.
 *
 * Verifies that async Rust futures are properly bridged to PHP fibers
 * WITHOUT making any external network requests.
 *
 * Usage (Linux / macOS):
 *   php -d extension=../../target/debug/libexample_reqwest.so test_local.php
 *
 * Usage (Windows):
 *   php -d extension=..\..\target\debug\example_reqwest.dll test_local.php
 */

use Reqwest\Client;

use function Amp\async;
use function Amp\Future\await;

require 'vendor/autoload.php';

if (!extension_loaded('example-reqwest')) {
    die("The example-reqwest extension is not loaded. Please load it to run this example." . PHP_EOL);
}

Client::init();

$failed = false;

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------
function assert_true(bool $condition, string $message): void
{
    global $failed;
    if ($condition) {
        echo "  PASS: $message" . PHP_EOL;
    } else {
        echo "  FAIL: $message" . PHP_EOL;
        $failed = true;
    }
}

// ---------------------------------------------------------------------------
// Test 1 – basic async sleep
// ---------------------------------------------------------------------------
echo PHP_EOL . "Test 1: basic async sleep (200 ms)" . PHP_EOL;

$t = microtime(true);
Client::sleep(200);
$elapsed = (microtime(true) - $t) * 1000;

assert_true($elapsed >= 190, "sleep(200) took at least 190 ms (took {$elapsed} ms)");
assert_true($elapsed < 1000, "sleep(200) took less than 1 s (took {$elapsed} ms)");

// ---------------------------------------------------------------------------
// Test 2 – concurrent async sleeps run in parallel
// ---------------------------------------------------------------------------
echo PHP_EOL . "Test 2: three concurrent 300 ms sleeps finish in ~300 ms total" . PHP_EOL;

function doSleep(int $ms): void
{
    Client::sleep($ms);
}

$t = microtime(true);

$futures = [];
$futures[] = async(doSleep(...), 300);
$futures[] = async(doSleep(...), 300);
$futures[] = async(doSleep(...), 300);

await($futures);

$elapsed = (microtime(true) - $t) * 1000;

// Sequential execution would take ~900 ms; parallel should be ~300 ms.
assert_true($elapsed >= 290, "three parallel sleeps took at least 290 ms (took {$elapsed} ms)");
assert_true($elapsed < 700, "three parallel sleeps finished well under 700 ms (took {$elapsed} ms)");

// ---------------------------------------------------------------------------
// Test 3 – return value is propagated correctly
// ---------------------------------------------------------------------------
echo PHP_EOL . "Test 3: async method return value is propagated" . PHP_EOL;

function fetchSleep(int $ms): bool
{
    Client::sleep($ms);
    return true;
}

$result = async(fetchSleep(...), 50);
$value  = $result->await();

assert_true($value === true, "return value from async fiber is true");

// ---------------------------------------------------------------------------
// Summary
// ---------------------------------------------------------------------------
echo PHP_EOL;
if ($failed) {
    echo "Some tests FAILED." . PHP_EOL;
    exit(1);
} else {
    echo "All tests passed." . PHP_EOL;
    exit(0);
}
