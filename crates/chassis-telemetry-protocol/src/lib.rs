//! Wire types and JSON schemas for Chassis telemetry v1.

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema, schema_for};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;

#[cfg(test)]
mod tests;

/// Major version of the telemetry protocol.
pub const PROTOCOL_MAJOR: u16 = 1;
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

// Keep unknown spellings intact so intermediaries can round-trip future values.
macro_rules! open_enum {
    ($name:ident, $fallback:ident; $($variant:ident => $wire:literal),+ $(,)?) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum $name {
            $($variant,)+
            $fallback(String),
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(match self {
                    $(Self::$variant => $wire,)+
                    Self::$fallback(value) => value,
                })
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Ok(match value.as_str() {
                    $($wire => Self::$variant,)+
                    _ => Self::$fallback(value),
                })
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({"anyOf": [{"type": "string", "enum": [$($wire),+]}, {"type": "string"}]})
            }
        }
    };
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ServerFrame {
    /// Protocol major version number.
    #[schemars(range(min = PROTOCOL_MAJOR, max = PROTOCOL_MAJOR))]
    pub v: u16,
    /// Sender wall-clock time in Unix epoch milliseconds.
    #[serde(deserialize_with = "safe_integer")]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub ts_ms: u64,
    /// Per-connection frame sequence number, starting at one.
    #[serde(deserialize_with = "sequence")]
    #[schemars(range(min = 1, max = MAX_SAFE_INTEGER))]
    pub seq: u64,
    #[serde(flatten)]
    pub body: ServerBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum ServerBody {
    #[serde(rename = "hello")]
    Hello(Hello),
    #[serde(rename = "host.sample")]
    HostSample(HostSample),
    #[serde(rename = "agents.snapshot")]
    AgentsSnapshot { sessions: Vec<AgentSession> },
    #[serde(rename = "agent.upsert")]
    AgentUpsert { session: AgentSession },
    #[serde(rename = "agent.remove")]
    AgentRemove { id: String },
    #[serde(rename = "error")]
    Error { code: ErrorCode, message: String },
    #[serde(rename = "auth.challenge")]
    AuthChallenge { nonce: String, proof: String },
}

const SERVER_TYPES: &[&str] = &[
    "auth.challenge",
    "hello",
    "host.sample",
    "agents.snapshot",
    "agent.upsert",
    "agent.remove",
    "error",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClientFrame {
    /// Protocol major version number.
    #[schemars(range(min = PROTOCOL_MAJOR, max = PROTOCOL_MAJOR))]
    pub v: u16,
    /// Sender wall-clock time in Unix epoch milliseconds.
    #[serde(deserialize_with = "safe_integer")]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub ts_ms: u64,
    #[serde(flatten)]
    pub body: ClientBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum ClientBody {
    #[serde(rename = "client.hello")]
    Hello {
        /// Protocol major version number spoken by the client.
        #[schemars(range(min = PROTOCOL_MAJOR, max = PROTOCOL_MAJOR))]
        protocol_major: u16,
        /// A fresh 32-byte nonce, encoded as unpadded base64url.
        nonce: String,
        client: ClientIdentity,
    },
    #[serde(rename = "client.auth")]
    Auth { proof: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClientIdentity {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(inline)]
pub struct Hello {
    pub daemon_version: String,
    pub protocol: ProtocolVersion,
    /// Sampling interval in milliseconds.
    pub sample_interval_ms: u32,
    pub host: HostIdentity,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProtocolVersion {
    /// Protocol major version number.
    pub major: u16,
    /// Protocol minor version number.
    pub minor: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HostIdentity {
    pub id: String,
    pub hostname: String,
    pub os_name: String,
    pub os_version: String,
    pub arch: String,
    pub cpu_brand: String,
    /// Number of logical CPUs. Per-process `cpu_pct` is a share of all of them.
    pub cpu_cores: u32,
    /// Total host memory in bytes.
    #[serde(deserialize_with = "safe_integer")]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub memory_total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Capabilities {
    pub metrics: MetricCapabilities,
    pub agents: Vec<AgentCapability>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MetricCapabilities {
    pub gpu_usage: Availability,
    pub gpu_memory: Availability,
    pub gpu_power: Availability,
    pub gpu_freq: Availability,
    pub temperatures: Availability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_pressure: Option<Availability>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentCapability {
    pub kind: AgentKind,
    pub availability: AvailabilityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Availability {
    pub status: AvailabilityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

open_enum!(AvailabilityStatus, Unknown;
    Available => "available", Partial => "partial", Unavailable => "unavailable",
);

open_enum!(PressureLevel, Unknown;
    Normal => "normal", Warning => "warning", Critical => "critical",
);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(inline)]
pub struct HostSample {
    pub cpu: CpuSample,
    pub memory: MemorySample,
    pub network: NetworkSample,
    pub disk: DiskSample,
    pub gpus: Vec<GpuSample>,
    pub top_processes: Vec<ProcessSample>,
    /// Host uptime in seconds.
    #[serde(deserialize_with = "safe_integer")]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub uptime_s: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CpuSample {
    /// Overall CPU usage as a percentage (0–100).
    #[serde(deserialize_with = "percentage")]
    #[schemars(range(min = 0, max = 100))]
    pub usage_pct: f64,
    /// Usage of each CPU core as a percentage (0–100).
    #[serde(deserialize_with = "percentages")]
    #[schemars(inner(range(min = 0, max = 100)))]
    pub per_core_pct: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemorySample {
    /// Used memory in bytes.
    #[serde(deserialize_with = "safe_integer")]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub used_bytes: u64,
    /// Available memory in bytes.
    #[serde(deserialize_with = "safe_integer")]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub available_bytes: u64,
    /// Total memory in bytes.
    #[serde(deserialize_with = "safe_integer")]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub total_bytes: u64,
    /// Used swap in bytes.
    #[serde(deserialize_with = "safe_integer")]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub swap_used_bytes: u64,
    /// Total swap in bytes.
    #[serde(deserialize_with = "safe_integer")]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub swap_total_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure: Option<PressureLevel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NetworkSample {
    /// Aggregate receive throughput in bytes per second.
    pub rx_bytes_per_s: f64,
    /// Aggregate transmit throughput in bytes per second.
    pub tx_bytes_per_s: f64,
    pub interfaces: Vec<NetworkInterfaceSample>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NetworkInterfaceSample {
    pub name: String,
    /// Interface receive throughput in bytes per second.
    pub rx_bytes_per_s: f64,
    /// Interface transmit throughput in bytes per second.
    pub tx_bytes_per_s: f64,
    #[serde(rename = "virtual", default, skip_serializing_if = "Option::is_none")]
    pub is_virtual: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiskSample {
    /// Disk read throughput in bytes per second.
    pub read_bytes_per_s: f64,
    /// Disk write throughput in bytes per second.
    pub write_bytes_per_s: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GpuSample {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    /// GPU usage as a percentage (0–100).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_percentage"
    )]
    #[schemars(range(min = 0, max = 100))]
    pub usage_pct: Option<f64>,
    /// Used video memory in bytes.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_safe_integer"
    )]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub vram_used_bytes: Option<u64>,
    /// Total video memory in bytes.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_safe_integer"
    )]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub vram_total_bytes: Option<u64>,
    /// GPU frequency in megahertz.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freq_mhz: Option<f64>,
    /// GPU power in watts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_w: Option<f64>,
    /// GPU temperature in degrees Celsius.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_c: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessSample {
    /// OS process identifier.
    pub pid: u32,
    pub name: String,
    /// Process CPU usage as a share of all logical CPUs (0–100). One fully busy
    /// core reads `100 / hello.host.cpu_cores`.
    #[serde(deserialize_with = "percentage")]
    #[schemars(range(min = 0, max = 100))]
    pub cpu_pct: f64,
    /// Process memory usage in bytes.
    #[serde(deserialize_with = "safe_integer")]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentSession {
    pub id: String,
    pub kind: AgentKind,
    pub native_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub workspace_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub state: AgentState,
    pub state_source: StateSource,
    /// Start of the current state in Unix epoch milliseconds.
    #[serde(deserialize_with = "safe_integer")]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub state_since_ms: u64,
    /// Last activity in Unix epoch milliseconds.
    #[serde(deserialize_with = "safe_integer")]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub last_activity_ms: u64,
    /// OS process identifier, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Cumulative tokens including all descendant sessions.
    pub tokens: TokenCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextUsage>,
    pub rate_limits: Vec<RateLimit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Cost including all descendant sessions.
    pub cost: Option<Cost>,
    /// Output rate in tokens per second.
    pub output_tokens_per_s: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TokenCounts {
    /// Cumulative input token count.
    #[serde(deserialize_with = "safe_integer")]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub input_tokens: u64,
    /// Cumulative output token count.
    #[serde(deserialize_with = "safe_integer")]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub output_tokens: u64,
    /// Cumulative cache-read token count.
    #[serde(deserialize_with = "safe_integer")]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub cache_read_tokens: u64,
    /// Cumulative cache-write token count.
    #[serde(deserialize_with = "safe_integer")]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub cache_write_tokens: u64,
    /// Cumulative reasoning token count, when known.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_safe_integer"
    )]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub reasoning_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContextUsage {
    /// Context usage in tokens.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_safe_integer"
    )]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub used_tokens: Option<u64>,
    /// Context window capacity in tokens.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_safe_integer"
    )]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub window_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RateLimit {
    /// Length of the rate-limit window in minutes.
    pub window_minutes: u32,
    /// Used share of the rate limit as a percentage (0–100).
    #[serde(deserialize_with = "percentage")]
    #[schemars(range(min = 0, max = 100))]
    pub used_pct: f64,
    /// Scheduled reset in Unix epoch milliseconds.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_safe_integer"
    )]
    #[schemars(range(max = MAX_SAFE_INTEGER))]
    pub resets_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Cost {
    pub source: CostSource,
    /// Cost in US dollars.
    pub usd: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_table: Option<String>,
}

impl<'de> Deserialize<'de> for Cost {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            source: CostSource,
            usd: f64,
            price_table: Option<String>,
        }
        let Wire {
            source,
            usd,
            price_table,
        } = Wire::deserialize(deserializer)?;
        if source == CostSource::Estimated && price_table.is_none() {
            return Err(de::Error::custom("estimated cost requires price_table"));
        }
        Ok(Self {
            source,
            usd,
            price_table,
        })
    }
}

open_enum!(CostSource, Unknown; Reported => "reported", Estimated => "estimated");

open_enum!(AgentKind, Unknown;
    ClaudeCode => "claude_code", Opencode => "opencode", Codex => "codex",
);
open_enum!(AgentState, Unrecognized;
    Thinking => "thinking", ToolUse => "tool_use", Working => "working",
    AwaitingApproval => "awaiting_approval", Idle => "idle", Unknown => "unknown",
);
open_enum!(StateSource, Unknown; Reported => "reported", Inferred => "inferred");
open_enum!(ErrorCode, Unknown;
    AuthFailed => "auth_failed", ProtocolMismatch => "protocol_mismatch", Internal => "internal",
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    ProtocolMismatch,
    UnknownType { type_name: String },
    Invalid { reason: String },
}

impl ServerFrame {
    /// Validate known frames and distinguish unknown types with a valid envelope.
    pub fn parse_lenient(text: &str) -> Result<Self, ParseError> {
        let value = version_checked_json(text)?;
        for (field, min, max) in [("ts_ms", 0, MAX_SAFE_INTEGER), ("seq", 1, MAX_SAFE_INTEGER)] {
            if !value[field]
                .as_u64()
                .is_some_and(|n| (min..=max).contains(&n))
            {
                return Err(ParseError::Invalid {
                    reason: format!("{field} must be an integer in {min}..={max}"),
                });
            }
        }
        let type_name = value["type"].as_str().ok_or_else(|| ParseError::Invalid {
            reason: "type must be a string".into(),
        })?;
        if !SERVER_TYPES.contains(&type_name) {
            return Err(ParseError::UnknownType {
                type_name: type_name.into(),
            });
        }
        serde_json::from_value(value).map_err(invalid_json)
    }
}

impl ClientFrame {
    /// Validate a client frame, rejecting unknown frame types.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        serde_json::from_value(version_checked_json(text)?).map_err(invalid_json)
    }
}

/// Check the envelope's version fields without validating the frame body.
pub fn check_protocol_version(text: &str) -> Result<(), ParseError> {
    version_checked_json(text).map(|_| ())
}

fn version_checked_json(text: &str) -> Result<Value, ParseError> {
    let value: Value = serde_json::from_str(text).map_err(invalid_json)?;
    let object = value.as_object().ok_or_else(|| ParseError::Invalid {
        reason: "frame must be an object".into(),
    })?;
    for field in ["v", "protocol_major"] {
        let Some(version) = object
            .get(field)
            .or_else(|| (field == "v").then_some(&Value::Null))
        else {
            continue;
        };
        if version.as_u64() == Some(u64::from(PROTOCOL_MAJOR)) {
            continue;
        }
        if version.is_i64() || version.is_u64() {
            return Err(ParseError::ProtocolMismatch);
        }
        return Err(ParseError::Invalid {
            reason: format!("{field} must be an integer"),
        });
    }
    Ok(value)
}

fn invalid_json(error: serde_json::Error) -> ParseError {
    ParseError::Invalid {
        reason: error.to_string(),
    }
}

pub fn server_schema() -> Schema {
    let mut schema = schema_for!(ServerFrame).to_value();
    // An estimate must include its price table; future sources need not.
    schema["$defs"]["Cost"]["allOf"] = serde_json::json!([{
        "anyOf": [
            {"type": "object", "properties": {"source": {"type": "string", "pattern": "^(?!estimated$)"}}},
            {"type": "object", "required": ["price_table"], "properties": {"price_table": {"type": "string"}}}
        ]
    }]);
    Schema::try_from(schema).expect("generated frame schema is an object")
}

pub fn client_schema() -> Schema {
    schema_for!(ClientFrame)
}

fn check_integer<E: de::Error>(number: u64) -> Result<u64, E> {
    if number <= MAX_SAFE_INTEGER {
        Ok(number)
    } else {
        Err(E::custom("integer exceeds 2^53 - 1"))
    }
}

fn safe_integer<'de, D: Deserializer<'de>>(de: D) -> Result<u64, D::Error> {
    check_integer(u64::deserialize(de)?)
}

fn optional_safe_integer<'de, D: Deserializer<'de>>(de: D) -> Result<Option<u64>, D::Error> {
    Option::<u64>::deserialize(de)?
        .map(check_integer)
        .transpose()
}

fn sequence<'de, D: Deserializer<'de>>(de: D) -> Result<u64, D::Error> {
    let number = safe_integer(de)?;
    if number == 0 {
        Err(de::Error::custom("sequence must be at least one"))
    } else {
        Ok(number)
    }
}

fn check_percentage<E: de::Error>(number: f64) -> Result<f64, E> {
    if (0.0..=100.0).contains(&number) {
        Ok(number)
    } else {
        Err(E::custom("percentage must be between 0 and 100"))
    }
}

fn percentage<'de, D: Deserializer<'de>>(de: D) -> Result<f64, D::Error> {
    check_percentage(f64::deserialize(de)?)
}

fn optional_percentage<'de, D: Deserializer<'de>>(de: D) -> Result<Option<f64>, D::Error> {
    Option::<f64>::deserialize(de)?
        .map(check_percentage)
        .transpose()
}

fn percentages<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<f64>, D::Error> {
    let values = Vec::<f64>::deserialize(de)?;
    for &value in &values {
        check_percentage::<D::Error>(value)?;
    }
    Ok(values)
}
