# Mission

Tu vas construire le POC d'un **orchestrateur de clients torrent multi-instance** en Rust, avec une architecture master/agent sécurisée par mTLS. Le produit est destiné à être déployé chez des utilisateurs non experts (VPS, NAS, homelabs simples) : la sécurité et la simplicité d'installation doivent être **par défaut**, sans configuration de PKI, VPN ou reverse proxy par l'utilisateur.

Nom de travail du projet : `nxtTorrent (renommable).

# Principes non négociables

1. **Le master est TOUJOURS la source de vérité.** Il détient l'état désiré ; les agents réconcilient vers cet état.
2. **L'agent se connecte au master, jamais l'inverse.** Un agent n'écoute sur aucun port de contrôle. Le master détecte les nœuds hors ligne via la fermeture du tunnel et un heartbeat applicatif.
3. **On ne réimplémente pas BitTorrent.** L'agent pilote un client existant (qBittorrent d'abord) via son API, derrière un trait `TorrentClient` pour ajouter Transmission / ruTorrent plus tard.
4. **Tout en Rust** (master, agent, proto). L'UI d'admin est en Svelte, servie par le master (assets embarqués dans le binaire).
5. **Un nœud compromis doit avoir un impact minimal** : jeu de commandes fermé, chemins cloisonnés, aucune exécution arbitraire.
6. **Pas de Docker imposé** : livrer un binaire statique (musl) ; une image Docker minimale non-root est un plus, pas une dépendance.

# Architecture

Workspace Cargo :

```
proto/    # messages protobuf (prost + tonic-build) + types partagés (états, enums, erreurs)
agent/    # tunnel, heartbeat, réconciliation locale, drivers (trait TorrentClient)
master/   # CA intégrée, enrôlement, tunnel serveur, scheduler, API admin, UI embarquée
```

Crates recommandés (justifie tout écart) : `tokio`, `tonic`/`prost`, `rustls` + `tokio-rustls`, `rcgen` (CA et émission des certificats), `sqlx` avec SQLite (ou `rusqlite`), `axum` (API admin + UI), `rust-embed`, `reqwest` (API qBittorrent), `clap`, `serde`, `tracing` + `tracing-subscriber`, `thiserror`/`anyhow`, `uuid`, `argon2`.

## 1. PKI intégrée au master

- Au premier lancement, le master génère sa propre CA (clé privée stockée en 0600) et son certificat serveur.
- Chaque agent reçoit un certificat client de courte durée (14 jours), renouvelé automatiquement via le tunnel existant quand il reste moins d'un tiers de validité. Pas de CRL : un nœud révoqué n'obtient simplement plus de renouvellement et son tunnel est fermé.
- L'identité d'un nœud est le SAN du certificat (`node-<uuid>`). Le master ignore toute identité déclarée par l'agent dans les messages.
- L'agent **épingle la CA du master** dès l'appairage.

## 2. Appairage (une seule fois par nœud)

1. Le master génère un code à usage unique `CODE-XXXX` (TTL 15 min, stocké haché) qui encode l'empreinte SHA-256 de sa CA.
2. Commande côté nœud : `agent pair <host:port> <CODE>`.
3. L'agent se connecte en TLS sans authentification client, **vérifie la CA via l'empreinte contenue dans le code** (protège du MITM au premier contact), génère sa clé privée localement et envoie `PairRequest{code, csr, hostname, agent_version, labels}`.
4. Le master valide et invalide le code, signe le certificat (SAN `node-<uuid>`), renvoie `PairResponse{cert, ca_pem, node_id}`.
5. L'agent stocke clé, certificat et CA (fichiers 0600) et bascule en mode normal.

## 3. Tunnel persistant

Un flux gRPC bidirectionnel unique, authentifié par mTLS :

```proto
service Orchestrator {
  rpc Connect(stream AgentMsg) returns (stream MasterMsg);
  rpc Pair(PairRequest) returns (PairResponse);   // seul RPC sans certificat client
}

