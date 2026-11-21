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

    for verdict in ["valid", "invalid", "unknown", "mismatch"] {
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
                | ("mismatch", Err(ParseError::ProtocolMismatch))
                | ("unknown", Err(ParseError::UnknownType { .. })) => {}
                (_, actual) => panic!("{}: expected {verdict}, got {actual:?}", path.display()),
            }
        }
    }

    assert_eq!(covered, server_types, "valid/ server frame type coverage");
}

#[test]
fn handshake_known_answer() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use hmac::{Hmac, KeyInit, Mac};
    use serde_json::Value;
    use sha2::{Digest, Sha256};

    let json = |text| serde_json::from_str::<Value>(text).unwrap();
    let vector = json(include_str!(
        "../../../protocol/fixtures/handshake/known-answer.json"
    ));
    let key = Sha256::digest(vector["token"].as_str().unwrap());
    let nc = URL_SAFE_NO_PAD
        .decode(vector["nc"].as_str().unwrap())
        .unwrap();
    let ns = URL_SAFE_NO_PAD
        .decode(vector["ns"].as_str().unwrap())
        .unwrap();
    assert_eq!(nc.len(), 32);
    assert_eq!(ns.len(), 32);
    let proof = |label: &[u8]| {
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap();
        mac.update(label);
        mac.update(&[0]);
        mac.update(&nc);
        mac.update(&ns);
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    };
    assert_eq!(proof(b"chassis/v1/server"), vector["proof_s"]);
    assert_eq!(proof(b"chassis/v1/client"), vector["proof_c"]);
    assert_ne!(vector["proof_s"], vector["proof_c"]);

    let hello = json(include_str!(
        "../../../protocol/fixtures/valid/client-hello.json"
    ));
    let challenge = json(include_str!(
        "../../../protocol/fixtures/valid/auth-challenge.json"
    ));
    let auth = json(include_str!(
        "../../../protocol/fixtures/valid/client-auth.json"
    ));
    assert_eq!(hello["nonce"], vector["nc"]);
    assert_eq!(challenge["nonce"], vector["ns"]);
    assert_eq!(challenge["proof"], vector["proof_s"]);
    assert_eq!(auth["proof"], vector["proof_c"]);
}
