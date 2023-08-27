# php-tokio example

Here's a usage example of [php-tokio](https://github.com/danog/php-tokio/), using the async Rust [reqwest](https://docs.rs/reqwest/latest/reqwest/) library to make asynchronous HTTP requests from PHP:

```php
<?php

use Reqwest\Client;

use function Amp\async;
use function Amp\Future\await;

require 'vendor/autoload.php';

Client::init();

$test = function (string $url): void {
    $t = time();
    echo "Making async parallel reqwest to $url (time $t)...".PHP_EOL;
    $t = time() - $t;
    echo "Got response from $url after ~".$t." seconds!".PHP_EOL;
};

$futures = [];
$futures []= async($test(...), "https://httpbin.org/delay/5");
$futures []= async($test(...), "https://httpbin.org/delay/5");
$futures []= async($test(...), "https://httpbin.org/delay/5");

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
Making async parallel reqwest to https://httpbin.org/delay/5 (time 1693160463)...
Making async parallel reqwest to https://httpbin.org/delay/5 (time 1693160463)...
Making async parallel reqwest to https://httpbin.org/delay/5 (time 1693160463)...
Got response from https://httpbin.org/delay/5 after ~6 seconds!
Got response from https://httpbin.org/delay/5 after ~6 seconds!
Got response from https://httpbin.org/delay/5 after ~6 seconds!
```

See the [source code](https://github.com/danog/php-tokio/tree/master/examples/reqwest) of the example for more info on how it works!
