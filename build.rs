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

//! Sets cfg `php_debug` for a debug build of PHP, whose stream functions take more arguments,
//! asking the PHP that ext-php-rs builds against: the `PHP` environment variable, else `php`.

use std::process::Command;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(php_debug)");
    println!("cargo:rerun-if-env-changed=PHP");
    // docs.rs has no PHP.
    if std::env::var_os("DOCS_RS").is_some() {
        return;
    }
    let php = std::env::var("PHP").unwrap_or_else(|_| "php".into());
    let output = Command::new(&php)
        .args(["-r", "echo PHP_DEBUG;"])
        .output()
        .unwrap_or_else(|err| panic!("Could not run {php}: {err}"));
    assert!(output.status.success(), "{php} failed: {output:?}");
    if output.stdout == b"1" {
        println!("cargo:rustc-cfg=php_debug");
    }
}
