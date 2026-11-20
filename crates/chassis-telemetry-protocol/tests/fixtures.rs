//! Shared JSON frame fixtures: `valid/` accepts, `invalid/` rejects, and
//! `unknown/` contains server frames with complete envelopes and unknown types.
//! Kebab-case filenames starting with `client-` use the client parser; all
//! other filenames use the server parser.

use std::{collections::BTreeSet, fs, path::Path};

use chassis_telemetry_protocol::{ClientFrame, ParseError, ServerFrame, server_schema};

#[test]
fn shared_fixtures() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../protocol/fixtures");
    let schema = server_schema().to_value();
    let server_types: BTreeSet<_> = schema["oneOf"]
        .as_array()
        .expect("server schema must have oneOf variants")
        .iter()
        .map(|variant| {
            variant["properties"]["type"]["const"]
                .as_str()
                .expect("server variant must have a constant type")
                .to_owned()
        })
        .collect();
    let mut covered = BTreeSet::new();

    for verdict in ["valid", "invalid", "unknown"] {
        let dir = root.join(verdict);
        let mut paths: Vec<_> = fs::read_dir(&dir)
            .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
            .map(|entry| {
                entry
                    .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
                    .path()
            })
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        paths.sort();
        assert!(!paths.is_empty(), "{}: no JSON fixtures", dir.display());

        for path in paths {
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let client = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("client-"));
            let result = if client {
                ClientFrame::parse(&text).map(|_| None)
            } else {
                ServerFrame::parse_lenient(&text).map(Some)
            };

            match (verdict, result) {
                ("valid", Ok(Some(_))) => {
                    let wire: serde_json::Value = serde_json::from_str(&text)
                        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                    covered.insert(wire["type"].as_str().unwrap().to_owned());
                }
                ("valid", Ok(None))
                | ("invalid", Err(ParseError::Invalid { .. }))
                | ("unknown", Err(ParseError::UnknownType { .. })) => {}
                (_, actual) => panic!("{}: expected {verdict}, got {actual:?}", path.display()),
            }
        }
    }

    assert_eq!(covered, server_types, "valid/ server frame type coverage");
}
