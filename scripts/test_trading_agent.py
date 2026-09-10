import copy
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch

import trading_agent as agent
import trading_mcp as mcp
from test_trading_mcp import FakeBackend, fixture


class ScriptedModel:
    def __init__(self, decisions):
        self.decisions = iter(decisions)
        self.calls = 0

    def identity(self):
        return {"provider": "scripted-test-fixture", "model": "no-llm", "digest": "fixture-v1"}

    def generate(self, messages, schema):
        self.calls += 1
        return {"content": json.dumps(next(self.decisions)), "usage": {}}


def finish():
    return {"kind": "finish", "rationale": "insufficient evidence to trade"}


class JournalTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.path = Path(self.directory.name) / "experiment.sqlite"
        self.journal = agent.Journal(self.path)

    def tearDown(self):
        self.journal.close()
        self.directory.cleanup()

    def test_source_revisions_preserve_first_availability_and_reopen(self):
        original = {"source_id": "source-a", "url": "fixture:news", "kind": "news",
                    "content": "original", "published_at_ms": 10, "event_at_ms": 5}
        with patch.object(agent, "now_ms", return_value=100):
            first = self.journal.ingest(original)
        with patch.object(agent, "now_ms", return_value=200):
            self.assertEqual(self.journal.ingest(original), first)
            revision = self.journal.ingest({**original, "content": "corrected"})
        self.assertEqual(self.journal.context_at(99), [])
        self.assertEqual(self.journal.context_at(150), [first])
        self.assertEqual(self.journal.context_at(200), [revision])
        exported = self.journal.export()
        self.journal.close()
        self.journal = agent.Journal(self.path)
        self.assertEqual(self.journal.export(), exported)

    def test_source_validation_and_budget(self):
        source = {"source_id": "source-a", "url": "fixture:data", "kind": "market", "content": "test"}
        for change in ({"first_seen_at_ms": 0}, {"url": "https://user:password@example.com"},
                       {"published_at_ms": 9223372036854775807}, {"content": "x" * 65537}):
            with self.assertRaises((agent.AgentError, mcp.Invalid)):
                self.journal.ingest({**source, **change})
        self.assertEqual(self.journal.events, [])

    def test_corruption_and_single_writer(self):
        with self.assertRaises(agent.AgentError):
            agent.Journal(self.path)
        self.journal.append("test", "episode", {"value": 1})
        with sqlite3.connect(self.path) as connection:
            connection.execute("UPDATE experiment_events SET hash='tampered'")
        with self.assertRaises(agent.AgentError):
            self.journal.reload()

    def test_finish_records_non_action_and_consumes_episode_identity(self):
        model = ScriptedModel([finish()])
        client = agent.ToolClient(FakeBackend())
        agent.run_episode(self.journal, model, client, "episode-a", "observe")
        self.assertEqual(self.journal.events[-1]["kind"], "episode_finished")
        self.assertEqual(self.journal.events[-1]["data"]["reason"], "model_finish")
        self.assertEqual(model.calls, 1)
        with self.assertRaises(agent.AgentError):
            agent.run_episode(self.journal, model, client, "episode-a", "observe")
        self.assertEqual(model.calls, 1)

    def test_bad_model_output_never_reaches_mutation(self):
        backend = FakeBackend()
        decision = {"kind": "tool", "name": "withdraw", "arguments": {}, "rationale": "invalid"}
        with self.assertRaises(mcp.Invalid):
            agent.run_episode(self.journal, ScriptedModel([decision]), agent.ToolClient(backend), "bad", "observe")
        self.assertEqual([c["operation"] for c in backend.calls], ["capabilities", "snapshot"])
        self.assertEqual(self.journal.events[-1]["kind"], "episode_failed")
        self.assertTrue(any(e["kind"] == "model_response" for e in self.journal.events))

    def test_budget_exhaustion_and_interrupted_tool_never_replayed(self):
        decision = {"kind": "tool", "name": "account_snapshot", "arguments": {}, "rationale": "observe"}
        self.journal.append("tool_requested", "old", {"call_id": "old:0", "name": "order_submit",
                            "arguments": {"client_order_id": "uncertain"}})
        backend = FakeBackend()
        model = ScriptedModel([decision, decision])
        agent.run_episode(self.journal, model, agent.ToolClient(backend), "new", "observe", max_steps=2)
        self.assertEqual(model.calls, 2)
        self.assertEqual(self.journal.events[-1]["data"]["reason"], "step_budget_exhausted")
        self.assertEqual(self.journal.unresolved()[0]["call_id"], "old:0")
        self.assertNotIn("dispatch", [c["operation"] for c in backend.calls])

    def test_exception_after_tool_claim_remains_visible_on_restart(self):
        class BrokenClient(agent.ToolClient):
            def call(self, name, arguments):
                if name == "order_submit":
                    raise RuntimeError("fixture process failure after possible effect")
                return super().call(name, arguments)
        model = ScriptedModel([{"kind": "tool", "name": "order_submit",
                               "arguments": {"client_order_id": "uncertain"}, "rationale": "fixture"}])
        with self.assertRaises(RuntimeError):
            agent.run_episode(self.journal, model, BrokenClient(FakeBackend()), "crash", "fixture")
        self.journal.close()
        self.journal = agent.Journal(self.path)
        self.assertEqual(self.journal.unresolved()[0]["name"], "order_submit")


