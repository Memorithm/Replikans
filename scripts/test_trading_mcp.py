"""Protocol tests plus optional real Rust subprocess acceptance (required in CI)."""
import copy
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

import trading_mcp as mcp


def fixture():
    now = time.time_ns() // 1_000_000
    config = {
        "schema_version": 1, "venue": "paper", "account_id": "mcp-test-account",
        "instruments": {"TEST-QUOTE": {
            "venue": "paper", "instrument_id": "TEST-QUOTE", "base_asset": "TEST",
            "quote_asset": "QUOTE", "rules_version": "fixture-v1", "price_tick": "0.01",
            "quantity_step": "0.01", "min_quantity": "0.01", "max_quantity": None,
            "min_notional": "1", "max_notional": None,
        }},
        "initial_balances": {"QUOTE": "1000"}, "max_quantity": "10",
        "max_order_notional": "1000", "max_open_orders": 5,
        "max_intent_age_ms": 60000, "paper_quote_fee": "1",
    }
    intent = {
        "intent_id": "intent-buy", "idempotency_key": "key-buy", "client_order_id": "buy",
        "agent_id": "scripted-fixture", "decision_id": "decision-buy", "strategy_version": "fixture-v1",
        "evidence_refs": ["synthetic:fixture"], "rationale": "MCP contract acceptance, not a model decision",
        "created_at_ms": now, "expires_at_ms": now + 60000,
        "request": {"instrument_id": "TEST-QUOTE", "side": "Buy", "order_type": "Market",
                    "quantity": "1", "tif": "Gtc", "reduce_only": False,
                    "post_only": False, "rules_version": "fixture-v1"},
        "reference": {"venue": "paper", "instrument_id": "TEST-QUOTE", "price": "100",
                      "observed_at_ms": now, "valid_until_ms": now + 60000},
    }
    return config, intent


def request(method, params=None, identifier=1):
    return {"jsonrpc": "2.0", "id": identifier, "method": method, "params": params or {}}


def initialize(server):
    result = server.handle(request("initialize", {"protocolVersion": mcp.PROTOCOL,
                           "capabilities": {}, "clientInfo": {"name": "fixture", "version": "1"}}))
    assert result["result"]["protocolVersion"] == mcp.PROTOCOL
    assert server.handle({"jsonrpc": "2.0", "method": "notifications/initialized"}) is None


class FakeBackend:
    def __init__(self):
        self.calls = []

    def call(self, value):
        self.calls.append(value)
        return {"mode": "paper", "live": False}


