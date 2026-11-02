//! Process-level CLI guards.
//!
//! Regression test for: `mempalace-mcp -help` used to fall through to the
//! stdio MCP server and block on stdin forever, which looked like a hang.

use std::process::{Command, Stdio};

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mempalace-mcp"))
        .args(args)
        // stdin = /dev/null so a regression exits instead of hanging the test.
        .stdin(Stdio::null())
        .output()
        .expect("failed to execute mempalace-mcp")
}

#[test]
fn help_flag_prints_usage_and_exits_zero() {
    for flag in ["-h", "--help", "-help", "help"] {
        let out = run(&[flag]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{flag}: expected exit 0, stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("Usage:") && stdout.contains("reindex"),
            "{flag}: expected usage text, got: {stdout}"
        );
    }
}

#[test]
fn version_flag_prints_version_and_exits_zero() {
    let out = run(&["--version"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "unexpected version output: {stdout}"
    );
}

#[test]
fn unknown_subcommand_exits_two_with_a_message() {
    let out = run(&["serch"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "typo must fail fast, not serve; stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("serch") && stderr.contains("Usage:"),
        "stderr should name the token and show usage, got: {stderr}"
    );
    assert!(out.stdout.is_empty(), "errors belong on stderr");
}
