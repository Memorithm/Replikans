# Étude Laya pour la chaîne de trading

Date : 25 septembre 2026. Verdict : **candidat à qualifier pour des décisions
courtes ; intégration de production non validée**. Aucun modèle ni ordre réel
n'a été exécuté pour cette étude.

Révision du code Laya examinée :
`4066d5d5fbf08b66c6757ddeedbd797bd7655bc0`, version déclarée `0.3.20`.
Base Replikans examinée : PR #42, tête `bc85a513b772cf734f61a5b57c3e5a28735ff58d`.
Les travaux sur les sorties Rust restent un chantier distinct en cours.

## Ce que Laya apporte

Laya produit des réponses structurées à des questions fermées : choix, score
ordinal ou oui/non probabiliste. Il évite la génération token par token. C'est
adapté au classement d'une information ou au choix d'un candidat déjà défini.
Le modèle n'écrit pas une stratégie complète ni les appels successifs de notre
agent actuel : il faut un adaptateur de décision, pas simplement changer le nom
du modèle Ollama.

Les checkpoints anglais et multilingue ont respectivement 421 M et 322 M de
paramètres. Python, HTTP, MCP et ONNX sont proposés ; le chemin GPU accéléré
utilise TileLang et des graphes CUDA. Les métadonnées du package et les fiches
Hugging Face affichent Apache-2.0. Archiver les licences exactes des poids et
dépendances retenus fait partie de la qualification de cette distribution.

