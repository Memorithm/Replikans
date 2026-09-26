from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
REPRODUCER = ROOT / "docs/evidence/laya-cpu-2026-09-26/reproduce.py"
SPEC = importlib.util.spec_from_file_location("laya_cpu_reproduce", REPRODUCER)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class LayaCpuReproducerTests(unittest.TestCase):
    def test_reviewed_sources_match_retained_headers(self) -> None:
        MODULE.verify_pinned_sources(REPRODUCER.parent, ROOT)

    def test_modified_dataset_is_rejected_before_inference(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            scripts = repo / "scripts"
            fixtures = scripts / "fixtures"
            evidence = repo / "docs/evidence/laya-cpu-2026-09-26"
            fixtures.mkdir(parents=True)
            evidence.mkdir(parents=True)
            (scripts / "benchmark_laya_shadow.py").write_bytes(
                (ROOT / "scripts/benchmark_laya_shadow.py").read_bytes()
            )
            (scripts / "trading_laya_shadow.py").write_bytes(
                (ROOT / "scripts/trading_laya_shadow.py").read_bytes()
            )
            fixture = (ROOT / "scripts/fixtures/laya-shadow-synthetic.jsonl").read_text(
                encoding="utf-8"
            ).replace("synthetic-0", "tampered-0", 1)
            (fixtures / "laya-shadow-synthetic.jsonl").write_text(
                fixture, encoding="utf-8"
            )
            (evidence / "summary.json").write_text(
                (REPRODUCER.parent / "summary.json").read_text(encoding="utf-8"),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "pinned dataset identity mismatch"):
                MODULE.verify_pinned_sources(evidence, repo)

    def test_duplicate_dataset_key_is_rejected_before_inference(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            scripts = repo / "scripts"
            fixtures = scripts / "fixtures"
            evidence = repo / "docs/evidence/laya-cpu-2026-09-26"
            fixtures.mkdir(parents=True)
            evidence.mkdir(parents=True)
            (scripts / "benchmark_laya_shadow.py").write_bytes(
                (ROOT / "scripts/benchmark_laya_shadow.py").read_bytes()
            )
            (scripts / "trading_laya_shadow.py").write_bytes(
                (ROOT / "scripts/trading_laya_shadow.py").read_bytes()
            )
            fixture = (ROOT / "scripts/fixtures/laya-shadow-synthetic.jsonl").read_text(
                encoding="utf-8"
            )
            fixture = fixture.replace(
                '"baseline_candidate": "ABSTAIN"}',
                '"baseline_candidate": "ABSTAIN", "baseline_candidate": "ABSTAIN"}',
                1,
            )
            (fixtures / "laya-shadow-synthetic.jsonl").write_text(
                fixture, encoding="utf-8"
            )
            (evidence / "summary.json").write_text(
                (REPRODUCER.parent / "summary.json").read_text(encoding="utf-8"),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "duplicate JSON key"):
                MODULE.verify_pinned_sources(evidence, repo)


if __name__ == "__main__":
    unittest.main()