message AgentMsg { oneof m {
  Hello hello = 1;        // node_id, agent_version, protocol_version, client_kind/version
  FullState state = 2;    // à chaque connexion : torrents, espace disque, applied_rev
  Heartbeat hb = 3;       // toutes les 10 s : stats, applied_rev
  Event event = 4;        // event_id unique : Completed, Error, Added, Removed
  Ack ack = 5;            // cmd_id + résultat
  CertRenewReq renew = 6; // CSR
}}
message MasterMsg { oneof m {
  Welcome welcome = 1;
  DesiredState desired = 2;  // révision + diff ou état complet
  Command cmd = 3;
  CertRenewed renewed = 4;
  Disconnect bye = 5;        // doublon de session, nœud révoqué, etc.
}}
```

**Jeu de commandes fermé** (rien d'autre n'est exécutable) : `Add{magnet|infohash, save_subdir}`, `Pause`, `Resume`, `Recheck`, `Remove{delete_files}`, `SetLimits`.

- Les chemins sont **toujours relatifs** à une racine configurée localement sur l'agent. Rejeter tout chemin absolu, `..`, ou symlink sortant de la racine (canonicaliser et vérifier le préfixe).
- Un seul tunnel actif par `node_id` : un second tunnel avec le même certificat ferme l'ancien (`Disconnect`) et lève une alerte.
- Valider strictement tous les messages entrants côté master (taille max, champs requis) ; un agent compromis ne doit pas pouvoir faire tomber le master.

## 4. Détection hors ligne et machine à états

```
Pending --pair--> Online --2-3 hb manqués / tunnel fermé--> Offline
Online <--reconnexion + FullState-- Offline
Offline --délai de grâce expiré--> Offline(reassignable)   # délai configurable, défaut 15 min
Tout état --révocation admin--> Revoked
```

- Conserver le dernier état connu et un `last_seen`. Ne rien supprimer quand un nœud tombe.
- Notifications sur les transitions `online -> offline` et inverse, après le délai de confirmation (anti-flapping). Interface de notifier extensible (webhook, ntfy) ; implémenter webhook générique en V1.

## 5. Réconciliation

- Le master maintient par nœud une `desired_rev` croissante ; l'agent acquitte `applied_rev` dans chaque heartbeat.
- À la reconnexion, l'agent envoie `FullState` ; le master calcule le diff avec l'état désiré et ne pousse que les commandes manquantes.
- Les commandes sont **idempotentes** (clé = infohash) et donc rejouables sans risque.
- L'agent persiste localement ses événements non acquittés (fichier ou SQLite léger) et les rejoue à la reconnexion ; dédoublonnage par `event_id`.
- Pendant une coupure, le client torrent continue de fonctionner selon le dernier état reçu : le master n'est pas un point de défaillance du transfert.
- Reconnexion de l'agent avec backoff exponentiel + jitter.

## 6. Scheduler (V1 minimal)

- Nœuds : `labels` (ex. `vpn-ch`, `ssd`). Torrents : `requires` (liste de labels).
- Éligibles = nœuds `Online` satisfaisant tous les `requires` avec assez d'espace disque.
- Choix : le moins chargé parmi les éligibles. Aucun éligible -> le torrent reste `Pending` (jamais assigné à un nœud non conforme).
- Règle de réplication : à l'événement `Completed`, ajouter le torrent sur les nœuds du groupe cible (même `save_subdir`, ajout en pause, `Recheck`, puis seed).

## 7. Driver qBittorrent

- Trait `TorrentClient` : `list`, `add`, `pause`, `resume`, `recheck`, `remove`, `set_limits`, `disk_free`, `version`.
- Implémentation qBittorrent via WebUI API v2 (`reqwest`, cookie de session, reconnexion auto).
- L'API du client est supposée liée à `127.0.0.1` ; l'agent n'expose jamais cette API à l'extérieur.
- Mapper les statuts qBittorrent vers un enum interne stable défini dans `proto`.

## 8. API admin et UI (après le cœur du protocole)

- Deux listeners séparés : un pour les agents (mTLS obligatoire), un pour l'UI/API admin, lié à `127.0.0.1` par défaut.
- Auth admin : création du compte au premier démarrage, mot de passe fort (argon2), protection anti brute force, sessions courtes. TOTP/passkey en phase ultérieure.
- Journal d'audit de toutes les actions d'admin et des événements de sécurité (appairage, révocation, échec mTLS, commande refusée).

# Périmètre du POC (dans cet ordre)

**Phase 1 — fondations**
- Workspace Cargo, crate `proto`, CI de base (`cargo fmt --check`, `clippy -D warnings`, `cargo test`).
- CA intégrée, génération des codes d'appairage, `Pair` RPC, émission et stockage des certificats.

**Phase 2 — tunnel**
- `Connect` en mTLS, `Hello`/`Welcome`, heartbeat, détection online/offline, doublon de session, renouvellement de certificat.

**Phase 3 — premier driver**
- Driver qBittorrent : `FullState`, `Add`, `Pause/Resume/Remove`, événement `Completed`.

**Phase 4 — réconciliation, puis scheduler/tags, puis réplication.**

Ne démarre pas la phase suivante avant que la précédente soit testée et fonctionnelle. L'UI Svelte et le multi-driver sont **hors périmètre du POC**.

# Contraintes de qualité

- Rust stable, édition 2021 ou plus récente, `#![forbid(unsafe_code)]` sauf justification explicite.
- Pas de `unwrap()`/`expect()` hors tests et initialisation ; erreurs typées (`thiserror`) aux frontières, `anyhow` dans les binaires.
- Journalisation structurée avec `tracing` ; **ne jamais logger** codes d'appairage, clés privées, mots de passe ou tokens.
- Tests : unitaires sur la validation des chemins, la machine à états, le calcul du diff de réconciliation et le scheduler ; **tests d'intégration** avec un master et un agent réels en local (appairage complet, coupure puis reprise du tunnel, doublon de session, certificat expiré/révoqué refusé, chemin malveillant rejeté).
- Pour qBittorrent, un faux serveur HTTP (mock) pour les tests du driver ; une instance réelle seulement en test optionnel marqué `#[ignore]`.
- Binaires compilables en statique musl (`x86_64-unknown-linux-musl` au minimum).
- Documenter les décisions non triviales dans `docs/decisions/` (court, format ADR).

