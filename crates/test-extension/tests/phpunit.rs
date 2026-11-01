// Copyright 2023-2024 Daniil Gentili
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Runs the PHPUnit suite (tests/, after `composer install` there) against the test extension
//! cargo just built, so that `cargo test` (and cargo-mutants) tests php-tokio's code, which only
//! runs inside PHP. `PHP_TOKIO_TESTS` gives the suite's directory (cargo-mutants builds copies of
//! the tree without tests/vendor), `PHP` the PHP binary.

use std::{
    env::{
        self,
        consts::{DLL_PREFIX, DLL_SUFFIX},
    },
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, Instant},
};

/// A normal run takes about 15 s: past this, a mutant made something hang.
const DEADLINE: Duration = Duration::from_secs(90);

#[test]
fn phpunit() {
    // target/<profile>/deps/phpunit-<hash>: as the extension is also an rlib, cargo test builds it
    // next to it (only cargo build copies it to target/<profile>).
    let deps = env::current_exe().unwrap().parent().unwrap().to_owned();
    let extension = deps.join(format!("{DLL_PREFIX}php_tokio_test_extension{DLL_SUFFIX}"));
    assert!(extension.exists(), "{} is missing", extension.display());
    let tests = env::var_os("PHP_TOKIO_TESTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests"));
    let script = tests.join("vendor/phpunit/phpunit/phpunit");
    assert!(
        script.exists(),
        "Run composer install in {}",
        tests.display()
    );

    let mut phpunit = Command::new(env::var_os("PHP").unwrap_or("php".into()))
        .arg("-d")
        .arg(format!("extension={}", extension.display()))
        .arg(&script)
        .arg("--configuration")
        .arg(tests.join("phpunit.xml"))
        .current_dir(&tests)
        .env("PHP_TOKIO_TEST_EXTENSION", &extension)
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(status) = phpunit.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > DEADLINE {
            phpunit.kill().unwrap();
            phpunit.wait().unwrap();
            panic!("PHPUnit didn't finish within {DEADLINE:?}");
        }
        thread::sleep(Duration::from_millis(100));
    };
    assert!(status.success(), "PHPUnit failed: {status}");
}
