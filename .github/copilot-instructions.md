# Replikans repository agent instructions

Before repository changes, fetch and read the persistent off-main roadmap:

```bash
git fetch origin agent/ecosystem-roadmap && \
git show origin/agent/ecosystem-roadmap:.agent/REPLIKANS_ECOSYSTEM_ROADMAP.yaml
```

Treat root `AGENTS.md` as mandatory bootstrap policy. If the roadmap is unavailable, fail closed for major custody, signing, financial-execution, replication-policy, cross-repository integration, or merge decisions.

Never introduce hard-coded private keys, seed phrases, reward wallets, payout destinations, or treasury destinations. Preserve deterministic financial authorization, custody separation, idempotent execution, solvency and realized-outcome accounting.
