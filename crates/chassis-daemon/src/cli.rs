//! Command line interface: argument parsing, serve guards, dispatch and exit codes.

use std::fmt;
use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use tracing::level_filters::LevelFilter;

use crate::auth::{self, TOKEN_HASH_FILE};
use crate::config;

/// Smallest accepted sampling interval.
pub const MIN_INTERVAL_MS: u32 = 250;

/// A runtime refusal or failure, printed as `error: <message>`.
#[derive(Debug)]
pub struct Error(String);

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Error(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "chassis-daemon",
    version,
    about = "Chassis host telemetry daemon"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a new pairing token (invalidates the previous one)
    Pair,
    /// Serve host telemetry over a WebSocket
    Serve(ServeArgs),
    /// Print the telemetry stream of a running daemon (debug client)
    Tail(TailArgs),
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Address to listen on
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:7317")]
    pub bind: SocketAddr,
    /// Allow binding a non-loopback address
    #[arg(long)]
    pub allow_non_loopback: bool,
    /// Sampling interval in milliseconds (minimum 250)
    #[arg(long, value_name = "MS", default_value_t = 1000)]
    pub interval_ms: u32,
    /// Log level: off, error, warn, info, debug or trace
    #[arg(long, value_name = "LEVEL", default_value = "info")]
    pub log_level: LevelFilter,
}

#[derive(Debug, Args)]
pub struct TailArgs {
    /// WebSocket URL of the daemon
    #[arg(long, value_name = "URL", default_value = "ws://127.0.0.1:7317/v1/ws")]
    pub url: String,
    /// Pairing token
    #[arg(
        long,
        env = "CHASSIS_TOKEN",
        hide_env_values = true,
        value_name = "TOKEN"
    )]
    pub token: String,
}

/// Validate `serve` arguments against the environment, before anything is started.
pub fn check_serve(args: &ServeArgs, config_dir: &Path) -> Result<(), Error> {
    if args.interval_ms < MIN_INTERVAL_MS {
        return Err(Error::new(format!(
            "--interval-ms must be at least {MIN_INTERVAL_MS} (got {})",
            args.interval_ms
        )));
    }
    if !args.bind.ip().is_loopback() && !args.allow_non_loopback {
        return Err(Error::new(format!(
            "refusing to bind non-loopback address {}; pass --allow-non-loopback to allow it",
            args.bind
        )));
    }
    if !config_dir.join(TOKEN_HASH_FILE).is_file() {
        return Err(Error::new(format!(
            "no pairing token found in {}; run `chassis-daemon pair` first",
            config_dir.display()
        )));
    }
    Ok(())
}

fn run(cli: Cli) -> Result<(), Error> {
    match cli.command {
        Command::Pair => {
            let dir = config::resolve_dir()?;
            let token = auth::pair(&dir)?;
            println!("{token}");
            eprintln!("This token is shown once; it is not stored. Save it now.");
            eprintln!("Any previously issued token no longer works.");
            eprintln!(
                "Token hash written to {}",
                dir.join(TOKEN_HASH_FILE).display()
            );
            eprintln!("Consider clearing your terminal scrollback.");
            Ok(())
        }
        Command::Serve(args) => {
            let dir = config::resolve_dir()?;
            check_serve(&args, &dir)?;
            Err(Error::new("serve is not implemented yet"))
        }
        Command::Tail(_args) => Err(Error::new("tail is not implemented yet")),
    }
}

/// Parse arguments, run the command and map the outcome to an exit code.
pub fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let code = if err.use_stderr() { 2 } else { 0 };
            let _ = err.print();
            return ExitCode::from(code);
        }
    };
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    fn serve_args(extra: &[&str]) -> ServeArgs {
        let mut argv = vec!["chassis-daemon", "serve"];
        argv.extend_from_slice(extra);
        match Cli::try_parse_from(argv).unwrap().command {
            Command::Serve(args) => args,
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn version_reports_package_version() {
        let error = Cli::try_parse_from(["chassis-daemon", "--version"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DisplayVersion);
        assert!(error.to_string().contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn help_lists_all_subcommands() {
        let error = Cli::try_parse_from(["chassis-daemon", "--help"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        let help = error.to_string();
        for name in ["pair", "serve", "tail"] {
            assert!(help.contains(name), "help is missing {name}:\n{help}");
        }
    }

    #[test]
    fn serve_defaults() {
        let args = serve_args(&[]);
        assert_eq!(args.bind, "127.0.0.1:7317".parse().unwrap());
        assert!(!args.allow_non_loopback);
        assert_eq!(args.interval_ms, 1000);
        assert_eq!(args.log_level, LevelFilter::INFO);
    }

    #[test]
    fn guard_rejects_short_interval_first() {
        let dir = tempfile::tempdir().unwrap();
        let args = serve_args(&["--interval-ms", "100", "--bind", "0.0.0.0:7317"]);
        let err = check_serve(&args, dir.path()).unwrap_err();
        assert!(err.to_string().contains("at least 250 (got 100)"));
    }

    #[test]
    fn guard_rejects_non_loopback_without_flag() {
        let dir = tempfile::tempdir().unwrap();
        let args = serve_args(&["--bind", "0.0.0.0:7317"]);
        let err = check_serve(&args, dir.path()).unwrap_err();
        assert!(err.to_string().contains("--allow-non-loopback"));
    }

    #[test]
    fn guard_rejects_unpaired_then_accepts_paired() {
        let dir = tempfile::tempdir().unwrap();
        let args = serve_args(&[]);
        let err = check_serve(&args, dir.path()).unwrap_err();
        assert!(err.to_string().contains("chassis-daemon pair"));
        auth::pair(dir.path()).unwrap();
        check_serve(&args, dir.path()).unwrap();
    }

    #[test]
    fn guard_allows_non_loopback_with_flag() {
        let dir = tempfile::tempdir().unwrap();
        auth::pair(dir.path()).unwrap();
        let args = serve_args(&["--bind", "0.0.0.0:7317", "--allow-non-loopback"]);
        check_serve(&args, dir.path()).unwrap();
    }
}
