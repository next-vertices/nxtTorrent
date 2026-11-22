# nxtTorrent (POC)

Orchestrateur de clients torrent multi-instance : un **master** (source de vérité) et des **agents**
(un par machine) reliés par un tunnel gRPC mTLS. Les agents pilotent un client existant —
**qBittorrent, Transmission, Deluge ou ruTorrent** — sans jamais ouvrir de port de contrôle.

## Démarrage rapide

```bash
# Master : CA et base créées au premier lancement (--san = nom/IP sous lequel les agents le joignent)
master --data-dir ./data pair-code --label ssd --label vpn-ch     # code à usage unique (15 min), labels fixés par l'admin
master --data-dir ./data serve --listen 0.0.0.0:7443 --san master.example.org

# Sur chaque nœud : appairage unique, puis tunnel
agent --data-dir ./agent-data pair master.example.org:7443 CODE-XXXX...
AGENT_CLIENT_PASSWORD=... agent --data-dir ./agent-data run --root /downloads --client transmission
```

`--client` : `qbittorrent` (défaut, `:8080`), `transmission` (`:9091`), `deluge` (`:8112`), `rutorrent` (`:8080`).
L'interface du client doit être sur la **boucle locale** (`--client-url` pour un autre port).
Identité perdue ou certificat expiré : `agent pair --force …` avec un nouveau code.

## Interface web

Servie par le master à l'adresse de l'API d'administration (`http://127.0.0.1:7080/` par défaut), embarquée
dans le binaire (design system maison, thème clair/sombre/système, aucune bibliothèque de composants). Pages :
**Vue d'ensemble** (KPIs du cluster, historique de bande passante et de torrents de 1 h à 30 j, points
d'attention), **Torrents** (tableau filtrable/triable/paginé côté serveur pour des milliers de lignes, sélection
groupée, panneau de détail, ajout par fichiers `.torrent` en glisser-déposer ou par liens magnet), **Nœuds**
(nom, labels et localisation modifiables, santé réseau avec diagnostic — port d'écoute joignable ? —, comparaison
du travail de chaque nœud), **Appairage**, **Journal** (événements d'audit en phrases
lisibles, par catégorie et gravité). Les mises à jour arrivent en direct par WebSocket (repli automatique sur
une actualisation toutes les 5 s). Pour la construire (Node 22) :

```bash
cd ui && npm ci && npm run build && cd .. && cargo build --release -p master   # sans cela, une page de secours s'affiche
cd ui && npm run dev                                                           # développement (proxy vers 127.0.0.1:7080)
cargo build -p master -p agent && cargo build -p testkit --example mock_qbit
node ui/e2e/smoke.mjs                                                          # test de bout en bout dans un vrai navigateur (400 torrents simulés)
```

## API d'administration

Listener séparé de celui des agents, sur **127.0.0.1:7080** par défaut (`--admin-listen` / `MASTER_ADMIN_LISTEN`) ;
une adresse non locale est refusée sauf `--admin-allow-remote`. Compte créé au premier démarrage, mot de passe
fort haché en Argon2id, anti brute-force (par IP et par compte), sessions courtes, protection CSRF, audit de
chaque action. Détails : [docs/decisions/0010-admin-api.md](docs/decisions/0010-admin-api.md).

