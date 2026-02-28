/// E2E tests against `bench/simulation/` — exercises every LSP feature per language.
///
/// These tests require LSP servers to be installed. Run with:
///   cargo test --test e2e_simulation -- --ignored
///
/// Each test starts the krait daemon in the relevant simulation sub-directory,
/// waits for servers to initialise, then exercises find symbol, find symbol --path,
/// find impl, and read symbol.

// All tests in this module are `#[ignore]` — they won't run in CI by default.

/// Path to the simulation root (relative to the crate root).
const SIM_ROOT: &str = "bench/simulation";

fn sim_path(sub: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(SIM_ROOT)
        .join(sub)
}

// ---------------------------------------------------------------------------
// Go
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn go_find_symbol_process_users() {
    let dir = sim_path("go");
    assert!(dir.exists(), "simulation/go not found — run from repo root");
    // TODO: start daemon in `dir`, wait for gopls, then:
    //   krait find symbol ProcessUsers
    //   expect: functions.go, kind=function
}

#[test]
#[ignore]
fn go_find_symbol_with_receiver_prefix() {
    let dir = sim_path("go");
    assert!(dir.exists());
    // TODO: krait find symbol CreateKnowledgeFromFile
    //   expect: matches even though gopls indexes it as (*T).CreateKnowledgeFromFile
}

// ---------------------------------------------------------------------------
// TypeScript
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn ts_find_symbol_create_promotions_overload() {
    let dir = sim_path("typescript");
    assert!(dir.exists(), "simulation/typescript not found");
    // TODO: krait find symbol createPromotions --path typescript/
    //   expect: functions.ts, kind=function (impl, not stub)
}

#[test]
#[ignore]
fn ts_find_symbol_user_service() {
    let dir = sim_path("typescript");
    assert!(dir.exists());
    // TODO: krait find symbol UserService
    //   expect: classes.ts, kind=class
}

// ---------------------------------------------------------------------------
// Python
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn python_find_symbol_base_service() {
    let dir = sim_path("python");
    assert!(dir.exists(), "simulation/python not found");
    // TODO: krait find symbol BaseService
    //   expect: classes.py, kind=class
}

// ---------------------------------------------------------------------------
// Java
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn java_find_symbol_user_service() {
    let dir = sim_path("java");
    assert!(dir.exists(), "simulation/java not found");
    // TODO: krait find symbol UserService
    //   expect: UserService.java, kind=class
}

#[test]
#[ignore]
fn java_find_impl_irepository() {
    let dir = sim_path("java");
    assert!(dir.exists());
    // TODO: krait find impl IRepository
    //   expect: UserService.java (concrete implementation)
}

// ---------------------------------------------------------------------------
// C#
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn csharp_find_symbol_user_service() {
    let dir = sim_path("csharp");
    assert!(dir.exists(), "simulation/csharp not found");
    // TODO: krait find symbol UserService
    //   expect: UserService.cs, kind=class
}

#[test]
#[ignore]
fn csharp_find_impl_irepository() {
    let dir = sim_path("csharp");
    assert!(dir.exists());
    // TODO: krait find impl IRepository
    //   expect: UserService.cs
}

// ---------------------------------------------------------------------------
// Ruby
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn ruby_find_symbol_user_service() {
    let dir = sim_path("ruby");
    assert!(dir.exists(), "simulation/ruby not found");
    // TODO: krait find symbol UserService
    //   expect: classes.rb, kind=class
}

// ---------------------------------------------------------------------------
// Lua
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn lua_find_symbol_process_users() {
    let dir = sim_path("lua");
    assert!(dir.exists(), "simulation/lua not found");
    // TODO: krait find symbol processUsers
    //   expect: functions.lua, kind=function
}

// ---------------------------------------------------------------------------
// C++
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn cpp_find_symbol_user_service() {
    let dir = sim_path("cpp");
    assert!(dir.exists(), "simulation/cpp not found");
    // TODO: krait find symbol UserService
    //   expect: classes.cpp or classes.h, kind=class
}

// ---------------------------------------------------------------------------
// Kotlin
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn kotlin_find_symbol_user_service() {
    let dir = sim_path("kotlin");
    assert!(dir.exists(), "simulation/kotlin not found");
    // TODO: krait find symbol UserService
    //   expect: Classes.kt, kind=class
}