Sources : [README](https://github.com/NandhaKishorM/laya/blob/4066d5d5fbf08b66c6757ddeedbd797bd7655bc0/README.md),
[package](https://github.com/NandhaKishorM/laya/blob/4066d5d5fbf08b66c6757ddeedbd797bd7655bc0/pyproject.toml),
[fiches des poids](https://huggingface.co/convaiinnovations/laya).

## Les chiffres de vitesse, avec leur portée

| Mesure publiée | Résultat | Portée |
|---|---:|---|
| Multilingue, T4, une question | 32,8 ms médiane | Benchmark amont, pas une mesure Replikans |
| Multilingue, RTX 4070 Ti SUPER, 72 tokens, une question | 3,209 ms moyenne accélérée, contre 16,349 ms standard | Fichier brut, `predict()` avec tokenisation, après préchauffage |
| Anglais, même GPU et cas | 5,269 ms moyenne accélérée, contre 19,881 ms standard | Même protocole |
| Multilingue, EPYC 9R14 4 cœurs, une question | 193 ms médiane | Autre matériel/protocole ; non interchangeable avec le GPU |

Les tableaux narratifs du dépôt affichent 2,8 et 4,6 ms pour les cas GPU courts.
Les JSON examinés donnent 3,209 et 5,269 ms : conserver les fichiers et la
révision avec chaque affirmation, sans les mélanger. `bench_fast.py` chronomètre
une boucle et divise par le nombre d'itérations, avec synchronisation CUDA. Ce
sont des **moyennes**, sans distribution p95/p99 dans ces JSON. Les mesures
excluent le démarrage à froid, la compilation initiale, le transport vers une
plateforme de trading et ses accusés de réception. Les gains calculés depuis
ces deux lignes brutes sont respectivement 5,09× et 3,77× face au chemin standard.

Sources : [benchmarks](https://github.com/NandhaKishorM/laya/blob/4066d5d5fbf08b66c6757ddeedbd797bd7655bc0/BENCHMARKS.md),
[protocole](https://github.com/NandhaKishorM/laya/blob/4066d5d5fbf08b66c6757ddeedbd797bd7655bc0/benchmarks/bench_fast.py),
[résultat multilingue](https://github.com/NandhaKishorM/laya/blob/4066d5d5fbf08b66c6757ddeedbd797bd7655bc0/benchmarks/results/fast_multilingual_rtx4070.json),
[résultat anglais](https://github.com/NandhaKishorM/laya/blob/4066d5d5fbf08b66c6757ddeedbd797bd7655bc0/benchmarks/results/fast_english_rtx4070.json).
Extraction avec empreintes : [laya-upstream-measurements.json](evidence/laya-upstream-measurements.json).

## Où l'intégrer

| Étape | Responsable proposé | Rôle |
|---|---|---|
| Comprendre l'objectif et préparer les stratégies | Agent de raisonnement, hors chemin rapide | Proposer un mandat et des variantes testables |
| Calculer les indicateurs et candidats | SciRust et règles Replikans | Calculs numériques exacts, données causales et identifiées |
| Interpréter un événement court / classer les candidats | Service Laya résident | Choix parmi quelques identifiants approuvés, avec abstention |
| Dimensionner, vérifier le risque et autoriser | Rust Replikans | Limites opérateur, portefeuille, prix, frais et fraîcheur |
| Soumettre, annuler, réconcilier et protéger | Runtime Rust et adaptateur de plateforme | Idempotence, sorties indépendantes du modèle, reçus durables |

Premier usage proposé : classifier les événements textuels en parallèle du flux
de marché, puis comparer ses choix à ceux de la stratégie sans lui donner de
droit d'exécution. Pour des signaux purement numériques déjà exprimés par une
règle, une fonction Rust constitue la référence à battre : ajouter un encodeur
textuel peut seulement augmenter la latence sans améliorer la décision.

Pour sélectionner une stratégie, le contrat d'entrée devrait contenir
`request_id`, `snapshot_id`, `candidate_set_hash`, les seuls candidats autorisés,
une version de schéma et une échéance. La sortie reprend ces identités avec un
`candidate_id` ou `ABSTAIN`, sa distribution et l'identité des poids. Le runtime
vérifie identité, appartenance et échéance. Il construit lui-même prix, quantité
et ordre. Toute réponse tardive, inconnue ou mal formée devient une abstention.
Un timeout de Laya ne retarde jamais les sorties de protection.

Garder un schéma compact et stable, un seul checkpoint choisi lors du déploiement
et un worker préchargé. Précompiler les tailles réellement utilisées. Employer
un appel local persistant ; le MCP reste utile pour l'orchestration, sans être
une étape obligatoire de chaque décision rapide. Le choix CPU/ONNX/GPU doit être
mesuré sur le serveur cible, sans extrapoler les performances CUDA à ONNX.

## Points de code à traiter dans l'adaptateur

- `fast=True` peut conserver le chemin standard en cas d'échec. Utiliser
  `accelerate(strict=True)` et vérifier le mode effectif au démarrage.
- Une saturation mémoire GPU peut provoquer une nouvelle tentative sur CPU.
  Une échéance contrôlée hors du worker doit rejeter sa réponse tardive ; le
  timeout client ne signifie pas que le calcul a cessé. Une file bornée et la
  supervision du worker évitent l'accumulation de décisions devenues obsolètes.
- `confidence` repose, pour certains types, sur l'entropie normalisée ;
  `answer_confidence` correspond à la probabilité maximale. Ne pas appliquer un
  même seuil aux deux. Calibrer sur les données de la tâche et garder une zone
  d'abstention ; aucune de ces valeurs n'est une probabilité de gain financier.
- Les entrées peuvent être tronquées. Vérifier le budget après tokenisation et
  rejeter les paquets dont les informations requises ne tiennent pas. Les
  signaux importants ne doivent pas disparaître silencieusement.
- Les révisions et empreintes de poids sont facultatives par défaut. Les rendre
  explicites, télécharger avant démarrage et journaliser code, poids,
  tokenizer, schéma, dtype et configuration. Aucun téléchargement à la décision.
- Le chemin rapide sérialise ses buffers de graphes avec un verrou. Mesurer
  l'attente sous concurrence ; multiplier les appels ne garantit pas un gain.

Sources de code : [agent.py](https://github.com/NandhaKishorM/laya/blob/4066d5d5fbf08b66c6757ddeedbd797bd7655bc0/laya/agent.py),
[common.py](https://github.com/NandhaKishorM/laya/blob/4066d5d5fbf08b66c6757ddeedbd797bd7655bc0/laya/common.py),
[fast.py](https://github.com/NandhaKishorM/laya/blob/4066d5d5fbf08b66c6757ddeedbd797bd7655bc0/laya/fast.py),
[revisions.py](https://github.com/NandhaKishorM/laya/blob/4066d5d5fbf08b66c6757ddeedbd797bd7655bc0/laya/revisions.py).

## Les autres coûts de notre chaîne

L'analyse du code Replikans relève trois limites actuelles :

1. Le collecteur HTTP public impose au moins 1 seconde entre observations. Il
   qualifie la provenance des cotations paper ; il ne constitue pas un flux
   événementiel de trading à très faible latence.
2. `RustBackend.call()` lance un processus par commande. Une connexion locale
   persistante supprimerait ce démarrage répété, avec un protocole de reprise
   explicite en cas d'interruption.
3. Chaque transaction rejoue l'intégralité du journal. Un service détenteur de
   l'état et des checkpoints vérifiés pourrait réduire ce coût. Il devra
   préserver la validation des séquences, la cohérence entre processus et
   l'écriture durable de l'intention avant l'envoi externe.

La mesure locale jointe compare des lectures `snapshot` à 0, 100 et 400 entrées
synthétiques, avec processus recréé ou conservé. Aucun ordre n'est envoyé : les
intentions sont seulement préparées puis abandonnées. Binaire **debug**, hôte
partagé Linux x86_64, 9 CPU visibles, 5 échauffements puis 20 observations par
cas. Le p95 empirique de ce petit échantillon n'est pas une garantie de service.
La sortie comprend sérialisation et volume croissant des ordres historiques ;
elle n'isole donc pas le seul coût du replay.


| Entrées du journal | Nouveau processus, p50 | Processus conservé, p50 |
|---:|---:|---:|
| 0 | 3.35 ms | 0.14 ms |
| 100 | 18.27 ms | 7.61 ms |
| 400 | 61.17 ms | 32.35 ms |

Reproduction :

```bash
python3 scripts/benchmark-trading-runtime.py /absolute/path/replikan-trading \
  --output /absolute/path/runtime-overhead.json
```

Résultats : [laya-study-runtime-overhead.json](evidence/laya-study-runtime-overhead.json).
Pour la production, mesurer séparément réception du marché, attente, calcul des
indicateurs, inférence, autorisation, écriture durable, envoi, accusé et fill.
Un fill dépend également de la liquidité ; la vitesse de soumission ne suffit pas.

## Qualification et décision d'adoption

L'environnement de cette étude n'expose aucun périphérique CUDA et ne contient
ni PyTorch ni Transformers. **L'inférence Laya n'a pas été reproduite ici.**
Les benchmarks amont inspectés ne constituent pas une qualification de stratégie
crypto ni de revenus. Une sortie structurée peut être fausse, même très confiante.

La prochaine expérience doit comparer : règles seules, agent actuel, Laya
standard, Laya accéléré. Utiliser les mêmes événements horodatés et des périodes
séparées chronologiquement pour entraînement, calibration et test, avec contrôle
des chevauchements des labels. Mesurer erreurs par classe, abstention, changement
d'avis selon l'ordre des options, robustesse aux textes hostiles et gain économique
net des frais, du spread, du slippage et du calcul. Les bénéfices d'un backtest
restent distincts des résultats d'une simulation en temps réel et d'une exécution
financée.

Pour la latence : essais à chaud et à froid, différentes tailles, rafales,
concurrence, perte du GPU et indisponibilité du service. Retenir au moins des
milliers de requêtes représentatives pour étudier les queues de distribution,
avec p50/p95/p99, maximum, débit, taux de timeout et mémoire. Les budgets sont
fixés selon la durée de validité du signal et le matériel choisi, pas à partir
d'une moyenne publicitaire.

**Décision : poursuivre un prototype en observation, avec Laya facultatif.**
L'adoption dépendra d'une amélioration mesurée par rapport aux règles et à l'agent
actuel. En parallèle, terminer la protection indépendante du modèle puis qualifier
un flux de marché et un adaptateur de plateforme persistants. Aucun engagement
de rendement ou de vitesse de bout en bout n'est établi par cette étude.
