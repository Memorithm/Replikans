# Replikans Security Invariants

Replikans treats custody and economic policy as separate trust domains.

## Custody

The repository must not contain hard-coded private keys, secret keys, seed phrases, mnemonics, reward wallets, treasury wallets, or payout wallets.

Network constants such as verified smart-contract addresses and token contract addresses are a different category, but they must be clearly named and documented as network metadata rather than payout destinations.

Production signing backends must expose signing capability without exposing secret key material to the economic engine, agent harness, opportunity providers, or replication logic.

Secrets must be provided at runtime by an approved signer backend. Plaintext secret persistence is not an accepted production backend.

## Economic integrity

External capital injections are not revenue and must never increase realized economic fitness.

Economic ledger entries require an evidence reference. Requiring a reference does not by itself prove that the evidence is authentic; cryptographic and provider-specific evidence verification will be implemented separately.

Replication must remain gated by deterministic economic policy. An LLM or strategy module may propose replication but must not bypass the replication gate.

## Reporting

Any discovered hard-coded custody secret or payout destination is a release blocker and must be reported before merge.
