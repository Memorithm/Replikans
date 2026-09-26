#!/usr/bin/env python3
"""Offline/shadow candidate selection. Deliberately has no trading backend/tools.

The packet binds causal source availability and a market snapshot. Selection is
an observation, never an order, a policy verdict or a profitability estimate.
"""
import hashlib
import importlib.metadata
import json
import math
import os
import platform
from pathlib import Path
import re
import time

LAYA_REVISION = '4066d5d5fbf08b66c6757ddeedbd797bd7655bc0'
ABSTAIN = 'ABSTAIN'
MAX_PACKET_BYTES = 16384
INSTRUCTIONS = 'Select the supported candidate using only the evidence. Abstain when evidence is insufficient or conflicting.'


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':'), allow_nan=False)


def fingerprint(value):
    return hashlib.sha256(canonical(value).encode()).hexdigest()


def strict_loads(text):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError('duplicate JSON key')
            result[key] = value
        return result
    def constant(_):
        raise ValueError('nonfinite JSON number')
    return json.loads(text, object_pairs_hook=pairs, parse_constant=constant)


def fields(value, expected):
    if type(value) is not dict or set(value) != set(expected):
        raise ValueError('invalid object fields')


def text(value, maximum):
    if type(value) is not str or not value.strip() or len(value) > maximum:
        raise ValueError('invalid bounded text')


def timestamp(value):
    if type(value) is not int or not 0 <= value <= 2**53-1:
        raise ValueError('invalid timestamp')


def validate_packet(packet):
    fields(packet, ('schema_version', 'request_id', 'snapshot', 'decision_at_ms', 'state', 'evidence', 'candidates'))
    if type(packet['schema_version']) is not int or packet['schema_version'] != 1:
        raise ValueError('unsupported packet version')
    if len(canonical(packet).encode()) > MAX_PACKET_BYTES:
        raise ValueError('packet exceeds byte budget')
    text(packet['request_id'], 128); text(packet['state'], 2048)
    fields(packet['snapshot'], ('id', 'observed_at_ms', 'valid_until_ms'))
    text(packet['snapshot']['id'], 128)
    for value in (packet['decision_at_ms'], packet['snapshot']['observed_at_ms'], packet['snapshot']['valid_until_ms']):
        timestamp(value)
    decision = packet['decision_at_ms']
    if not packet['snapshot']['observed_at_ms'] <= decision < packet['snapshot']['valid_until_ms']:
        raise ValueError('snapshot is future-dated or stale at the replay decision')
    if type(packet['evidence']) is not list or not 1 <= len(packet['evidence']) <= 8:
        raise ValueError('invalid evidence count')
    seen = set()
    for item in packet['evidence']:
        fields(item, ('id', 'available_at_ms', 'text'))
        text(item['id'], 128); text(item['text'], 1024); timestamp(item['available_at_ms'])
        if item['id'] in seen or item['available_at_ms'] > decision:
            raise ValueError('duplicate or future evidence')
        seen.add(item['id'])
    if type(packet['candidates']) is not list or not 1 <= len(packet['candidates']) <= 8:
        raise ValueError('invalid candidate count')
    seen = {ABSTAIN}
    for item in packet['candidates']:
        fields(item, ('id', 'description'))
        text(item['id'], 32); text(item['description'], 160)
        if not re.fullmatch(r'[A-Za-z][A-Za-z0-9_]*', item['id']) or item['id'] in seen:
            raise ValueError('invalid or duplicate candidate identity')
        seen.add(item['id'])
    return packet


def packet_from_market_snapshot(request_id, market, context, evidence, candidates, decision_at_ms):
    """Read-only bridge from a Rust market_snapshot result to an offline packet.

    Caller owns source capture; this is not a journal or exchange attestation.
    """
    quote = market['snapshot']
    buy, sell = market['buy_reference'], market['sell_reference']
    if (market.get('mode') != 'paper' or quote['received_at_ms'] > decision_at_ms
            or buy['venue'] != 'paper' or sell['venue'] != 'paper'
            or buy['instrument_id'] != quote['instrument_id'] or sell['instrument_id'] != quote['instrument_id']
            or buy['price'] != quote['ask'] or sell['price'] != quote['bid']
            or buy['observed_at_ms'] != quote['request_started_at_ms']
            or sell['observed_at_ms'] != quote['request_started_at_ms']):
        raise ValueError('market observation identity or availability mismatch')
    state = canonical({'context': context, 'market': {key: quote[key] for key in
                       ('instrument_id', 'bid', 'ask', 'bid_quantity', 'ask_quantity')}})
    return validate_packet({'schema_version': 1, 'request_id': request_id,
        'snapshot': {'id': quote['snapshot_id'], 'observed_at_ms': quote['request_started_at_ms'],
                     'valid_until_ms': min(buy['valid_until_ms'], sell['valid_until_ms'])},
        'decision_at_ms': decision_at_ms, 'state': state, 'evidence': evidence, 'candidates': candidates})


