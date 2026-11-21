# Telemetry protocol v1

The Rust types are the source of truth. The published
[server schema](schema/server-frame.schema.json) and
[client schema](schema/client-frame.schema.json) define the JSON wire format.

Version 1 is a draft until the first tagged client release. Until then,
incompatible changes are made in place; after that release the compatibility
rules below apply.

## Frames

- `hello` — server host identity, capabilities, and sample interval.
- `host.sample` — one host measurement.
- `agents.snapshot` — the full list of agent sessions.
- `agent.upsert` — an added or updated session.
- `agent.remove` — the ID of a session that ended.
- `error` — server error code and message before disconnecting.
- `client.hello` — first client frame: `protocol_major`, client name and version, and `nonce`.
- `auth.challenge` — first server frame (`seq: 1`): server `nonce` and `proof`.
- `client.auth` — client's `proof`, sent only after verifying the server's proof.

Each frame is one JSON message. The envelope fields are at the same level as
the body: `v` is the protocol major, `ts_ms` is the sender's Unix epoch time in
milliseconds, and `type` identifies the frame. Server frames also have `seq`,
which starts at 1 on each connection and increases by 1 per frame.

## Mutual authentication

Each peer sends a fresh random 32-byte nonce as base64url without padding.
Let `nc` and `ns` be the decoded client and server nonces. For pairing token
`T`, `K = SHA-256(T)` is the raw 32-byte hash, not its hex encoding.

```text
proof_s = HMAC-SHA-256(K, "chassis/v1/server" || 0x00 || nc || ns)
proof_c = HMAC-SHA-256(K, "chassis/v1/client" || 0x00 || nc || ns)
```

Proofs are base64url without padding and verified in constant time. Neither
`T` nor `K` crosses the wire. The server sends `hello` and telemetry only after
verifying `client.auth`. This handshake does not encrypt the connection.

## Units

| Suffix | Unit |
| --- | --- |
| `_bytes` | Bytes |
| `_bytes_per_s` | Bytes per second |
| `_pct` | Percent (0 to 100) |
| `_ms` | Milliseconds, or Unix epoch milliseconds for timestamps |
| `_s` | Seconds |
| `_mhz` | Megahertz |
| `_w` | Watts |
| `_c` | Degrees Celsius |
| `_tokens` | Token count |
| `_per_s` | Per second (e.g. tokens per second) |
| `_minutes` | Minutes (rate-limit window length) |

Integer values must not exceed 2^53 − 1. Numbers must be finite: a NaN or
infinity serialises as `null` and fails validation.

`top_processes[].cpu_pct` is a share of all logical CPUs, so it stays within
0 to 100. One fully busy core reads `100 / hello.host.cpu_cores`; multiply by
`cpu_cores` to get the per-core figure Activity Monitor and `top` show.

## Compatibility and provenance

Within a major version, changes may only add optional fields or new frame
types. Receivers ignore unknown fields. The lenient server-frame parsers skip an
unknown type only if its envelope is complete and valid; otherwise they reject
the frame. Both parsers check that `v` equals 1 and that `protocol_major`,
if present, also equals 1 before checking the body. An unsupported integer
version is a protocol mismatch, not a malformed body. A server that receives
an unsupported protocol major replies with `protocol_mismatch` and closes.

Agent kind, agent state, state source, availability status, error code, cost
source and memory pressure level are open enums: accept and preserve unknown
strings, but reject non-strings. Known memory pressure levels are `normal`,
`warning`, and `critical`. A virtual network interface may carry `"virtual":
true`; a physical interface can omit the flag. Session token and cost totals
already include all descendant sessions.

- `state_source` is `reported` when the agent supplies its state, or `inferred`
  when the adapter derives it from logs, timestamps, or process presence.
- `cost.source` is `reported` for agent-recorded cost or `estimated` for cost
  calculated from token counts. An estimate includes `cost.price_table`, the
  version of the price table used.
- Unavailable measurements are absent or `null`, never replaced with zero.
  `hello.capabilities` declares which optional metric groups and agent kinds
  are available, partial, or unavailable, with reasons.

## Fixtures and tooling

Shared fixtures live in [`fixtures/`](fixtures/): `valid/` frames are accepted,
`invalid/` frames are rejected, and `unknown/` frames have valid envelopes but
unrecognized server types. `mismatch/` frames fail the major-version check;
`handshake/` holds a synthetic known-answer vector, not a frame.
Client fixtures have a `client-` filename prefix;
the CLI uses the JSON `type`, not the filename, to choose a parser.

Run these commands from the public repo root (`oss/` in the monorepo):

```sh
just protocol-gen
just protocol-validate protocol/fixtures/valid/hello.json
just protocol-drift
```

`just protocol-gen` exports the JSON schemas from Rust, then generates the
TypeScript types. `just protocol-validate <file>` prints the Rust and TypeScript
verdicts and any validation issues. The file path is relative to the directory
you run the command from, or absolute. In the monorepo, regeneration and
validation are also available from the top-level justfile.

`just protocol-drift`, also run by `just check`, snapshots the schemas and
generated types, regenerates them, and fails with a list of stale files if
anything changed. Review and commit the regenerated files with the Rust changes.

### Known differences

Rust rejects integers written with a fractional part (such as `1.0`), even
though TypeScript and JSON Schema accept them. Senders must write integers
without a fractional part.
