#!/usr/bin/env python3
"""Local paper runtime overhead probe. No network, fills or model inference.

Uses the existing synthetic test fixture and prepares/abandons local intents.
Results include IPC, JSON and replay; they are not exchange execution latency.
"""
import argparse
import copy
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import statistics
import subprocess
import tempfile
import time

from test_trading_mcp import fixture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve()
    report = {'kind': 'synthetic_local_snapshot_latency', 'model_inference': False,
              'network_io': False, 'financial_dispatches': 0,
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'host': {'platform': platform.system(), 'machine': platform.machine(),
                       'visible_cpu_count': os.cpu_count()}, 'rows': []}
    with tempfile.TemporaryDirectory(prefix='runtime-latency-') as directory:
        root = Path(directory)
        config, template = fixture()
        (root / 'config.json').write_text(json.dumps(config))
        command = [str(binary), str(root/'config.json'), str(root/'runtime.sqlite'), str(root/'paper.sqlite')]

        def one_shot(commands):
            result = subprocess.run(command, input=''.join(json.dumps(c)+'\n' for c in commands),
                                    text=True, capture_output=True, timeout=60, check=True)
            responses = [json.loads(line) for line in result.stdout.splitlines()]
            assert len(responses) == len(commands) and all(r['ok'] for r in responses), responses
            return responses

        for records in (0, 100, 400):
            old = report['rows'][-1]['journal_records'] if report['rows'] else 0
            for index in range(old//2, records//2):
                intent = copy.deepcopy(template)
                now = time.time_ns()//1_000_000
                for key in ('intent_id', 'idempotency_key', 'client_order_id', 'decision_id'):
                    intent[key] = f'latency-{index}'
                intent.update(created_at_ms=now, expires_at_ms=now+60000)
                intent['reference'].update(observed_at_ms=now, valid_until_ms=now+60000)
                one_shot([{'operation':'prepare','intent':intent},
                          {'operation':'abandon','client_order_id':f'latency-{index}','reason':'synthetic latency probe'}])
            for mode in ('new_process_per_call', 'persistent_json_lines'):
                samples = []
                child = None
                if mode == 'persistent_json_lines':
                    child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                             stderr=subprocess.PIPE, text=True, bufsize=1)
                try:
                    for index in range(25):
                        start = time.perf_counter_ns()
                        if child:
                            child.stdin.write('{"operation":"snapshot"}\n'); child.stdin.flush()
                            response = json.loads(child.stdout.readline())
                            assert response['ok'], response
                        else:
                            response = one_shot([{'operation':'snapshot'}])[0]
                        elapsed = (time.perf_counter_ns()-start)/1_000_000
                        assert response['result']['journal_sequence'] == records
                        if index >= 5: samples.append(elapsed)
                finally:
                    if child:
                        child.stdin.close(); child.wait(timeout=10)
                        child.stdout.close(); child.stderr.close()
                report['rows'].append({'journal_records':records, 'mode':mode, 'warmups':5,
                                       'samples':len(samples), 'p50_ms':statistics.median(samples),
                                       'p95_ms':sorted(samples)[math.ceil(.95*len(samples))-1],
                                       'raw_ms':samples})
    args.output.write_text(json.dumps(report, indent=2)+'\n')
    print(json.dumps([{k:v for k,v in row.items() if k!='raw_ms'} for row in report['rows']], indent=2))


if __name__ == '__main__':
    main()
