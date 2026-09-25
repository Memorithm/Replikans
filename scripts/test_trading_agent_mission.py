"""Mission/campaign acceptance; all prices and model decisions are test fixtures."""
import copy
import json
import os
from pathlib import Path
import tempfile
import time
import unittest

import trading_agent as agent
import trading_mcp as mcp
from test_trading_mcp import fixture
from test_trading_agent import ScriptedModel, finish


def mission_policy():
    now = time.time_ns() // 1_000_000
    return {"schema_version": 1, "mission_id": "fixture-mission",
            "objective": "Synthetic test only: target 8 QUOTE net after fees",
            "instrument_id": "TEST-QUOTE", "starts_at_ms": now - 1000,
            "expires_at_ms": now + 60000, "target_net_profit": "8",
            "max_net_realized_loss": "5", "max_buy_spend": "303"}


class MissionBackend:
    def __init__(self):
        self.policy = mission_policy()
        self.block = None
        self.recovery = []
        self.calls = []

    def call(self, command):
        self.calls.append(command)
        operation = command["operation"]
        if operation == "capabilities":
            return {"mode": "paper", "live": False, "operations": ["mission_status"]}
        if operation == "mission_status":
            return {"mode": "paper", "mission": {"policy": self.policy,
                    "runtime_journal_hash": "fixture",
                    "new_buys_blocked_by": self.block, "progress": {"base_inventory": "0"}}}
        if operation == "snapshot":
            return {"mode": "paper", "balances": {}, "orders": [], "fills": [],
                    "journal_hash": "fixture", "recovery_required": self.recovery}
        if operation == "export":
            return {"journal_hash": "fixture", "entries": []}
        raise AssertionError(operation)


class CampaignTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.journal = agent.Journal(Path(self.directory.name) / "experiment.sqlite")
        self.backend = MissionBackend()
        self.client = agent.ToolClient(self.backend)

    def tearDown(self):
        self.journal.close()
        self.directory.cleanup()

    def test_bounded_episodes_and_consumed_campaign_identity(self):
        model = ScriptedModel([finish(), finish()])
        sleeps = []
        result = agent.run_campaign(self.journal, model, self.client, "bounded", 2, 3,
                                    sleeper=sleeps.append)
        self.assertEqual(result["reason"], "episode_budget_exhausted")
        self.assertEqual(result["episodes"], 2)
        self.assertEqual(sleeps, [3])
        with self.assertRaises(agent.AgentError):
            agent.run_campaign(self.journal, model, self.client, "bounded", 2, 0)
        self.assertEqual(model.calls, 2)

    def test_stopped_flat_mission_never_invokes_model(self):
        self.backend.block = "target_reached"
        model = ScriptedModel([])
        result = agent.run_campaign(self.journal, model, self.client, "complete", 2, 0)
        self.assertEqual(result["episodes"], 0)
        self.assertEqual(model.calls, 0)

    def test_uncertain_runtime_or_experiment_mutation_never_retries(self):
        model = ScriptedModel([])
        self.backend.recovery = ["unknown-order"]
        with self.assertRaises(agent.AgentError):
            agent.run_campaign(self.journal, model, self.client, "unknown", 2, 0)
        self.assertEqual(self.journal.events[-1]["kind"], "campaign_failed")
        self.backend.recovery = []
        self.journal.append("tool_requested", "old", {"call_id": "old:1", "name": "order_submit",
                            "arguments": {"client_order_id": "unknown-order"}})
        with self.assertRaises(agent.AgentError):
            agent.run_campaign(self.journal, model, self.client, "pending", 2, 0)
        self.assertEqual(model.calls, 0)

    def test_expiry_during_wait_is_checked_before_another_model_call(self):
        model = ScriptedModel([finish()])
        def sleep(_):
            self.backend.block = "mission_expired"
        result = agent.run_campaign(self.journal, model, self.client, "expiry", 2, 1, sleeper=sleep)
        self.assertEqual(result["reason"], "mission_expired")
        self.assertEqual(model.calls, 1)

    def test_concurrent_projection_change_is_not_treated_as_a_flat_success(self):
        original = self.backend.call
        def changing(command):
            result = original(command)
            if command["operation"] == "snapshot":
                result["journal_hash"] = "different-runtime-revision"
            return result
        self.backend.call = changing
        self.backend.block = "target_reached"
        model = ScriptedModel([])
        with self.assertRaises(agent.AgentError):
            agent.run_campaign(self.journal, model, self.client, "race", 2, 0)
        self.assertEqual(model.calls, 0)
        self.assertEqual(self.journal.events[-1]["kind"], "campaign_failed")

    def test_policy_drift_and_model_error_abort_without_retry(self):
        model = ScriptedModel([finish()])
        def sleep(_):
            self.backend.policy = {**self.backend.policy, "target_net_profit": "900"}
        with self.assertRaises(agent.AgentError):
            agent.run_campaign(self.journal, model, self.client, "drift", 2, 1, sleeper=sleep)
        self.assertEqual(model.calls, 1)
        bad_model = ScriptedModel([{"kind": "unsupported"}])
        with self.assertRaises(mcp.Invalid):
            agent.run_campaign(self.journal, bad_model, self.client, "bad-model", 2, 0)
        self.assertEqual(bad_model.calls, 1)
        self.assertEqual(self.journal.events[-1]["kind"], "campaign_failed")


