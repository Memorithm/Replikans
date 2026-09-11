# Audit Replikans (2026-09-11)

Périmètre : dépôt `Memorithm/Replikans@a589aa1` (`feat(trading): local Ollama agent and durable experiment evidence`).
Roadmap lue depuis `origin/agent/ecosystem-roadmap` (non fusionnée dans `main`, conformément à `AGENTS.md`).

## Synthèse

Le cœur économique est sérieux : séparation stratégie / garde / exécution,
journal à preuves, gel en insolvabilité, paper trading durable. Ce n’est pas
encore un agent autonome en production. Les faiblesses principales étaient
l’arithmétique `Money` non bornée sur les opérateurs, une CLI no-op, un
contrôle de réplication sans fenêtre temporelle (REP5), un scanner de secrets
trop étroit, et l’absence de `LICENSE` alors que Cargo déclare MIT.

Aucune clé privée, seed, wallet de récompense ou destination de trésorerie
n’a été trouvée dans l’arbre audité.

## Points forts

- `unsafe` interdit ; Clippy refuse `unwrap` / `expect` / `panic` au workspace.
- `Signer` n’expose pas l’export de secret.
- Le ledger sépare explicitement `CapitalInjection` du PnL réalisé.
- Orchestrateur fail-closed sur IDs d’opportunité dupliqués et sources menteuses.
- Leases d’exécution liées à une décision `Run`, une autorisation active et des preuves.
- Runtime paper : journal SQLite chaîné SHA-256, claim avant appel venue, pas de live.
- CI : fmt, clippy `-D warnings`, tests, scan wallets, démos paper / MCP / agent.

## Problèmes corrigés dans cette passe

1. **Débordement monétaire.** `Add`/`Sub` sur `Money` pouvaient wrapper en
   release. Les opérateurs saturent désormais ; les chemins de politique
   utilisent l’arithmétique checked et rejettent `ArithmeticOverflow`.
2. **Politique de survie invalide.** `SurvivalPolicy::new` refuse une réserve
   critique supérieure à la réserve contrainte, et les réserves négatives.
3. **Réplication one-shot.** `evaluate_replication_with_history` exige une
   fenêtre de fitness réalisée (REP5) avant d’autoriser une réplication.
4. **CLI stub.** `replikan demo|version|money` exerce fitness, ledger et gate
   de réplication sans toucher à la garde.
5. **Scanner wallets.** Couvre hex 64, WIF-like, artefacts commités `.pem`/`.key`.
6. **Licence et docs.** `LICENSE` MIT, README d’architecture, `CONTRIBUTING.md`.

## Risques restants (non corrigés ici)

| Sévérité | Sujet | Détail |
| --- | --- | --- |
| Haute | Preuves non authentifiées | Le ledger exige une *référence* de preuve, pas une vérification crypto. `SECURITY.md` le dit. |
| Haute | Feeds marché | Consensus multi-sources, mais un adaptateur HTTP malveillant ou stale mal classé reste un risque d’action. Fail-closed existe ; la provenance réelle dépend des feeds. |
| Moyenne | MSRV fragmenté | Workspace 1.85 / edition 2024 ; `replikan-trading` impose 1.89 et un git pin SciRust. |
| Moyenne | Opérateurs `Money` | `+`/`-` saturent au lieu d’échouer. Tout nouveau code financier doit utiliser `checked_*`. |
| Moyenne | Scanner imparfait | Un secret dans un commentaire, un tableau d’octets ou un fichier hors motifs passe encore. |
| Moyenne | CLI trading JSON-lines | Surface locale utile, mais pas d’authn du processus appelant. |
| Basse | `replikan` vs `replikan-trading` | Deux binaires, responsabilités différentes, peu découvrables avant le README. |
| Basse | Décision ledger in-memory | Pas de persistance du decision ledger hors cycle appelant. |
| Info | 0 issue / 0 star | Projet jeune ; pas de tracker public des dettes. |

## Invariants vérifiés

- Injection de capital ≠ revenu (`replikan-ledger` tests).
- Parent non `Healthy` ⇒ réplication refusée.
- Insolvable ⇒ `Freeze` avant évaluation d’opportunité.
- Mode `PreserveCapital` / `EssentialOnly` restreint le capital nouveau.
- Paper venue only (`Config.venue == "paper"`).
- Pas de `.env` committé.

## Recommandations suivantes

1. Vérification cryptographique des preuves de ledger (mentionnée dans SECURITY).
2. Persistance append-only du decision ledger, alignée sur le journal trading.
3. Matrice d’adaptateurs (réseau / exchange / mining) avec dry-run obligatoire.
4. Fenêtre de fitness soutenue branchée dans `replikan-cycle` plutôt que seulement exposée en API.
5. Property tests sur `Money` et les gates (overflow, timestamps, preuves vides).
6. Ne pas activer d’adaptateur live tant que le paper runtime n’a pas d’évidence bornée dans le temps.
