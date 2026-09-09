# Local paper trading over MCP

The Python 3.11+ standard-library bridge implements MCP stdio protocol
`2025-11-25` for the existing Rust `replikan-trading` binary. It does not replace
Rust authorization, decimals, journal replay, reservations or adapter semantics.
It is a POSIX transport adapter, not an LLM client or an exchange connector.

## Build and qualify

```bash
cargo build -p replikan-trading --bin replikan-trading
REPLIKAN_TRADING_BINARY=target/debug/replikan-trading python3 -m unittest discover -s scripts -p test_trading_mcp.py -v
```

The acceptance test launches the actual stdio server and Rust binary, negotiates
MCP, discovers capabilities/rules, prepares and submits an exact purchase,
rejects duplicate dispatch, restarts the server, reconciles, sells, checks balances
and exported journal identity, and cancels a resting limit. TEST/QUOTE prices are
scripted fixtures, not market data or model performance. Protocol unit tests also
cover invalid nested fields, notifications, duplicate JSON keys, timeouts and
input/output budgets. Without the environment variable, the Rust acceptance test
is explicitly skipped; this is not full qualification. CI always supplies it.

## Connect a local MCP client

Configure its stdio command as `python3`, with these arguments using your actual
absolute paths (configuration and both databases are operator-controlled):

```text
/absolute/Replikans/scripts/trading_mcp.py
--runtime /absolute/Replikans/target/debug/replikan-trading
--config /absolute/session/config.json
--journal /absolute/session/journal.sqlite
--paper-venue /absolute/session/venue.sqlite
```

Use the configuration/intent fixture in `scripts/trading-paper-demo.py` to
understand the exact serde fields. That fixture is synthetic and disposable.
Never replace an existing journal with a new empty database to recover a session.
Only protocol responses go to stdout. The client must send `initialize`, then
`notifications/initialized`, before calling `tools/list` or `tools/call`.

| Tool | Effect |
| --- | --- |
| `capabilities` | Actual runtime mode plus bridge tool inventory |
| `instrument_rules` | Configured instrument rules, no market-price fabrication |
| `order_prepare` | Persist full intent/decision and reserve inventory, no send |
| `order_submit` | Dispatch a previously prepared `client_order_id` once |
| `order_get` | Local state, not an external status query |
| `order_cancel` | Request venue cancellation, not reversal of fills |
| `execution_reconcile` | Query and ingest authoritative paper receipts |
| `account_snapshot` | Exact balances, orders, fills and recovery list |
| `session_export` | Complete bounded journal evidence |

Tool arguments use strict nested JSON schemas: financial values are plain decimal
strings, timestamps are integer milliseconds, and unknown fields/order flags are
rejected. No tool accepts a binary path, config path, shell command, key, receipt
injection, withdrawal or operator-policy modification. Underlying Rust checks
remain authoritative even if a client ignores schema annotations.

## Failure and resource contract

Requests execute sequentially. Each tool runs one Rust child with fixed operator
arguments, no shell, and no automatic retry. Each child reopens the same durable
databases. A timeout, broken pipe, child exit or excess output may occur after a
durable claim or external effect: inspect `order_get`/`account_snapshot`, then use
`execution_reconcile`. An error is never proof that submission did not happen.

Input is bounded to 1 MiB per line and child output to 8 MiB. Child timeout defaults
to 15 seconds, operator-configurable up to 60. Child stderr is not exposed to the
model. Large exports fail explicitly, not as successful truncated evidence.
Duplicate JSON keys and nonfinite JSON constants are rejected. Notifications
never execute tools and receive no response. Unsupported request methods and
unknown tools return protocol errors; invalid tool arguments and runtime failures
return `isError: true` tool results.

This synchronous server does not advertise tasks, streaming, progress, resources,
prompts, HTTP, sampling, or cancellation of an active tool. A cancellation
notification does not cancel an order: use `order_cancel` explicitly. Tool
annotations describe behavior and are not access control. Local OS permissions
must restrict the binary, script, operator config and database directory.

## Explicit remaining work

Paper full-fill snapshot semantics are unchanged. No market feed, autonomous
model client, live exchange, FIFO PnL, research-job scheduler, or comparative
profitability evidence is delivered by this bridge. SciRust's existing analysis
MCP server may be configured separately; its tool results cannot bypass the
Replikans authorization boundary.

Protocol references (Model Context Protocol specification, version 2025-11-25):

- https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle
- https://modelcontextprotocol.io/specification/2025-11-25/basic/transports
- https://modelcontextprotocol.io/specification/2025-11-25/server/tools