def questions(packet):
    criteria = {item['id']: item['description'] for item in packet['candidates']}
    criteria[ABSTAIN] = 'Insufficient, conflicting or irrelevant evidence.'
    return {'candidate': {'type': 'choice', 'instructions': INSTRUCTIONS, 'criteria': criteria}}


def model_state(packet):
    # Labels and evaluation baselines never enter this context.
    return canonical({'state': packet['state'], 'evidence': packet['evidence']})


def select(packet, model, min_probability=0.8, min_margin=0.05, max_latency_ms=100, clock=time.perf_counter_ns):
    for value, low, high in ((min_probability, 0, 1), (min_margin, 0, 1), (max_latency_ms, 0.001, 60000)):
        if type(value) not in (int, float) or not math.isfinite(value) or not low <= value <= high:
            raise ValueError('invalid shadow gate')
    start = clock()
    # Snapshot the caller input so provenance cannot drift during inference.
    packet = validate_packet(strict_loads(canonical(packet)))
    allowed = list(questions(packet)['candidate']['criteria'])
    result = {'schema_version': 1, 'mode': 'shadow', 'execution_authorized': False,
              'request_id': packet['request_id'], 'snapshot_id': packet['snapshot']['id'],
              'packet_sha256': fingerprint(packet), 'candidate_set_sha256': fingerprint(packet['candidates']),
              'model': model.identity, 'candidate_id': ABSTAIN, 'reason': 'model_error',
              'probabilities': None, 'answer_confidence': None,
              'gates': {'min_probability': min_probability, 'min_margin': min_margin, 'max_latency_ms': max_latency_ms}}
    try:
        response = model.predict(model_state(packet), questions(packet))
        answer = response['answers']['candidate']
        choice, probabilities, confidence = answer['choice'], answer['probabilities'], answer['answer_confidence']
        if type(probabilities) is not dict or set(probabilities) != set(allowed) or choice not in allowed:
            raise ValueError('unknown candidate or missing distribution')
        if any(type(p) not in (int, float) or not math.isfinite(p) or not 0 <= p <= 1 for p in probabilities.values()):
            raise ValueError('invalid probability')
        ranked = sorted(probabilities.values(), reverse=True)
        if (abs(sum(probabilities.values())-1) > 0.001
                or abs(probabilities[choice]-ranked[0]) > 0.0001
                or type(confidence) not in (int, float) or not math.isfinite(confidence) or not 0 <= confidence <= 1
                or abs(confidence-ranked[0]) > 0.001):
            raise ValueError('inconsistent answer probability')
        result.update(probabilities=probabilities, answer_confidence=confidence)
        if confidence < min_probability or ranked[0]-ranked[1] < min_margin:
            result['reason'] = 'uncertain'
        else:
            result.update(candidate_id=choice, reason='model_abstained' if choice == ABSTAIN else 'selected')
    except Exception as error:
        result['error_type'] = type(error).__name__
    elapsed = (clock()-start)/1_000_000
    if elapsed < 0:
        raise ValueError('monotonic clock regressed')
    result['elapsed_ms'] = elapsed
    # Historical replay TTL is a latency budget, not current wall-clock freshness.
    budget = min(max_latency_ms, packet['snapshot']['valid_until_ms']-packet['decision_at_ms'])
    if elapsed >= budget:
        result.update(candidate_id=ABSTAIN, reason='late')
    return result


def model_manifest(directory):
    root = Path(directory).resolve(strict=True)
    files = {}
    for path in sorted(root.rglob('*')):
        if path.is_symlink():
            raise ValueError('materialize model files; symlinks are not supported')
        if path.is_file():
            digest = hashlib.sha256()
            with path.open('rb') as stream:
                for chunk in iter(lambda: stream.read(1024*1024), b''):
                    digest.update(chunk)
            files[path.relative_to(root).as_posix()] = digest.hexdigest()
    if ('model.safetensors' not in files or 'rl_agent_config.json' not in files
            or not any(p.startswith('tokenizer/') for p in files)
            or not any(p.startswith('encoder/') for p in files)):
        raise ValueError('incomplete local Laya checkpoint')
    return {'schema_version': 1, 'laya_code_revision': LAYA_REVISION, 'files': files}


