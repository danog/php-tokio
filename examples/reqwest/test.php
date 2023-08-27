<?php

use Reqwest\Client;

use function Amp\async;
use function Amp\Future\await;

require 'vendor/autoload.php';

Client::init();

$test = function (string $url): void {
    $t = time();
    echo "Making async parallel reqwest to $url (time $t)...".PHP_EOL;
    var_dump(Client::get($url));
    $t = time() - $t;
    echo "Got response from $url after ~".$t." seconds!".PHP_EOL;
};

$futures = [];
$futures []= async($test(...), "https://httpbin.org/delay/5");
$futures []= async($test(...), "https://httpbin.org/delay/5");
$futures []= async($test(...), "https://httpbin.org/delay/5");

await($futures);