# Addendum au handoff « trust layer POC » — à intégrer en cours de route

Date : 2026-09-18. Destinataire : l'agent qui travaille déjà à partir de `docs/HANDOFF-trust-layer-poc.md` (version initiale).

**Ceci ne remplace pas le handoff initial.** Les objectifs, les décisions déjà prises avec Mathieu et le travail déjà fait restent valables. Cet addendum ajoute des exigences issues d'une réflexion menée avec Mathieu sur la pertinence du `trustManifest`. Intègre-les dans ton plan sans repartir de zéro ni jeter ce qui fonctionne.

## 0. Première chose à faire : faire le point

Avant de continuer, rédige pour Mathieu un **état d'avancement court** qui liste :

- ce qui est déjà fait (fichiers, branche, tests qui passent) ;
- ce qui est en cours ;
- pour chaque point A à F ci-dessous : « déjà couvert », « à ajuster » (préciser quoi) ou « à ajouter » ;
- l'ordre dans lequel tu comptes intégrer ces points.

Ne supprime ni ne réécris du code existant tant que ce point n'est pas validé par Mathieu. Préfère les ajustements incrémentaux.

## Rappel : ce que le trustManifest prouve (et ne prouve pas)

- Il relie de façon infalsifiable **une entrée de catalogue à une carte A2A exacte** (via `subject.digest`), sous la signature d'un **garant identifié**, pendant une période de validité.
- Si la carte change d'un octet après validation (par un attaquant, l'hébergeur ou le propriétaire lui-même), le client doit bloquer. Une modification légitime doit être re-garantie, c'est-à-dire recevoir un nouveau manifest.
- Il ne prouve **rien sur le comportement** de l'agent. Il ne protège que **l'appelant** qui découvre un agent. Il **n'authentifie pas l'appelant** auprès de l'agent appelé.
- Le blocage est une **politique côté client**, que le protocole n'impose pas lui-même.

La documentation du POC doit dire tout ça explicitement.

## A. Audit des SDK — obligatoire, y compris de façon rétroactive

Constat préliminaire à confirmer sur les **dernières versions publiées** (pas seulement sur `.build/reference/`) :

- Le SDK A2A Rust (`a2a-lf`, `a2a-client-lf`, `a2a-server-lf`) modélise `AgentCard.signatures` mais ne semble rien vérifier. `create_from_card` ne fait aucun contrôle.
- Le SDK AI Catalog Rust (`Agent-Card/ai-catalog-rust`) se compose de :
  - `ai-catalog` : types ;
  - `ai-catalog-validate` : conformité et niveau ;
  - `ai-catalog-trust` : `analyze_catalog`, `canonicalize_catalog`, `canonicalize_trust_manifest`, `verify_digest`. Son README dit qu'elle **ne fait pas de vérification cryptographique de signature**.
- À vérifier aussi : `Agent-Card/ai-catalog-cli` et les SDK A2A dans les autres langages (Python, JS, Go, Java). Le but est de savoir si une vérification de carte signée existe déjà quelque part, et de s'aligner sur son algorithme, sa canonicalisation et sa découverte de clé (`jku`/`kid`).

Actions :

- Livrable `docs/sdk-capabilities.md` (en anglais) : un tableau « capacité → SDK/version → fourni ? → preuve (fichier/ligne ou doc) ».
- Règle : **réutiliser tout ce que les SDK fournissent**. Au minimum : `verify_digest` et la canonicalisation d'`ai-catalog-trust`, et `ai-catalog-validate` dans les tests. Ne coder soi-même que ce qui manque.
- **Si tu as déjà codé de la vérification, de la canonicalisation JCS ou du calcul d'empreinte**, compare avec les SDK. Remplace par l'appel au SDK quand il fournit la même chose, ou justifie l'écart dans `sdk-capabilities.md`.
- Isole le code maison (vérification JWS du catalogue, du manifest et de la carte) pour qu'il puisse être proposé en **contribution upstream** (`ai-catalog-trust`, SDK A2A Rust).

## B. Deux rôles, deux clés : opérateur ≠ garant de confiance

Si `LocalTrust` utilise aujourd'hui une seule clé pour tout, sépare les rôles :

