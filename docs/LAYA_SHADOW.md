# Laya shadow selector and evaluation runner

Status: prototype, 2026-09-26. The adapter is executable; real Laya inference and
trading usefulness are **not yet qualified**. It never imports the trading MCP,
loads account configuration, constructs an order or calls a venue. Every result
is marked `mode=shadow` and `execution_authorized=false`. Rust authorization and
the independent protection worker remain authoritative and unchanged.

## Implemented contract

`scripts/trading_laya_shadow.py` accepts one bounded packet containing a request
identity, snapshot identity and validity interval, replay decision timestamp,
short state, timestamped evidence and 1–8 named candidates. It adds an explicit
`ABSTAIN` option. Names and descriptions come from the experiment owner, not model
output. Neither a candidate name nor a successful classification approves a trade.

The adapter rejects future/unavailable evidence, a stale snapshot at the replay
decision, duplicate identities, nonfinite values, excessive input and unknown
fields. `packet_from_market_snapshot(...)` converts the result of the existing
Rust `market_snapshot` read into this packet, retaining exact price strings and
checking its side references and receipt availability. Capture that result outside
the selector; this helper does not independently verify an exchange or journal.

The model returns a candidate, full distribution and `answer_confidence`. The
adapter checks label membership, finite probabilities, normalization and
consistency with the chosen maximum. Low probability or a close second option
causes abstention. It deliberately ignores the entropy-based `confidence` field.
The default probability/margin gates are experiment settings, not calibrated
financial probabilities or approved trading thresholds.

Results retain packet/candidate hashes, request/snapshot IDs, model identity,
gates, elapsed time and an explicit outcome reason. Malformed responses and
inference errors produce an abstention; they never become successful predictions
in the evaluation statistics. If an inference error also exceeds its deadline,
the error remains the primary outcome while `deadline_exceeded=true` records the
independent timing failure. Labels and baseline decisions stay outside model
input. Provenance hashes detect accidental drift, not a malicious rewrite of all
evidence by a privileged host.

## Resident checkpoint, explicit failures

`LocalLaya` loads one checkpoint once and reuses it for all cases. The reviewed
code revision is `4066d5d5fbf08b66c6757ddeedbd797bd7655bc0` (version 0.3.20).
Installation metadata must identify that exact Git commit. A complete SHA-256
manifest binds all local checkpoint files; symlinks and missing weight/config/
tokenizer/encoder files are rejected. Hashing is done at startup, not per decision.
Creating a manifest records identity; it does not establish trusted provenance.
Obtain and verify the intended weight revision before approving the manifest.

Model loading is offline. Any automatic device change is rejected. GPU acceleration
uses `accelerate(strict=True)`. The adapter replaces the pinned runtime's private
`_infer` hook with the same five-tensor forward under no-grad/autocast **without
its automatic CPU retry**. Device, OOM or kernel failures are observable errors.
This private integration and formatter dependency require requalification before
any upstream revision change; unit stubs do not qualify actual GPU kernels.

Before forwarding, the adapter checks the actual tokenized state, instructions
and options against the pinned formatter's budgets. It refuses inputs that would
be truncated, including per-option truncation. The checks add work to prediction;
upstream raw latency numbers do not describe this adapter's full cost.

The latency gate uses a monotonic clock and the smaller of the experiment budget
and remaining snapshot TTL at the historical decision time. This is a **replay
budget**, not current wall-clock market freshness. A late result becomes an
abstention when the call returns. It does not preempt a hung kernel. Run this
foreground evaluator separately from trading/protection; process-level deadline,
resource supervision and remote service deployment remain future work.

## Smoke test without weights

```bash
python3 -m unittest discover -s scripts -p 'test_trading_agent_laya.py' -v
python3 scripts/benchmark_laya_shadow.py \
  --fixture --dataset-kind synthetic \
  --dataset scripts/fixtures/laya-shadow-synthetic.jsonl \
  --output /absolute/new-run.jsonl
```

The fixture predictor always abstains. Its timings only measure adapter overhead;
its labels are synthetic examples, not evidence of domain accuracy. There is no
fallback from a missing real checkpoint into fixture mode. The output file must
not already exist, preserving previous experiments.

## Prepare actual inference on CPU or GPU

Use an isolated environment and the exact reviewed code:

```bash
python3 -m venv /absolute/laya-env
/absolute/laya-env/bin/pip install \
  'laya @ git+https://github.com/NandhaKishorM/laya.git@4066d5d5fbf08b66c6757ddeedbd797bd7655bc0'
```

For CUDA fast mode use the `laya[fast] @ git+...` variant with the same commit and
a compatible operator-selected PyTorch/CUDA/TileLang stack. Retain a dependency
lock from the environment; this command alone does not pin transitive dependencies.
No GPU rental, installation or weight download is performed by this repository.

Materialize an explicitly selected checkpoint into a local directory (no symlinks).
Prepare a manifest outside that directory and review its origin and hashes:

```bash
python3 scripts/benchmark_laya_shadow.py --write-manifest \
  --model-dir /absolute/checkpoint --output /absolute/checkpoint-manifest.json
```

If loading rewrites tokenizer configuration, the adapter refuses the run. Prepare
and review those files offline, then create a new manifest rather than bypassing
the check. Run the evaluator using the environment containing the pinned package:

```bash
/absolute/laya-env/bin/python scripts/benchmark_laya_shadow.py \
  --model-dir /absolute/checkpoint --manifest /absolute/checkpoint-manifest.json \
  --device cuda --fast \
  --dataset /absolute/held-out-cases.jsonl --dataset-kind historical \
  --max-latency-ms 100 --warmups 3 --repeats 1 \
  --output /absolute/new-laya-run.jsonl
```

`--device cpu` without `--fast` supports a separate CPU experiment. Budget 100 ms
is an illustration, not a promised service level. The runner records initialization
cost, GPU/device name, dtype, library versions, thread counts and hashes of the
adapter, runner, model manifest and canonical dataset.

## Dataset and interpretation

Each JSONL row has exactly `packet`, `expected_candidate`, `baseline_candidate`.
See the synthetic fixture for the complete schema. Cases must have unique request
IDs and nondecreasing decision times. Evidence availability must precede the
decision; source timestamps and label correctness remain the dataset curator's
responsibility. These checks cannot detect every form of look-ahead leakage.
Chronological train/calibration/test splits and overlapping-label controls must
be established before supplying a held-out dataset. The runner does not train or
calibrate the model.

Warm-up applies only to the first case; unseen shapes later may still incur first-use
costs. Repeats are marked and counted separately from unique cases. Outputs retain
individual observations plus empirical nearest-rank p50/p95/p99/max, reason counts,
coverage, valid-prediction accuracy, selected-only accuracy and the supplied
baseline's accuracy. Model-error/late abstentions are excluded from prediction
accuracy, so read error counts and coverage together with accuracy. Sample sizes
below 1,000 carry an explicit p99 warning; larger repeated samples still do not
establish independent coverage or a production SLO.

The next qualification needs actual weights, representative labeled data and
hardware measurements, followed by comparison with rules and the existing agent.
Financial simulation after fees/slippage and funded performance remain separate
work. No trading profitability metric is fabricated by this runner.