```bash
master admin set-password --username admin                      # (ou POST /api/setup depuis la machine locale)
H='-H X-Requested-With:nxt -H Content-Type:application/json'     # en-tête anti-CSRF + JSON
curl -c jar $H -d '{"username":"admin","password":"..."}' http://127.0.0.1:7080/api/login
curl -b jar http://127.0.0.1:7080/api/nodes                      # nœuds, état, labels, disque...
curl -b jar $H -d '{"name":"NAS du salon","labels":["ssd"]}' http://127.0.0.1:7080/api/pairing-codes   # code d'appairage (affiché une fois) ; nom et labels facultatifs
curl -b jar $H -d '{"name":"Seedbox 2"}' http://127.0.0.1:7080/api/nodes/<id>/rename                  # renommer un nœud
curl -b jar $H -d '{"infohash":"<40 hex>","save_subdir":"films","requires":["vpn-ch"]}' http://127.0.0.1:7080/api/torrents
curl -b jar $H -H 'Content-Type: application/x-bittorrent' --data-binary @film.torrent \
  'http://127.0.0.1:7080/api/torrents/file?save_subdir=films'     # fichier .torrent (4 Mo max ; indispensable aux trackers privés)
curl -b jar 'http://127.0.0.1:7080/api/torrents?q=ubuntu&status=seeding&sort=size&dir=desc&limit=50&offset=0'   # liste paginée/filtrée
curl -b jar $H -d '{"action":"pause","infohashes":["<40 hex>"]}' http://127.0.0.1:7080/api/torrents/bulk       # pause/reprise/suppression groupées
curl -b jar 'http://127.0.0.1:7080/api/torrents?tracker=tracker.exemple.org&sort=peers&dir=desc'   # filtre/tri par tracker, pairs, essaim
curl -b jar 'http://127.0.0.1:7080/api/nodes/stats?range=24h'    # comparaison des nœuds (volumes, débits, disponibilité)
curl -b jar http://127.0.0.1:7080/api/stats                      # KPIs du cluster ; /api/metrics?range=24h : historique
curl -b jar 'http://127.0.0.1:7080/api/audit?limit=50&category=security'   # journal lisible (title, severity, actor_label…)
```

### Installer un agent en une commande

Page « Appairage » → « Installer sur un serveur Linux » : choisissez le client torrent du serveur, copiez la commande
générée et collez-la en SSH. Elle crée un compte système sans privilège, écrit la configuration en 0600, installe le
service systemd durci, appaire l'agent et le démarre ; le mot de passe du client est demandé sur le serveur. Pour que
le script télécharge l'agent lui-même (avec vérification SHA-256), déclarez `MASTER_AGENT_BINARY_URL` et
`MASTER_AGENT_BINARY_SHA256` sur le master ; sinon copiez le binaire et ajoutez `NXT_AGENT_BIN=/chemin/nxt-agent`.
Pour un parc **mixte** (VPS amd64, Raspberry Pi arm64), l'URL contient `{arch}` et l'empreinte est une table
`amd64=<sha>,arm64=<sha>` : `sh deploy/release/master-env.sh SHA256SUMS <URL de la release>` écrit ces deux lignes pour vous.
Détails : [docs/decisions/0015-installation-agent-une-commande.md](docs/decisions/0015-installation-agent-une-commande.md).

### Sauvegarde et restauration

« Mon compte » → Sauvegarde : un fichier **chiffré** (phrase de passe, jamais récupérable) avec la base, la CA et les clés.
En ligne de commande, automatisable : `MASTER_BACKUP_PASSPHRASE=… master --data-dir … admin backup --out master.nxtbak`
puis, master arrêté, `master --data-dir … admin restore --from master.nxtbak [--force]`. Les agents se reconnectent
tels quels (même CA). Détails : [docs/decisions/0017-sauvegarde-restauration.md](docs/decisions/0017-sauvegarde-restauration.md).

### Alertes

Page « Alertes » : ajoutez un sujet **ntfy** ou un **webhook** JSON, puis choisissez quand être prévenu (nœud hors
ligne, port fermé, disque presque plein, torrents en erreur) avec un délai anti-battement ; un message de rétablissement
suit. L'adresse d'un canal est chiffrée et jamais réaffichée. Détails :
[docs/decisions/0016-alertes.md](docs/decisions/0016-alertes.md).

### Double authentification

Page « Mon compte » : TOTP (Aegis, Google Authenticator, 1Password, Bitwarden…) avec 10 codes de secours à usage
unique. Le secret est chiffré au repos (clé `totp.key` en 0600 à côté de `master.db` : **sauvegardez les deux
ensemble**). Téléphone ET codes de secours perdus : `master --data-dir … admin disable-totp --username admin` depuis la
machine du master. Détails : [docs/decisions/0014-double-authentification.md](docs/decisions/0014-double-authentification.md).

## Réplication entre nœuds (sans stockage partagé)

Avec `--tracker-listen 0.0.0.0:7444` (`MASTER_TRACKER_LISTEN`), le master héberge un **tracker BitTorrent privé** :
les copies d'un torrent (groupe cible) se téléchargent **depuis les autres nœuds** par le protocole BitTorrent, puis
repassent sur le torrent d'origine (cross-seed). Les nœuds ne contactent que le master pour s'indexer. Il faut avoir
ajouté le torrent par **fichier `.torrent`**, et que les nœuds puissent se joindre entre eux. Sans cette option, une
copie suppose que les fichiers sont déjà présents (comportement précédent). Voir
[docs/decisions/0018-replication-par-swarm-prive.md](docs/decisions/0018-replication-par-swarm-prive.md).

