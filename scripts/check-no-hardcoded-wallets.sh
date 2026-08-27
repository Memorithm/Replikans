#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-.}"

forbidden='(const|static)[[:space:]]+[A-Z0-9_]*(PRIVATE_KEY|SECRET_KEY|SEED_PHRASE|MNEMONIC|REWARD_WALLET|TREASURY_WALLET|PAYOUT_WALLET)[A-Z0-9_]*[[:space:]]*:[^=]+=[[:space:]]*"[^"]+"'

matches="$(grep -RInE --include='*.rs' "$forbidden" "$ROOT/crates" || true)"
if [[ -n "$matches" ]]; then
    printf '%s\n' 'ERROR: forbidden hard-coded custody or payout material detected:' >&2
    printf '%s\n' "$matches" >&2
    exit 1
fi

committed_env="$(git -C "$ROOT" ls-files | grep -E '(^|/)\.env($|\.)' || true)"
if [[ -n "$committed_env" ]]; then
    printf '%s\n' 'ERROR: committed environment file detected:' >&2
    printf '%s\n' "$committed_env" >&2
    exit 1
fi

printf '%s\n' 'wallet literal policy: OK'
