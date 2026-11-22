# 0015 — Installation d'un agent en une commande

## Contexte
Public visé : des utilisateurs non experts (VPS, NAS). Installer un agent demandait : copier un binaire, créer un
compte système, écrire une configuration, installer l'unité systemd, appairer, démarrer.

## Décisions
- **Le master génère un script** (`master/assets/install-agent.sh`, gabarit statique) qui embarque un code
  d'appairage à usage unique (15 min). L'interface (Appairage → « Installer sur un serveur Linux ») affiche une commande
  `sudo sh -c "$(echo '<base64>' | base64 -d)"` à coller en SSH, et permet de lire/télécharger le script. Le script est
  passé à `sh -c` (et non via un tube) pour que le terminal reste libre pour la saisie du mot de passe du client.
- **Aucun secret ne transite par la page** : le mot de passe de l'interface du client torrent est demandé sur le serveur
  (`stty -echo`) ou lu dans `NXT_CLIENT_PASSWORD`. Le journal ne garde que « script généré » (nom, labels), jamais le code.
- **Le script s'exécute en root : toute valeur injectée est validée par liste blanche** de caractères, côté serveur,
  avant même la création du code (une requête refusée ne laisse aucun code valide). L'URL du client doit viser la
  machine elle-même (règle « l'API du client reste sur la boucle locale »). Testé : apostrophes, `$()`, backticks,
  `;`, `&`, `|`, retours à la ligne, `..`, chemins relatifs, URL non locales, `127.0.0.1.evil.example`.
- **Binaire de l'agent** : jamais téléchargé sans vérification. Le master peut déclarer une URL https et une empreinte
  SHA-256 (`MASTER_AGENT_BINARY_URL` / `MASTER_AGENT_BINARY_SHA256`) ; le script refuse un binaire dont l'empreinte
  diffère, et refuse une URL sans empreinte. Sans configuration, l'utilisateur fournit le binaire (`NXT_AGENT_BIN`,
  `./nxt-agent`) : le master ne sait pas où le projet publie ses binaires, il ne l'invente pas.
- **Ce que fait le script** : compte système dédié sans shell, binaire en `/usr/local/bin`, état en 0700,
  configuration `/etc/nxt/agent.env` en 0600 root (valeurs échappées pour `EnvironmentFile`), unité systemd durcie
  (la même que `deploy/systemd`), appairage exécuté sous le compte de l'agent (l'empreinte de la CA est dans le code :
  protection contre l'homme du milieu au premier contact), démarrage. Rejouable sans casser l'existant.
- **Test** : `deploy/systemd/test/install-agent-dryrun.sh` exécute le script dans un conteneur Debian avec un agent et
  un `systemctl` factices : droits (0600/0700), échappement d'un mot de passe piégé, appairage sous le bon compte,
  démarrage, idempotence. Le script est aussi vérifié par `sh -n` dans les tests Rust.

## Limites
- Linux + systemd uniquement (Windows : voir le README, service natif). Architecture : celle du binaire fourni.
- Le serveur doit pouvoir joindre le master sur le port des agents (pare-feu) : l'interface le rappelle.
