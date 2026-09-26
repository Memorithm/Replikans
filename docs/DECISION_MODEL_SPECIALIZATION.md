# Decision models and finance/Rust specialization

2026-09-26. Laya is an optional comparator. This program compares actual SciAgent
model decisions with Laya and explicit rules before selecting or training a
replacement. The earlier SciAgent optimization CLI campaign did not perform this
model comparison.

## First diagnostic: completed, not a qualification

Twelve assistant-authored synthetic news-routing cases, balanced across three
labels: A confirmed market event, B unrelated information, C unverified/conflicting
information. Each backend receives the same text and decision definitions;
expected labels and rationales are withheld from model input. Four cases per
class, including one French case in each class. Three repetitions establish only
repeatability on the same cases, not 36 independent quality samples.

| Backend | Correct unique cases | Invalid outputs | Explicit C choices |
|---|---:|---:|---:|
| Handwritten keyword rules | 12/12 | 0/12 | 4/12 |
| Actual Laya multilingual | 6/12 | 0/12 | 6/12 |
| Actual SciAgent small checkpoint | 0/12 | 12/12 | 0/12 |

All predictions repeat across the three passes: 108 measured calls plus three
warm-ups. The rule baseline and cases are co-designed, public and trivial; its
perfect score is **not held-out accuracy**. Laya raw choices are evaluated without
confidence thresholds or latency rejection, unlike the previous trading shadow
bench. Its 6/12 is specific to this prompt and dataset, not a general accuracy
estimate. No probability calibration is claimed.

SciAgent loads the real `small-20M/final` checkpoint from SciRust revision
`8479ab7a7ed20db2b8fae8a779a31c4c94f4bb0e`. The pinned embedded BPE tokenizer and
public CPU `Generator` perform actual inference. Parameters: greedy decoding,
seed 42, repetition penalty 1.3, at most eight generated tokens. Prompt tokens plus
output budget must fit the checkpoint's 256-token context; no truncation is
accepted. This generator recomputes the context each token; it is not a benchmark
of every available SciAgent inference backend. The continuation must be exactly
A, B or C after whitespace stripping. Generated prose is not searched for a lucky
letter, and invalid output is not converted into a correct abstention. No forced
choice decoding, instruction tuning or training was performed.

The measured resident request medians are retained (rules 0.002237 ms, Laya
68.315681 ms, SciAgent 103.033495 ms). **No speed superiority follows**: only 36
calls per backend, fixed order on a virtualized host, different native decision
mechanisms, SciAgent IPC included, and SciAgent provides no valid decisions.
Loading is separately recorded. Both models remain resident; runs are sequential.
The Laya task/prompt differs from the earlier 97 ms campaign.

Attempt v1 stopped on an adapter `KeyError` before any Laya or SciAgent decision;
its raw rows and traceback are retained. Its timing is also contaminated by a
concurrent Clippy build and is not used. V2 corrects the question key to the
existing LocalLaya `candidate` contract, completes all three backends, and retains
its complete outputs. Scoring tests ensure invalid outputs cannot inflate
abstention accuracy. No trade was sent and no backend was promoted.

## Reproduce

Use the checkpoint/environment pinned in `LAYA_CPU_QUALIFICATION.md`. Build and
run from the repository root, clearing `LAYA_CPU_AMP`:

```bash
cargo +stable build --release --locked --manifest-path experiments/decision_models/Cargo.toml
/absolute/laya-env/bin/python experiments/decision_models/compare.py \
  --binary experiments/decision_models/target/release/decision-model-probe \
  --checkpoint /absolute/scirust/scirust-sciagent/checkpoints/small-20M/final \
  --model-dir /absolute/laya-weights --manifest /absolute/prepared-manifest.json \
  --output /absolute/new-comparison.jsonl
```

The [evidence directory](evidence/decision-models-2026-09-26/) contains both
attempts, raw generated text, weights/binary/source hashes, build logs and
independently recomputed summaries. This is a model capability diagnostic, not
financial-data training, a held-out financial benchmark or an investment system.

## Ordered training program

1. **Data inventory and rights.** Before downloading a corpus for training, record
   publisher, exact revision, URL, license and redistribution/commercial-use
   constraints. Distinguish market observations, news, filings, task labels and
   Rust source. Do not invent training rights. Store raw corpora outside Git.
2. **Financial examples.** Preserve publication and first-available timestamps,
   venue, instrument, event and label horizon; distinguish unverified information
   from confirmed events. Include abstention, conflicting reports, negation,
   costs and explicit risk constraints. News routing and trade profitability
   labels are separate tasks. Use exact arithmetic for fee/risk target generation.
3. **Rust/tool examples.** Use licensed source and validated structured calls.
   Attach compile/test outcomes to code examples. Deduplicate by repository,
   upstream origin and content; do not split copies of one project across sets.
4. **Split before training.** Finance uses chronological train/validation/test
   boundaries and excludes overlapping event/label horizons. Fit preprocessing
   and calibration on training/validation only. Rust splits by project/origin.
   Freeze final evaluation and exclude these public diagnostic cases from any
   claim of held-out generalization.
5. **Controlled specialization.** Compare unchanged SciAgent, finance-only,
   Rust-only and mixed finance/Rust at documented token/compute budgets and
   initial checkpoint. Record mixture proportions and forgetting on both domains.
   Do not assume a mixed corpus improves either task. Consider a typed decision
   head or constrained decoding as separately declared candidates, with their
   own training and semantic-quality gates.
6. **Acceptance.** Report per-class quality, invalid outputs, abstention coverage,
   harmful confusions, calibration where applicable, Rust compile/test success,
   resident latency and RAM. Select thresholds before final test. A low invalid
   rate alone is insufficient; correct syntax does not imply correct decisions.
7. **Paper strategy qualification.** Only after model acceptance, evaluate causal
   decisions including fees, spread, slippage and reconciliation. Replikans Rust
   authorizes financial actions. No training result alone authorizes funded use.

Current state: first synthetic comparison complete; corpus selection, rights
review, representative labels, training and paper-strategy qualification remain
pending. No financial dataset has been downloaded and no model has been trained
by this change.

## Ownership and reuse

SciRust owns reusable tokenizer/model/training primitives. Replikans owns the
financial task, dataset contract, evaluator and policy boundary. This experiment
consumes the existing public SciAgent API without copying kernels or changing
shared contracts. A reusable training/data primitive should be implemented
upstream only when a concrete missing capability is established; no cross-repo
performance improvement is claimed here. Memorithm/RemoteOps remains the remote
control plane for subsequent host execution.
