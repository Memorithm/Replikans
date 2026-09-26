# Actual Laya CPU inference qualification

2026-09-26: real checkpoint loading and the resident shadow adapter work on this
CPU. **The measured configuration fails the 100 ms latency budget.** This is
functional and timing evidence, not a trading strategy qualification. No financial
action was sent. CUDA/TileLang acceleration has not been measured here.

Follow-up: [actual SciRust kernel and thread comparison](LAYA_SCIRUST_CPU.md).
The two configurations below are initial baselines, not an exhaustive CPU verdict.

## Measured result

Each main run uses the same three synthetic packets, repeated 100 times, after
10 warm-ups of the first packet. Calls are sequential; FP32 and BF16 runs did not
overlap. Timing includes packet validation, token admission, prediction and output
validation in `select()`, but excludes model initialization and JSONL writing.

| CPU precision | Trials | p50 ms | p95 ms | p99 ms | Maximum ms | Over 100 ms |
|---|---:|---:|---:|---:|---:|---:|
| FP32 | 300 | 105.437 | 177.736 | 278.624 | 348.911 | 174 / 300 (58%) |
| BF16 autocast | 300 | 258.717 | 392.351 | 1173.169 | 1785.421 | 300 / 300 (100%) |

Both runs completed with zero model errors and returned full finite probability
distributions. Late results were discarded. FP32 accepted 40 `ignore_news`
selections, returned 86 uncertain abstentions and discarded 174 late results.
BF16 discarded every result. These are empirical nearest-rank percentiles from
300 repeated observations; the runner explicitly flags the small p99 sample.
Three unique packets do not establish workload coverage or a production SLO.
This virtualized host is not a controlled, isolated performance laboratory.

BF16 is slower on **this host and stack**; it must not be assumed faster on every
CPU. No comparison with upstream GPU timings is like-for-like. The 100 ms limit
is a benchmark gate, not a financial authorization deadline or exchange latency.

## Decision behavior

The fixtures cover a trading suspension, an unrelated bread recipe and conflicting
unverified claims. In FP32 the first observed distributions were:

| Expected candidate | review_news | ignore_news | ABSTAIN |
|---|---:|---:|---:|
| review_news | 0.1401 | 0.2951 | 0.5648 |
| ignore_news | 0.0159 | 0.9541 | 0.0300 |
| ABSTAIN | 0.2964 | 0.2120 | 0.4916 |

The suspension case fails the desired classification. With probability >= 0.8
and margin >= 0.05, only the recipe is confidently selected; the other two abstain
before timing rejection. BF16 produces slightly different probabilities with the
same qualitative choices. The report's 100% selected-only FP32 accuracy concerns
40 timely repetitions of **one easy case**, not trading competence. Valid-only
accuracy and coverage also depend on which calls meet the deadline. Do not use
these metrics to promote a strategy or infer profit.

## Provenance

- Replikans: `46ce028ae2405994b6c4ede22b4e69e53857a550`, including the correction
  preserving model errors even when their calls are late.
- Laya 0.3.20: `4066d5d5fbf08b66c6757ddeedbd797bd7655bc0`, installed from Git.
- Checkpoint: `convaiinnovations/laya-multilingual`, revision
  `e4e9ddf21a7b1903b7acffd8814ad4307bf63a67`.
- Weights: 643,835,514 bytes, SHA-256
  `9d628fd971b700382ac6f65920a86f149777b2e748e0c955fb3b19695aa8f204`.
- Intel Xeon Platinum 8573C, 9 visible logical CPUs, cgroup quota 8 CPU equivalents;
  PyTorch intra-op threads 4, inter-op threads 9. No CUDA device is exposed.
- Python 3.12.14, PyTorch 2.14.0+cpu, Transformers 5.17.0.

Downloaded artifacts were checked against the pinned Hub tree: SHA-256 for LFS
objects, Git blob SHA-1 and byte length for ordinary files. The tokenizer's
`extra_special_tokens` list was converted to the equivalent `extra_0`/`extra_1`
mapping, matching the pinned upstream compatibility normalization. No weight was
changed. Both original and prepared manifests are retained. Offline model loading
then passed the adapter's before/after full-manifest check.

The [evidence directory](evidence/laya-cpu-2026-09-26/) retains all 603 measured
observations (including the three-call initial smoke run), losslessly compressed
as JSONL, run headers, summary, raw-output hashes, hardware, dependency freeze,
Hub metadata and both manifests. Warm-ups are not counted as observations.
Initialization times and source/dataset hashes are recorded in each run header.

## Reproduce

From the repository root, create a fresh virtual environment and install:

```bash
python3 -m venv /absolute/laya-env
/absolute/laya-env/bin/pip install torch==2.14.0+cpu \
  --index-url https://download.pytorch.org/whl/cpu
/absolute/laya-env/bin/pip install \
  -r docs/evidence/laya-cpu-2026-09-26/requirements.txt
/absolute/laya-env/bin/python docs/evidence/laya-cpu-2026-09-26/reproduce.py \
  --work-dir /absolute/new-laya-experiment
```

The helper downloads the five exact public model artifacts, verifies the retained
hashes, applies only the reviewed tokenizer normalization and executes both CPU
runs. The work directory must be new. Dependencies were frozen after installation;
this is a version freeze, not a wheel-hash lock or a guarantee of future package
availability. The helper restates the executed preparation and benchmark steps;
the full helper was not separately rerun after recording the evidence.

## Remaining qualification

Keep Laya an optional shadow selector. GPU stock and strict TileLang measurements
remain open, as do representative chronological labeled evaluation, financial
simulation after costs, and independent process supervision. On this session the
connected Hugging Face account reports no Pro subscription, while the remote
terminal connector returns an unavailable/404 response; no GPU job was submitted
and no subscription was changed. Repairing an existing GPU connection or enabling
an eligible compute account is necessary to execute that part of the protocol.
