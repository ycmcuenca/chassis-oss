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
| `--allow-non-loopback`  | off              | Allow binding an address that is not loopback   |
| `--interval-ms <MS>`    | `1000`           | Sampling interval, at least `250`               |
| `--log-level <LEVEL>`   | `info`           | `off`, `error`, `warn`, `info`, `debug`, `trace` |

`just daemon` runs `serve --log-level debug`. Extra flags pass through, for
example `just daemon --interval-ms 500`.

`serve` refuses to start if no token has been paired, if the interval is below
250, or if the address is not loopback and `--allow-non-loopback` is missing.
Press Ctrl-C to stop it. Connected clients are closed with code 1001.

The server answers `GET /healthz` with `ok` and no host data. Clients connect to
`ws://127.0.0.1:7317/v1/ws` and send `client.hello` with the token as their
first frame.

### Tail

```sh
export CHASSIS_TOKEN=<token from pair>
chassis-daemon tail        # or: just tail
```

`tail` connects, prints one line per frame, and exits non-zero if the daemon
refuses it. It takes `--url` (default `ws://127.0.0.1:7317/v1/ws`) and
`--token`, which defaults to the `CHASSIS_TOKEN` environment variable. Unknown
frame types print as `? <type>`.

### Config directory

The token hash (`token.sha256`) and the host id (`host-id`) live in:

| Platform | Directory                                                  |
| -------- | ---------------------------------------------------------- |
| macOS    | `~/Library/Application Support/dev.ycmcuenca.chassis-daemon` |
| Linux    | `~/.config/chassis-daemon`                                 |
| Windows  | `%APPDATA%\ycmcuenca\chassis-daemon\config`                 |

Set `CHASSIS_DAEMON_CONFIG_DIR` to use another directory.

### Close codes

| Code | Meaning                                              |
| ---- | ---------------------------------------------------- |
| 4400 | The first frame was not a valid `client.hello`       |
| 4401 | Authentication failed, or no `client.hello` in time  |
| 4426 | The client's protocol major version is not supported |
| 1001 | The daemon is shutting down                          |

### Security notes

- The daemon listens on loopback only by default.
- Loopback is not a guard on its own: a web page open on the same machine can
  open a WebSocket to `127.0.0.1`. The token is the real protection.
- Treat the token like a password. Do not commit it or paste it into chat or
  issues. Clear your terminal scrollback after `pair`.
- `--allow-non-loopback` exists for the later Tailscale work. Do not expose the
  port on an untrusted network: the connection is not encrypted.

Run `just check` from this directory to format-check, lint, and test the workspace.

Chassis OSS is dual-licensed under MIT OR Apache-2.0 at your option.
See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
