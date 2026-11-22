# ADR 0009 — Images Docker minimales (non-root)

Le Docker n'est pas imposé (principe 6) : le binaire statique reste la voie principale ; l'image est un plus. Fichiers : `Dockerfile`, `.dockerignore`, `deploy/docker/`.

## Décisions
1. **`scratch`** : une image par composant (`--target master` / `--target agent`) contenant uniquement le binaire musl statique, un `/data` et un `/tmp`. Ni shell, ni gestionnaire de paquets, ni libc, ni certificats système (`reqwest` utilise les racines intégrées `webpki-roots`). Tailles : ~15 Mo (master) et ~12 Mo (agent) sur disque. Compilation multi-étapes sur `rust:alpine`, avec profil release `strip` + LTO léger.
2. **Non-root** : `USER 65532:65532` (UID « nonroot » des images distroless). Banc d'essai : lancée avec `--read-only --cap-drop ALL --security-opt no-new-privileges` ; uid 65532, `CapEff=0` et `NoNewPrivs=1` vérifiés dans `/proc`.
3. **Pas de shell -> sonde intégrée** : `nxt-master healthcheck` (connexion TCP locale au listener, `0.0.0.0` joint via la boucle locale) alimente le `HEALTHCHECK`. L'agent n'écoute sur aucun port : pas de healthcheck ; la supervision se fait par le master (nœud online/offline, webhook).
4. **Configuration par variables d'environnement** : `MASTER_SANS` (obligatoire : sans lui `serve` échoue franchement), `MASTER_LISTEN`, `AGENT_ROOT`, `AGENT_CLIENT*`. Volumes : `/data` (CA + base, ou identité + outbox).
5. **La règle « API du client sur la boucle locale » reste vraie en conteneur** : l'agent partage la pile réseau du conteneur du client (`network_mode: "service:<client>"`), donc `127.0.0.1:8080` y désigne le client, qui n'est publié nulle part. On n'assouplit PAS la règle (`http://qbittorrent:8080` serait refusé).
6. **Racine de téléchargement en lecture seule** pour l'agent (`downloads:/downloads:ro`) : il ne fait que résoudre des chemins.
7. **Dossier de données privé** : `ensure_private_dir` (0700) à la création de la CA et à l'appairage, y compris pour un dossier préexistant. Défaut trouvé par le banc d'essai : `COPY --chown` perdait le `chmod 0700` du builder (le dossier était en 0755 ; la clé de CA restait en 0600) ; corrigé dans le Dockerfile ET dans le code, pour que la protection ne dépende pas de l'image.

## Vérification (`deploy/docker/test.sh`, 23 contrôles)
Taille, utilisateur, absence de shell ; healthcheck ; droits de `/data` et des clés (0700/0600, propriété 65532) ; appairage via `docker exec` / `docker run` ; tunnel mTLS établi entre deux conteneurs ; code d'appairage absent des journaux ; **plantage** (SIGKILL envoyé depuis l'espace de PID de l'hôte : `docker kill` est un arrêt « manuel » qui désactive la politique de redémarrage, et le noyau ignore un SIGKILL envoyé au PID 1 depuis son propre espace de PID) -> conteneur relancé avec un nouveau PID et reconnexion de l'agent ; `docker stop` -> code de sortie 0 en moins d'une seconde (SIGTERM géré, le binaire est PID 1).

## Limites
- Pas de publication sur un registre ni de signature d'image (hors périmètre).
- Image `amd64` uniquement testée ; `arm64` nécessite un build musl ARM (non testé).
- Le mot de passe du client passe par une variable d'environnement (visible dans `docker inspect`) ; les secrets Docker (`/run/secrets`) ne sont pas encore lus par l'agent.