## Créer un torrent

Page **Torrents → Créer un torrent** : choisissez un nœud en ligne, naviguez dans ses dossiers, indiquez les trackers (un palier
par bloc, une adresse par ligne), privé ou non, puis lancez. **Le nœud lit et hache ses propres fichiers** (le master n'a pas les
données) ; vous téléchargez le `.torrent` et le lien magnet, et pouvez l'**ajouter au nœud** pour qu'il partage aussitôt. Les adresses
de trackers (qui contiennent un passkey) ne sont ni enregistrées ni journalisées ; le fichier n'est conservé que le temps de la fenêtre.
Sans master, la même chose en ligne de commande :

```bash
agent create-torrent /srv/media/Film.2024 -o film.torrent   --tracker https://tracker.exemple.org/announce?passkey=… --private --source MonTracker
```

Voir [docs/decisions/0021-creation-de-torrents.md](docs/decisions/0021-creation-de-torrents.md).

## Cross-seed

Pour un contenu déjà complet sur un nœud, le master peut retrouver le **même contenu chez d'autres indexeurs**
(Prowlarr ou Jackett, Torznab) et l'ajouter sur ce nœud, pointé sur les fichiers existants : plusieurs trackers, un seul
stockage, aucun nouveau téléchargement. Page **Cross-seed** : ajoutez un indexeur (testez-le), laissez le mode
**Simulation** (par défaut) pour voir ce qui serait ajouté, puis passez en **Actif**. L'appariement se fait sur la liste
des fichiers ; un candidat dont la vérification est incomplète est retiré. **Chaque indexeur a ses réglages de prudence**
(délai entre requêtes, budgets par heure et par jour) pour ne pas faire bannir votre compte ; un indexeur qui demande de
ralentir est mis en pause, un accès refusé le désactive automatiquement. Un autre tracker qui nomme le dossier ou
les fichiers autrement est géré par des **liens physiques** créés par l'agent (aucune copie ; l'agent doit pouvoir écrire
dans ses téléchargements, avec le même utilisateur que le client). Dans la liste des torrents, un contenu partagé
sur plusieurs trackers reste **une seule ligne** (dépliable). Voir
[docs/decisions/0019-cross-seed-integre.md](docs/decisions/0019-cross-seed-integre.md).

## Installer une version publiée

Chaque version (étiquette `vX.Y.Z`) publie sur GitHub : `nxt-master-linux-{amd64,arm64}`, `nxt-agent-linux-{amd64,arm64}`
(binaires **statiques**, aucune dépendance), `nxt-{master,agent}-windows-amd64.exe`, un fichier **`SHA256SUMS`** et une
**attestation de provenance** ; et sur GHCR les images `nxt-master` et `nxt-agent` (amd64 et arm64, un seul tag).

```bash
# Vérifier un téléchargement
sha256sum -c SHA256SUMS --ignore-missing
gh attestation verify nxt-agent-linux-amd64 --repo <utilisateur>/<dépôt>     # qui l'a construit, depuis quel commit
# Images
docker pull ghcr.io/<utilisateur>/nxt-master:1.0.0
```

Publier une version : mettre à jour `version` dans le `Cargo.toml` racine, étiqueter `vX.Y.Z` et pousser l'étiquette ; le
workflow refuse une étiquette qui ne correspond pas à la version. Le déclenchement manuel du workflow `release` (sans
« publish ») fait une répétition complète sans rien publier. Détails :
[docs/decisions/0020-distribution.md](docs/decisions/0020-distribution.md).

## Docker

Images minimales (`scratch`, ~15 Mo, binaire statique, utilisateur non-root, sonde de santé) :

```bash
docker build --target master -t nxt-master .
docker build --target agent  -t nxt-agent  .
docker compose -f deploy/docker/compose.example.yml up -d      # master + qBittorrent + agent
```

Essai rapide avec **Transmission** (master + Transmission + agent, tout dans Docker, sans rien installer) :
voir les commandes en tête de [deploy/docker/compose.transmission-test.yml](deploy/docker/compose.transmission-test.yml)
(tableau de bord sur `http://127.0.0.1:17080`).

