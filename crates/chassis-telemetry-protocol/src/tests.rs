use std::collections::BTreeSet;

use serde_json::json;

use super::*;

fn server(body: Value) -> Value {
    let mut frame = body;
    frame["v"] = json!(1);
    frame["ts_ms"] = json!(123);
    frame["seq"] = json!(1);
    frame
}

fn client_hello() -> Value {
    json!({
        "v": 1, "ts_ms": 123, "type": "client.hello",
        "protocol_major": 1, "nonce": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "client": {"name": "test", "version": "1"}
    })
}

fn host_sample() -> Value {
    json!({
        "type": "host.sample", "cpu": {"usage_pct": 42, "per_core_pct": [42]},
        "memory": {"used_bytes": 50, "available_bytes": 50, "total_bytes": 100,
                   "swap_used_bytes": 0, "swap_total_bytes": 0},
        "network": {"rx_bytes_per_s": 0, "tx_bytes_per_s": 0, "interfaces": []},
        "disk": {"read_bytes_per_s": 0, "write_bytes_per_s": 0},
        "gpus": [{"name": "gpu", "usage_pct": 0}],
        "top_processes": [], "uptime_s": 1
    })
}

fn agent_upsert() -> Value {
    json!({
        "type": "agent.upsert",
        "session": {
            "id": "codex:one", "kind": "codex", "native_id": "one", "workspace_dir": "/work",
            "state": "tool_use", "state_source": "inferred", "state_since_ms": 1,
            "last_activity_ms": 2,
            "tokens": {"input_tokens": 3, "output_tokens": 4,
                       "cache_read_tokens": 0, "cache_write_tokens": 0},
            "rate_limits": [{"window_minutes": 60, "used_pct": 20}],
            "cost": {"usd": 0.01, "source": "estimated", "price_table": "test-v1"},
            "output_tokens_per_s": 4
        }
    })
}

#[test]
fn round_trip_every_variant() {
    let available = json!({"status": "available"});
    let hello = json!({
        "type": "hello", "daemon_version": "1", "protocol": {"major": 1, "minor": 0},
        "sample_interval_ms": 1000,
        "host": {"id": "host", "hostname": "test", "os_name": "test", "os_version": "1",
                 "arch": "test", "cpu_brand": "test", "cpu_cores": 1, "memory_total_bytes": 100},
        "capabilities": {
            "metrics": {"gpu_usage": available, "gpu_memory": available,
                        "gpu_power": {"status": "unavailable", "reason": "no sensor"},
                        "gpu_freq": available, "temperatures": available},
            "agents": [{"kind": "opencode", "availability": "available"}]
        }
    });
    let upsert = agent_upsert();
    for body in [
        hello,
        host_sample(),
        json!({"type": "agents.snapshot", "sessions": [upsert["session"]]}),
        upsert,
        json!({"type": "agent.remove", "id": "codex:one"}),
        json!({"type": "error", "code": "auth_failed", "message": "Invalid token"}),
    ] {
        let wire = server(body);
        let frame = ServerFrame::parse_lenient(&wire.to_string()).unwrap();
        let encoded = serde_json::to_value(&frame).unwrap();
        assert_eq!(encoded["type"], wire["type"]);
        assert_eq!(encoded["seq"], wire["seq"]);
        assert_eq!(ServerFrame::parse_lenient(&encoded.to_string()), Ok(frame));
    }
    let wire = client_hello();
    let frame = ClientFrame::parse(&wire.to_string()).unwrap();
    assert_eq!(serde_json::to_value(frame).unwrap(), wire);
}

