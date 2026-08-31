# Replikans Agent Bootstrap Contract

Before autonomous coding, wallet/custody work, destination/signing changes, financial execution, replication-policy changes, ecosystem integration, PR creation, or merge decisions, read:

```bash
git fetch origin agent/ecosystem-roadmap && \
git show origin/agent/ecosystem-roadmap:.agent/REPLIKANS_ECOSYSTEM_ROADMAP.yaml
```

If the roadmap cannot be fetched or read, fail closed for major custody, signing, financial-execution, replication-policy, cross-repository integration, or merge decisions. Read-only diagnosis and hard-coded-wallet auditing are allowed.

## Mandatory wallet rule

Never introduce hard-coded private keys, seed phrases, reward wallets, payout destinations, or treasury destinations. When touching wallet, payout, mining, treasury, exchange-withdrawal, or signing code, search the changed scope for embedded secrets and destination addresses. If a production hard-coded wallet or secret is found, stop financial-path work and report it explicitly before proceeding.

The current wallet abstraction intentionally exposes public identity and signing capability without private-key export. Preserve that boundary.

## Financial control boundary

Agents may propose strategies. Deterministic Rust policy must authorize financial actions. Strategy, custody, authorization, execution, and realized-result accounting remain separate trust domains. Realized PnL, solvency, survival reserves, risk and post-cost fitness dominate nominal revenue or predicted profit.

SoulSystem may later provide reasoning; ElasticXxx may adapt compute resources; Verify may seal evidence; Hub may orchestrate non-custodial capabilities. None may bypass Replikans financial policy or obtain ordinary access to secret custody material.

Required CI must be green on the exact PR head before merge.

Reread the roadmap at every session start, before wallet/signing/execution/replication changes, before ecosystem integrations, and before relevant PR/merge decisions.

Do not merge the roadmap itself into `main` unless the user explicitly requests it.