# Méthode de travail

1. **Commence par explorer le dossier courant.** S'il est vide, initialise le workspace ; sinon, lis le code existant avant de modifier.
2. Avant d'écrire du code, propose un **plan court** de la phase en cours (fichiers, types principaux, risques), puis exécute-le.
3. Avance par petits incréments compilables : à chaque étape, `cargo fmt`, `cargo clippy` et `cargo test` doivent passer.
4. Quand un choix de conception n'est pas couvert ici, tranche selon les principes non négociables, note la décision dans `docs/decisions/` et continue. Ne pose une question que si le choix est irréversible et ambigu.
5. En fin de phase, résume : ce qui fonctionne, ce qui reste, les écarts éventuels avec cette spec.

# Ce que tu ne dois pas faire

- Ne pas écrire de moteur BitTorrent ni de parsing du protocole pair-à-pair.
- Ne pas ouvrir de port d'écoute de contrôle côté agent.
- Ne pas ajouter de commande arbitraire (shell, chemin absolu, URL libre) dans le protocole.
- Ne pas désactiver la vérification TLS, même « temporairement » ou pour les tests : utiliser une CA de test générée à la volée.
- Ne pas stocker de secrets en clair hors des fichiers 0600 prévus.
- Ne pas ajouter de dépendances lourdes ou d'abstractions prématurées au-delà du périmètre du POC.