"""Run a bounded SciAgent campaign on actual Laya; never authorizes trading.

The generator is an explicit two-candidate adapter, not an LLM invocation.
SciAgent owns stage orchestration and the promotion verdict.
"""
import argparse
from decimal import Decimal
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
EXPERIMENT = Path(__file__).resolve().parent
SCI_REVISION = '8479ab7a7ed20db2b8fae8a779a31c4c94f4bb0e'


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def write(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + '\n')


def frozen_sources():
    files = ['Cargo.toml', 'Cargo.lock', 'src/lib.rs', 'probe.py', 'optimize.py']
    paths = [EXPERIMENT / name for name in files]
    paths += [ROOT / 'scripts' / name for name in
              ['trading_laya_shadow.py', 'benchmark_laya_shadow.py',
               'fixtures/laya-shadow-synthetic.jsonl']]
    return {str(p.relative_to(ROOT)): digest(p) for p in paths}


def run_probe(params, backend, threads, output, repeats):
    env = dict(os.environ, OMP_NUM_THREADS=str(threads), MKL_NUM_THREADS=str(threads),
               TOKENIZERS_PARALLELISM='false')
    env.pop('LAYA_CPU_AMP', None)
    subprocess.run([sys.executable, str(EXPERIMENT / 'probe.py'),
                    '--model-dir', params['model_dir'], '--manifest', params['manifest'],
                    '--backend', backend, '--threads', str(threads), '--repeats', str(repeats),
                    '--library', str(EXPERIMENT / 'target/release/liblaya_scirust_cpu_probe.so'),
                    '--output', str(output)], env=env, check=True, timeout=600)
    rows = [json.loads(line) for line in output.read_text().splitlines()]
    header, summary = rows[0], rows[-1]
    if summary['kind'] != 'summary' or summary['status'] != 'completed':
        raise RuntimeError('incomplete or failed real-model run')
    if any(row['result']['probabilities'] is None for row in rows[1:-1]):
        raise RuntimeError('missing actual inference distribution')
    if summary['trials'] != 3 * repeats or summary['unique_cases'] != 3:
        raise RuntimeError('protocol trial count drift')
    return header, summary


def profile(params, config, output):
    # Profile a separate forward sequence, never the timed benchmark process.
    import torch
    sys.path.insert(0, str(EXPERIMENT))
    from probe import Bridge, LocalLaya, load_cases, model_state, questions
    torch.set_num_threads(4)
    torch.set_num_interop_threads(1)
    model = LocalLaya(params['model_dir'], params['manifest'], device='cpu')
    bridge = Bridge(EXPERIMENT / 'target/release/liblaya_scirust_cpu_probe.so', config['backend'], 4)
    for name, module in model.agent.model.named_modules():
        if name.startswith('encoder.') and isinstance(module, torch.nn.Linear):
            bridge.replace(module)
    cases = load_cases(ROOT / 'scripts/fixtures/laya-shadow-synthetic.jsonl')
    for case in cases:
        model.predict(model_state(case['packet']), questions(case['packet']))
    bridge.check_oracle = False
    multiply = bridge.multiply
    def measured(*args):
        with torch.profiler.record_function('scirust_gemm_and_output_buffer'):
            return multiply(*args)
    bridge.multiply = measured
    start = time.perf_counter_ns()
    with torch.profiler.profile(activities=[torch.profiler.ProfilerActivity.CPU]) as profiler:
        for case in cases:
            model.predict(model_state(case['packet']), questions(case['packet']))
    write(output, {'instrumented_wall_ms': (time.perf_counter_ns()-start)/1e6,
                   'backend': config['backend'], 'real_inference': True,
                   'events': [{'name': e.key, 'self_cpu_us': e.self_cpu_time_total,
                               'total_cpu_us': e.cpu_time_total, 'count': e.count}
                              for e in profiler.key_averages()]})


