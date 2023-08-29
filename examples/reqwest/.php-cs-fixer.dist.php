<?php

$config = new class extends Amp\CodeStyle\Config {
    public function getRules(): array
    {
        return array_merge(parent::getRules(), [
            'void_return' => true,
            'array_indentation' => true,
            'ternary_to_null_coalescing' => true,
            'assign_null_coalescing_to_coalesce_equal' => true,
        ]);
    }
};

$config->getFinder()
    ->in(__DIR__ . '/lib')
    ->append([__DIR__ . '/test.php']);

$cacheDir = getenv('TRAVIS') ? getenv('HOME') . '/.php-cs-fixer' : __DIR__;

$config->setCacheFile($cacheDir . '/.php_cs.cache');

return $config;
