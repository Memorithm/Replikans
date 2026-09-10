# Local model and durable experiment evidence

`scripts/trading_agent.py` connects an explicitly configured local Ollama model
to the same negotiated MCP handler and strict tool schemas used by
`trading_mcp.py`. Rust remains the authority for every financial operation.
No custody, model download, funded exchange call or live operation is added.

## Qualification

```bash
cargo build -p replikan-trading --bin replikan-trading
REPLIKAN_TRADING_BINARY=target/debug/replikan-trading python3 -m unittest discover -s scripts -p 'test_trading_agent.py' -v
```

Tests exercise a deterministic model fixture through actual Rust buy/sell and
reopen, plus the Ollama HTTP shapes using a local fixture HTTP server. This is
not evidence that any real model achieves good decisions or that a particular
installed model accepts structured output. No real Ollama model was invoked
during development. Its capabilities are tested at runtime by model discovery
and strict validation of actual generated output.

## Operator setup

Requires Python 3.11+, POSIX file locks, the Rust binary and an already installed
local Ollama model. Choose an exact model name returned by `/api/tags`; the agent
does not guess aliases, download models or use cloud fallback. Use an explicit
loopback IP, for example `http://127.0.0.1:11434`. Redirects/proxies are disabled.
Only that endpoint receives model requests. No URL can be supplied by a model.

With an existing paper configuration and writable session directory:

```bash
python3 scripts/trading_agent.py --experiment /absolute/session/experiment.sqlite run \
  --runtime /absolute/Replikans/target/debug/replikan-trading \
  --config /absolute/session/config.json \
  --journal /absolute/session/runtime.sqlite \
  --paper-venue /absolute/session/venue.sqlite \
  --endpoint http://127.0.0.1:11434 \
  --model YOUR_EXACT_INSTALLED_MODEL \
  --episode UNIQUE_EPISODE_ID \
  --goal 'Inspect available evidence and decide whether to trade in paper mode.' \
  --max-steps 16
```

There is no repeated unattended schedule in this command. It executes one bounded
episode and exits. A model may finish without acting; that decision is retained.
Model output is one validated JSON decision (tool call plus brief explicit
rationale, or finish). A request for an unavailable tool fails and is recorded.
Temperature 0 does not promise deterministic regeneration of model responses.

## Source revisions and temporal evidence

Ingest an operator-provided JSON revision; the importer does not fetch a URL:

```json
{"source_id":"fixture-context","url":"fixture:example","kind":"news","content":"Synthetic context for a test, not a market observation."}
```

```bash
python3 scripts/trading_agent.py --experiment /absolute/session/experiment.sqlite ingest source.json
python3 scripts/trading_agent.py --experiment /absolute/session/experiment.sqlite export > experiment-export.json
```

Kinds: market/economics/news/weather/operator. Optional `published_at_ms` and
`event_at_ms` are asserted source metadata, not verified facts. `first_seen_at_ms`
is assigned by the journal and cannot be backdated by the input. Identical source
revisions retain their original first-seen time; corrections get distinct hashes.
An episode records its cutoff and exact source hashes. Future revisions cannot
replace the historical context. No source is claimed useful for prediction
without a separate ablation experiment. Store only material you may retain and
do not put credentials in goals, excerpts or URLs.

## Persistence and recovery

The experiment SQLite database uses WAL/FULL sync, contiguous events and a
configuration-domain SHA-256 chain. An OS lock prevents concurrent writers through
this implementation. Internal corruption is detected on reopen/export; hashes do
not protect against privileged whole-log rewriting or deletion of a valid tail.
SQLite depends on storage honoring sync. The event limit is 10,000 and each record
at most 1 MiB. A backward wall clock prevents further append.

The journal records goal, observed model digest, source context, public prompts,
actual output, reported token/duration fields, explicit decisions, tool requests
before invocation, responses and the final Rust journal export. Provider timings
are reported values, not measured electrical costs. It never requests hidden
model reasoning. Finished episodes bind the runtime journal hash. Financial
fills/fees remain authoritative in the Rust export, not in model assertions.

Episode identities are never replayed. After interruption, use a new episode;
pending old tool calls are included as uncertain evidence and never retried
automatically. Runtime `order_get`/`execution_reconcile` must establish outcomes.
No blanket clearing of uncertain calls is performed. Failed tool responses are
retained even when they reflect an ambiguous external outcome.

## Limits

This module adds a local provider and evidence loop, not financial PnL accounting,
external news/weather collectors, live market feeds, a live exchange adapter,
strategy qualification or competitor benchmarks. Model identity is observed at
episode start; do not modify the serving model during an episode. HTTP uses a
30-second socket timeout and 1 MiB response cap; it is not a hard process-wide
inference deadline. Model calls are capped at 32 per episode, generation at 2,048
tokens per call. Prompt/context budget exhaustion fails explicitly. The final
export must fit the experiment record budget; large sessions require a future
paginated artifact contract. These limits and the paper simulator's limitations
remain exposed rather than silently producing partial evidence.

Official API references (Ollama, consulted for implementation):

- https://docs.ollama.com/api/chat
- https://docs.ollama.com/api/tags
