# Durable spot paper runtime

This slice consumes SciRust's exact financial and lifecycle contracts. Replikans
owns SQLite storage, authorization, balance reservations, adapter calls and
recovery. Existing mining leases and survival policies are unchanged. The new
paper configuration is supplied by the operator, never by agent commands.

## Implemented

- append-only SQLite journal with WAL, synchronous FULL, contiguous sequence and
  a SHA-256 content chain bound to configuration;
- intent, decision, strategy and evidence references stored before submission;
- durable submission claim before adapter invocation, including across processes;
- transactional receipt batches and replayed exact per-asset balances;
- account/instrument binding, expiry, inventory and conservative reservations;
- SciRust lifecycle deduplication by native fill identity;
- durable, separate paper venue receipt database;
- query-based recovery after lost response or actual process termination;
- JSON-lines local-agent binary with prepare/dispatch/reconcile/cancel/snapshot/export;
- process-crash, storage-failure, duplicate-fill, balance and corruption tests.

## Run

Requires Rust 1.89 or newer for the SciRust dependency; other Replikans crates
retain their existing MSRV declaration. SciRust remains a pinned Git dependency,
with its own license; no SciRust source is copied or relicensed as MIT. The
paper-runtime qualification currently pins merged SciRust commit
`8f597e2edbe281b01cfa30a99b1a25d57c32fb70` (PR #1387 exact-notional contract).

```bash
cargo test -p replikan-trading
cargo build -p replikan-trading --bin replikan-trading
python3 scripts/trading-paper-demo.py target/debug/replikan-trading
```

The demo creates disposable synthetic TEST/QUOTE configuration and databases,
executes one purchase and resale, checks exact closing balances, and verifies
replay after reopening. It uses no account, key, wallet or external network.

For a persistent operator-configured session:

```bash
replikan-trading CONFIG.json JOURNAL.sqlite PAPER_VENUE.sqlite
```

Send one JSON object per line. Time comes from the runtime clock, never from a
command's caller-supplied `now`. For example:

```json
{"operation":"capabilities"}
{"operation":"snapshot"}
{"operation":"export"}
```

`prepare` takes an `intent` matching the public Rust/serde Intent contract. See
the executable demo for every required field. `dispatch`, `cancel` and
`reconcile` take `client_order_id`. No command accepts a receipt or changes the
operator configuration. Outputs have `{ok,result}` or `{ok:false,error}`.

`abandon` takes `client_order_id` and a nonempty `reason`. It durably releases
reservations for an intent that has NEVER had a dispatch claim, including one
that expired before submission. It records a local rejection, not a venue
cancellation. Claimed/ambiguous orders must be reconciled, never abandoned.
The original identifiers remain consumed. Repeated abandonment is rejected.
Journals containing this additive event require a runtime supporting `Abandon`;
older binaries fail to replay rather than silently discard the new event.

## Recovery contract

A claim committed before a crash may or may not have reached the venue. It is
never automatically submitted again. Query its stable identity. `None`, timeout
or contradictory evidence leaves the operation unresolved. Losing the journal
and creating a new empty database is not recovery and cannot preserve dedup.

Reconciliation returns an error if receipts were persisted but the order still
requires recovery. Inspect the snapshot for the retained state; success is not
reported merely because an adapter query returned some observations.

Transactions replay current database state before changing it, so a second
connection cannot submit using a stale local projection. An aborted receipt
batch leaves no partially applied fill. Raw authoritative fills can expose a
negative asset balance; that deficit is retained and blocks additional orders.

SQLite durability depends on the filesystem and storage honoring sync. Network
filesystems and hostile local writers are not qualified. The hash chain detects
internal corruption, not a rewrite by someone controlling the complete database.

## Explicit limits and remaining work

- Paper only. No signing, funded operation, withdrawal or live exchange adapter.
- Snapshot-only market/limit GTC fills; no depth, queue, partial-fill generation,
  latency simulation or later matching of resting orders. Partial receipts are
  handled by the runtime and covered by adapter-ingestion tests.
- Flat configured quote fee, not an exchange fee schedule.
- No amend command; no automatic clearing of contradictory reconciliation state.
- Conservative full reservation until terminal status may reject an otherwise
  affordable order after a partial fill; no optimistic reserve release.
- Balances and fills are exact; FIFO cost basis, realized/unrealized PnL and
  operating-cost ingestion are subsequent ledger capabilities.
- The Rust binary retains its JSON-lines protocol. An optional strict MCP stdio
  bridge is documented in [TRADING_MCP.md](TRADING_MCP.md); it delegates financial
  actions to this binary. Model client and context collection remain future work.
- Replay is O(journal size), capped at one million records and one MiB per record.
  Checkpoints/compaction require a versioned migration before increasing scale.
- No comparative performance or profitability claim.

## Audit progress

This PR advances prompt lots D/F/I through an executable paper vertical slice.
It does not close the entire audit. Remaining scientific work includes F08,
F10-F15, full F16 conventions and research cancellation; advanced strategies,
first live adapter and competitor benchmarks are not marked done.
