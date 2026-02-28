//! End-to-end tests for Phase 3 — Discovery commands.
//! All tests are #[ignore] because they require rust-analyzer installed.
//! Run with: `cargo test --test e2e_phase3 -- --ignored`

use std::path::Path;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

fn krait() -> assert_cmd::Command {
    cargo_bin_cmd!("krait")
}

fn rust_multi_file_dir() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/rust-multi-file"
    ))
}

#[test]
#[ignore = "requires rust-analyzer installed"]
fn find_symbol_via_cli() {
    krait()
        .args(["find", "symbol", "greet"])
        .current_dir(rust_multi_file_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("src/lib.rs"));
}

#[test]
#[ignore = "requires rust-analyzer installed"]
fn find_symbol_json_output() {
    let output = krait()
        .args(["find", "symbol", "Config", "--format", "json"])
        .current_dir(rust_multi_file_dir())
        .output()
        .expect("failed to run krait");

    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).expect("invalid utf8");
    let data: serde_json::Value = serde_json::from_str(&stdout).expect("invalid JSON");
    assert!(data.is_array());

    let items = data.as_array().unwrap();
    assert!(!items.is_empty());
    assert!(items.iter().any(|i| i["kind"] == "struct"));
}

#[test]
#[ignore = "requires rust-analyzer installed"]
fn find_symbol_not_found_exits_cleanly() {
    let output = krait()
        .args(["find", "symbol", "ZzzNonExistentSymbol999"])
        .current_dir(rust_multi_file_dir())
        .output()
        .expect("failed to run krait");

    // Should succeed with "no results" or empty array
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("no results") || stdout.contains("[]"),
        "unexpected output: {stdout}"
    );
}

#[test]
#[ignore = "requires rust-analyzer installed"]
fn find_refs_via_cli() {
    krait()
        .args(["find", "refs", "greet"])
        .current_dir(rust_multi_file_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("src/lib.rs"));
}

#[test]
#[ignore = "requires rust-analyzer installed"]
fn list_symbols_via_cli() {
    krait()
        .args(["list", "symbols", "src/lib.rs"])
        .current_dir(rust_multi_file_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("greet"))
        .stdout(predicate::str::contains("Config"));
}

#[test]
#[ignore = "requires rust-analyzer installed"]
fn list_symbols_json_output() {
    let output = krait()
        .args(["list", "symbols", "src/lib.rs", "--format", "json"])
        .current_dir(rust_multi_file_dir())
        .output()
        .expect("failed to run krait");

    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).expect("invalid utf8");
    let data: serde_json::Value = serde_json::from_str(&stdout).expect("invalid JSON");
    assert!(data.is_array());
}