class OllamaTests(unittest.TestCase):
    def test_endpoint_restrictions(self):
        for endpoint in ("http://example.com", "http://localhost:11434", "http://127.0.0.1/api", "http://u:p@127.0.0.1"):
            with self.assertRaises(agent.AgentError):
                agent.Ollama(endpoint, "fixture")

    def test_official_http_shapes_and_structured_response(self):
        seen = []
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def respond(self, value):
                payload = json.dumps(value).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def do_GET(self):
                seen.append(self.path)
                self.respond({"models": [{"name": "fixture", "digest": "fixture-digest"}]})

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                seen.append((self.path, body))
                self.respond({"model": "fixture", "done": True, "message": {"role": "assistant", "content": json.dumps(finish())},
                              "eval_count": 12, "total_duration": 100})

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            model = agent.Ollama(f"http://127.0.0.1:{server.server_port}", "fixture")
            self.assertEqual(model.identity()["digest"], "fixture-digest")
            result = model.generate([{"role": "user", "content": "fixture"}], agent.decision_schema())
            self.assertEqual(json.loads(result["content"]), finish())
            self.assertEqual(seen[0], "/api/tags")
            self.assertEqual(seen[1][0], "/api/chat")
            self.assertFalse(seen[1][1]["stream"])
            self.assertEqual(seen[1][1]["format"], agent.decision_schema())
            with self.assertRaises(agent.AgentError):
                agent.Ollama(model.endpoint, "absent").identity()
        finally:
            server.shutdown()
            server.server_close()
            worker.join(timeout=2)


@unittest.skipUnless(os.environ.get("REPLIKAN_TRADING_BINARY"), "real Rust binary not provided")
class RustAgentAcceptance(unittest.TestCase):
    def test_scripted_agent_roundtrip_and_evidence_reopen(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config, buy = fixture()
            (root / "config.json").write_text(json.dumps(config))
            sell = copy.deepcopy(buy)
            for key in ("intent_id", "idempotency_key", "client_order_id", "decision_id"):
                sell[key] = sell[key].replace("buy", "sell")
            sell["request"]["side"] = "Sell"
            sell["reference"]["price"] = "110"
            decisions = []
            for intent in (buy, sell):
                decisions += [{"kind": "tool", "name": "order_prepare", "arguments": {"intent": intent}, "rationale": "synthetic test"},
                              {"kind": "tool", "name": "order_submit", "arguments": {"client_order_id": intent["client_order_id"]}, "rationale": "synthetic test"}]
            decisions.append(finish())
            backend = mcp.RustBackend([str(Path(os.environ["REPLIKAN_TRADING_BINARY"]).resolve()),
                                       str(root / "config.json"), str(root / "runtime.sqlite"), str(root / "venue.sqlite")])
            journal = agent.Journal(root / "experiment.sqlite")
            try:
                journal.ingest({"source_id": "fixture", "url": "fixture:prices", "kind": "market", "content": "Synthetic buy 100, sell 110; not observed market data."})
                agent.run_episode(journal, ScriptedModel(decisions), agent.ToolClient(backend), "roundtrip", "test-only roundtrip")
                self.assertEqual(backend.call({"operation": "snapshot"})["balances"], {"QUOTE": "1008", "TEST": "0"})
                decisions_saved = [e for e in journal.events if e["kind"] == "decision"]
                self.assertEqual(len(decisions_saved), 5)
                self.assertEqual(journal.unresolved(), [])
                before = journal.export()
            finally:
                journal.close()
            reopened = agent.Journal(root / "experiment.sqlite")
            try:
                self.assertEqual(reopened.export(), before)
            finally:
                reopened.close()


if __name__ == "__main__":
    unittest.main()
