# Goal-driven paper missions

Status: development implementation, 2026-09-25. Paper only. This is not a live
exchange integration or evidence of profitable model decisions.

The local model already accepts a natural-language goal and proposes orders
through MCP. A mission now binds that goal to an immutable operator policy in
Rust. `supervise` repeats bounded model episodes, reads authoritative results,
and stops when the mandate blocks new buys and no inventory or open orders remain.
No human confirmation is needed for each paper order within the configured limits.

## Operator mandate

Add `mission` to a **new** runtime configuration before opening its databases:

```json
{
  "schema_version": 1,
  "mission_id": "operator-chosen-unique-id",
  "objective": "Evaluate available evidence and seek net quote gains within this mandate; do nothing if evidence is insufficient.",
  "instrument_id": "YOUR_CONFIGURED_INSTRUMENT",
  "starts_at_ms": 0,
  "expires_at_ms": 1,
  "target_net_profit": "8",
  "max_net_realized_loss": "5",
  "max_buy_spend": "303"
}
```

These values are a schema illustration, not a trading recommendation. Replace
the identity, instrument, timestamps and budgets with explicit operator choices;
the illustrated expired interval cannot open a current position. Amounts are exact
decimal strings in the configured quote asset, not implicitly USD. One instrument,
zero opening base inventory, and no non-quote opening balance are required.

The policy is persisted with the existing runtime configuration. Editing it for
an existing journal fails rather than resetting consumed budgets. Omission keeps
legacy sessions compatible; `supervise` refuses sessions without a mandate.
The model has no tool to create, approve or revise this operator policy.

## Execution and accounting

- `max_buy_spend` counts all historical buy notionals and positive buy quote fees,
  plus pending buy reservations. Sales and rebates do not refill it. Reservations
  stay conservative at their full original size during partial execution.
- The policy is checked in the same SQLite transaction as intent preparation and
  again before dispatch. Competing processes cannot use an earlier projection to
  bypass the budget. Dispatch excludes its own already-counted reservation.
- Every deduplicated fill updates exact quote cash flows, fees and base inventory.
  At a full return to zero base inventory, cumulative cash flow becomes
  `completed_cycle_pnl`. Partial sales are not assigned a FIFO or average-cost PnL.
- Reaching `target_net_profit` or losing `max_net_realized_loss` on cumulative
  fully closed cycles permanently blocks new buys in this journal. The loss limit
  is neither a daily limit nor an unrealized-loss or drawdown limit.
- The entry window is `[starts_at_ms, expires_at_ms)`. Existing spot inventory can
  still be sold after the entry window or a stop, subject to normal order limits,
  available inventory and fresh intent/reference checks. No shorting is enabled.
- Ambiguous runtime orders block new mission buys pending reconciliation. A
  non-quote fee is retained in the balance ledger, flags
  `quote_accounting_complete=false` and stops new buys; no fee conversion is guessed.
- Without the optional [protection policy and guard](TRADING_PROTECTION.md),
  mission stops do not cancel resting orders or submit liquidation orders by
  themselves. The agent can abandon undispatched entries, request cancellation,
  or propose inventory exits. Existing externally accepted orders can still fill.

The optional protection report supplies bid-liquidation valuation separately.
The mission report intentionally separates `quote_cash_flow` (which includes cash spent
on open inventory) from `completed_cycle_pnl`. It does not value open inventory,
electricity, model costs, transfers, taxes or currencies. Without configured
public collection, source prices remain asserted paper references. With collection,
the retained provider bid/ask constrains paper fills. Paper gains are not revenue
earned on an exchange.

## Run and inspect

Build and exercise the actual Rust/MCP/campaign path with synthetic fixtures:

```bash
cargo test -p replikan-trading --locked
cargo build -p replikan-trading --locked --bin replikan-trading
REPLIKAN_TRADING_BINARY=target/debug/replikan-trading python3 -m unittest discover -s scripts -p 'test_trading_*.py' -v
```

For a configured local model and a new mission session:

```bash
python3 scripts/trading_agent.py --experiment /absolute/session/experiment.sqlite supervise \
  --runtime /absolute/Replikans/target/debug/replikan-trading \
  --config /absolute/session/config.json \
  --journal /absolute/session/runtime.sqlite \
  --paper-venue /absolute/session/venue.sqlite \
  --endpoint http://127.0.0.1:11434 \
  --model YOUR_EXACT_INSTALLED_MODEL \
  --campaign UNIQUE_CAMPAIGN_ID \
  --max-episodes 16 --max-steps 16 --interval-seconds 60
```

The objective comes from the mandate. No new network source is silently added.
The existing source-ingestion workflow in [TRADING_LOCAL_AGENT.md](TRADING_LOCAL_AGENT.md)
provides context. A model may choose to do nothing on every episode.

The JSON-lines command `{"operation":"mission_status"}` and the MCP read-only
tool `mission_status` expose the same policy and replayed accounting. Legacy
sessions return `mission: null`. Agent tool arguments cannot override policy.
Mission reads do not append a financial event or authorize a future order.

## Supervision and recovery

Campaigns allow 1–128 episodes, 1–32 model decisions per episode and a configured
interval from 0–3600 seconds. Existing 10,000-event/one-MiB experiment-journal caps
remain in force and can stop a campaign earlier. This foreground process is not a
daemon installer. Remote deployment/service lifecycle remains owned by RemoteOps.

The supervisor checks the policy fingerprint and runtime recovery state between
episodes and after waiting. It does not automatically repeat failed episodes or
ambiguous mutations. Errors retain evidence and exit. Use a fresh campaign ID
after inspection/reconciliation, with the **same** runtime and venue databases;
creating empty databases is not recovery. An unresolved experiment mutation still
requires a future explicit resolution record workflow before automatic resumption.

Target, deadline or budget stopping does not imply that all positions were closed.
The final report retains inventory and the stop reason. Episode-budget exhaustion
can leave inventory or orders open; it is not represented as mission completion.

## Remaining delivery gates

1. [Runtime-collected public bid/ask snapshots](TRADING_MARKET_DATA.md) now bind
   paper order references to retained provider responses. Historical causal data,
   streaming sequence recovery and exchange-origin freshness remain open.
2. Feasibility/counterproposal based on qualified strategy evidence and explicit
   capital, horizon, fees and liquidity assumptions; a target alone is not evidence.
3. [Observed bid-liquidation loss/drawdown and runtime exits](TRADING_PROTECTION.md)
   now have paper qualification. Operating costs, a qualified partial-lot realized
   PnL convention, fee conversions and deeper liquidity remain open.
4. One operator-selected exchange: native instrument rules, private stream,
   submit/cancel/query, actual fee receipts and crash/reconnection qualification.
5. Qualify the independent protection worker for deployment, outages and exit
   liquidity; its full-position paper exits cannot guarantee a maximum loss.
6. Versioned experiment recovery resolution, trusted-feed refresh, long-running
   service deployment via RemoteOps and retained real-model evaluation evidence.
7. Only after those gates: bounded funded qualification and measured net results.
   No guaranteed return or competitor superiority is asserted.

SciRust continues to own exact decimal and execution lifecycle contracts, pinned
at `8f597e2edbe281b01cfa30a99b1a25d57c32fb70`. Replikans owns mission policy,
durability and agent supervision. No shared primitive is forked into another repo.
