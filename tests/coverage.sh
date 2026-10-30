#!/usr/bin/env bash
# Measures the coverage of php-tokio's Rust code by the test suite, failing if a line or a
# function isn't covered: builds the test extension instrumented, runs the PHPUnit suite against
# it (the processes ShutdownTest starts write their own profiles) and the macros' unit tests,
# then reports. Needs cargo-llvm-cov.
#
# Regions are reported, not enforced: the uncovered ones are the errors of operations that can't
# fail (set_nonblocking() on a new socket), and code of generic functions that only some of their
# instantiations run.
#
# Usage: tests/coverage.sh [extra cargo llvm-cov report arguments, such as --html]
set -euo pipefail
cd "$(dirname "$0")/.."

eval "$(cargo llvm-cov show-env --sh)"
cargo llvm-cov clean --workspace
cargo build -p php-tokio-test-extension
# The macros run in the compiler: their unit tests cover them.
cargo test -p php-tokio-derive

case "$(uname -s)" in
    Linux) extension=target/debug/libphp_tokio_test_extension.so ;;
    Darwin) extension=target/debug/libphp_tokio_test_extension.dylib ;;
    *) extension=target/debug/php_tokio_test_extension.dll ;;
esac
extension="$PWD/$extension"
(cd tests && PHP_TOKIO_TEST_EXTENSION="$extension" php -d "extension=$extension" vendor/bin/phpunit)

# Windows can't limit a process's descriptors (ShutdownTest's ulimit), so there the error
# closures of the socket creation never run: only their lines are covered.
functions=(--fail-uncovered-functions 0)
case "$(uname -s)" in Linux | Darwin) ;; *) functions=() ;; esac

# The test extension is the tests' own code.
cargo llvm-cov report --workspace --ignore-filename-regex '(crates[/\\]test-extension|examples)[/\\]' \
    --show-missing-lines --fail-uncovered-lines 0 "${functions[@]}" "$@"
