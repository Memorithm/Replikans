import copy
from contextlib import nullcontext
import json
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import patch

import trading_laya_shadow as shadow
from benchmark_laya_shadow import FixtureModel, evaluate, load_cases

FIXTURE = Path(__file__).with_name('fixtures') / 'laya-shadow-synthetic.jsonl'


def packet():
    return load_cases(FIXTURE)[0]['packet']


class Predictor:
    identity = {'provider': 'scripted_test_only'}
    def __init__(self, answer=None, error=None):
        self.answer = answer or {'choice': 'review_news', 'answer_confidence': .9,
                                'confidence': .01, 'probabilities': {'review_news': .9, 'ignore_news': .06, 'ABSTAIN': .04}}
        self.error = error
        self.calls = []
    def predict(self, state, questions):
        self.calls.append((state, questions))
        if self.error:
            raise self.error
        return {'answers': {'candidate': self.answer}}


class ShadowTests(unittest.TestCase):
    def test_selection_binds_packet_and_uses_answer_probability_not_entropy(self):
        p = packet(); model = Predictor()
        result = shadow.select(p, model)
        self.assertEqual(result['candidate_id'], 'review_news')
        self.assertEqual(result['reason'], 'selected')
        self.assertFalse(result['execution_authorized'])
        self.assertEqual(result['packet_sha256'], shadow.fingerprint(p))
        self.assertEqual(result['snapshot_id'], p['snapshot']['id'])
        self.assertNotIn('order', result)

    def test_future_evidence_stale_snapshot_and_unknown_fields_never_reach_model(self):
        for mutation in ('future', 'stale', 'duplicate', 'order', 'nan', 'boolean_time'):
            p = packet(); model = Predictor()
            if mutation == 'future': p['evidence'][0]['available_at_ms'] = p['decision_at_ms']+1
            if mutation == 'stale': p['snapshot']['valid_until_ms'] = p['decision_at_ms']
            if mutation == 'duplicate': p['candidates'].append(copy.deepcopy(p['candidates'][0]))
            if mutation == 'order': p['order'] = {'quantity': '1000'}
            if mutation == 'nan': p['decision_at_ms'] = float('nan')
            if mutation == 'boolean_time': p['decision_at_ms'] = True
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                shadow.select(p, model)
            self.assertFalse(model.calls)

    def test_invalid_probabilities_or_choice_always_abstain(self):
        for field, value in [('choice', 'unauthorized'), ('answer_confidence', float('nan')),
                             ('answer_confidence', True), ('answer_confidence', 1.0001),
                             ('probabilities', {'review_news': 1}), ('probabilities', {'review_news': .9, 'ignore_news': .6, 'ABSTAIN': .04})]:
            model = Predictor(); model.answer[field] = value
            result = shadow.select(packet(), model)
            self.assertEqual(result['candidate_id'], shadow.ABSTAIN)
            self.assertEqual(result['reason'], 'model_error')

    def test_late_answer_rejected_even_if_confident(self):
        times = iter((0, 101_000_000))
        result = shadow.select(packet(), Predictor(), max_latency_ms=100, clock=lambda: next(times))
        self.assertEqual(result['reason'], 'late')
        self.assertEqual(result['candidate_id'], shadow.ABSTAIN)

    def test_remaining_snapshot_ttl_limits_replay_budget(self):
        p = packet(); p['snapshot']['valid_until_ms'] = p['decision_at_ms']+2
        times = iter((0, 2_000_000))
        result = shadow.select(p, Predictor(), max_latency_ms=100, clock=lambda: next(times))
        self.assertEqual(result['reason'], 'late')

    def test_uncertainty_and_model_failure_do_not_produce_a_selection(self):
        model = Predictor(); model.answer.update(choice='review_news', answer_confidence=.5,
                        probabilities={'review_news':.5,'ignore_news':.49,'ABSTAIN':.01})
        self.assertEqual(shadow.select(packet(), model, min_probability=.4)['reason'], 'uncertain')
        result = shadow.select(packet(), Predictor(error=RuntimeError('GPU unavailable')))
        self.assertEqual(result['reason'], 'model_error')
        self.assertEqual(result['candidate_id'], shadow.ABSTAIN)

    def test_late_model_failure_preserves_error_classification(self):
        times = iter((0, 101_000_000))
        result = shadow.select(
            packet(),
            Predictor(error=RuntimeError('slow GPU failure')),
            max_latency_ms=100,
            clock=lambda: next(times),
        )
        self.assertEqual(result['reason'], 'model_error')
        self.assertEqual(result['error_type'], 'RuntimeError')
        self.assertTrue(result['deadline_exceeded'])
        self.assertEqual(result['candidate_id'], shadow.ABSTAIN)

    def test_dataset_labels_never_reach_model_and_invalid_answers_are_not_correct(self):
        cases = load_cases(FIXTURE); model = Predictor(error=RuntimeError('offline'))
        rows = []
        summary = evaluate(cases, model, 1, 0, rows.append)
        self.assertEqual(summary['valid_predictions'], 0)
        self.assertIsNone(summary['valid_prediction_accuracy'])
        self.assertEqual(summary['status'], 'completed_with_model_errors')
        self.assertTrue(all('expected_candidate' not in state for state, _ in model.calls))
        self.assertFalse(summary['profitability_evaluated'])

    def test_dataset_refuses_duplicate_ids_and_nonchronological_cases(self):
        cases = load_cases(FIXTURE)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'cases.jsonl'
            for rows in ([cases[0], cases[0]], list(reversed(cases))):
                path.write_text(''.join(json.dumps(row)+'\n' for row in rows))
                with self.assertRaises(ValueError): load_cases(path)

    def test_duplicate_json_and_nonfinite_constants_are_rejected(self):
        for raw in ('{"a":1,"a":2}', '{"a":NaN}', '{"a":Infinity}'):
            with self.assertRaises(ValueError): shadow.strict_loads(raw)

    def test_manifest_detects_changed_weights_and_symlinks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ('model.safetensors', 'rl_agent_config.json', 'tokenizer/t.json', 'encoder/c.json'):
                path = root/name; path.parent.mkdir(exist_ok=True); path.write_text('fixture')
            before = shadow.model_manifest(root)
            (root/'model.safetensors').write_text('changed fixture')
            self.assertNotEqual(before, shadow.model_manifest(root))
            (root/'link').symlink_to(root/'model.safetensors')
            with self.assertRaises(ValueError): shadow.model_manifest(root)

    def test_token_admission_rejects_silent_truncation_before_forward(self):
        model = shadow.LocalLaya.__new__(shadow.LocalLaya)
        calls = []
        model.identity = {'device':'cpu'}
        model.agent = types.SimpleNamespace(
            tok=types.SimpleNamespace(mask_token='[MASK]'), cfg={'head_max_len':192,'max_len':50},
            device=types.SimpleNamespace(type='cpu'),
            _to_internal=lambda q: {'t':'choice','ins':q['instructions'],'options':list(q['criteria'].values())},
            predict=lambda *args: calls.append(args))
        common = types.ModuleType('laya.common')
        common.encode_text = lambda tok, value, **kw: {'input_ids': value.split()}
        common.render_options = lambda q: q['options']
        common.serialize_state = lambda value: value
        with patch.dict('sys.modules', {'laya': types.ModuleType('laya'), 'laya.common': common}):
            with self.assertRaisesRegex(ValueError, 'truncated'):
                model.predict('evidence '*100, shadow.questions(packet()))
        self.assertFalse(calls)

    def test_strict_forward_does_not_retry_gpu_failure_on_cpu(self):
        model = shadow.LocalLaya.__new__(shadow.LocalLaya)
        model.identity = {'device':'cuda', 'fast':False}
        model.torch = types.SimpleNamespace(no_grad=nullcontext, autocast=lambda *a, **kw: nullcontext())
        calls = []
        def failed(*args):
            calls.append(args)
            raise RuntimeError('out of memory')
        model.agent = types.SimpleNamespace(device=types.SimpleNamespace(type='cuda'), dtype='fixture',
                                            _amp_enabled_for=lambda _: False, model=failed)
        tensor = types.SimpleNamespace(shape=(1,8), to=lambda _: 'fixture tensor')
        batch = {key: tensor for key in ('input_ids','attention_mask','marker_pos','marker_mask','qtype')}
        with self.assertRaisesRegex(RuntimeError, 'out of memory'):
            model._infer_strict(batch)
        self.assertEqual(len(calls), 1)

    def test_readonly_market_bridge_preserves_exact_price_and_rejects_unavailable_quote(self):
        p = packet()
        quote = {'snapshot_id':'runtime-id', 'request_started_at_ms':990, 'received_at_ms':995,
                 'instrument_id':'TEST-QUOTE','bid':'99.01','ask':'99.02','bid_quantity':'2','ask_quantity':'3'}
        ref = {'venue':'paper','instrument_id':'TEST-QUOTE','observed_at_ms':990,'valid_until_ms':1100}
        market = {'mode':'paper','snapshot':quote, 'buy_reference':dict(ref, price='99.02'),
                  'sell_reference':dict(ref, price='99.01')}
        result = shadow.packet_from_market_snapshot('bridge', market, 'synthetic', p['evidence'], p['candidates'], 1000)
        self.assertEqual(json.loads(result['state'])['market']['bid'], '99.01')
        self.assertEqual(result['snapshot']['id'], 'runtime-id')
        quote['received_at_ms'] = 1001
        with self.assertRaises(ValueError):
            shadow.packet_from_market_snapshot('bridge', market, 'synthetic', p['evidence'], p['candidates'], 1000)


    def test_fixture_run_reports_coverage_and_separates_repetitions(self):
        summary = evaluate(load_cases(FIXTURE), FixtureModel(), 2, 0, lambda _: None)
        self.assertEqual(summary['trials'], 6)
        self.assertEqual(summary['unique_cases'], 3)
        self.assertEqual(summary['coverage'], 0)
        self.assertIsNone(summary['selected_accuracy'])
        self.assertTrue(summary['p99_sample_warning'])
        self.assertEqual(summary['deadline_exceeded_trials'], 0)


if __name__ == '__main__':
    unittest.main()
