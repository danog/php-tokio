<?php declare(strict_types=1);

use Reqwest\Client;
use function PhpTokio\async;

if (!extension_loaded('example-reqwest-native')) {
    die("The example-reqwest extension is not loaded. Please load it to run this example.".PHP_EOL);
}

function test(int $delay): void
{
    $url = "https://httpbin.org/delay/$delay";
    $t = time();
    echo "Making async reqwest to $url that will return after $delay seconds...".PHP_EOL;
    Client::get($url);
    $t = time() - $t;
    echo "Got response from $url after ~".$t." seconds!".PHP_EOL;
};

async(fn() => test(5));
async(fn() => test(5));
async(fn() => test(5));

\PhpTokio\run();