#[test]
fn parse_lenient_distinguishes_frame_unknown_and_invalid() {
    for (wire, expected) in [
        (
            r#"{"v":1,"ts_ms":1,"seq":1,"type":"agent.remove","id":"one"}"#,
            "frame",
        ),
        (r#"{"v":1,"ts_ms":1,"seq":1,"type":"future"}"#, "unknown"),
        (r#"{"v":1,"ts_ms":1,"type":"future"}"#, "invalid"),
        (r#"{"v":1,"ts_ms":1,"seq":"1","type":"future"}"#, "invalid"),
        (
            r#"{"seq":1,"ts_ms":1,"type":"agent.remove","id":"one"}"#,
            "invalid",
        ),
        (r#"{"v":1,"seq":1,"type":"future"}"#, "invalid"),
        (r#"{"v":1,"ts_ms":1,"seq":1}"#, "invalid"),
        (
            r#"{"v":1,"ts_ms":1,"seq":1,"type":"agent.remove","id":123}"#,
            "invalid",
        ),
        ("{", "invalid"),
    ] {
        let actual = match ServerFrame::parse_lenient(wire) {
            Ok(_) => "frame",
            Err(ParseError::UnknownType { type_name }) => {
                assert_eq!(type_name, "future");
                "unknown"
            }
            Err(ParseError::Invalid { .. }) => "invalid",
            Err(ParseError::ProtocolMismatch) => "mismatch",
        };
        assert_eq!(actual, expected, "{wire}");
    }
}

#[test]
fn unknown_fields_are_ignored_and_optional_values_stay_absent() {
    let mut wire = server(host_sample());
    wire["future"] = json!(123);
    wire["gpus"][0]["future"] = json!(123);
    wire["gpus"][0]["power_w"] = Value::Null;
    let frame = ServerFrame::parse_lenient(&wire.to_string()).unwrap();
    let serialized = serde_json::to_value(frame).unwrap();
    assert!(serialized.get("future").is_none());
    assert_eq!(
        serialized["gpus"][0],
        json!({"name": "gpu", "usage_pct": 0.0})
    );
}

#[test]
fn client_parse_rejects_missing_nonce_and_unknown_type() {
    let mut wire = client_hello();
    wire.as_object_mut().unwrap().remove("nonce");
    assert!(matches!(
        ClientFrame::parse(&wire.to_string()),
        Err(ParseError::Invalid { .. })
    ));
    let mut wire = client_hello();
    wire["type"] = json!("client.future");
    assert!(matches!(
        ClientFrame::parse(&wire.to_string()),
        Err(ParseError::Invalid { .. })
    ));
}

#[test]
fn protocol_bounds_and_cost_provenance_are_validated() {
    let mut wire = server(host_sample());
    wire["memory"]["total_bytes"] = json!(MAX_SAFE_INTEGER);
    assert!(ServerFrame::parse_lenient(&wire.to_string()).is_ok());
    wire["memory"]["total_bytes"] = json!(MAX_SAFE_INTEGER + 1);
    assert!(matches!(
        ServerFrame::parse_lenient(&wire.to_string()),
        Err(ParseError::Invalid { .. })
    ));
    let mut wire = server(host_sample());
    wire["cpu"]["per_core_pct"][0] = json!(101);
    assert!(matches!(
        ServerFrame::parse_lenient(&wire.to_string()),
        Err(ParseError::Invalid { .. })
    ));
    let mut wire = server(agent_upsert());
    wire["session"]["cost"]
        .as_object_mut()
        .unwrap()
        .remove("price_table");
    assert!(matches!(
        ServerFrame::parse_lenient(&wire.to_string()),
        Err(ParseError::Invalid { .. })
    ));
}

#[test]
fn server_schema_tags_match_parser() {
    let schema = serde_json::to_value(server_schema()).unwrap();
    let variants = schema["oneOf"].as_array().unwrap();
    let tags: BTreeSet<_> = variants
        .iter()
        .map(|variant| variant["properties"]["type"]["const"].as_str().unwrap())
        .collect();
    assert_eq!(tags.len(), variants.len());
    assert_eq!(tags, SERVER_TYPES.iter().copied().collect());
}

fn visit_properties(
    schema: &Value,
    root: &Value,
    visited_refs: &mut BTreeSet<String>,
    check: &mut impl FnMut(&str, &Value),
) {
    if let Some(reference) = schema["$ref"].as_str()
        && visited_refs.insert(reference.into())
    {
        visit_properties(resolve(root, reference), root, visited_refs, check);
    }
    if let Some(properties) = schema["properties"].as_object() {
        for (name, property) in properties {
            check(name, property);
            visit_properties(property, root, visited_refs, check);
        }
    }
    if let Some(items) = schema.get("items") {
        visit_properties(items, root, visited_refs, check);
    }
    for keyword in ["oneOf", "anyOf", "allOf"] {
        if let Some(variants) = schema[keyword].as_array() {
            for variant in variants {
                visit_properties(variant, root, visited_refs, check);
            }
        }
    }
}

fn resolve<'a>(root: &'a Value, reference: &str) -> &'a Value {
    root.pointer(reference.strip_prefix('#').unwrap()).unwrap()
}

fn has_type(schema: &Value, root: &Value, kind: &str) -> bool {
    schema["type"] == kind
        || schema["type"]
            .as_array()
            .is_some_and(|types| types.iter().any(|value| value == kind))
        || schema["$ref"]
            .as_str()
            .is_some_and(|reference| has_type(resolve(root, reference), root, kind))
        || ["oneOf", "anyOf", "allOf"].iter().any(|keyword| {
            schema[keyword].as_array().is_some_and(|variants| {
                variants.iter().any(|variant| has_type(variant, root, kind))
            })
        })
}

fn for_each_schema_property(mut check: impl FnMut(&str, &Value, &Value)) {
    for schema in [server_schema(), client_schema()] {
        let root = serde_json::to_value(schema).unwrap();
        visit_properties(&root, &root, &mut BTreeSet::new(), &mut |name, property| {
            check(name, property, &root);
        });
    }
}

#[test]
fn schema_numeric_properties_name_their_unit() {
    const SUFFIXES: &[&str] = &[
        "_bytes",
        "_bytes_per_s",
        "_pct",
        "_ms",
        "_s",
        "_mhz",
        "_w",
        "_c",
        "_tokens",
        "_per_s",
    ];
    const EXCEPTIONS: &[&str] = &[
        "seq",            // Per-connection sequence number.
        "v",              // Envelope protocol version number.
        "major",          // Hello protocol major version number.
        "minor",          // Hello protocol minor version number.
        "protocol_major", // Client protocol major version number.
        "pid",            // Process identifier.
        "cores",          // CPU core count (spec allow-list).
        "cpu_cores",      // Host CPU core count.
        "window_minutes", // Rate-limit window length, with minutes spelled out.
        "usd",            // Cost in US dollars; the name is the currency unit.
    ];
    for_each_schema_property(|name, property, root| {
        let numeric =
            |schema| has_type(schema, root, "integer") || has_type(schema, root, "number");
        if numeric(property) || property.get("items").is_some_and(numeric) {
            assert!(
                EXCEPTIONS.contains(&name) || SUFFIXES.iter().any(|suffix| name.ends_with(suffix)),
                "numeric property {name} lacks a unit"
            );
        }
    });
}

#[test]
fn schema_has_no_conversation_content_fields() {
    const CONTENT_FIELDS: &[&str] = &[
        "prompt",
        "content",
        "text",
        "output",
        "message_body",
        "arguments",
        "result",
        "file_contents",
    ];
    for_each_schema_property(|name, _, _| {
        // `error.message` is a short protocol diagnostic, not conversation content.
        assert!(!CONTENT_FIELDS.contains(&name), "content field {name}");
    });
}

#[test]
fn schema_identifiers_are_strings() {
    for_each_schema_property(|name, property, root| {
        if name == "id" || name.ends_with("_id") {
            assert!(has_type(property, root, "string"), "{name} is not a string");
            assert!(!has_type(property, root, "integer"), "{name} is an integer");
        }
    });
}
