# Replikans custody gate

24 crates, `unsafe_code = forbid`, clippy `unwrap_used` / `expect_used` / `panic` deny. That is the lint bar. It is not a custody review.

## Before any demo or external run

1. Secret scan of the default branch and of every workspace member (`crates/replikan-*`).
2. No private keys, seeds, wallet files, `.env`, or exchange credentials in git.
3. Mining / bitcoin planner crates stay behind an explicit authorized feature; default build does not talk to a network.
4. External custody review before treating this as a finance SKU.

## Orchestrator

AUTO_MERGE denied. Autonomous agents may not add network endpoints, seed loaders, or "just for demo" key files.

Canon: this repository is the finance vertical. SoulSystem trading copies are not a second source.