class LocalLaya:
    """One resident pinned checkpoint; no runtime or exchange capability."""
    def __init__(self, directory, manifest, device='cpu', fast=False):
        if device not in ('cpu', 'cuda') or (fast and device != 'cuda'):
            raise ValueError('fast path requires explicit CUDA device')
        expected = strict_loads(Path(manifest).read_text())
        actual = model_manifest(directory)
        if expected != actual:
            raise ValueError('checkpoint manifest mismatch')
        dist = importlib.metadata.distribution('laya')
        origin = strict_loads(dist.read_text('direct_url.json') or '{}')
        if dist.version != '0.3.20' or origin.get('vcs_info', {}).get('commit_id') != LAYA_REVISION:
            raise ValueError('install the reviewed Laya Git revision, not a floating release')
        os.environ['HF_HUB_OFFLINE'] = '1'
        os.environ['TRANSFORMERS_OFFLINE'] = '1'
        import laya
        import torch
        self.agent = laya.load(str(Path(directory).resolve()), device=device, expected_sha256=expected['files'])
        if self.agent.device.type != device:
            raise ValueError('Laya silently changed the requested device')
        if fast:
            self.agent.accelerate(strict=True)
        self.torch = torch
        # Bypass upstream _infer's automatic CPU retry. Any device/OOM/kernel
        # failure becomes an observed error, never an unreported slow fallback.
        self.agent._infer = self._infer_strict
        if model_manifest(directory) != expected:
            raise ValueError('loading modified checkpoint files; prepare and review them offline')
        self.identity = {'provider': 'laya', 'code_revision': LAYA_REVISION,
                         'artifact_manifest_sha256': fingerprint(expected), 'device': device, 'fast': fast,
                         'dtype': str(self.agent.dtype), 'torch': torch.__version__,
                         'device_name': torch.cuda.get_device_name(0) if device == 'cuda' else platform.processor(),
                         'cuda': torch.version.cuda, 'torch_threads': torch.get_num_threads(),
                         'torch_interop_threads': torch.get_num_interop_threads(),
                         'transformers': importlib.metadata.version('transformers')}

    def _infer_strict(self, batch):
        agent, torch = self.agent, self.torch
        if agent.device.type != self.identity['device']:
            raise ValueError('inference device changed')
        if self.identity['fast'] and (agent._fast is None or batch['input_ids'].shape[1] > agent._fast.max_len):
            raise ValueError('required fast path unavailable for this shape')
        with torch.no_grad(), torch.autocast(agent.device.type, dtype=agent.dtype,
                                             enabled=agent._amp_enabled_for(batch['input_ids'].shape[0])):
            return agent.model(*(batch[key].to(agent.device) for key in
                                 ('input_ids', 'attention_mask', 'marker_pos', 'marker_mask', 'qtype')))

    def predict(self, state, question_set):
        # Admission against the pinned formatter prevents its silent head/state
        # truncation. This adapter handles exactly one choice question.
        from laya.common import encode_text, render_options, serialize_state
        agent = self.agent
        q = agent._to_internal(question_set['candidate'])
        cleaned = lambda value: value.replace(agent.tok.mask_token, ' ')
        tokens = lambda value: encode_text(agent.tok, cleaned(value), add_special_tokens=False)['input_ids']
        head = tokens('%s question: %s' % (q['t'], q['ins']))
        options = [tokens(' ' + option) for option in render_options(q)]
        state_tokens = tokens(serialize_state(state))
        option_size = sum(1+len(option) for option in options)
        head_budget = agent.cfg.get('head_max_len', 192)-option_size
        if (any(len(option) > 48 for option in options) or head_budget < 16 or len(head) > head_budget
                or 4+len(head)+option_size+len(state_tokens) > agent.cfg.get('max_len', 512)):
            raise ValueError('packet would be truncated by Laya')
        if agent.device.type != self.identity['device']:
            raise ValueError('inference device changed')
        response = agent.predict(state, question_set)
        if agent.device.type != self.identity['device']:
            raise ValueError('inference fell back to another device')
        return response
