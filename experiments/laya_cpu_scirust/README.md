# Laya / SciRust CPU experiment

This isolated benchmark executes the actual pinned Laya multilingual checkpoint
with SciRust kernels inside its mmBERT encoder. It is not imported by the trading
runtime, changes no financial policy and sends no order.

The bridge replaces 88 encoder `torch.nn.Linear.forward` calls. It transposes and
materializes the real weights once, then calls the pinned `scirust-simd` crate
through an in-process C ABI. Attention, embeddings, RoPE, LayerNorm, activation
functions and the decision head remain PyTorch/Laya. This is a hybrid inference
experiment, not a native SciRust implementation of the entire model.

Backends:

- `torch`: unmodified model, with an explicit intra-op thread count.
- `scirust-prepared`: `GemmPlanF32` with a reusable packing workspace per shape,
  capped at 64 plans per calling thread. GEMM itself is single-threaded; the
  selected PyTorch thread count still applies to the rest of the model.
- `scirust-parallel`: `sgemm_parallel` with 2, 4 or 8 threads in the recorded
  comparison. This API creates scoped threads and allocates packing per call.
- `scirust-persistent`: persistent Rayon workers split the output by rows and
  execute the same SciRust prepared GEMM with per-worker reusable workspaces.
- `scirust-reuse`: the persistent variant also reuses initialized output buffers
  per layer and shape (maximum eight shapes). This requires sequential completed
  forwards; it is not safe as a general concurrent model adapter.

SciRust's available AVX-512 path is checked at startup; the experiment refuses a
different machine path. Workspace reuse does **not** mean static weights are
permanently packed in the GEMM kernel's internal layout. NumPy output allocation,
contiguity conversions and bridge overhead are included in the measured path.
The private FFI requires valid, initialized, disjoint buffers and synchronous
ownership; it is not an API for untrusted raw pointers or concurrent model use.

Before timing, the harness compares every replaced layer against its original
PyTorch operation on the same activations (264 checks across three packets), then
compares both final logit tensors and answer probabilities against the untouched
model. Tensor tolerance is `abs(error) <= 1e-4 + 1e-4 * abs(reference)`, not a pure
absolute 1e-4 bound. Choice must agree and rounded probability drift must stay
within 0.0002. Timing starts only after these checks and ten warm-ups. Timed calls
perform no oracle calculation. Torch inter-op threads are fixed at one.

## Reproduction

Use the environment, exact checkpoint and prepared manifest documented in
[`LAYA_CPU_QUALIFICATION.md`](../../docs/LAYA_CPU_QUALIFICATION.md). Then:

```bash
cargo +stable build --release --locked \
  --manifest-path experiments/laya_cpu_scirust/Cargo.toml
TOKENIZERS_PARALLELISM=false OMP_NUM_THREADS=4 MKL_NUM_THREADS=4 \
  /absolute/laya-env/bin/python experiments/laya_cpu_scirust/probe.py \
  --model-dir /absolute/checkpoint \
  --manifest /absolute/prepared-manifest.json \
  --backend scirust-parallel --threads 4 --repeats 100 \
  --library experiments/laya_cpu_scirust/target/release/liblaya_scirust_cpu_probe.so \
  --output /absolute/new-scirust-run.jsonl
```

For the comparator use `--backend torch --threads 8` and set `OMP_NUM_THREADS`
and `MKL_NUM_THREADS` to 8. For the prepared path use `--backend scirust-prepared`.
Clear `LAYA_CPU_AMP`; this experiment intentionally compares FP32 operators.
The recorded build used rustc 1.98.1; Cargo.lock pins the crate and libc revisions.
Run variants sequentially, keeping host conditions comparable. Output paths must
be new. Model errors are retained and produce a nonzero exit; timing rejection is
recorded separately. Three synthetic cases repeated many times are not an accuracy
dataset, and these percentiles do not establish a production service guarantee.

Portable bridge oracle and boundary checks:

```bash
cargo +stable test --locked --manifest-path experiments/laya_cpu_scirust/Cargo.toml
cargo +stable clippy --locked --all-targets \
  --manifest-path experiments/laya_cpu_scirust/Cargo.toml -- -D warnings
```

These two Rust tests use an independent f64 sum oracle and rejection checks; they
do not need Torch or model downloads. Actual checkpoint evidence is separate in
[`LAYA_SCIRUST_CPU.md`](../../docs/LAYA_SCIRUST_CPU.md).

## Bounded SciAgent optimization campaign

`optimize.py` invokes the actual `sciagent-optimize run` CLI from the pinned
SciRust revision. Its explicit generator selects two preimplemented candidates:
persistent workers/workspaces, then output-buffer reuse. It does not invoke an
LLM or train SciAgent. Only `candidate.json` is changed by the generator; source,
oracle, checkpoint manifest and benchmark hashes are frozen throughout the run.

```bash
/absolute/laya-env/bin/python experiments/laya_cpu_scirust/optimize.py \
  --work-dir /absolute/new-campaign \
  --model-dir /absolute/checkpoint \
  --manifest /absolute/prepared-manifest.json \
  --sciagent /absolute/scirust/target/release/sciagent-optimize \
  --sciagent-source /absolute/scirust \
  --cargo /absolute/cargo
```

Build SciAgent from a clean checkout at the revision pinned in `optimize.py`
using `cargo +stable build --release --locked -p scirust-sciagent --bin
sciagent-optimize`. The wrapper records the binary hash and checkout revision;
these are provenance, not a cryptographic build attestation.

The frozen comparator is a fresh 300-call Torch eight-thread run. Each candidate
uses four threads, passes compile/lint/Rust tests and real-model parity, then runs
300 calls with ten warm-ups. The extra verification stage records three measured
calls. SciAgent requires at least 1.05x median speedup and numerical correctness.
The original mixed tolerance remains mandatory; SciAgent's additional scalar
absolute-error cap is 0.01. No standalone maximum relative error is reported.
Profiles are separate instrumented runs and never used as gate timings.

Keep the entire campaign directory, including failed stages. SciAgent exit 2
means the bounded campaign found no promotable candidate; the wrapper records
this result and exits successfully only when no stage failures were recorded.
No verdict activates a trading backend or dispatches financial actions.
