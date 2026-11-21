//! Command line interface: argument parsing, serve guards, dispatch and exit codes.

use std::fmt;
use std::io::IsTerminal;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use tracing::info;
use tracing::level_filters::LevelFilter;

use crate::auth::{self, TOKEN_HASH_FILE};
use crate::{config, sampler, server, tail};

/// Smallest accepted sampling interval.
pub const MIN_INTERVAL_MS: u32 = 250;

/// How long a client has to authenticate after the upgrade.
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);

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
    if auth::TokenHash::load(config_dir)?.is_none() {
        return Err(Error::new(format!(
            "no pairing token found in {}; run `chassis-daemon pair` first",
            config_dir.display()
        )));
    }
    Ok(())
}

/// Run the server until Ctrl-C.
fn serve(args: &ServeArgs, config_dir: PathBuf) -> Result<(), Error> {
    let host_id = config::host_id(&config_dir)?;
    let interval = Duration::from_millis(u64::from(args.interval_ms));
    let sampler = sampler::spawn(interval, host_id)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::new(format!("cannot start the async runtime: {e}")))?;
    let outcome = runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(args.bind)
            .await
            .map_err(|e| Error::new(format!("cannot bind {}: {e}", args.bind)))?;
        let addr = listener.local_addr().unwrap_or(args.bind);
        info!("listening on {addr}");
        let config = server::ServerConfig {
            config_dir,
            info: Arc::new(sampler.info.clone()),
            interval_ms: args.interval_ms,
            auth_timeout: AUTH_TIMEOUT,
        };
        let ctrl_c = async {
            if let Err(e) = tokio::signal::ctrl_c().await {
                tracing::warn!("cannot listen for Ctrl-C: {e}");
                std::future::pending::<()>().await;
            }
        };
        server::serve(listener, config, sampler.samples.clone(), ctrl_c)
            .await
            .map_err(|e| Error::new(format!("server failed: {e}")))
    });
    sampler.stop();
    outcome
}

fn init_logging(level: LevelFilter) {
    // A second initialisation (in-process tests) is harmless.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_max_level(level)
        .with_target(false)
        .try_init();
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
            serve(&args, dir)
        }
        Command::Tail(args) => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| Error::new(format!("cannot start the async runtime: {e}")))?;
            runtime.block_on(tail::run(
                &args.url,
                &args.token,
                &mut std::io::stdout(),
                None,
            ))
        }
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
    let level = match &cli.command {
        Command::Serve(args) => args.log_level,
        _ => LevelFilter::WARN,
    };
    init_logging(level);
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
