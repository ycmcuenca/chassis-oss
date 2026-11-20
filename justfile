default:
    @just --list

# Format, lint, test and boundary-check everything in oss/
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    node --test scripts/
    node scripts/check-boundary.mjs .
    pnpm -r --if-present run lint
    pnpm -r --if-present run typecheck
    pnpm -r --if-present run test
