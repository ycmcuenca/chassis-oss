# Telemetry protocol v1

The Rust types are the source of truth. The published
[server schema](schema/server-frame.schema.json) and
[client schema](schema/client-frame.schema.json) define the JSON wire format.

## Frames

- `hello` — server host identity, capabilities, and sample interval.
- `host.sample` — one host measurement.
- `agents.snapshot` — the full list of agent sessions.
- `agent.upsert` — an added or updated session.
- `agent.remove` — the ID of a session that ended.
- `error` — server error code and message before disconnecting.
- `client.hello` — the first client frame: protocol major, pairing token, and client name and version.

Each frame is one JSON message. The envelope fields are at the same level as
the body: `v` is the protocol major, `ts_ms` is the sender's Unix epoch time in
milliseconds, and `type` identifies the frame. Server frames also have `seq`,
which starts at 1 on each connection and increases by 1 per frame.

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

Integer values must not exceed 2^53 − 1.

## Compatibility and provenance

Within a major version, changes may only add optional fields or new frame
types. Receivers ignore unknown fields. The lenient server-frame parsers skip an
unknown type only if its envelope is complete and valid; otherwise they reject
the frame. The first client frame must be `client.hello`. A server that
receives an unsupported protocol major replies with `protocol_mismatch` and
closes the connection.

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
unrecognized server types. Client fixtures have a `client-` filename prefix;
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
