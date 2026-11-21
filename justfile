default:
    @just --list

# Format, lint, test and boundary-check. Refresh a stale Cargo.lock with cargo update --workspace.
check:
    cargo fmt --all --check
    cargo clippy --locked --workspace --all-targets -- -D warnings
    cargo test --locked --workspace
    just protocol-drift
    node --test scripts/
    node scripts/check-boundary.mjs .
    pnpm -r --if-present run lint
    pnpm -r --if-present run typecheck
    pnpm -r --if-present run test

# Pair a client: print a new token (invalidates the previous one)
pair:
    cargo run --locked --release -q -p chassis-daemon -- pair

# Run the daemon with debug logs (extra flags pass through, e.g. just daemon --interval-ms 500)
daemon *ARGS:
    cargo run --locked --release -q -p chassis-daemon -- serve --log-level debug {{ARGS}}

# Tail a running daemon (token from CHASSIS_TOKEN or --token)
tail *ARGS:
    cargo run --locked --release -q -p chassis-daemon -- tail {{ARGS}}

# Regenerate protocol schemas and TypeScript types
protocol-gen:
    cargo run --locked -q -p chassis-telemetry-protocol --example export-schema
    pnpm --filter @chassis/telemetry-protocol run generate

# Validate one JSON frame with both protocol implementations
protocol-validate FILE:
    #!/bin/sh
    file={{quote(join(invocation_directory(), FILE))}}
    rust_status=0
    ts_status=0
    cargo run --locked -q -p chassis-telemetry-protocol --example validate -- "$file" || rust_status=$?
    pnpm --filter @chassis/telemetry-protocol exec tsx scripts/validate.ts "$file" || ts_status=$?
    if [ "$rust_status" -ne 0 ]; then exit "$rust_status"; fi
    exit "$ts_status"

# Check that the protocol schemas and TypeScript types are current
protocol-drift:
    #!/bin/sh
    set -eu
    snapshot=$(mktemp -d)
    trap 'rm -rf "$snapshot"' 0
    trap 'exit 1' HUP INT TERM
    schema=protocol/schema
    types=packages/telemetry-protocol/src/generated
    cp -R "$schema" "$types" "$snapshot"
    just protocol-gen
    status=0
    for tree in "$schema" "$types"; do
        before=$snapshot/${tree##*/}
        result=0
        diff -r "$before" "$tree" >/dev/null || result=$?
        if [ "$result" -gt 1 ]; then exit "$result"; fi
        if [ "$result" -eq 0 ]; then continue; fi
        status=1
        find "$tree" -type f -exec sh -c '
            before=$1 tree=$2; shift 2
            for file do
                if ! cmp -s "$before/${file#"$tree"/}" "$file"; then
                    printf "stale: %s\n" "$file"
                fi
            done
        ' sh "$before" "$tree" {} +
        find "$before" -type f -exec sh -c '
            before=$1 tree=$2; shift 2
            for file do
                current=$tree/${file#"$before"/}
                if [ ! -f "$current" ]; then printf "stale: %s\n" "$current"; fi
            done
        ' sh "$before" "$tree" {} +
    done
    exit "$status"
