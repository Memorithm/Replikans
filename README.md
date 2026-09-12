# Replikans

Replikans is a Rust-first autonomous economic agent system focused on measurable
crypto-denominated survival and replication.

The project studies and reimplements useful ideas from Conway Research's
Automaton while deliberately replacing its funding-driven survival model with an
economic fitness model based on realized profit, solvency, risk, and replication
cost.

## Core rules

- No hard-coded private keys, seed phrases, reward wallets, or treasury destinations.
- No replication without demonstrated economic fitness.
- Financial actions are proposed by agents but enforced by deterministic Rust policy.
- Strategy, custody, and execution are separate trust domains.
- Realized PnL and survival reserves take precedence over nominal revenue or hashrate.

## Status

Initial Rust foundation under active development. Paper trading is the only
enabled execution venue. Live adapters stay out of scope until time-bounded
realized paper evidence exists.

## Workspace map

| Crate | Role |
| --- | --- |
| `replikan-core` | Fixed-point `Money`, basis points, public identity |
| `replikan-economics` | Fitness, opportunity policy |
| `replikan-wallet` | Signing capability without key export |
| `replikan-survival` | Solvency classification and spending modes |
| `replikan-replication` | Replication gate, including sustained-fitness window |
| `replikan-ledger` | Evidence-backed economic journal |
| `replikan-control` | Survival-aware run / hold / freeze |
| `replikan-cycle` | Authorized Bitcoin planning cycle and post-cycle replication gate (ledger, archive, journal) |
| `replikan-decision-ledger` | Decision journal and append-only fitness archive |
| `replikan-execution-lease` | Time-bounded execution authorization |
| `replikan-trading` | Durable paper spot runtime (SciRust contracts) |
| `replikan-cli` | Read-only policy demonstration CLI |

## Quick start

Requires Rust 1.85+ for the workspace, and Rust 1.89+ for `replikan-trading`.

```bash
bash scripts/check-no-hardcoded-wallets.sh
cargo test --workspace --exclude replikan-trading
cargo run -p replikan-cli -- demo
```

Paper trading:

```bash
cargo test -p replikan-trading
cargo build -p replikan-trading --bin replikan-trading
python3 scripts/trading-paper-demo.py target/debug/replikan-trading
```

See `docs/TRADING_RUNTIME.md`, `docs/TRADING_MCP.md`, `docs/TRADING_LOCAL_AGENT.md`,
and the audit notes in `docs/AUDIT.md`.

## Security

Read `SECURITY.md`. The wallet-literal scanner in
`scripts/check-no-hardcoded-wallets.sh` is a CI gate, not a complete secret
detector.

## License

MIT. See `LICENSE`.
