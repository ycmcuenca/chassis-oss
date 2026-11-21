use chassis_telemetry_protocol::{ClientFrame, ParseError, ServerFrame};
use serde_json::Value;
use std::{env, fs, process::ExitCode};

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let Some(path) = args.next().filter(|_| args.next().is_none()) else {
        eprintln!("usage: validate FILE");
        return ExitCode::from(2);
    };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("rust: could not read {path:?}: {error}");
            return ExitCode::from(2);
        }
    };
    let json = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
    let type_name = json["type"].as_str().unwrap_or("<no type>");
    let result = if type_name.starts_with("client.") {
        ClientFrame::parse(&text).map(|_| ())
    } else {
        ServerFrame::parse_lenient(&text).map(|_| ())
    };
    match result {
        Ok(()) => println!("rust: accepted ({type_name})"),
        Err(ParseError::UnknownType { type_name }) => println!("rust: unknown ({type_name})"),
        Err(ParseError::ProtocolMismatch) => println!("rust: protocol mismatch ({type_name})"),
        Err(ParseError::Invalid { reason }) => {
            println!("rust: rejected ({type_name})");
            for line in reason.lines() {
                println!("  {line}");
            }
        }
    }
    ExitCode::SUCCESS
}