def stage(name):
    params = json.loads(Path('parameters.json').read_text())
    if frozen_sources() != params['source_hashes']:
        raise RuntimeError('frozen implementation, benchmark or oracle changed')
    if digest(params['manifest']) != params['manifest_sha256']:
        raise RuntimeError('checkpoint manifest changed')
    run_dir = Path(os.environ['SCIAGENT_OPT_RUN_DIR'])
    iteration = int(os.environ['SCIAGENT_OPT_ITERATION'])
    if name in ('generate', 'rewrite'):
        candidates = {1: 'scirust-persistent', 2: 'scirust-reuse'}
        write('candidate.json', {'backend': candidates[iteration], 'threads': 4})
        write(run_dir / f'{iteration:02}-candidate-config.json', json.loads(Path('candidate.json').read_text()))
    elif name == 'compile':
        for action, extra in [('fmt', ['--', '--check']), ('clippy', ['--locked', '--all-targets', '--', '-D', 'warnings']),
                              ('test', ['--locked']), ('build', ['--locked', '--release'])]:
            subprocess.run([params['cargo'], '+stable', action, '--manifest-path',
                            str(EXPERIMENT / 'Cargo.toml'), *extra], check=True, timeout=300)
        write(run_dir / f'{iteration:02}-binary.json', {
            'sha256': digest(EXPERIMENT / 'target/release/liblaya_scirust_cpu_probe.so')})
    elif name in ('baseline', 'verify', 'benchmark'):
        config = {'backend': 'torch', 'threads': 8} if name == 'baseline' else json.loads(Path('candidate.json').read_text())
        header, summary = run_probe(params, **config, output=run_dir / f'{iteration:02}-{name}.jsonl',
                                    repeats=1 if name == 'verify' else 100)
        if name == 'verify':
            parity = header['parity']
            if (parity.get('layer_checks') != 264 or not parity.get('answer_parity')
                    or parity['absolute_tolerance'] != 1e-4 or parity['relative_tolerance'] != 1e-4):
                raise RuntimeError('strict mixed-tolerance oracle evidence missing')
            write(os.environ['SCIAGENT_OPT_VERIFY_METRICS'], {
                'passed': True,
                'max_abs_error': max(parity['max_layer_abs_error'], parity['max_logit_abs_error']),
                'notes': 'Live 264 layer and final-logit checks enforce atol=rtol=1e-4. Scalar absolute cap is additional, not a replacement; standalone maximum relative error is not measured.'})
        else:
            key = 'SCIAGENT_OPT_BASELINE_METRICS' if name == 'baseline' else 'SCIAGENT_OPT_CANDIDATE_METRICS'
            write(os.environ[key], {'median_ns': int(Decimal(str(summary['latency_ms']['p50'])) * 1_000_000)})
    elif name == 'profile':
        profile(params, json.loads(Path('candidate.json').read_text()), run_dir / f'{iteration:02}-profile.json')
    else:
        raise ValueError('unknown stage')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--stage', choices=('baseline', 'generate', 'compile', 'verify', 'benchmark', 'profile', 'rewrite'))
    parser.add_argument('--work-dir', type=Path)
    parser.add_argument('--model-dir', type=Path)
    parser.add_argument('--manifest', type=Path)
    parser.add_argument('--sciagent', type=Path)
    parser.add_argument('--sciagent-source', type=Path)
    parser.add_argument('--cargo', type=Path)
    args = parser.parse_args()
    if args.stage:
        stage(args.stage)
        return
    for key in ('work_dir', 'model_dir', 'manifest', 'sciagent', 'sciagent_source', 'cargo'):
        if getattr(args, key) is None:
            parser.error(f'--{key.replace("_", "-")} is required')
    revision = subprocess.check_output(['git', '-C', str(args.sciagent_source), 'rev-parse', 'HEAD'], text=True).strip()
    if revision != SCI_REVISION:
        raise ValueError('unexpected SciAgent source revision')
    if subprocess.check_output(['git', '-C', str(args.sciagent_source), 'status', '--porcelain'], text=True).strip():
        raise ValueError('SciAgent source checkout must be clean')
    work = args.work_dir.resolve()
    work.mkdir(parents=True, exist_ok=False)
    params = {'model_dir': str(args.model_dir.resolve()), 'manifest': str(args.manifest.resolve()),
              'manifest_sha256': digest(args.manifest), 'cargo': str(args.cargo.absolute()),
              'source_hashes': frozen_sources(), 'sciagent_revision': revision,
              'sciagent_binary_sha256': digest(args.sciagent), 'build_attested': False,
              'generator': 'bounded_explicit_two_candidate_adapter_no_LLM',
              'financial_dispatches': 0}
    write(work / 'parameters.json', params)
    task = {'id': 'laya-persistent-cpu-v1', 'crate_name': 'laya-scirust-cpu-probe',
            'backend': 'cpu', 'goal': 'Preserve real Laya outputs and improve end-to-end median over fresh Torch8 by >=5%',
            'allowed_paths': ['candidate.json'],
            'budget': {'max_iterations': 2, 'min_speedup': 1.05, 'max_abs_error': .01,
                       'max_rel_error': .0001, 'command_timeout_secs': 900},
            'commands': {name: {'program': sys.executable, 'args': [str(Path(__file__).resolve()), '--stage', name],
                               'env': {'TOKENIZERS_PARALLELISM': 'false', 'LAYA_CPU_AMP': ''}}
                         for name in ('baseline', 'generate', 'compile', 'verify', 'benchmark', 'profile', 'rewrite')}}
    write(work / 'task.json', task)
    result = subprocess.run([str(args.sciagent.resolve()), 'run', '--manifest', str(work / 'task.json'),
                             '--workspace', str(work), '--run-root', str(work / 'runs'), '--json'], check=False)
    # SciAgent exit 2 is a completed campaign without a promotable candidate.
    if result.returncode not in (0, 2):
        raise SystemExit(result.returncode)
    report_path = work / 'runs' / task['id'] / 'report.json'
    report = json.loads(report_path.read_text())
    write(work / 'completion.json', {'sciagent_exit_code': result.returncode,
          'decision': report['final_decision'], 'failure_count': len(report['failures']),
          'report_sha256': digest(report_path), 'runtime_promotion_performed': False})
    if report['failures']:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
