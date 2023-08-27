# php-tokio - Use any async Rust library from PHP!

Created by Daniil Gentili ([@danog](https://github.com/danog)).  

This library allows you to use any async rust library from PHP, asynchronously.  

It's fully integrated with [revolt](https://revolt.run): this allows full compatibility with [amphp](https://amphp.org), [PSL](https://github.com/azjezz/psl) and reactphp.  

## Example

Here's an example, using the async Rust [reqwest](https://docs.rs/reqwest/latest/reqwest/) library to make asynchronous HTTP requests from PHP:

```php
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
```

Usage:

```bash
cd examples/reqwest && \
    cargo build && \
    composer update && \
    php -d extension=../../target/debug/libexample_reqwest.so test.php
```

Result:

```
Making async reqwest to https://httpbin.org/delay/5 that will return after 5 seconds...
Making async reqwest to https://httpbin.org/delay/5 that will return after 5 seconds...
Making async reqwest to https://httpbin.org/delay/5 that will return after 5 seconds...
Got response from https://httpbin.org/delay/5 after ~5 seconds!
Got response from https://httpbin.org/delay/5 after ~5 seconds!
Got response from https://httpbin.org/delay/5 after ~5 seconds!
```

See the [source code](https://github.com/danog/php-tokio/tree/master/examples/reqwest) of the example for more info on how it works!

## Built with php-tokio

Here's a list of async PHP extensions built with php-tokio ([add yours by editing this file!](https://github.com/danog/php-tokio/edit/master/README.md)):

- [nicelocal/mongo-php-async-driver](https://github.com/Nicelocal/mongo-php-async-driver) - An async MongoDB PHP extension