class ProtocolTests(unittest.TestCase):
    def setUp(self):
        self.backend = FakeBackend()
        self.server = mcp.Server(self.backend)

    def test_lifecycle_and_version_negotiation(self):
        self.assertEqual(self.server.handle(request("tools/list"))["error"]["code"], -32002)
        self.assertEqual(self.server.handle(request("ping"))["result"], {})
        response = self.server.handle(request("initialize", {"protocolVersion": "unknown-version",
                                     "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}))
        self.assertEqual(response["result"]["protocolVersion"], mcp.PROTOCOL)
        self.assertEqual(self.server.handle(request("tools/list"))["error"]["code"], -32002)
        self.server.handle({"jsonrpc": "2.0", "method": "notifications/initialized"})
        self.assertEqual(len(self.server.handle(request("tools/list"))["result"]["tools"]), 10)
        self.assertEqual(self.server.handle(request("initialize"))["error"]["code"], -32602)

    def test_notifications_cannot_execute_and_have_no_response(self):
        initialize(self.server)
        for method in ("tools/call", "unknown", "notifications/cancelled"):
            self.assertIsNone(self.server.handle({"jsonrpc": "2.0", "method": method,
                              "params": {"name": "order_submit", "arguments": {"client_order_id": "buy"}}}))
        self.assertEqual(self.backend.calls, [])

    def test_invalid_envelopes_and_methods(self):
        initialize(self.server)
        for value in ([], None, {"jsonrpc": "1.0", "method": "ping"}, request("ping", identifier=True),
                      request("ping", identifier=None)):
            self.assertEqual(self.server.handle(value)["error"]["code"], -32600)
        self.assertEqual(self.server.handle(request("unknown"))["error"]["code"], -32601)
        self.assertEqual(self.server.handle(request("tools/call", {"name": "withdraw"}))["error"]["code"], -32602)

    def test_strict_nested_schemas_reject_without_side_effects(self):
        initialize(self.server)
        _, original = fixture()
        invalid = []
        for key, value in (("quantity", 1.0), ("post_only", 0), ("reduce_only", True),
                           ("order_type", "StopMarket"), ("unexpected", "x")):
            item = copy.deepcopy(original)
            item["request"][key] = value
            invalid.append(item)
        for key, value in (("created_at_ms", True), ("evidence_refs", []), ("config", {})):
            item = copy.deepcopy(original)
            item[key] = value
            invalid.append(item)
        for item in invalid:
            response = self.server.handle(request("tools/call", {"name": "order_prepare", "arguments": {"intent": item}}))
            self.assertTrue(response["result"]["isError"])
        self.assertEqual(self.backend.calls, [])

    def test_schema_accepts_market_and_limit_and_forwards_unchanged(self):
        initialize(self.server)
        _, intent = fixture()
        for order_type in ("Market", {"Limit": {"price": "90"}}):
            intent["request"]["order_type"] = order_type
            response = self.server.handle(request("tools/call", {"name": "order_prepare", "arguments": {"intent": intent}}))
            self.assertFalse(response["result"]["isError"])
            self.assertEqual(self.backend.calls[-1], {"operation": "prepare", "intent": intent})

    def test_duplicate_fields_nonfinite_and_parse_recovery(self):
        for value in ('{"id":1,"id":2}', '{"x":NaN}', '{"x":Infinity}'):
            with self.assertRaises(ValueError):
                mcp.strict_json(value)
        reader = io.BytesIO(b'not json\n' + json.dumps(request("ping", identifier="next")).encode() + b'\n')
        writer = io.BytesIO()
        mcp.serve(self.server, reader, writer)
        results = [json.loads(line) for line in writer.getvalue().splitlines()]
        self.assertEqual(results[0]["error"]["code"], -32700)
        self.assertEqual(results[1]["id"], "next")

    def test_input_budget(self):
        with self.assertRaises(mcp.Invalid):
            mcp.serve(self.server, io.BytesIO(b" " * (mcp.MAX_INPUT + 1)), io.BytesIO())

    def test_tool_result_and_annotations(self):
        initialize(self.server)
        response = self.server.handle(request("tools/call", {"name": "capabilities"}))
        self.assertEqual(response["result"]["structuredContent"]["protocol"], "mcp-stdio")
        self.assertFalse(response["result"]["structuredContent"]["live"])
        self.assertFalse(mcp.BY_NAME["order_submit"]["annotations"]["idempotentHint"])
        self.assertTrue(mcp.BY_NAME["account_snapshot"]["annotations"]["readOnlyHint"])


class BackendTests(unittest.TestCase):
    def backend(self, program, **kwargs):
        return mcp.RustBackend([sys.executable, "-c", program], **kwargs)

    def test_valid_response(self):
        backend = self.backend('import sys; sys.stdin.read(); print(\'{"ok":true,"result":{"value":1}}\')')
        self.assertEqual(backend.call({"operation": "snapshot"}), {"value": 1})

    def test_failure_timeout_and_output_budget(self):
        programs = ["import sys; sys.exit(1)", "import time; time.sleep(5)",
                    "print('x' * 10000)", "print('not-json')",
                    "print('{\"ok\":true}')"]
        for program in programs:
            with self.subTest(program=program), self.assertRaises(mcp.BackendError):
                self.backend(program, timeout=0.2, output_limit=1000).call({"operation": "dispatch"})

    def test_no_implicit_retry_after_uncertain_result(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "calls"
            program = ("import pathlib,sys; sys.stdin.read(); p=pathlib.Path(sys.argv[1]); "
                       "p.write_text(p.read_text()+'x' if p.exists() else 'x'); sys.exit(1)")
            backend = mcp.RustBackend([sys.executable, "-c", program, str(marker)])
            with self.assertRaises(mcp.BackendError):
                backend.call({"operation": "dispatch"})
            self.assertEqual(marker.read_text(), "x")


@unittest.skipUnless(os.environ.get("REPLIKAN_TRADING_BINARY"), "real Rust binary not provided")
class RustAcceptance(unittest.TestCase):
    def test_mcp_stdio_roundtrip_restart_cancel_and_policy(self):
        with tempfile.TemporaryDirectory(prefix="replikan-mcp-test-") as directory:
            root = Path(directory)
            config, buy = fixture()
            config_path = root / "config.json"
            config_path.write_text(json.dumps(config), encoding="utf-8")
            command = [sys.executable, str(Path(mcp.__file__).resolve()),
                       "--runtime", str(Path(os.environ["REPLIKAN_TRADING_BINARY"]).resolve()),
                       "--config", str(config_path), "--journal", str(root / "journal.sqlite"),
                       "--paper-venue", str(root / "venue.sqlite")]
            hello = request("initialize", {"protocolVersion": mcp.PROTOCOL, "capabilities": {},
                            "clientInfo": {"name": "scripted-acceptance", "version": "1"}})
            ready = {"jsonrpc": "2.0", "method": "notifications/initialized"}

            def session(calls):
                messages = [hello, ready] + [request("tools/call", {"name": name, "arguments": arguments}, index + 2)
                                             for index, (name, arguments) in enumerate(calls)]
                result = subprocess.run(command, input="".join(json.dumps(m) + "\n" for m in messages),
                                        text=True, capture_output=True, check=True, timeout=60)
                responses = [json.loads(line) for line in result.stdout.splitlines()]
                self.assertEqual(len(responses), len(messages) - 1)
                return [item["result"] for item in responses[1:]]

            first = session([("capabilities", {}), ("instrument_rules", {"instrument_id": "TEST-QUOTE"}),
                             ("order_prepare", {"intent": buy}), ("order_prepare", {"intent": buy}),
                             ("order_submit", {"client_order_id": "buy"}),
                             ("order_submit", {"client_order_id": "buy"}),
                             ("order_get", {"client_order_id": "buy"}), ("account_snapshot", {})])
            self.assertFalse(first[0]["structuredContent"]["live"])
            self.assertEqual(first[1]["structuredContent"]["rules"], config["instruments"]["TEST-QUOTE"])
            self.assertTrue(first[5]["isError"])
            self.assertEqual(first[6]["structuredContent"]["order"]["status"], "Filled")
            self.assertEqual(first[7]["structuredContent"]["balances"], {"QUOTE": "899", "TEST": "1"})
            sell = copy.deepcopy(buy)
            for key in ("intent_id", "idempotency_key", "client_order_id", "decision_id"):
                sell[key] = sell[key].replace("buy", "sell")
            sell["request"]["side"] = "Sell"
            sell["reference"]["price"] = "110"
            second = session([("execution_reconcile", {"client_order_id": "buy"}),
                              ("order_prepare", {"intent": sell}), ("order_submit", {"client_order_id": "sell"}),
                              ("account_snapshot", {}), ("session_export", {})])
            self.assertTrue(all(not result["isError"] for result in second), second)
            snapshot = second[3]["structuredContent"]
            self.assertEqual(snapshot["balances"], {"QUOTE": "1008", "TEST": "0"})
            self.assertEqual(len(snapshot["fills"]), 2)
            self.assertEqual(session([("account_snapshot", {})])[0]["structuredContent"], snapshot)
            self.assertEqual(second[4]["structuredContent"]["journal_hash"], snapshot["journal_hash"])
            resting = copy.deepcopy(buy)
            for key in ("intent_id", "idempotency_key", "client_order_id", "decision_id"):
                resting[key] = resting[key].replace("buy", "resting")
            resting["request"]["order_type"] = {"Limit": {"price": "90"}}
            canceled = session([("order_prepare", {"intent": resting}),
                                ("order_submit", {"client_order_id": "resting"}),
                                ("order_cancel", {"client_order_id": "resting"}),
                                ("order_get", {"client_order_id": "resting"})])
            self.assertTrue(all(not result["isError"] for result in canceled), canceled)
            self.assertEqual(canceled[-1]["structuredContent"]["order"]["status"], "Canceled")
            bad = copy.deepcopy(buy)
            bad["request"]["quantity"] = "100"
            self.assertTrue(session([("order_prepare", {"intent": bad})])[0]["isError"])
            abandoned = copy.deepcopy(buy)
            for key in ("intent_id", "idempotency_key", "client_order_id", "decision_id"):
                abandoned[key] = abandoned[key].replace("buy", "abandoned")
            results = session([("order_prepare", {"intent": abandoned}),
                               ("order_abandon", {"client_order_id": "abandoned", "reason": "strategy changed"}),
                               ("order_get", {"client_order_id": "abandoned"}),
                               ("order_abandon", {"client_order_id": "buy", "reason": "cannot undo"})])
            self.assertFalse(results[1]["isError"])
            self.assertEqual(results[2]["structuredContent"]["order"]["status"], "Rejected")
            self.assertTrue(results[3]["isError"])


if __name__ == "__main__":
    unittest.main()
