#!/usr/bin/env python3
"""Synthetic paper acceptance: no exchange, funds, keys or live network."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def main():
    binary = str(Path(sys.argv[1]).resolve())
    now = time.time_ns() // 1_000_000
    config = {
        "schema_version": 1, "venue": "paper", "account_id": "demo-account",
        "instruments": {"TEST-QUOTE": {
            "venue": "paper", "instrument_id": "TEST-QUOTE", "base_asset": "TEST",
            "quote_asset": "QUOTE", "rules_version": "demo-v1", "price_tick": "0.01",
            "quantity_step": "0.01", "min_quantity": "0.01", "max_quantity": None,
            "min_notional": "1", "max_notional": None,
        }},
        "initial_balances": {"QUOTE": "1000"}, "max_quantity": "10",
        "max_order_notional": "1000", "max_open_orders": 5,
        "max_intent_age_ms": 60000, "paper_quote_fee": "1",
    }

    def intent(identifier, side, price):
        return {
            "intent_id": "intent-" + identifier, "idempotency_key": "key-" + identifier,
            "client_order_id": identifier, "agent_id": "scripted-fixture",
            "decision_id": "decision-" + identifier, "strategy_version": "demo-v1",
            "evidence_refs": ["synthetic:fixture"], "rationale": "paper contract acceptance",
            "created_at_ms": now, "expires_at_ms": now + 60000,
            "request": {"instrument_id": "TEST-QUOTE", "side": side, "order_type": "Market",
                        "quantity": "1", "tif": "Gtc", "reduce_only": False,
                        "post_only": False, "rules_version": "demo-v1"},
            "reference": {"venue": "paper", "instrument_id": "TEST-QUOTE", "price": price,
                          "observed_at_ms": now, "valid_until_ms": now + 60000},
        }

    with tempfile.TemporaryDirectory(prefix="replikan-paper-") as directory:
        path = Path(directory)
        (path / "config.json").write_text(json.dumps(config), encoding="utf-8")
        command = [binary, str(path / "config.json"), str(path / "journal.sqlite"), str(path / "venue.sqlite")]
        requests = [{"operation": "capabilities"}]
        for identifier, side, price in [("buy", "Buy", "100"), ("sell", "Sell", "110")]:
            requests += [{"operation": "prepare", "intent": intent(identifier, side, price)},
                         {"operation": "dispatch", "client_order_id": identifier}]
        requests += [{"operation": "snapshot"}, {"operation": "export"}]
        result = subprocess.run(command, input="".join(json.dumps(r) + "\n" for r in requests),
                                text=True, capture_output=True, check=True, timeout=60)
        responses = [json.loads(line) for line in result.stdout.splitlines()]
        assert len(responses) == len(requests), responses
        assert all(r["ok"] for r in responses), responses
        snapshot = responses[-2]["result"]
        assert snapshot["balances"] == {"QUOTE": "1008", "TEST": "0"}, snapshot
        assert len(snapshot["fills"]) == 2, snapshot
        replay = subprocess.run(command, input='{"operation":"snapshot"}\n', text=True,
                                capture_output=True, check=True, timeout=60)
        assert json.loads(replay.stdout)["result"] == snapshot
        print(json.dumps({"mode": "paper", "roundtrip": "passed", "restart_replay": "passed",
                          "balances": snapshot["balances"], "journal_hash": snapshot["journal_hash"]}))


if __name__ == "__main__":
    main()
