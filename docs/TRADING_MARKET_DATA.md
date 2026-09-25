# Runtime-collected public quotes

The optional public-data path connects Replikans paper execution to Binance Spot
`bookTicker` via the existing `replikan-market-http` transport. It selects a data
source, not a funded execution venue. No exchange account, API key or secret is used.

Official Binance documentation consulted 2026-09-25:

- https://github.com/binance/binance-spot-api-docs/blob/master/faqs/market_data_only.md
- https://github.com/binance/binance-spot-api-docs/blob/master/rest-api.md#symbol-order-book-ticker

The endpoint is fixed to HTTPS `data-api.binance.vision`, with one operator-bound
symbol and `symbolStatus=TRADING`. The transport rejects redirects, non-success
statuses and oversized replies, with connect/request deadlines of 10/20 seconds.
The collector does not retry errors or rate limits. Host proxy configuration
remains under operator control through the existing transport.
If `SSL_CERT_FILE` is set by the operator, its bounded PEM CA bundle is added via
the shared transport's `new_with_ca_bundle` constructor. Invalid/empty bundles
fail closed. Built-in roots, hostname and certificate validation remain enabled;
no insecure TLS fallback is used. Other Replikans collectors can reuse this API.

## Enable in a new paper configuration

```json
"market_data": {
  "instrument_id": "BTC-USDT",
  "symbol": "BTCUSDT",
  "max_age_ms": 30000,
  "min_refresh_interval_ms": 1000
}
```

This is an illustrative data mapping, not a strategy or a native-rule download.
The configuration must contain exactly one matching instrument with `base_asset`
`BTC` and `quote_asset` `USDT`. Its trading rules/fees remain operator-configured;
this collector does not certify them against Binance exchange rules. The optional
policy is immutable once the runtime journal is opened. Legacy sessions without
it keep the explicitly synthetic, caller-supplied-reference behavior.

## Collection, provenance and agent boundary

Trusted host operation: `{"operation":"market_refresh"}` on the Rust JSON-lines
binary. It has no arguments for endpoint, price, response bytes or timestamp.
The bounded `supervise` command invokes it before each episode when enabled.
Choose an episode interval no shorter than the configured refresh interval.
A refresh error aborts supervision; it is not automatically retried.

The read-only MCP tool `market_snapshot` exposes the latest quote, its immutable
ID, raw response hash, exact buy/sell references and consumed-side flags. Refresh
and raw-response import are not model tools. The quote is included in the initial
model context. The model must copy `market_snapshot_id` and the relevant reference
into its intent; both are rechecked by Rust at preparation and dispatch.

Each accepted observation is persisted in the same hash-chained runtime journal.
Replay reconstructs the parsed prices/quantities and identity from the retained
raw response and rejects disagreement. The hash is a local integrity/provenance
mechanism, not an independently signed attestation by the exchange. Runtime code,
transport, host configuration and local storage are trusted capabilities.

## Execution semantics and limits

- Exact decimal ask for purchases, bid for sales, never an agent-selected midpoint.
- Only market orders are qualified with a collected quote. No automatic resting
  limit matching, partial fills, queue model or full-depth simulation is claimed.
- Positive bid/ask sizes, uncrossed prices and exact symbol identity are required.
  Unsupported/duplicate fields or non-string decimal values fail parsing.
- An order cannot exceed the displayed size on its side. Each snapshot side can
  be claimed for **at most one dispatch**, including across crashes or separate
  processes. This deliberately conservative limit avoids reusing observed depth
  repeatedly within one observation. It does not establish available liquidity
  across later observations or account for market competition.
- A newer observation invalidates previously prepared intents. Undispatched
  intents can be abandoned; their identity must not be reused with new content.
- Age starts at the local HTTP request start, including transit/response time.
  Future, regressing, overlapping and over-age observations are rejected. The
  endpoint supplies no exchange event timestamp, so exchange-origin staleness and
  clock synchronization are not inferred from a fresh local receipt.
- HTTP timeout, malformed feed, stale data or exhausted quote capacity produces
  an explicit error. There is no synthetic-price fallback in configured feed mode.
- Public quote collection is real network I/O; all financial execution remains
  simulated, with the existing flat configured quote fee.

Qualify with `cargo test -p replikan-trading --locked` and the existing Python/MCP
acceptance suite. Transport fixtures exercise adverse responses and exact spread
accounting; any actual provider observation must be reported separately from those
fixture results. No network call is needed by CI.

## Qualification evidence (2026-09-25)

The actual Rust JSON-lines binary completed `market_refresh` then
`market_snapshot` against the fixed public endpoint through the host HTTPS proxy.
Both responses had the same persisted snapshot and journal hash; no trade was sent.

- Symbol: `BTCUSDT`.
- Local request/receipt epoch ms: `1790369157693` / `1790369166536`.
- Snapshot: `9fa61be0398e950d86b4ee37052fd2a3a2376ab50c3f85a9156cf15d4f558676`.
- Raw response SHA-256: `ff65ba6fe8712e6e8f907fe26b39ae4dc489bf930adc54580501380717a78dbe`.

This single connectivity observation is not a sustained availability, exchange
freshness, strategy or funded-execution qualification. The MCP runtime timeout
default is 30 seconds, exceeding the collector's 20-second request bound; custom
shorter timeouts can interrupt collection and require inspecting the journal.
