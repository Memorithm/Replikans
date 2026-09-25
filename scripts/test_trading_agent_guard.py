import unittest
import json
import os
import tempfile
from pathlib import Path
from test_trading_mcp import fixture
from trading_mcp import RustBackend

from trading_guard import run_guard


class Backend:
    def __init__(self, refresh_error=False, protect_error=False):
        self.calls = []
        self.refresh_error = refresh_error
        self.protect_error = protect_error

    def call(self, value):
        operation = value['operation']; self.calls.append(operation)
        if operation == 'capabilities':
            return {'mode': 'paper', 'live': False, 'protection_enabled': True, 'market_data_enabled': True}
        if operation == 'market_refresh' and self.refresh_error:
            raise RuntimeError('feed unavailable')
        if operation == 'protect':
            if self.protect_error:
                raise RuntimeError('outcome unknown')
            return {'progress': {'stopped': 'mission_expired'}, 'flat': True, 'open_orders': 0}
        return {}


class GuardTests(unittest.TestCase):
    def test_stopped_flat_finishes_without_any_model(self):
        backend = Backend()
        result = run_guard(backend, 3, 1, sleeper=lambda _: None, emit=lambda _: None)
        self.assertEqual(result, {'reason': 'mission_expired', 'ticks': 1, 'flat': True})
        self.assertEqual(backend.calls, ['capabilities', 'market_refresh', 'protect'])

    def test_feed_failure_still_checks_protection_once_then_exits(self):
        backend = Backend(refresh_error=True)
        with self.assertRaisesRegex(RuntimeError, 'feed unavailable'):
            run_guard(backend, 3, 1, sleeper=lambda _: None, emit=lambda _: None)
        self.assertEqual(backend.calls.count('protect'), 1)
        self.assertEqual(backend.calls.count('market_refresh'), 1)

    def test_ambiguous_protection_never_retries(self):
        backend = Backend(protect_error=True)
        with self.assertRaisesRegex(RuntimeError, 'outcome unknown'):
            run_guard(backend, 3, 1, sleeper=lambda _: None, emit=lambda _: None)
        self.assertEqual(backend.calls.count('protect'), 1)

    def test_invalid_budget_cannot_touch_backend(self):
        backend = Backend()
        for ticks, interval in [(0, 1), (1, float('nan')), (1, 0), (100001, 1)]:
            with self.assertRaises(ValueError):
                run_guard(backend, ticks, interval)
        self.assertEqual(backend.calls, [])


@unittest.skipUnless(os.environ.get("REPLIKAN_TRADING_BINARY"), "actual Rust binary not supplied")
class GuardRuntimeTests(unittest.TestCase):
    def test_expired_flat_mission_latches_in_actual_rust_without_quote_or_model(self):
        config, _ = fixture()
        config["mission"] = {"schema_version": 1, "mission_id": "guard-fixture", "objective": "Expired synthetic mandate", "instrument_id": "TEST-QUOTE", "starts_at_ms": 0, "expires_at_ms": 1, "target_net_profit": "8", "max_net_realized_loss": "5", "max_buy_spend": "303"}
        config["market_data"] = {"instrument_id": "TEST-QUOTE", "symbol": "TESTQUOTE", "max_age_ms": 5000, "min_refresh_interval_ms": 1000}
        config["protection"] = {"max_net_loss": "10", "max_drawdown": "6"}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); path = root / "config.json"; path.write_text(json.dumps(config))
            backend = RustBackend([str(Path(os.environ["REPLIKAN_TRADING_BINARY"]).resolve()), str(path), str(root / "runtime.sqlite"), str(root / "venue.sqlite")])
            result = backend.call({"operation": "protect"})
            self.assertEqual(result["progress"]["stopped"], "mission_expired")
            self.assertTrue(result["flat"])
            self.assertEqual(result["open_orders"], 0)
            self.assertEqual(backend.call({"operation": "protection_status"})["runtime_journal_hash"], result["runtime_journal_hash"])
            self.assertEqual(backend.call({"operation": "mission_status"})["mission"]["new_buys_blocked_by"], "mission_expired")
