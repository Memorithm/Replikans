# Laya CPU: actual SciRust kernel comparison

2026-09-26. The earlier FP32/BF16 test did not exhaust CPU inference options.
This follow-up executes SciRust inside the **real Laya multilingual model** and
compares eight configurations. It establishes a working experimental bridge with
numerical parity on three packets. **The tested SciRust GEMM paths do not improve
end-to-end latency on this host.** This result does not reject other SciRust CPU
optimizations, quantization or a future native model implementation.

## Extended measurements

The best pilot configuration in each family was run for 300 measured predictions,
sequentially, after parity checks and ten warm-ups. All runs use the same three
synthetic packets, exact weights, candidate gate and 100 ms replay budget.

| Configuration | p50 ms | p95 ms | p99 ms | Maximum ms | Over 100 ms |
|---|---:|---:|---:|---:|---:|
| PyTorch FP32, 8 intra-op / 1 inter-op threads | 92.536 | 108.040 | 115.027 | 129.394 | 54 / 300 (18%) |
| SciRust parallel GEMM, 4 threads; remaining Torch ops 4 / 1 | 283.008 | 308.488 | 323.980 | 908.272 | 300 / 300 (100%) |

Neither configuration meets the 100 ms budget consistently. The Torch run makes
82 timely `ignore_news` selections and 164 uncertain abstentions; all SciRust
results are discarded as late. Both runs have zero model errors. The selections
still concern the easy, unrelated-text fixture; the trading-suspension case is
not correctly selected. No strategy usefulness or profitability is established.

The first 30-observation pilot for each configuration was:

| Backend | GEMM threads / remaining Torch intra-op threads | p50 ms | p95 ms |
|---|---|---:|---:|
| PyTorch | 1 / 1 | 283.992 | 302.307 |
| PyTorch | 2 / 2 | 158.469 | 175.903 |
| PyTorch | 4 / 4 | 151.065 | 163.541 |
| PyTorch | 8 / 8 | 97.844 | 119.044 |
| SciRust prepared AVX-512 | 1 / 4 | 437.206 | 445.686 |
| SciRust parallel AVX-512 | 2 / 2 | 322.718 | 337.985 |
| SciRust parallel AVX-512 | 4 / 4 | 283.051 | 311.036 |
| SciRust parallel AVX-512 | 8 / 8 | 560.777 | 599.475 |

These are descriptive, fixed-order runs on a virtualized host, not a randomized
hardware comparison. The earlier four-thread baseline and this run also differ
in inter-op setting and host timing conditions; do not attribute their entire
difference to one setting. Percentiles have fewer than 1,000 observations, and
repetition does not add independent domain cases. The 300-call comparison is an
extended measurement on the same cases, not a held-out quality confirmation.

## What actually executes

The source is `Memorithm/scirust` at
`8479ab7a7ed20db2b8fae8a779a31c4c94f4bb0e`. The isolated benchmark consumes its
`scirust-simd` crate without copying or changing the kernels. The production
SciRust dependency and financial runtime in Replikans are unchanged.

Profiling three original Laya forwards places about 71% of instrumented self CPU
time in `aten::mm`; other matrix operations add more. This identifies a real hot
path, although profiler timings include instrumentation overhead. The bridge
replaces all 88 `nn.Linear` operations in the mmBERT encoder using real checkpoint
weights transposed once outside timed inference. Its paths are:

- `GemmPlanF32` with cached shape plans and reusable `GemmWorkspaceF32` storage;
- `sgemm_parallel` using 2, 4 and 8 threads.

Actual AVX-512 availability is checked before execution. Other model operations
and the Laya decision head remain PyTorch. This is hybrid inference, not a full
native Rust port. The generic SciRust decoder block is not interchangeable with
mmBERT's bidirectional/local/global attention, RoPE and gated MLP semantics.

Each SciRust run verifies 264 individual projections against the original
PyTorch operation on identical activations, and compares both final logit tensors
and answer probabilities against untouched-model outputs. All pass the declared
mixed tolerance `atol=1e-4, rtol=1e-4`; this is not bitwise parity. In the extended
run the maximum absolute layer error is 0.00683594 and final-logit error is
0.00073242, both within the relative-aware criterion. Choices agree and rounded
probability differences remain within 0.0002. The extended timed run plus warm-up
executes 27,280 actual SciRust GEMMs. Oracle work is disabled during timing.

## Reproduction and retained evidence

See the [experiment instructions](../experiments/laya_cpu_scirust/README.md).
The [evidence directory](evidence/laya-scirust-cpu-2026-09-26/) retains 843 measured
observations: 3 smoke, 240 pilot and 600 extended, all in losslessly compressed
JSONL. Warm-ups, reference forwards and parity forwards are additional calls and
are not included in that total. Raw hashes and recomputed summaries are retained,
along with the profiler table data, hardware, rustc identity and extended-build
hashes. Model manifests and dependency versions are in the
[initial CPU evidence](evidence/laya-cpu-2026-09-26/).

The pilot binary preceded a cache-entry lookup cleanup, formatting and addition
of bridge tests. Extended-build hashes bind the final source, Cargo.lock, harness
and binary used by the 300-call runs. The cleanup does not change the selected
SciRust kernels. Two bridge tests check an independent f64 oracle on odd matrix
dimensions, workspace reuse and rejected invalid parameters. They, formatting
and Clippy pass locally; CI now runs the bridge checks separately from the main
workspace. Full model benchmarks require the explicitly installed environment
and checkpoint and are not silently substituted by CI fixtures.

## CPU work still open

Keep this bridge experimental. The source offers further reusable mechanisms,
but their effect on Laya must be measured through actual compatible integration:

1. **Persistent packed weights and persistent workers.** The tested prepared
   workspace reuses allocation but still packs matrices per call; the parallel
   API creates scoped threads per GEMM. These are concrete places to investigate,
   not yet measured explanations of the entire observed slowdown.
2. **Fused ModernBERT execution.** A native or compiled graph must preserve the
   original architecture and pass intermediate/output oracles. A generic decoder
   benchmark is insufficient.
3. **Quantized inference.** SciRust has quantization and AMX kernels, but this
   host exposes no AMX tile capability. Other quantized paths remain untested;
   they require an actual Laya adapter and quality/calibration evaluation.

No CPU option is declared exhausted, no GPU is declared mandatory, and no
performance or ML-maturity promotion is made. Representative chronological data,
process supervision and trading outcomes after costs remain separate gates.
