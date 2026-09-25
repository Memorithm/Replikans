#!/usr/bin/env python3
"""Bounded paper protection worker independent of model availability.

Run alongside the model with the SAME immutable config, runtime and venue DBs.
This is a foreground worker, not a daemon installer. Failures require inspection.
"""
import argparse
import json
import math
from pathlib import Path
import time

from trading_mcp import RustBackend


def run_guard(backend, ticks, interval_seconds, sleeper=time.sleep, emit=None):
    if not 1 <= ticks <= 100000 or not math.isfinite(interval_seconds) or not 1 <= interval_seconds <= 60:
        raise ValueError('invalid guard tick/interval budget')
    emit = emit or (lambda value: print(json.dumps(value), flush=True))
    capabilities = backend.call({'operation': 'capabilities'})
    if (capabilities.get('mode') != 'paper' or capabilities.get('live') is not False
            or not capabilities.get('protection_enabled') or not capabilities.get('market_data_enabled')):
        raise ValueError('guard requires paper protection and public quotes')
    for tick in range(ticks):
        if tick:
            sleeper(interval_seconds)
        # Refresh is bounded by the HTTP deadline. Even after collection fails,
        # run protection once so deadline stops can abandon/cancel pending orders.
        # An error never authorizes resubmitting a possibly dispatched exit.
        refresh_error = None
        try:
            backend.call({'operation': 'market_refresh'})
        except Exception as error:
            refresh_error = error
        try:
            protection = backend.call({'operation': 'protect'})
        except Exception as error:
            emit({'tick': tick, 'state': 'protection_failed', 'error_type': type(error).__name__,
                  'refresh_failed': refresh_error is not None,
                  'recovery': 'inspect and reconcile; no blind resubmission'})
            raise
        emit({'tick': tick, 'state': 'observed', 'protection': protection,
              'refresh_failed': refresh_error is not None})
        if refresh_error:
            raise refresh_error
        if (protection.get('progress', {}).get('stopped')
                and protection.get('flat') is True and protection.get('open_orders') == 0):
            return {'reason': protection['progress']['stopped'], 'ticks': tick + 1, 'flat': True}
    return {'reason': 'tick_budget_exhausted', 'ticks': ticks,
            'flat': protection.get('flat'), 'open_orders': protection.get('open_orders')}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('runtime', 'config', 'journal', 'paper-venue'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--ticks', type=int, default=100)
    parser.add_argument('--interval-seconds', type=float, default=10)
    args = parser.parse_args()
    backend = RustBackend([str(Path(p).resolve()) for p in
                           (args.runtime, args.config, args.journal, args.paper_venue)])
    result = run_guard(backend, args.ticks, args.interval_seconds)
    print(json.dumps(result), flush=True)


if __name__ == '__main__':
    main()
