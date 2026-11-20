#!/usr/bin/env bash
# Starts PHP 3 times in one process (the embed SAPI, as FrankenPHP restarts it), with the test
# extension, running restart.php each time: php-tokio's module outlives PHP (it stays loaded), so
# nothing it keeps may refer to a PHP that ended, such as the type of its resources.
#
# Needs the embed SAPI's library and the headers of the same PHP.
# Usage: tests/embed/restart.sh <test extension> [php-config]
set -euo pipefail
dir=$(cd "$(dirname "$0")" && pwd)
extension=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
php_config=${2:-php-config}

prefix=$("$php_config" --prefix)
library=$(ls "$prefix"/lib/libphp*.so | head -1)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# shellcheck disable=SC2046
cc -o "$tmp/restart" "$dir/restart.c" $("$php_config" --includes) "$library" -Wl,-rpath,"$(dirname "$library")"
printf 'extension=%s\nerror_reporting=E_ALL\ndisplay_errors=stderr\n' "$extension" > "$tmp/php.ini"

PHPRC=$tmp/php.ini PHP_INI_SCAN_DIR= "$tmp/restart" "$dir/restart.php" 3 > "$tmp/out" 2> "$tmp/err" || status=$?
if [ "${status:-0}" != 0 ] || [ "$(cat "$tmp/out")" != $'1\n1\n1' ] || [ -s "$tmp/err" ]; then
    echo "Exit status ${status:-0}, expected 0; output (expected 1 per run):"
    cat "$tmp/out"
    echo "Standard error (expected empty):"
    cat "$tmp/err"
    exit 1
fi
echo "PHP started 3 times in one process: OK"
