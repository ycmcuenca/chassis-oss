use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "chassis-daemon",
    version,
    about = "Chassis host telemetry daemon"
)]
struct Cli {}

fn main() {
    Cli::parse();
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn version_reports_package_version() {
        let error = Cli::try_parse_from(["chassis-daemon", "--version"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DisplayVersion);
        assert!(error.to_string().contains(env!("CARGO_PKG_VERSION")));
    }
}
