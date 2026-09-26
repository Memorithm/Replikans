"""Reproduce CPU inference with reviewed public weights; never sends orders.

Install the adjacent requirements.txt in an isolated environment first.
Run from any directory with --work-dir pointing to a new directory.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--work-dir', type=Path, required=True)
    args = parser.parse_args()
    evidence = Path(__file__).resolve().parent
    repo = evidence.parents[2]
    work = args.work_dir.resolve()
    work.mkdir(parents=True, exist_ok=False)
    weights = work / 'weights'
    original = json.loads((evidence / 'original-manifest.json').read_text())
    prepared = json.loads((evidence / 'prepared-manifest.json').read_text())
    sizes = {row['path']: row['size'] for row in
             json.loads((evidence / 'hub-tree.json').read_text()) if row['type'] == 'file'}
    revision = 'e4e9ddf21a7b1903b7acffd8814ad4307bf63a67'
    for name, expected in original['files'].items():
        target = weights / name
        target.parent.mkdir(parents=True, exist_ok=True)
        url = f'https://huggingface.co/convaiinnovations/laya-multilingual/resolve/{revision}/{name}'
        digest, count = hashlib.sha256(), 0
        with urllib.request.urlopen(url, timeout=60) as response, target.open('xb') as out:
            while chunk := response.read(1024 * 1024):
                count += len(chunk)
                if count > sizes[name]:
                    raise ValueError(f'oversized artifact: {name}')
                digest.update(chunk)
                out.write(chunk)
        if count != sizes[name] or digest.hexdigest() != expected:
            raise ValueError(f'artifact identity mismatch: {name}')
        print(f'verified {name}', flush=True)
    config_path = weights / 'tokenizer/tokenizer_config.json'
    config = json.loads(config_path.read_text())
    config['extra_special_tokens'] = {
        f'extra_{i}': token for i, token in enumerate(config['extra_special_tokens'])}
    config_path.write_text(json.dumps(config, indent=2))
    for name, expected in prepared['files'].items():
        if hashlib.sha256((weights / name).read_bytes()).hexdigest() != expected:
            raise ValueError(f'prepared artifact mismatch: {name}')
    env = dict(os.environ, OMP_NUM_THREADS='4', MKL_NUM_THREADS='4',
               TOKENIZERS_PARALLELISM='false')
    env.pop('LAYA_CPU_AMP', None)
    for precision in ('fp32', 'bf16'):
        if precision == 'bf16':
            env['LAYA_CPU_AMP'] = 'bf16'
        subprocess.run([
            sys.executable, str(repo / 'scripts/benchmark_laya_shadow.py'),
            '--model-dir', str(weights), '--manifest', str(evidence / 'prepared-manifest.json'),
            '--device', 'cpu', '--dataset-kind', 'synthetic',
            '--dataset', str(repo / 'scripts/fixtures/laya-shadow-synthetic.jsonl'),
            '--warmups', '10', '--repeats', '100', '--max-latency-ms', '100',
            '--output', str(work / f'cpu-{precision}.jsonl')], env=env, check=True)
        rows = [json.loads(line) for line in (work / f'cpu-{precision}.jsonl').read_text().splitlines()]
        if rows[-1]['status'] != 'completed':
            raise RuntimeError(f'{precision} completed with model errors; inspect retained evidence')


if __name__ == '__main__':
    main()
