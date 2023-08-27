<?php

use Reqwest\Client;

use function Amp\async;
use function Amp\Future\await;

require 'vendor/autoload.php';

Client::init();

function test(int $delay): void {
    $url = "https://httpbin.org/delay/$delay";
    $t = time();
    echo "Making async reqwest to $url that will return after $delay seconds...".PHP_EOL;
    Client::get($url);
    $t = time() - $t;
    echo "Got response from $url after ~".$t." seconds!".PHP_EOL;
};

$futures = [];
$futures []= async(test(...), 5);
$futures []= async(test(...), 5);
$futures []= async(test(...), 5);

await($futures);