L'agent partage la pile réseau du conteneur du client (`network_mode: "service:qbittorrent"`) : l'API du
client reste sur la boucle locale et n'est publiée nulle part. Voir [deploy/docker/](deploy/docker/).

## Linux (systemd)

Binaires statiques musl (`cargo build --release --target x86_64-unknown-linux-musl -p master -p agent`).
Unités durcies (comptes dédiés, aucune capacité, système de fichiers en lecture seule, filtre seccomp,
redémarrage après plantage, arrêt propre sur SIGTERM) dans [deploy/systemd/](deploy/systemd/) :

```bash
sudo sh deploy/systemd/install.sh --bin-dir target/x86_64-unknown-linux-musl/release both
sudoedit /etc/nxt/master.env            # MASTER_LISTEN, MASTER_SANS (0600 root:root)
sudo systemctl enable --now nxt-master
sudo -u nxt-master nxt-master --data-dir /var/lib/nxt-master pair-code --label ssd   # code d'appairage

# Sur chaque nœud :
sudo -u nxt-agent nxt-agent --data-dir /var/lib/nxt-agent pair master.example.org:7443 CODE-XXXX...
sudoedit /etc/nxt/agent.env             # AGENT_ROOT, AGENT_CLIENT, AGENT_CLIENT_PASSWORD (0600 root:root)
sudo systemctl enable --now nxt-agent
```

Si la racine de téléchargement est protégée, donner à l'agent le droit de la parcourir (il n'y écrit jamais) :
`systemctl edit nxt-agent` puis `[Service]` / `SupplementaryGroups=<groupe du client torrent>`.

## Windows / Windows Server

Binaires `master.exe` et `agent.exe` (cible `x86_64-pc-windows-msvc`, runtime C lié statiquement :
rien à installer). Mêmes commandes qu'ailleurs ; les clients pris en charge sous Windows sont
qBittorrent, Transmission et Deluge (ruTorrent/rTorrent n'existe que sous Unix).

- Les fichiers secrets (clés, certificats) reçoivent une **ACL restreinte** (SYSTEM, Administrateurs et
  l'utilisateur courant, héritage coupé) via `icacls`, équivalent du `0600` Unix.
- **Service Windows natif** (démarrage automatique différé, compte LocalSystem, redémarrage automatique
  après un plantage, arrêt propre à la demande du gestionnaire de services ou à l'extinction de Windows).
  Dans un terminal **Administrateur** :
  ```powershell
  # Agent : appairer d'abord, puis installer (le mot de passe du client va dans un fichier protégé)
  .\agent.exe --data-dir C:\nxt\agent pair master.example.org:7443 CODE-XXXX...
  $env:AGENT_CLIENT_PASSWORD = "..."
  .\agent.exe --data-dir C:\nxt\agent service install --root D:\Downloads --client qbittorrent
  .\agent.exe --data-dir C:\nxt\agent service start

  # Master
  .\master.exe --data-dir C:\nxt\master service install --san master.example.org --listen 0.0.0.0:7443
  .\master.exe --data-dir C:\nxt\master service start
  New-NetFirewallRule -DisplayName nxt-master -Direction Inbound -Protocol TCP -LocalPort 7443 -Action Allow
  ```
  `service stop|start|status|uninstall` pilotent le service (noms : `nxt-agent`, `nxt-master`). Journaux :
  `agent.log` / `master.log` dans le répertoire de données (rotation à 10 Mo). Aucun secret n'est placé
  dans la configuration du service (lisible par les comptes locaux) : mot de passe du client et URL de
  webhook sont dans des fichiers à ACL restreinte.
- Compte `SYSTEM` : l'agent et le client torrent doivent avoir accès au même dossier de téléchargement.

## Tests

```bash
cargo test --workspace                       # unitaires + intégration (faux clients, sans Docker)

docker compose -f testkit/docker/compose.yml up -d     # vrais clients (qBittorrent, Transmission, Deluge, ruTorrent)
QBIT_PASS=adminadmin cargo test -p agent --test real_clients -- --ignored --test-threads=1
docker compose -f testkit/docker/compose.yml down -v
```

Décisions d'architecture : [docs/decisions/](docs/decisions/).
