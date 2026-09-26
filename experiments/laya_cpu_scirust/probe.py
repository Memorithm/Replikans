"""Experimental real-Laya CPU comparison. No runtime, order or venue capability."""
import argparse
import ctypes
import hashlib
import json
from pathlib import Path
import sys
import time
import types

import numpy as np
import torch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
from benchmark_laya_shadow import evaluate, load_cases
from trading_laya_shadow import LocalLaya, canonical, fingerprint, model_state, questions

SCIRUST_REVISION = '8479ab7a7ed20db2b8fae8a779a31c4c94f4bb0e'


class Bridge:
    def __init__(self, path, backend, threads):
        self.lib = ctypes.CDLL(str(path.resolve()))
        self.lib.laya_probe_avx512.restype = ctypes.c_uint32
        if self.lib.laya_probe_avx512() != 1:
            raise RuntimeError('this experiment requires the actual AVX-512 path')
        self.fn = self.lib.laya_probe_gemm
        pointer = ctypes.POINTER(ctypes.c_float)
        self.fn.argtypes = [pointer, pointer, pointer, ctypes.c_size_t,
                           ctypes.c_size_t, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_size_t]
        self.fn.restype = ctypes.c_int32
        self.mode = {'scirust-prepared': 0, 'scirust-parallel': 1, 'scirust-persistent': 2, 'scirust-reuse': 2}[backend]
        self.reuse = backend == 'scirust-reuse'
        self.threads = threads
        self.calls = 0
        self.oracle_checks = 0
        self.max_abs_error = 0.0
        self.check_oracle = True

    def multiply(self, a, b, buffers):
        # All buffers remain owned, initialized and disjoint until synchronous return.
        if (a.dtype != np.float32 or b.dtype != np.float32 or a.ndim != 2 or b.ndim != 2
                or not a.flags.c_contiguous or not b.flags.c_contiguous or a.shape[1] != b.shape[0]):
            raise ValueError('invalid benchmark buffer')
        m, k = a.shape
        n = b.shape[1]
        shape = (m, n)
        if self.reuse:
            if shape not in buffers:
                if len(buffers) >= 8:
                    raise ValueError('too many shapes for the bounded output cache')
                buffers[shape] = np.zeros(shape, dtype=np.float32)
            c = buffers[shape]
        else:
            c = np.zeros(shape, dtype=np.float32)
        pointer = ctypes.POINTER(ctypes.c_float)
        status = self.fn(a.ctypes.data_as(pointer), b.ctypes.data_as(pointer),
                         c.ctypes.data_as(pointer), m, k, n, self.mode, self.threads)
        if status:
            raise RuntimeError(f'SciRust GEMM failed: {status}')
        self.calls += 1
        return c

    def replace(self, module):
        original = module.forward
        weight = np.ascontiguousarray(module.weight.detach().numpy().T)
        bias = module.bias.detach().numpy().copy() if module.bias is not None else None
        bridge = self
        buffers = {}  # Per-layer; only sequential completed forwards may reuse outputs.

        def forward(_module, x):
            if x.device.type != 'cpu' or x.dtype != torch.float32 or torch.is_grad_enabled():
                raise ValueError('CPU float32 inference only')
            a = x.detach().contiguous().numpy().reshape(-1, weight.shape[0])
            c = bridge.multiply(a, weight, buffers)
            if bias is not None:
                c += bias
            actual = torch.from_numpy(c).reshape(*x.shape[:-1], weight.shape[1])
            if bridge.check_oracle:
                expected = original(x)
                torch.testing.assert_close(actual, expected, atol=1e-4, rtol=1e-4)
                bridge.max_abs_error = max(bridge.max_abs_error,
                                           (actual - expected).abs().max().item())
                bridge.oracle_checks += 1
            return actual
        module.forward = types.MethodType(forward, module)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--model-dir', type=Path, required=True)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--library', type=Path)
    parser.add_argument('--backend', choices=('torch', 'scirust-prepared', 'scirust-parallel', 'scirust-persistent', 'scirust-reuse'), required=True)
    parser.add_argument('--threads', type=int, choices=(1, 2, 4, 8), default=4)
    parser.add_argument('--repeats', type=int, default=10)
    args = parser.parse_args()
    if args.output.exists():
        parser.error('output must be new')
    if args.backend != 'torch' and args.library is None:
        parser.error('SciRust requires --library')
    torch.set_num_threads(args.threads)
    torch.set_num_interop_threads(1)
    dataset = ROOT / 'scripts/fixtures/laya-shadow-synthetic.jsonl'
    cases = load_cases(dataset)
    started = time.perf_counter_ns()
    model = LocalLaya(args.model_dir, args.manifest, device='cpu')
    if model.agent.dtype != torch.float32:
        raise ValueError('clear LAYA_CPU_AMP; this comparison is FP32 only')
    captures = []
    handle = model.agent.model.register_forward_hook(
        lambda _m, _i, out: captures.append(tuple(t.detach().clone() for t in out)))
    reference = [model.predict(model_state(c['packet']), questions(c['packet'])) for c in cases]
    reference_logits = list(captures)
    captures.clear()
    bridge = None
    replaced = []
    parity = {'absolute_tolerance': 1e-4, 'relative_tolerance': 1e-4}
    if args.backend != 'torch':
        bridge = Bridge(args.library, args.backend, args.threads)
        # Only actual nn.Linear calls inside the mmBERT encoder are replaced.
        # Attention, embeddings, normalization, RoPE and decision head stay intact.
        for name, module in model.agent.model.named_modules():
            if name.startswith('encoder.') and isinstance(module, torch.nn.Linear):
                bridge.replace(module)
                replaced.append(name)
        if len(replaced) != 88:
            raise ValueError('unexpected checkpoint architecture')
        actual = [model.predict(model_state(c['packet']), questions(c['packet'])) for c in cases]
        if bridge.oracle_checks != len(cases) * len(replaced):
            raise ValueError('a replaced layer did not execute')
        max_logit_error = 0.0
        for expected, observed in zip(reference_logits, captures, strict=True):
            for e, o in zip(expected, observed, strict=True):
                torch.testing.assert_close(o, e, atol=1e-4, rtol=1e-4)
                max_logit_error = max(max_logit_error, (o - e).abs().max().item())
        for expected, observed in zip(reference, actual, strict=True):
            e, o = expected['answers']['candidate'], observed['answers']['candidate']
            if e['choice'] != o['choice'] or any(abs(e['probabilities'][k]-o['probabilities'][k]) > .0002 for k in e['probabilities']):
                raise ValueError('end-to-end answer parity failed')
        parity.update(layer_checks=bridge.oracle_checks, max_layer_abs_error=bridge.max_abs_error,
                      max_logit_abs_error=max_logit_error, answer_parity=True)
        bridge.check_oracle = False
        bridge.calls = 0
    handle.remove()
    model.identity.update(experimental_linear_backend=args.backend,
                          scirust_revision=SCIRUST_REVISION if bridge else None,
                          bridge_sha256=hashlib.sha256(args.library.read_bytes()).hexdigest() if bridge else None)
    initialization_ms = (time.perf_counter_ns() - started) / 1e6
    with args.output.open('x') as stream:
        def emit(row):
            stream.write(canonical(row) + '\n')
            stream.flush()
        emit({'kind': 'run', 'model': model.identity, 'backend': args.backend,
              'dataset_sha256': fingerprint(cases), 'dataset_kind': 'synthetic',
              'probe_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              'adapter_sha256': hashlib.sha256((ROOT / 'scripts/trading_laya_shadow.py').read_bytes()).hexdigest(),
              'initialization_and_parity_ms': initialization_ms, 'parity': parity,
              'replaced_layers': replaced, 'warmups': 10, 'repeats': args.repeats,
              'real_inference': True, 'financial_dispatches': 0})
        summary = evaluate(cases, model, args.repeats, 10, emit, max_latency_ms=100)
        emit({'kind': 'summary', **summary,
              'scirust_gemm_calls_including_warmups': bridge.calls if bridge else 0})
    print(json.dumps(summary, indent=2))
    if summary['status'] != 'completed':
        raise RuntimeError('model errors occurred; inspect retained observations')


if __name__ == '__main__':
    main()
