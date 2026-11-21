# Chassis OSS

`chassis-daemon` is the host daemon for Chassis telemetry. It samples CPU, memory,
network, disk, GPU and the busiest processes, and streams them to paired clients
over a WebSocket. See the [telemetry protocol reference](protocol/README.md) for
the wire format and tools.

## Running the daemon

Install the binary, or use the `just` recipes below (they build in release mode):

```sh
cargo install --path crates/chassis-daemon
```

### Pair

```sh
chassis-daemon pair        # or: just pair
```

This prints a new token on stdout. It is shown once. Only its SHA-256 hash is
stored, so it cannot be shown again. Pairing again invalidates the previous
token, and a running daemon picks up the new one without a restart.

### Serve

```sh
chassis-daemon serve       # or: just daemon
```

| Flag                    | Default          | Meaning                                         |
| ----------------------- | ---------------- | ----------------------------------------------- |
| `--bind <ADDR>`         | `127.0.0.1:7317` | Address to listen on                            |
| `--allow-origin <ORIGIN>` | none           | Allow this exact browser Origin (repeatable)    |
| `--interval-ms <MS>`    | `1000`           | Sampling interval, at least `250`               |
| `--log-level <LEVEL>`   | `info`           | `off`, `error`, `warn`, `info`, `debug`, `trace` |

`just daemon` runs `serve --log-level debug`. Extra flags pass through, for
example `just daemon --interval-ms 500`.

`serve` refuses to start if no token has been paired, if the interval is below
250, or if the address is not loopback. There is no bind override. For remote
access, put Tailscale Serve in front of the daemon and use `wss://`.
Verification of WebSocket upgrades through Tailscale Serve is deferred to
later work.
Press Ctrl-C to stop it. Connected clients are closed with code 1001.

Clients connect to `ws://127.0.0.1:7317/v1/ws`. An `Origin` header must match
one of the `--allow-origin` values exactly, or the upgrade gets HTTP 403.
The list is empty by default; native clients without an `Origin` can connect.
The daemon caps upgraded connections at 64 and rejects excess upgrades with
HTTP 503. `GET /healthz` returns HTTP 200 with `ok` while sampling is healthy,
or HTTP 503 otherwise. It never returns host data.

The client sends `client.hello` with a fresh nonce. The daemon replies with
`auth.challenge` containing its own nonce and proof. The client verifies that
proof before sending its own in `client.auth`. Once authenticated, it receives
`hello`, `agents.snapshot`, and then `host.sample` frames. The token never
crosses the wire, but the handshake does not encrypt traffic. See the
[protocol reference](protocol/README.md) for the proof calculations.

### Tail

```sh
export CHASSIS_TOKEN='REPLACE_WITH_TOKEN_FROM_PAIR'
chassis-daemon tail        # or: just tail
```

`tail` connects, prints one line per frame, and exits non-zero if the daemon
refuses it. `--url` defaults to `ws://127.0.0.1:7317/v1/ws`. Set
`CHASSIS_TOKEN` to the token printed by `pair`, replacing the placeholder above.
`--token` also works, but warns that command-line arguments are visible to
other users. Unknown frame types print as `? <type>`.

### Config directory

The token hash (`token.sha256`) and the host id (`host-id`) live in:

| Platform | Directory                                                  |
| -------- | ---------------------------------------------------------- |
| macOS    | `~/Library/Application Support/dev.ycmcuenca.chassis-daemon` |
| Linux    | `~/.config/chassis-daemon`                                 |
| Windows  | `%APPDATA%\ycmcuenca\chassis-daemon\config`                 |

Set `CHASSIS_DAEMON_CONFIG_DIR` to use another directory.

Treat `token.sha256` like the token itself: anyone who reads it can authenticate
as a client or impersonate the daemon. On Unix, the file is mode `0600` and the
config directory is forced to mode `0700`, even if it already exists. The daemon
does not set a Windows ACL.

### Close codes

| Code | Meaning for clients |
| ---- | ------------------- |
| 1001 | Daemon is shutting down; reconnect when it is back. |
| 1009 | Message too big; stay within the 16 KiB limit. |
| 1011 | Internal daemon fault; retry with backoff. No re-pairing is needed. |
| 4400 | Malformed handshake frame; check the client implementation. |
| 4401 | Authentication failed or the handshake timed out; check the token and handshake timing. |
| 4408 | Heartbeat or send timeout; reconnect and keep up with pings and reads. |
| 4426 | Protocol version mismatch; use compatible client and daemon versions. |

### Security notes

- Treat the token like a password. Do not commit it or paste it into chat or
  issues. Clear your terminal scrollback after `pair`.

Run `just check` from this directory to format-check, lint, and test the workspace.

Chassis OSS is dual-licensed under MIT OR Apache-2.0 at your option.
See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
