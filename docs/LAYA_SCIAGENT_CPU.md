# Laya CPU: bounded SciAgent optimization campaign

2026-09-26. This campaign runs the actual `sciagent-optimize run` protocol and
the real pinned Laya multilingual checkpoint. It tests persistent SciRust
workers/workspaces, then per-layer output-buffer reuse. Numerical correctness
and end-to-end latency are separate hard gates.

**Completed: neither candidate meets the performance gate.** SciAgent returns
`budget-exhausted` after two verified iterations, with zero stage failures and
no runtime promotion. Its exit code 2 is the expected negative optimization
verdict, not a crashed campaign. All 906 measured predictions are retained.

| Configuration | p50 ms | p95 ms | p99 ms | Over 100 ms |
|---|---:|---:|---:|---:|
| Fresh PyTorch FP32, 8 threads | 97.262 | 118.741 | 125.813 | 111 / 300 |
| SciRust persistent workers/workspaces, 4 threads | 320.517 | 348.121 | 369.974 | 300 / 300 |
| Same, plus output-buffer reuse | 321.857 | 362.853 | 409.710 | 300 / 300 |

The best candidate is approximately 3.30 times slower than the frozen reference,
not an acceleration. The first profile records 840.7 ms inside 264 instrumented
SciRust GEMM/output-buffer calls across three complete model forwards. This range
combines kernel execution, synchronization and buffer handling; it does not prove
which internal cost dominates. Buffer reuse shows no observed end-to-end gain in
this sequential campaign. Neither path consistently meets the 100 ms budget.

Both candidates pass all recorded layer and final-output checks, with maximum
absolute layer error 0.0068359375 and logit error 0.000732421875, within the
unchanged mixed tolerance. Zero model errors occur. The result supports keeping
PyTorch as the measured comparator and rejecting these two backend promotions.

## Protocol

SciAgent and `scirust-simd` use revision
`8479ab7a7ed20db2b8fae8a779a31c4c94f4bb0e`. The real CLI was built with
`cargo +stable build --release --locked -p scirust-sciagent --bin sciagent-optimize`.
Its hash and clean checkout revision are retained. This is local build
provenance, not cryptographic build attestation.

The generator is an explicit bounded adapter selecting two preimplemented
configurations. SciAgent orchestrates baseline, generation, compilation,
verification, benchmarking, profiling and rewriting; its small language model is
not invoked. Each generation changes only `candidate.json`. Hashes freeze all
benchmark, bridge, oracle and dataset sources during the run.

1. Fresh untouched FP32 PyTorch reference: eight intra-op threads, one inter-op.
2. Persistent Rayon workers: four workers call the pinned SciRust prepared GEMM,
   with cached per-worker plans/workspaces. Other Torch operators use four threads.
3. Same workers plus initialized output-buffer reuse per encoder layer and shape.

Each long measurement uses 300 predictions on the same three synthetic packets,
after ten warm-ups. Separate verification stages each retain three measured
predictions. Every SciRust process first checks all 264 encoder projections
against their original Torch operators, final logits, answer choices and rounded
probabilities. The mixed tolerance remains `atol=rtol=1e-4`; probability drift is
bounded by 0.0002. SciAgent additionally checks maximum absolute error <=0.01.
Standalone maximum relative error is not measured and is explicitly absent.
Rust checks independently cover odd-dimension GEMM against an f64 sum, output
overwrite, repeated workspace use and invalid parameters.

The promotion threshold is frozen at a 1.05x median speedup over the new Torch
reference. Each unsuccessful candidate receives a separate instrumented profile.
No profile time enters the promotion calculation. The two-candidate budget is
bounded; a rejected candidate is retained as evidence.

## Scope and interpretation

Persistent workspaces still repack matrices per call. They do not permanently
prepack model weights. Output reuse requires sequential completed forwards and
is not a concurrent production model adapter. Attention, normalization, RoPE,
activation functions and the decision head remain PyTorch/Laya; 88 encoder
projections execute actual SciRust GEMMs through the private synchronous bridge.

The host and model environment are the same as the
[earlier CPU qualification](LAYA_CPU_QUALIFICATION.md). Fixed-order measurements
on a virtualized host cannot isolate scheduler or host-load effects. Historical
SciRust timings are not a contemporaneous control for these two changes. Repeated
packets provide latency samples, not 300 independent accuracy cases. The existing
trading-suspension fixture remains an uncertain abstention instead of the expected
selection. No task-quality, profitability or production latency claim follows.

## Reproduction and evidence

See the [campaign command](../experiments/laya_cpu_scirust/README.md) and
[retained evidence](evidence/laya-sciagent-cpu-2026-09-26/). The archive preserves
the complete run directory, raw JSONL, all stage logs, metric files, candidate
configurations, profile data and final SciAgent report. Summaries are independently
recomputed from raw observations and source hashes are checked before retention.
Additional reference, parity, warm-up and profiling calls are not included in the
measured-observation count. Actual model runs are local; CI validates the portable
Rust bridge and the existing trading contracts without substituting fixtures for
model qualification.

This experiment does not change production dependencies, financial authorization
or the shared SciRust API. No candidate is activated in the trading runtime, and
no financial action is dispatched. Persistent packed weights, architecture-
preserving fusion and calibrated quantization remain separate CPU candidates;
this bounded campaign does not exhaust them.