// ---------------------------------------------------------------------------
// Swift
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn swift_find_symbol_user_service() {
    let dir = sim_path("swift");
    assert!(dir.exists(), "simulation/swift not found");
    // TODO: krait find symbol UserService
    //   expect: Classes.swift, kind=class
}

// ---------------------------------------------------------------------------
// PHP
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn php_find_symbol_user_service() {
    let dir = sim_path("php");
    assert!(dir.exists(), "simulation/php not found");
    // TODO: krait find symbol UserService
    //   expect: Classes.php, kind=class
}

// ---------------------------------------------------------------------------
// Dart
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn dart_find_symbol_user_service() {
    let dir = sim_path("dart");
    assert!(dir.exists(), "simulation/dart not found");
    // TODO: krait find symbol UserService
    //   expect: classes.dart, kind=class
}

// ---------------------------------------------------------------------------
// Scala
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn scala_find_symbol_user_service() {
    let dir = sim_path("scala");
    assert!(dir.exists(), "simulation/scala not found");
    // TODO: krait find symbol UserService
    //   expect: Classes.scala, kind=class
}

// ---------------------------------------------------------------------------
// R
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn r_find_symbol_user_service() {
    let dir = sim_path("r");
    assert!(dir.exists(), "simulation/r not found");
    // TODO: krait find symbol UserService
    //   expect: classes.R, kind=function/variable
}

// ---------------------------------------------------------------------------
// Haskell
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn haskell_find_symbol_user_service() {
    let dir = sim_path("haskell");
    assert!(dir.exists(), "simulation/haskell not found");
    // TODO: krait find symbol UserService
    //   expect: Classes.hs, kind=class
}

// ---------------------------------------------------------------------------
// Elixir
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn elixir_find_symbol_user_service() {
    let dir = sim_path("elixir");
    assert!(dir.exists(), "simulation/elixir not found");
    // TODO: krait find symbol UserService
    //   expect: classes.ex, kind=module
}

// ---------------------------------------------------------------------------
// Perl
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn perl_find_symbol_user_service() {
    let dir = sim_path("perl");
    assert!(dir.exists(), "simulation/perl not found");
    // TODO: krait find symbol UserService
    //   expect: Classes.pm, kind=class
}

// ---------------------------------------------------------------------------
// Zig
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn zig_find_symbol_user_service() {
    let dir = sim_path("zig");
    assert!(dir.exists(), "simulation/zig not found");
    // TODO: krait find symbol UserService
    //   expect: classes.zig, kind=struct
}

// ---------------------------------------------------------------------------
// Bash
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn bash_find_symbol_validate_email() {
    let dir = sim_path("bash");
    assert!(dir.exists(), "simulation/bash not found");
    // TODO: krait find symbol validate_email
    //   expect: functions.sh, kind=function
}

// ---------------------------------------------------------------------------
// YAML
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn yaml_list_symbols() {
    let dir = sim_path("yaml");
    assert!(dir.exists(), "simulation/yaml not found");
    // TODO: krait list symbols config.yaml
    //   expect: top-level keys
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn json_list_symbols() {
    let dir = sim_path("json");
    assert!(dir.exists(), "simulation/json not found");
    // TODO: krait list symbols config.json
    //   expect: top-level keys
}

// ---------------------------------------------------------------------------
// Markdown
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn markdown_list_symbols() {
    let dir = sim_path("markdown");
    assert!(dir.exists(), "simulation/markdown not found");
    // TODO: krait list symbols README.md
    //   expect: heading symbols
}

// ---------------------------------------------------------------------------
// Concurrent load
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn concurrent_find_symbol_15_requests() {
    // TODO: spin up simulation/ root daemon, then fire 15 concurrent
    //   krait find symbol UserService requests and verify all return results.
    // This exercises the LRU pool at capacity (10→20 bump).
    let _sim = sim_path("");
    assert!(_sim.exists(), "bench/simulation not found");
}

// ---------------------------------------------------------------------------
// LRU Stress Test
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn lru_global_cap_10_cycles_24_languages() {
    // TODO: Set max_language_servers=10, cycle through all 24 simulation dirs,
    // verify that after 10 languages are active, further queries cause evictions,
    // and re-querying an evicted language triggers a cold re-boot.
    let _sim = sim_path("");
    assert!(_sim.exists(), "bench/simulation not found");
}
