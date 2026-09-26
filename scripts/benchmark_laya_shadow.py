#!/usr/bin/env python3
"""Replay causal packets through a resident Laya selector; never executes orders."""
import argparse
from collections import Counter
import hashlib
import json
import math
from pathlib import Path
import platform
import time

from trading_laya_shadow import (ABSTAIN, LocalLaya, canonical, fields, fingerprint,
                                model_manifest, questions, select, strict_loads, validate_packet)


class FixtureModel:
    identity = {'provider': 'constant_abstain_fixture', 'real_inference': False}

    def predict(self, state, question_set):
        labels = question_set['candidate']['criteria']
        return {'answers': {'candidate': {'choice': ABSTAIN, 'answer_confidence': 1.0,
                'probabilities': {label: float(label == ABSTAIN) for label in labels}}}}


def load_cases(path):
    cases, seen, size, previous = [], set(), 0, -1
    with Path(path).open('rb') as stream:
        while True:
            line = stream.readline(32769)
            if not line:
                break
            size += len(line)
            if len(line) > 32768 or size > 16*1024*1024 or len(cases) >= 10000:
                raise ValueError('dataset budget exceeded')
            case = strict_loads(line.decode())
            fields(case, ('packet', 'expected_candidate', 'baseline_candidate'))
            packet = validate_packet(case['packet'])
            allowed = questions(packet)['candidate']['criteria']
            if case['expected_candidate'] not in allowed or case['baseline_candidate'] not in allowed:
                raise ValueError('label or baseline outside candidate set')
            if packet['request_id'] in seen or packet['decision_at_ms'] < previous:
                raise ValueError('duplicate identity or nonchronological dataset')
            previous = packet['decision_at_ms']; seen.add(packet['request_id']); cases.append(case)
    if not cases:
        raise ValueError('empty dataset')
    return cases


def summarize(rows):
    latencies = sorted(row['result']['elapsed_ms'] for row in rows)
    selected = [row for row in rows if row['result']['reason'] == 'selected']
    usable = [row for row in rows if row['result']['reason'] in ('selected', 'model_abstained', 'uncertain')]
    correct = lambda row: row['result']['candidate_id'] == row['expected_candidate']
    quantile = lambda p: latencies[max(0, math.ceil(p*len(latencies))-1)]
    return {'trials': len(rows), 'unique_cases': len({row['result']['request_id'] for row in rows}),
            'latency_ms': {'p50': quantile(.5), 'p95': quantile(.95), 'p99': quantile(.99), 'max': latencies[-1]},
            'reasons': dict(Counter(row['result']['reason'] for row in rows)),
            'coverage': len(selected)/len(rows),
            'valid_predictions': len(usable),
            'status': 'completed_with_model_errors' if any(row['result']['reason'] == 'model_error' for row in rows) else 'completed',
            # Infrastructure/late abstentions are never counted as a correct prediction.
            'valid_prediction_accuracy': sum(correct(row) for row in usable)/len(usable) if usable else None,
            'selected_accuracy': sum(correct(row) for row in selected)/len(selected) if selected else None,
            'baseline_accuracy': sum(row['baseline_candidate'] == row['expected_candidate'] for row in rows)/len(rows),
            'p99_sample_warning': len(rows) < 1000,
            'execution_authorized': False, 'profitability_evaluated': False}


def evaluate(cases, model, repeats, warmups, emit, **gates):
    if type(repeats) is not int or not 1 <= repeats <= 100 or not 0 <= warmups <= 100:
        raise ValueError('invalid evaluation budget')
    if repeats*len(cases) > 100000:
        raise ValueError('too many trials')
    for _ in range(warmups):
        select(cases[0]['packet'], model, **gates)
    rows = []
    for repeat in range(repeats):
        for case in cases:
            row = {'kind': 'observation', 'repeat': repeat, 'expected_candidate': case['expected_candidate'],
                   'baseline_candidate': case['baseline_candidate'], 'result': select(case['packet'], model, **gates)}
            emit(row); rows.append(row)
    return summarize(rows)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dataset', type=Path)
    parser.add_argument('--dataset-kind', choices=('synthetic', 'historical'))
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--model-dir', type=Path)
    parser.add_argument('--manifest', type=Path)
    parser.add_argument('--write-manifest', action='store_true')
    parser.add_argument('--fixture', action='store_true')
    parser.add_argument('--device', choices=('cpu', 'cuda'), default='cpu')
    parser.add_argument('--fast', action='store_true')
    parser.add_argument('--repeats', type=int, default=1)
    parser.add_argument('--warmups', type=int, default=3)
    parser.add_argument('--min-probability', type=float, default=.8)
    parser.add_argument('--min-margin', type=float, default=.05)
    parser.add_argument('--max-latency-ms', type=float, default=100)
    args = parser.parse_args()
    if args.write_manifest:
        if not args.model_dir:
            parser.error('--model-dir is required')
        with args.output.open('x') as stream:
            stream.write(canonical(model_manifest(args.model_dir))+'\n')
        return
    if not args.dataset or not args.dataset_kind:
        parser.error('--dataset and --dataset-kind are required')
    if args.fixture and (args.dataset_kind != 'synthetic' or args.device != 'cpu' or args.fast or args.model_dir or args.manifest):
        parser.error('fixture mode is synthetic only and cannot take model options')
    if not args.fixture and (not args.model_dir or not args.manifest):
        parser.error('real inference requires a local checkpoint and reviewed manifest')
    cases = load_cases(args.dataset)
    dataset_hash = fingerprint(cases)
    start = time.perf_counter_ns()
    model = FixtureModel() if args.fixture else LocalLaya(args.model_dir, args.manifest, args.device, args.fast)
    initialization_ms = (time.perf_counter_ns()-start)/1_000_000
    # Exclusive creation prevents silently overwriting experiment evidence.
    with args.output.open('x') as stream:
        def emit(value):
            stream.write(canonical(value)+'\n'); stream.flush()
        emit({'kind': 'run', 'schema_version': 1, 'dataset_sha256': dataset_hash,
              'adapter_sha256': hashlib.sha256(Path(__file__).with_name('trading_laya_shadow.py').read_bytes()).hexdigest(),
              'runner_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              'dataset_kind': args.dataset_kind, 'real_inference': not args.fixture,
              'model': model.identity, 'initialization_ms': initialization_ms,
              'warmups': args.warmups, 'repeats': args.repeats,
              'python': platform.python_version(), 'platform': platform.platform(),
              'financial_dispatches': 0, 'execution_authorized': False})
        try:
            summary = evaluate(cases, model, args.repeats, args.warmups, emit,
                               min_probability=args.min_probability, min_margin=args.min_margin,
                               max_latency_ms=args.max_latency_ms)
        except Exception as error:
            emit({'kind': 'failed', 'error_type': type(error).__name__})
            raise
        emit({'kind': 'summary', **summary})
    print(json.dumps(summary, indent=2))


if __name__ == '__main__':
    main()
