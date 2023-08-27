# php-tokio example

Here's a usage example of [php-tokio](https://github.com/danog/php-tokio/), using the async Rust [reqwest](https://docs.rs/reqwest/latest/reqwest/) library to make asynchronous HTTP requests from PHP:

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
