//! End-to-end tests for Phase 2 — LSP bootstrap.
//! All tests are #[ignore] because they require rust-analyzer installed.
//! Run with: `cargo test --test e2e_phase2 -- --ignored`

use std::path::Path;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

fn krait() -> assert_cmd::Command {
    cargo_bin_cmd!("krait")
}

fn rust_hello_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rust-hello"))
}

#[test]
#[ignore = "requires rust-analyzer installed"]
fn krait_status_shows_rust_language() {
    krait()
        .arg("status")
        .current_dir(rust_hello_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("rust"));
}

#[test]
#[ignore = "requires rust-analyzer installed"]
fn krait_status_json_has_lsp_section() {
    let output = krait()
        .args(["status", "--format", "json"])
        .current_dir(rust_hello_dir())
        .output()
        .expect("failed to run krait");

    assert!(output.status.success(), "krait status failed");

    let stdout = String::from_utf8(output.stdout).expect("invalid utf8");
    let data: serde_json::Value = serde_json::from_str(&stdout).expect("invalid JSON");

    assert_eq!(data["lsp"]["language"], "rust");
    assert!(data["project"]["root"].is_string());
    assert!(data["project"]["languages"].is_array());
}

#[test]
#[ignore = "requires rust-analyzer installed"]
fn krait_status_json_lsp_has_server_field() {
    let output = krait()
        .args(["status", "--format", "json"])
        .current_dir(rust_hello_dir())
        .output()
        .expect("failed to run krait");

    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).expect("invalid utf8");
    let data: serde_json::Value = serde_json::from_str(&stdout).expect("invalid JSON");

    // Server field should be rust-analyzer
    assert_eq!(data["lsp"]["server"], "rust-analyzer");
}
