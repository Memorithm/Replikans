import json
from pathlib import Path
import tempfile
import unittest

import trading_agent as agent
import trading_mcp as mcp


class AmbiguousDispatchBackend:
    def call(self, command):
        operation = command["operation"]
        if operation == "capabilities":
            return {"mode": "paper", "live": False}
        if operation == "snapshot":
            return {"mode": "paper", "balances": {}, "orders": [], "fills": [], "journal_hash": "fixture"}
        if operation == "dispatch":
            raise mcp.BackendError(
                "runtime timeout; outcome may be unknown; query/reconcile before any new action"
            )
        raise AssertionError(f"unexpected operation: {operation}")


class OneDecisionModel:
    def identity(self):
        return {"provider": "fixture", "model": "none", "digest": "fixture-v1"}

    def generate(self, messages, schema):
        decision = {
            "kind": "tool",
            "name": "order_submit",
            "arguments": {"client_order_id": "ambiguous-order"},
            "rationale": "exercise ambiguous dispatch recovery",
        }
        return {"content": json.dumps(decision), "usage": {}}


class AmbiguousMutationRecoveryTests(unittest.TestCase):
    def test_ambiguous_mutation_remains_unresolved_after_mcp_error(self):
        with tempfile.TemporaryDirectory() as directory:
            journal = agent.Journal(Path(directory) / "experiment.sqlite")
            try:
                with self.assertRaises(agent.AgentError):
                    agent.run_episode(
                        journal,
                        OneDecisionModel(),
                        agent.ToolClient(AmbiguousDispatchBackend()),
                        "ambiguous",
                        "exercise recovery",
                    )
                pending = journal.unresolved()
                self.assertEqual(len(pending), 1)
                self.assertEqual(pending[0]["call_id"], "ambiguous:0")
                self.assertEqual(pending[0]["name"], "order_submit")
                self.assertEqual(
                    pending[0]["arguments"], {"client_order_id": "ambiguous-order"}
                )
                self.assertFalse(
                    any(
                        event["kind"] == "tool_response"
                        and event["data"]["call_id"] == "ambiguous:0"
                        for event in journal.events
                    )
                )
            finally:
                journal.close()


if __name__ == "__main__":
    unittest.main()
