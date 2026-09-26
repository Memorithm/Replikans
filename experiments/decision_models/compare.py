"""Synthetic routing diagnostic with real resident models; no financial dispatch."""
import argparse
from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import select
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
INSTRUCTION = 'Choose A for a confirmed market event, B for unrelated text, C for unverified or conflicting information.'
CHOICES = {'A': 'Confirmed market event.', 'B': 'Unrelated text.', 'C': 'Unverified or conflicting information.'}


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def label(text):
    # Reject invalid output; never extract a lucky letter from generated prose.
    return text.strip() if isinstance(text, str) and text.strip() in CHOICES else None


def rule(text):
    text = text.lower()
    if any(s in text for s in ('unverified', 'conflicting', 'no source', 'non vérifiée')):
        return 'C'
    if any(s in text for s in ('confirmed', 'confirmé')) and any(s in text for s in ('trading', 'withdrawals', 'fees', 'retraits')):
        return 'A'
    if any(s in text for s in ('recipe', 'football', 'flowers', 'recette')):
        return 'B'
    return 'C'


def summarize(rows):
    n = len(rows)
    return {'observations': n, 'unique_cases': len({r['id'] for r in rows}),
            'correct': sum(r['prediction'] == r['expected'] for r in rows),
            'invalid': sum(r['prediction'] is None for r in rows),
            'explicit_abstentions': sum(r['prediction'] == 'C' for r in rows),
            'missed_review_as_unrelated': sum(r['expected'] == 'A' and r['prediction'] == 'B' for r in rows),
            'confusion': dict(Counter(f"{r['expected']}->{r['prediction']}" for r in rows)),
            'median_request_ms': sorted(r['request_ms'] for r in rows)[(n-1)//2]}


def read_line(proc, timeout=60):
    if not select.select([proc.stdout], [], [], timeout)[0]:
        raise TimeoutError('SciAgent response timeout')
    line = proc.stdout.readline()
    if not line:
        raise RuntimeError('SciAgent exited without a response')
    return json.loads(line)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--binary', type=Path, required=True)
    ap.add_argument('--checkpoint', type=Path, required=True)
    ap.add_argument('--model-dir', required=True)
    ap.add_argument('--manifest', required=True)
    ap.add_argument('--output', type=Path, required=True)
    args = ap.parse_args()
    dataset = Path(__file__).with_name('cases.jsonl')
    cases = [json.loads(s) for s in dataset.read_text().splitlines()]
    assert len(cases) == 12 and len({c['id'] for c in cases}) == 12
    assert all(c['expected'] in CHOICES for c in cases)
    if args.output.exists():
        ap.error('output must be new')
    import torch
    from trading_laya_shadow import LocalLaya
    torch.set_num_threads(8)
    torch.set_num_interop_threads(1)
    started = time.perf_counter_ns()
    model = LocalLaya(args.model_dir, args.manifest, device='cpu')
    load_ms = (time.perf_counter_ns()-started)/1e6
    if model.agent.dtype != torch.float32:
        raise ValueError('FP32 required')
    question = {'candidate': {'type': 'choice', 'instructions': INSTRUCTION, 'criteria': CHOICES}}
    with args.output.open('x') as out:
        def emit(row):
            out.write(json.dumps(row, ensure_ascii=False, allow_nan=False)+'\n'); out.flush()
        source_paths = [Path(__file__), dataset, Path(__file__).with_name('Cargo.toml'),
                        Path(__file__).with_name('Cargo.lock'), Path(__file__).with_name('src')/'main.rs',
                        ROOT/'scripts/trading_laya_shadow.py']
        emit({'kind':'provenance', 'synthetic':True, 'financial_dispatches':0,
              'source_sha256':{str(p.relative_to(ROOT)):digest(p) for p in source_paths},
              'binary_sha256':digest(args.binary), 'checkpoint_sha256':{
                  name:digest(args.checkpoint/name) for name in ('meta.json','model.safetensors')},
              'laya':model.identity,'laya_load_ms':load_ms,'instruction':INSTRUCTION,
              'repeats':3,'warmups_per_backend':1,'host':os.uname().machine,
              'latency_scope':'resident request; SciAgent includes IPC; no load time; not a speed qualification'})
        with args.output.with_suffix('.stderr.log').open('x') as err:
            proc = subprocess.Popen([str(args.binary.resolve()), str(args.checkpoint.resolve())],
                                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err, text=True, bufsize=1)
            try:
                emit({'kind':'sciagent_ready', **read_line(proc)})
                for backend in ('rules', 'laya', 'sciagent'):
                    observations=[]
                    for i, case in enumerate([cases[0]] + cases*3):
                        # Labels/rationales never enter either model prompt.
                        started=time.perf_counter_ns()
                        if backend=='rules':
                            raw={'response':rule(case['text'])}
                        elif backend=='laya':
                            result=model.predict(case['text'], question)['answers']['candidate']
                            probabilities=result['probabilities']
                            if (set(probabilities)!=set(CHOICES) or
                                any(not math.isfinite(v) or not 0<=v<=1 for v in probabilities.values()) or
                                abs(sum(probabilities.values())-1)>.001):
                                raise ValueError('invalid Laya distribution')
                            raw={'response':result['choice'],'probabilities':probabilities}
                        else:
                            prompt=INSTRUCTION+'\nText: '+case['text']+'\nReturn only A, B or C:\n'
                            proc.stdin.write(json.dumps({'prompt':prompt})+'\n'); proc.stdin.flush()
                            raw=read_line(proc)
                            raw['prompt']=prompt
                        elapsed=(time.perf_counter_ns()-started)/1e6
                        row={'kind':'warmup' if i==0 else 'observation','backend':backend,'id':case['id'],
                             'expected':case['expected'],'prediction':label(raw.get('response')),
                             'raw':raw,'request_ms':elapsed}
                        emit(row)
                        if i: observations.append(row)
                    emit({'kind':'summary','backend':backend,**summarize(observations)})
            finally:
                proc.stdin.close()
                try: proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill(); proc.wait()


if __name__=='__main__':
    main()
