use std::process::{Command, Output};

fn daemon(config_dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_chassis-daemon"))
        .args(args)
        .env("CHASSIS_DAEMON_CONFIG_DIR", config_dir)
        .env_remove("CHASSIS_TOKEN")
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn serve_unpaired_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let out = daemon(dir.path(), &["serve"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("chassis-daemon pair"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn serve_with_malformed_pairing_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("token.sha256"), "invalid").unwrap();
    let out = daemon(dir.path(), &["serve"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("malformed; run `chassis-daemon pair` again"));
}

#[test]
fn serve_non_loopback_without_flag_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let out = daemon(dir.path(), &["serve", "--bind", "0.0.0.0:7317"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("--allow-non-loopback"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn serve_interval_below_minimum_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let out = daemon(dir.path(), &["serve", "--interval-ms", "100"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("250"), "{}", stderr(&out));
}

#[test]
fn pair_prints_only_the_token_and_writes_the_hash() {
    let dir = tempfile::tempdir().unwrap();
    let out = daemon(dir.path(), &["pair"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let token = stdout.strip_suffix('\n').expect("trailing newline");
    assert_eq!(token.len(), 43);
    assert!(
        token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "not base64url: {token}"
    );
    assert!(dir.path().join("token.sha256").is_file());
}

#[test]
fn unknown_subcommand_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = daemon(dir.path(), &["bogus"]);
    assert_eq!(out.status.code(), Some(2));
}