- **Opérateur** : héberge le catalogue et les cartes, et signe le catalogue (signature racine). Il a sa propre JWKS.
- **Garant de confiance simulé** : signe les `trustManifest` et les attestations. Il a sa propre JWKS et est exposé sous un chemin distinct (par exemple `/trust-provider/.well-known/jwks.json`). Choix du chemin ou d'un domaine séparé : **décision à demander à Mathieu**, avec le chemin séparé par défaut.
- Le client **épingle par configuration** la liste des garants qu'il reconnaît. Un manifest signé par un garant inconnu entraîne un refus.
- L'interface `TrustProvider` du handoff initial doit correspondre au rôle de **garant**, pour qu'un fournisseur externe puisse le remplacer plus tard. Il ne faut toujours pas nommer ce futur fournisseur dans le code.

## C. Politique de confiance par opération

La vérification ne doit pas être « tout ou rien ». Elle applique une politique explicite, configurable et documentée, **avant** l'appel A2A (avant `create_from_card`) :

| Opération | Exigence minimale |
|---|---|
| Agent de démo / `get_availability` mock | intégrité : `sha256(carte) == subject.digest` |
| Envoi de disponibilités réelles | + manifest signé par un garant reconnu, non expiré, `subject.url`/`type` cohérents |
| Réservation (`book`) | + attestation « compte vérifié » émise par le garant |

Chaque refus a un code distinct, loggé et visible sur `/logs` (`card_digest_mismatch`, `manifest_signature_invalid`, `manifest_expired`, `untrusted_guarantor`, `attestation_missing`, `trust_downgrade`…).

## D. Labo de scénarios (preuve pour les mainteneurs)

Servir des catalogues volontairement piégés sous `/lab/<scenario>/.well-known/ai-catalog.json`, avec un vérificateur (test d'intégration et/ou commande) qui produit un rapport. Chaque résultat est visible sur `/logs`.

| Scénario | Attendu |
|---|---|
| Catalogue altéré après signature | refus |
| Carte substituée à la même URL | refus (digest) |
| Entrée rejouée dans un autre catalogue | refus (`subject.url`/identité) |
| Manifest expiré | refus |
| `trustManifest` retiré (rétrogradation) | refus si l'opération exige le niveau 3 |
| Manifest signé par un garant non reconnu | refus |
| Rotation de clé du garant | accepté si la nouvelle clé est publiée correctement |
| Agent révoqué, mais carte encore signée | documenter la **limite de la spec** (pas de révocation) |
| Carte miroir légitime hébergée ailleurs | accepté si le digest correspond |

Les URL du labo doivent être documentées pour que les mainteneurs puissent y pointer **leurs propres clients**.

## E. Authentification de l'appelant (sens entrant)

Le manifest n'authentifie que l'agent appelé. Il faut étudier, puis implémenter ou au moins prototyper, comment l'agent appelé B vérifie que l'appelant A est bien un agent garanti : par exemple une requête signée avec la clé de A (dont la carte est vérifiée via le catalogue), ou un jeton émis par le garant. Il faut documenter ce que les specs A2A et AI Catalog couvrent ou non sur ce point, et ajouter un scénario au labo (« appelant non garanti » → refus).

## F. Livrable d'évaluation

`docs/trust-manifest-evaluation.md` (en anglais) contient :

- une **grille champ par champ** (`identity`, `subject.digest`, `subject.url`, `signature`, `issuedAt`/`expiresAt`, `provenance`, `attestations`, `publisher`, `host.trustManifest`, signature racine) : question couverte, ce que le champ ne prouve pas, clarté de la spec ;
- les **résultats du labo** ;
- les **limites constatées**, par exemple : auto-signature quand l'opérateur est aussi le garant, redondance entre la JWS de la carte et la signature du manifest, absence de révocation, découverte et rotation des clés, sémantique libre des `attestations` et de `trustSchema`, authentification de l'appelant ;
- les **questions et propositions** pour les mainteneurs A2A et AI Catalog.

## Ordre d'intégration recommandé

1. Faire le point pour Mathieu (section 0).
2. Audit des SDK (A), avec mise en conformité du code déjà écrit.
3. Séparation des deux rôles (B).
4. Politique par opération (C).
5. Labo (D), avec affichage sur `/logs`.
6. Authentification de l'appelant (E).
7. Rédaction de l'évaluation (F), mise à jour de README, `trust-layer.md` et `logging.md`.

Les garde-fous du handoff initial restent inchangés : aucun déploiement ni push sans accord de Mathieu, ne jamais afficher `.env`/secrets/tfstate, versions de crates épinglées, et suppression totale du terme « aithos ».
