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
    assert!(stderr(&out).lines().any(|line| line.contains("ERROR")
        && line.contains("token.sha256")
        && line.contains("malformed")));
}

#[cfg(unix)]
#[test]
fn pair_tightens_existing_config_directory() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(daemon(dir.path(), &["pair"]).status.success());
    assert_eq!(
        fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[cfg(unix)]
#[test]
fn serve_tightens_existing_config_directory_with_host_id() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    assert!(daemon(dir.path(), &["pair"]).status.success());
    chassis_daemon::config::host_id(dir.path()).unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let log = tempfile::NamedTempFile::new().unwrap();
    let mut server = Command::new(env!("CARGO_BIN_EXE_chassis-daemon"))
        .args(["serve", "--bind", "127.0.0.1:0"])
        .env("CHASSIS_DAEMON_CONFIG_DIR", dir.path())
        .stderr(log.reopen().unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let listening = loop {
        if fs::read_to_string(log.path()).is_ok_and(|text| text.contains("listening on")) {
            break true;
        }
        if Instant::now() >= deadline || server.try_wait().ok().flatten().is_some() {
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let _ = server.kill();
    server.wait().unwrap();
    assert!(listening, "daemon did not start");
    assert_eq!(
        fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn serve_non_loopback_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let out = daemon(dir.path(), &["serve", "--bind", "0.0.0.0:7317"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("Tailscale Serve"), "{}", stderr(&out));
}

#[test]
fn removed_bind_override_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let removed = ["--allow", "non", "loopback"].join("-");
    let out = daemon(dir.path(), &["serve", &removed]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("unexpected argument"));
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