@unittest.skipUnless(os.environ.get("REPLIKAN_TRADING_BINARY"), "real Rust binary not provided")
class RustMissionCampaignAcceptance(unittest.TestCase):
    def test_goal_buy_sell_net_result_stop_and_durable_restart(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config, buy = fixture()
            config["mission"] = mission_policy()
            (root / "config.json").write_text(json.dumps(config))
            sell = copy.deepcopy(buy)
            for key in ("intent_id", "idempotency_key", "client_order_id", "decision_id"):
                sell[key] = "sell-" + sell[key]
            sell["request"]["side"] = "Sell"
            sell["reference"]["price"] = "110"
            decisions = []
            for intent in (buy, sell):
                decisions += [
                    {"kind": "tool", "name": "order_prepare", "arguments": {"intent": intent},
                     "rationale": "synthetic mission acceptance"},
                    {"kind": "tool", "name": "order_submit",
                     "arguments": {"client_order_id": intent["client_order_id"]}, "rationale": "fixture"},
                    finish(),
                ]
            model = ScriptedModel(decisions)
            command = [str(Path(os.environ["REPLIKAN_TRADING_BINARY"]).resolve()),
                       str(root / "config.json"), str(root / "runtime.sqlite"), str(root / "venue.sqlite")]
            backend = mcp.RustBackend(command)
            journal = agent.Journal(root / "experiment.sqlite")
            try:
                result = agent.run_campaign(journal, model, agent.ToolClient(backend), "acceptance", 4, 0)
                self.assertEqual(result["reason"], "target_reached")
                self.assertEqual(result["episodes"], 2)
                self.assertEqual(result["mission"]["progress"]["completed_cycle_pnl"], "8")
                self.assertEqual(result["mission"]["progress"]["quote_fees"], "2")
                self.assertEqual(model.calls, 6)
                # New process per call and new campaign cannot reset Rust budget or target latch.
                result = agent.run_campaign(journal, ScriptedModel([]), agent.ToolClient(backend), "reopen", 4, 0)
                self.assertEqual(result["episodes"], 0)
                with self.assertRaises(mcp.BackendError):
                    extra = copy.deepcopy(buy)
                    for key in ("intent_id", "idempotency_key", "client_order_id", "decision_id"):
                        extra[key] = "extra-" + extra[key]
                    backend.call({"operation": "prepare", "intent": extra})
                self.assertEqual(len(backend.call({"operation": "snapshot"})["fills"]), 2)
            finally:
                journal.close()


if __name__ == "__main__":
    unittest.main()
