#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-.}"

forbidden_named='(const|static)[[:space:]]+[A-Z0-9_]*(PRIVATE_KEY|SECRET_KEY|SEED_PHRASE|MNEMONIC|REWARD_WALLET|TREASURY_WALLET|PAYOUT_WALLET)[A-Z0-9_]*[[:space:]]*:[^=]+=[[:space:]]*"[^"]+"'

# 32-byte hex blobs assigned as Rust string literals (common accidental key paste).
forbidden_hex='(const|static|let)[[:space:]]+[A-Za-z0-9_]+[[:space:]]*(:[^=]+)?=[[:space:]]*"(0x)?[0-9a-fA-F]{64}"'

# Bitcoin WIF-like or seed-looking assignments in source.
forbidden_wif='(const|static)[[:space:]]+[A-Z0-9_]+[[:space:]]*:[^=]+=[[:space:]]*"[5KL][1-9A-HJ-NP-Za-km-z]{50,51}"'

scan() {
    local pattern="$1"
    grep -RInE --include='*.rs' --include='*.py' --include='*.json' --include='*.toml' "$pattern" "$ROOT/crates" "$ROOT/scripts" 2>/dev/null || true
}

matches="$(scan "$forbidden_named")"
matches+=$'\n'"$(scan "$forbidden_hex")"
matches+=$'\n'"$(scan "$forbidden_wif")"
matches="$(printf '%s\n' "$matches" | sed '/^$/d' || true)"

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

committed_secrets="$(git -C "$ROOT" ls-files | grep -Ei '(^|/)(\.pem|\.key|\.seed|\.keystore|wallet\.json|id_rsa|id_ed25519)$' || true)"
if [[ -n "$committed_secrets" ]]; then
    printf '%s\n' 'ERROR: committed secret-looking artifact detected:' >&2
    printf '%s\n' "$committed_secrets" >&2
    exit 1
fi

printf '%s\n' 'wallet literal policy: OK'
