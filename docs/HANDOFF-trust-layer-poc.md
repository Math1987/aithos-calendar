# Handoff — POC A2A + AI Catalog niveau 3, sans registre externe

Date : 2026-09-18. À lancer dans un nouveau contexte, sur ce repo (`calendar`, Rust, Lambda + API Gateway + DynamoDB, Terraform, CI GitHub).

## 1. Objectif

Transformer ce repo en **POC A2A autonome et auditable** par les mainteneurs d'A2A et d'AI Catalog :

1. **Supprimer toute dépendance au registre externe actuel** (`registry.aithos.world`, crates `aithos-a2a-card`, `aithos-registry-core`).
2. **Atteindre le niveau 3 « Trusted » d'AI Catalog**, porté par l'app elle-même : cartes A2A signées (JWS) servies sur notre domaine, `trustManifest` signé avec `subject.digest` sur chaque entrée, et signature du catalogue lui-même.
3. **Faire vérifier réellement la confiance par le client de découverte** (signature et empreinte), au lieu de s'appuyer sur une simple liste d'origines autorisées.
4. **Isoler la couche confiance derrière une interface** (trait), pour qu'un fournisseur de confiance externe puisse plus tard remplacer l'implémentation locale sans toucher au code A2A ni au format du catalogue. Ne pas nommer ce futur fournisseur dans le code.
5. **Supprimer le terme « aithos » partout** : code, page web, URN, titres de rendez-vous, docs, logs, noms de propriétés d'événements Google.
6. **Titre des rendez-vous = noms des deux personnes**, par exemple `John Doe / Jane Doe`.
7. **Créer une page publique `/logs`** qui montre ouvertement les logs A2A SDK, les logs de la couche AI Catalog/confiance et les logs applicatifs.
8. **Tout rédiger en anglais** (code, docs, README), clair et auditable : un mainteneur doit comprendre le flux de confiance de bout en bout en lisant le README et un doc dédié.

Hors périmètre : le catalogue racine sur un registre externe, et les catalogues imbriqués inter-apps.

## 2. Références officielles (à relire avant de coder)

- Spec A2A 1.0 : https://a2a-protocol.org/latest/specification/ (§ Agent Card, Agent Card Signing — JWS + JCS, `signatures`, `jku`/`kid`, well-known `/.well-known/agent-card.json`, `GetExtendedAgentCard`)
- Découverte A2A : https://a2a-protocol.org/latest/topics/agent-discovery/ (registres, `Cache-Control`/`ETag`)
- Spec AI Catalog : https://github.com/Agent-Card/ai-catalog (fichier `specification/ai-catalog.md`), https://ai-catalog.io/
- Points clés d'AI Catalog pour le niveau 3 :
  - Media type `application/ai-catalog+json`, well-known `/.well-known/ai-catalog.json`, relation de lien `rel="ai-catalog"`.
  - `trustManifest` : `identity` requis, plus au moins un élément substantiel. Pour le niveau 3 : `signature` (JWS détachée) avec `subject {type, digest:"sha256:<hex>", url}` et `issuedAt`. Éléments optionnels : `expiresAt`, `provenance[]`, `identityType`, `privacyPolicyUrl`, `termsOfServiceUrl`.
  - Algorithmes asymétriques uniquement (ES256 recommandé ici) ; `none` et HS* interdits ; empreinte en SHA-256 minimum.
  - Signature du catalogue : champ `signature` au niveau racine, calculé sur le document canonicalisé JCS sans le champ `signature`.
  - `publisher {identifier, displayName}` sur les entrées ; `host {displayName, identifier, documentationUrl, logoUrl}`.
  - Récupération sûre : HTTPS uniquement, taille bornée, refus des IP privées, loopback et metadata.
  - Identifiant recommandé : `urn:air:{publisher}:{namespace}:{name}`.
