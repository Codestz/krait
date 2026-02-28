use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

fn krait() -> assert_cmd::Command {
    cargo_bin_cmd!("krait")
}

#[test]
fn krait_help_shows_all_commands() {
    krait()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("find"))
        .stdout(predicate::str::contains("read"))
        .stdout(predicate::str::contains("edit"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("daemon"))
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("check"))
        .stdout(predicate::str::contains("list"));
}

#[test]
fn krait_version() {
    krait()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("krait"));
}

#[test]
fn krait_find_subcommands_in_help() {
    krait()
        .args(["find", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("symbol"))
        .stdout(predicate::str::contains("refs"));
}

#[test]
fn krait_read_subcommands_in_help() {
    krait()
        .args(["read", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("file"))
        .stdout(predicate::str::contains("symbol"));
}

#[test]
fn krait_edit_subcommands_in_help() {
    krait()
        .args(["edit", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("replace"))
        .stdout(predicate::str::contains("insert-after"))
        .stdout(predicate::str::contains("insert-before"));
}

#[test]
fn krait_unknown_command_fails() {
    krait()
        .arg("nonexistent")
        .assert()
        .failure();
}
