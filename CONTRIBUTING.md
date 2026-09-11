# Contributing to Replikans

1. Read `AGENTS.md` and, before custody/signing/execution/replication work, the off-main roadmap on `agent/ecosystem-roadmap`.
2. Never introduce hard-coded private keys, seed phrases, reward wallets, payout destinations, or treasury destinations.
3. Strategy modules may propose actions. Deterministic Rust policy must authorize them.
4. Prefer checked fixed-point arithmetic (`Money::checked_*`) on financial paths.
5. Keep `unsafe` forbidden. Do not add `unwrap`, `expect`, or `panic` outside tests.
6. Required CI must stay green: fmt, clippy `-D warnings`, workspace tests, wallet-literal scan, trading paper acceptance.

## Local checks

```bash
bash scripts/check-no-hardcoded-wallets.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