- Crates Rust : `ai-catalog` 0.2.1 (déjà utilisée), `ai-catalog-validate` (niveau de conformité, à ajouter aux tests), `ai-catalog-trust` (analyse ; **elle ne fait pas la vérification cryptographique**, c'est à nous de la faire). Source de référence locale : `.build/reference/ai-catalog-rust/`, `.build/reference/a2a-rs/`.
- CLI de test : `.build/tools/a2acli`.

## 3. État actuel (constaté le 18/09)

**Déjà en place et fonctionnel :**
- A2A 1.0 via les crates `a2a-lf` 0.3.1, `a2a-client-lf` 0.2.5 et `a2a-server-lf` 0.4.4. Binding JSONRPC sur `/a2a`, multi-tenant via `tenant`.
- Catalogue AI Catalog servi par `src/lib.rs::catalog` à `/.well-known/ai-catalog.json`. C'est aujourd'hui le **niveau 2** : `host` minimal, pas de `trustManifest`, pas de `publisher`/`version`/`updatedAt`, `Content-Type: application/json`, `cache-control: no-store`.
- Client de découverte : `src/discovery.rs::PeerDirectory` (`account_call`, `availability`). Il ne vérifie **aucune signature** et fait seulement confiance à une liste d'origines (`agent_origin` + `registry_origin`). Le contrôle `spec_version` et de doublons est dupliqué dans `availability()`, tout comme le contrôle de type. Il exige `specVersion == "1.0"` exactement (il faudrait accepter `1.x`).
- Logs : JSON `tracing` vers CloudWatch, avec les cibles `calendar::*`, `a2a_client::middleware` et `a2a_server::middleware`, et des spans `tenant`/`trace_id` (voir `docs/logging.md`, `src/logging.rs`).

**Dépendances au registre externe à retirer :**
- `src/registry.rs` (`Registry`) : génère une clé P-256 par agent (`prepare`, `prepare_signed`), signe la carte (JWS ES256, `kid` = empreinte SHA-256 du JWK en JCS, `jku` = `https://registry…/v1/agents/{kid}/jwks.json`), puis fait un `PUT` de la carte, de la clé et des preuves vers le registre et relit la carte. Il contient aussi les migrations opérateur `upgrade_to_live` et `upgrade_account`.
- Utilisations : `src/identities.rs` (`create`, publication, `registry_id`, `agent_card_url`), `src/auth.rs` (Google sign-in → `prepare`/`publish`), `src/lib.rs`, `src/main.rs` (`REGISTRY_ORIGIN`), `src/discovery.rs` (`new_with_registry`), `src/storage.rs` (`Record.registry_id`, `card_url`, `card_bytes`), et la clé stockée via `store.create(&candidate, &key)` (vérifier comment elle est stockée et chiffrée).
- Crates : `a2a-card = aithos-a2a-card` (fonctions `canonical::{b64url, b64url_decode, canonicalize, signing_input}`, `validate_value`) et `registry-core = aithos-registry-core` (dev-dependency).
- Tests : `tests/identities.rs` et `src/auth/tests.rs` (faux registre avec `registry_core::write::evaluate_write`), `tests/registry_migration.rs`, `src/connected/tests.rs`, et `tests/collaboration.rs` (`/registry/catalog`).
- Infra : `infra/production/api.tf` (`REGISTRY_ORIGIN`).
- Exemples : `examples/upgrade_cards.rs`, `examples/upgrade_account_cards.rs`.

**Occurrences de « aithos » (hors `target/`, `.build/`, tfstate) :**
- Code : `urn:aithos:calendar:agent:` dans `src/agents.rs`, `src/connected.rs`, `src/discovery.rs` et `web/index.html` (validation côté front) ; commentaire dans `src/discovery.rs:100`.
- `src/google_calendar.rs:471-473` : `"summary":"Aithos Calendar meeting"` et `extendedProperties.private.aithosBooking` (la réconciliation des réservations existantes s'appuie peut-être dessus, à vérifier dans `booking.rs`/`booking_store.rs` et `examples/reconcile_booking.rs`).
- `web/index.html` : constantes `API`/`WEBSITE` (`*.calendar.aithos.world`) et messages d'erreur (« Aithos agent link »).
- Infra et CI : domaines `calendar.aithos.world` et `api.calendar.aithos.world` (`infra/production/versions.tf`, `api.tf`, `dns.tf`), buckets `aithos-calendar-*`, compte AWS attendu « aithos-prod » (`scripts/bootstrap.py`), `GOOGLE_OAUTH_TEST_USERS` (e-mails), dépôt GitHub `aithos-calendar` (condition OIDC dans `infra/bootstrap/main.tf`), smoke tests CI.
- Docs : quasiment tous les fichiers `docs/*.md` et `README.md`. `.env` et `.env.example` en contiennent aussi (**ne pas afficher `.env`**, qui contient des secrets).

## 4. Conception cible (proposition à valider, puis à ajuster)

### 4.1 Interface de confiance

Nouveau module, par exemple `src/trust/` :

```rust
#[async_trait]
pub trait TrustProvider: Send + Sync {
    /// Sign an A2A AgentCard (A2A §Agent Card Signing: JWS + JCS) and return the exact published bytes.
    async fn sign_card(&self, card: serde_json::Value, agent_key: &AgentKey) -> Result<SignedCard>;
    /// Build a signed AI Catalog trustManifest for an entry (subject = sha256 of the card bytes).
    async fn manifest_for(&self, entry: &CatalogEntryDraft, card_bytes: &[u8]) -> Result<TrustManifest>;
    /// Sign the whole catalog (JCS without the `signature` field).
    async fn sign_catalog(&self, catalog: &mut AiCatalog) -> Result<()>;
    /// Public keys (JWKS) needed to verify everything above.
    async fn jwks(&self) -> Result<Jwks>;
}
pub trait TrustVerifier { /* verify catalog signature, manifest signature, digest(card)==subject.digest, card JWS */ }
```

- Implémentation POC : `LocalTrust`, qui signe dans le process.
- Un futur fournisseur externe = une autre implémentation, sélectionnée par configuration (`TRUST_PROVIDER=local`), sans le nommer dans le code.
- Remplacer `aithos-a2a-card` par un module local `src/trust/jose.rs` (JCS RFC 8785, b64url, signing input) avec des vecteurs de test. Si une crate JCS maintenue existe, la privilégier et la justifier dans la doc.

### 4.2 Clés

À décider et documenter :

- **Clé de l'opérateur du catalogue** (signe les `trustManifest` et le catalogue). Recommandation : clé KMS asymétrique `ECC_NIST_P256`/`ECDSA_SHA_256`, pour que la clé privée ne quitte jamais KMS. KMS est déjà utilisé, mais seulement pour chiffrer les tokens Google (`src/google_calendar.rs`).
- **Clés par agent** (signent la carte A2A). On garde le modèle actuel, avec une clé P-256 par agent. `jku` pointe désormais vers **notre** domaine, par exemple `https://<api>/agents/{id}/jwks.json`.
- Publication de `/.well-known/jwks.json` (clé opérateur) et de `/agents/{id}/jwks.json`.

### 4.3 Catalogue (serveur)

Le catalogue `/.well-known/ai-catalog.json` doit :

- être servi avec `Content-Type: application/ai-catalog+json` ;
- avoir un `host` complet (`displayName`, `identifier`, `documentationUrl`, `trustManifest` de l'opérateur) ;
- porter, pour chaque entrée : `identifier`, `displayName`, `type: application/a2a-agent-card+json`, `url` (carte sur **notre** domaine), `version` (version de la carte), `updatedAt`, `publisher`, `tags`, `description` et un `trustManifest` signé ;
- avoir une `signature` racine ;
- envoyer un `Cache-Control` raisonnable et un `ETag` (le digest), plus un en-tête `Link: <…/.well-known/ai-catalog.json>; rel="ai-catalog"` sur le site web et `<link rel="ai-catalog">` dans `index.html`.

Contenu du `trustManifest` d'une entrée :

- `identity` ;
- `identityType: "dns"` ;
- `subject {type, digest:"sha256:…", url}` ;
- `issuedAt`, `expiresAt` ;
- `provenance: [{relation:"publishedFrom", sourceId:<identifier>}]` ;
- `signature`.

Contraintes :

- Garder la limite de 64 Kio, sans troncature silencieuse.
- Tester avec `ai-catalog-validate` : le niveau détecté doit être « Trusted ».

### 4.4 Découverte (client)

`PeerDirectory` doit :

1. Récupérer le catalogue et vérifier sa signature avec la JWKS de l'opérateur, épinglée par configuration.
2. Trouver l'entrée et vérifier la signature de son `trustManifest`, ainsi que `subject.type == entry.type` et `subject.url == entry.url`.
3. Récupérer la carte, vérifier que `sha256(bytes) == subject.digest`, puis vérifier la JWS de la carte (A2A).
4. Ensuite seulement, désérialiser la carte et appeler le pair en A2A.

Consignes associées :

- Chaque échec doit avoir un code d'erreur distinct et loggé (`catalog_signature_invalid`, `manifest_signature_invalid`, `card_digest_mismatch`, `card_signature_invalid`, `manifest_expired`…).
- Supprimer les doublons de contrôles, accepter `specVersion` en `1.x` et contrôler l'unicité sur le couple identifier + version.
- Conserver les protections existantes (pas de redirection, timeouts, taille bornée, HTTPS).

### 4.5 Identifiants et migration

- Nouveau schéma d'URN sans « aithos », conforme à la recommandation AI Catalog. Par exemple `urn:air:<domaine-publisher>:a2a-agent:<id>`, en choisissant `<domaine>` une fois le point 5.1 tranché.
- À mettre à jour de façon cohérente : `agents.rs`, `connected.rs`, `discovery.rs`, `web/index.html`, les tests et les scripts.
- Données existantes en production (6 agents publiés, cartes sur le registre externe) : **décider entre migration et purge/recréation** (voir 5.3). Si migration : exemple opérateur idempotent qui re-signe les cartes, met à jour `card_url` et supprime `registry_id`.
- Supprimer `registry_id` de `Record` et des réponses API (`identities.rs:65`).

### 4.6 Rendez-vous

- `summary` = `"<Nom hôte> / <Nom invité>"`, par exemple `John Doe / Jane Doe`.
- Nom de l'hôte : claim `name` du Google sign-in (`src/google_identity.rs`), à conserver dans le record du compte si ce n'est pas déjà le cas.
- Nom de l'invité : **aujourd'hui l'insertion ne reçoit que `guest_email`**. Il faut propager le nom, soit depuis le compte Google de l'invité s'il est connecté, soit via un champ saisi, et le valider (longueur, caractères de contrôle).
- Définir un repli si un nom manque (par exemple la partie locale de l'e-mail, ou « Guest ») et le documenter.
- Renommer `extendedProperties.private.aithosBooking` en un nom neutre (par exemple `a2aBookingId`) **en gardant la lecture de l'ancien nom** si la réconciliation des réservations existantes en dépend.
- Attention : les logs ne doivent **pas** contenir ces noms (voir 4.7).

### 4.7 Page publique `/logs`

Objectif : un auditeur voit en direct ce qui se passe, par catégorie et par `trace_id`.

- **Ne pas exposer CloudWatch brut.** Ajouter un puits dédié : une couche `tracing` qui recopie une **liste blanche** d'événements et de champs vers une table DynamoDB `public_logs` (TTL, par exemple 7 jours ; clé de partition jour/heure ; tri par timestamp).
- Chaque ligne publique a les champs `timestamp`, `level`, `source` (`a2a-sdk` | `ai-catalog` | `trust` | `app`), `target`, `event`/`message`, `method`, `trace_id`, `tenant` (id pseudonyme), `status`/`code`, `duration_ms`.
- **Jamais** d'e-mails, de noms, de tokens, de corps de requête, d'intervalles de calendrier ni d'en-têtes.
- Ajouter des événements explicites pour la couche AI Catalog et confiance : `catalog_served`, `catalog_fetched`, `catalog_signature_verified`, `manifest_verified`, `card_digest_verified`, `card_signature_verified` et leurs échecs.
- API : `GET /logs/events?since=&trace_id=&source=&limit=` (paginée, bornée, cache court). Page : `https://<site>/logs` (statique, dans le même style que `web/index.html`), avec filtres par source et par trace, et rafraîchissement par polling.
- Ajouter des tests qui prouvent la rédaction : un événement contenant un e-mail ou un nom n'arrive jamais dans la table.
- Mettre à jour Terraform (table, IAM en moindre privilège, route API Gateway, et `/logs` sur le site).
- Documenter dans `docs/logging.md` ce qui est public et pourquoi.

### 4.8 Documentation pour l'audit

- Réécrire `README.md` : ce que démontre le POC, le schéma du flux (catalogue → manifest → carte → A2A), comment le vérifier soi-même avec `curl`, `a2acli`, `ai-catalog` CLI/validate, et un lien vers `/logs`.
- Nouveau `docs/trust-layer.md` : les clés, ce qui est signé, par qui et sur quels octets exacts ; l'algorithme de vérification client pas à pas ; la correspondance avec les sections de chaque spec ; les limites connues ; et comment un fournisseur de confiance externe pourrait remplacer `LocalTrust`.
- Nettoyer ou archiver les docs historiques qui citent le registre externe (`docs/dynamic-agents.md`, `public-onboarding.md`, `architecture.md`…).

## 5. Décisions à faire trancher par Mathieu AVANT de coder

1. **Domaines** : `calendar.aithos.world` et `api.calendar.aithos.world` contiennent « aithos ». Faut-il un nouveau domaine (lequel ?), ou garder temporairement le domaine et ne retirer le terme que du contenu ? Même question pour les buckets S3 (renommer = recréer), le compte AWS « aithos-prod » et le dépôt GitHub `aithos-calendar` (condition OIDC).
2. **Clé opérateur** : KMS asymétrique (recommandé) ou clé en Secrets Manager ?
3. **Agents de production existants** : migrer (re-signer) ou purger et recréer ?
4. **Nom de l'invité** pour le titre du RDV : le prendre au Google sign-in de l'invité, le faire saisir, ou les deux ?
5. **Rétention des logs publics** et niveau de détail (7 jours ? `tenant` affiché ou haché ?).

## 6. Plan de travail suggéré (par étapes vérifiables)

1. Lire les specs (section 2) et le code cité (section 3). Faire confirmer les décisions de la section 5.
2. Créer `src/trust/` (JOSE/JCS local, `LocalTrust`, `TrustVerifier`) avec tests unitaires et vecteurs de test. Retirer `aithos-a2a-card`.
3. Remplacer `Registry` par la publication locale des cartes et des JWKS. Supprimer `REGISTRY_ORIGIN` et `registry_id`, et réécrire les tests sans faux registre (retirer `aithos-registry-core`).
4. Passer le catalogue au niveau 3, avec des tests `ai-catalog-validate` et des tests de vérification.
5. Passer la découverte en vérification stricte, avec des tests négatifs (catalogue altéré, manifest altéré, carte substituée, signature expirée).
6. Nouveau schéma d'URN, retrait de « aithos » (code, web, Google `summary`/`extendedProperties`), titre de RDV avec les noms.
7. Pipeline des logs publics, API `/logs/events` et page `/logs`, avec tests de rédaction.
8. Infra Terraform (+ domaine selon la décision 5.1) et CI (smoke tests : vérifier le niveau 3 et la chaîne de confiance de bout en bout contre la prod).
9. Docs (README, `trust-layer.md`, `logging.md`), puis `grep -ri aithos` à zéro hors historique git.
10. Vérification finale : `cargo test`, `cargo clippy`, smoke tests locaux puis production, `a2acli` entre deux agents, et page `/logs` montrant la trace complète d'un `find_common_slot`.

## 7. Garde-fous

- Ne rien déployer en production et ne rien pousser sans accord explicite de Mathieu. Les commits se font sur une branche.
- Ne jamais afficher `.env`, les secrets ou les tfstate dans la conversation.
- Garder les versions de crates épinglées (`=x.y.z`), comme le reste du `Cargo.toml`.
- Ne pas casser les protections existantes (budget Bedrock, autorisation des opérations sur compte, limites de taille, absence de redirections).
