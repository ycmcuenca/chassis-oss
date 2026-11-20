use chassis_telemetry_protocol::{client_schema, server_schema};
use std::{fs, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../protocol/schema");
    fs::create_dir_all(&directory)?;
    for (filename, schema) in [
        ("server-frame.schema.json", server_schema()),
        ("client-frame.schema.json", client_schema()),
    ] {
        let json = serde_json::to_string_pretty(&schema)?;
        fs::write(directory.join(filename), json + "\n")?;
    }
    Ok(())
}
