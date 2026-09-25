# Runtime-owned paper position protection

The optional `protection` policy adds bid-liquidation valuation and sticky loss,
drawdown, target and deadline stops to an immutable single-instrument mission.
Rust owns the checks and exits. No model response is required to operate the guard.
This remains a paper execution path, not an exchange-hosted stop order or a bound
on the maximum loss that can occur between observations.

## Configuration

Add this to a **new** configuration that already includes `mission` and
`market_data`:

```json
"protection": {
  "max_net_loss": "10",
  "max_drawdown": "6"
}
```

These are schema examples, not recommended budgets. Both limits are positive
exact decimal amounts in the mission's quote asset. Existing configuration-hash
binding prevents changing them for an already-opened journal. Legacy sessions
without the field preserve their original journal identity and behavior.

## Valuation and entry gates

For positive inventory, liquidation PnL is:

`net quote cash flow + base inventory × current collected bid − one exit quote fee`

At zero inventory it is simply net quote cash flow. This is a cumulative
mark-to-market estimate, distinct from the mission's closed-cycle realized PnL.
It assumes valuation of the entire position at the best bid; it does **not** imply
that displayed depth or order limits permit executing that entire position.

Every journaled fresh quote and deduplicated fill updates the valuation and its
high-water mark, which starts at zero. A drop from that observed high-water mark
of at least `max_drawdown`, a PnL of at most `-max_net_loss`, or reaching the mission
net-profit target latches an entry stop. A protection check also latches an expired
mission and propagates accounting-related mission stops. These are permanent for
the journal, including after prices recover or the process restarts.

Preparation and dispatch additionally project each buy's spread and fees into the
liquidation valuation. Nonterminal buy reservations are included conservatively
at full original size; dispatch excludes its own already-counted reservation.
A projected loss/drawdown threshold breach rejects the buy before sending.
Existing order, inventory, notional, freshness and quote-capacity rules still apply.

Non-quote fees, negative inventory or absent/stale quotes cannot produce a valid
current open-position valuation. `protection_status.current_liquidation_pnl` is
then null. Historical values in `progress` have a `valued_at_ms`; they must not be
presented as current prices. Operating costs, taxes, fee conversions and FIFO or
average-cost attribution for partial sales are not included.

## Trusted host operations and independent guard

- `protection_status`: read-only JSON-lines and MCP tool. Exposes the policy,
  latched reason, current valuation, flat/open-order status and journal hash.
- `protect`: trusted host JSON-lines operation; **not** a model MCP tool. It
  journals a risk check, abandons undispatched orders, queries dispatched orders,
  requests cancellation where necessary, waits for terminal evidence, then
  attempts one full inventory-reducing market sale through normal prepare/dispatch.

A protection dispatch is claimed durably before the venue call. If its response
is lost, a later `protect` queries the original client-order identity. An absent
or ambiguous answer never permits resubmission. Every resulting fill uses the
same exact, deduplicated ledger as agent-proposed execution.

Run the independent foreground worker first, with the **same absolute paths**
as the model campaign:

```bash
python3 scripts/trading_guard.py \
  --runtime /absolute/Replikans/target/debug/replikan-trading \
  --config /absolute/session/config.json \
  --journal /absolute/session/runtime.sqlite \
  --paper-venue /absolute/session/venue.sqlite \
  --ticks 100 --interval-seconds 10
```

The worker has no model dependency. It refreshes market data, then executes
protection once per tick, and stops when a latched mission is flat with no open
orders. If refresh fails, it still attempts protection once so deadline cleanup
can take place, then exits with the error. It never automatically repeats an
ambiguous mutation. Inspect/reconcile before restarting with the same databases.

With protection enabled, the model's `supervise` campaign consumes the guard's
quotes and no longer starts a competing collector. Start the guard before the
campaign. Configure its interval at least as long as the market refresh cadence
and short enough for the intended observation window; inference can outlive a
quote and correctly receive a stale-intent rejection. Concurrent state changes
can also cause the campaign's existing consistency checks to stop it for inspection.

The interval is bounded to 1–60 seconds and the tick budget to 1–100,000. These
are safety bounds, not tested sustained-throughput claims. A tick includes the
HTTP request (up to its configured 20-second bound), storage and adapter work;
there is no hard real-time deadline. The runtime still replays its journal and
current MCP calls still recreate processes, as documented in the Laya study.
RemoteOps service installation, alerting, restart policy and operational recovery
remain separate deployment qualification work.

## Explicit failure behavior

The runtime refuses an exit with stale prices, insufficient displayed bid depth,
a consumed snapshot sell side, insufficient fee reserve, invalid lot/notional
rules, or unresolved existing orders. It retains the stop and reports the error;
it does not bypass authorization or report the position as closed. This version
does not slice large positions, guarantee exit liquidity or cancel independently
of a running worker. A feed outage, stopped worker, gap in prices or insufficient
liquidity can leave inventory exposed beyond a configured threshold.

Qualification covers exact net liquidation, observed drawdown, target exit,
pending-entry abandonment, durable restart, stale-data/deadline handling,
reservation accounting, depth refusal and a lost sale reply reconciled without
a duplicate sale. Guard tests cover operation without a model, feed failure and
no retry after ambiguous protection. No funded execution is exercised.
