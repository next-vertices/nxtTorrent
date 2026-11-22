# ADR 0008 — Unités systemd (Linux)

Même niveau que le service Windows (ADR 0007) : démarrage au boot, redémarrage après plantage, arrêt propre, isolation. Fichiers : `deploy/systemd/`.

## Décisions
1. **Arrêt sur SIGTERM** (changement de code, pas seulement de configuration) : master et agent s'arrêtaient seulement sur Ctrl+C. systemd, Docker et `kill` envoient SIGTERM ; sans gestion, `systemctl stop` tuait le processus (base SQLite et outbox non fermées proprement, unité marquée « failed »). `shutdown_signal()` attend SIGINT ou SIGTERM ; sortie avec le code 0. Test : `master/tests/sigterm.rs`.
2. **Redémarrage** : `Restart=on-failure`, `RestartSec=5s`, `StartLimitIntervalSec=0` (jamais de blocage après des plantages répétés). Un arrêt demandé n'est pas relancé (vérifié).
3. **Comptes dédiés statiques** (`nxt-master`, `nxt-agent`, via `systemd-sysusers`, sans shell) plutôt que `DynamicUser=` : l'appairage manuel (`sudo -u nxt-agent … pair`) doit écrire dans le dossier d'état AVANT que le service existe, ce que les utilisateurs dynamiques rendent fragile. `install.sh` crée `/var/lib/nxt-*` en 0700.
4. **Configuration par variables d'environnement** (`EnvironmentFile=/etc/nxt/*.env`, 0600 root:root) : aucun secret en argument. systemd lit le fichier en root avant d'abandonner ses privilèges. Le master lit désormais `MASTER_LISTEN` et `MASTER_SANS` (valeurs séparées par des virgules) ; l'agent `AGENT_ROOT`, `AGENT_CLIENT*` comme avant.
5. **Durcissement** : `NoNewPrivileges`, capacités vides (`CapabilityBoundingSet=` et `AmbientCapabilities=` vides), `ProtectSystem=strict`, `ProtectHome` (master : `yes` ; agent : `read-only`, il doit parcourir la racine de téléchargement), `PrivateTmp/Devices`, protections noyau/cgroups/horloge/hostname, `ProtectProc=invisible`, `RestrictAddressFamilies=AF_INET AF_INET6`, `RestrictNamespaces`, `MemoryDenyWriteExecute`, `SystemCallFilter=@system-service` moins `@privileged @resources`. Exposition mesurée par `systemd-analyze security` : **1,2 (master) et 1,3 (agent)**.
6. **L'agent n'a aucun accès en écriture au dossier de téléchargement** (le client torrent écrit, l'agent ne fait que résoudre des chemins) : système de fichiers en lecture seule, seul `/var/lib/nxt-agent` est inscriptible. Si la racine est protégée, un drop-in `SupplementaryGroups=` donne le droit de parcours.
7. Binaires installés sous `/usr/local/bin/nxt-master` et `nxt-agent` (noms non ambigus).

## Vérification (systemd 255 réel, conteneur Ubuntu 24.04, PID 1 = systemd)
`deploy/systemd/test/run-tests.sh` : 33 contrôles — `systemd-analyze verify`, permissions (clé de CA 0600, dossier d'état 0700, env 0600), isolation lue dans `/proc` (NoNewPrivs, CapEff/CapBnd nuls, seccomp, utilisateur, `/home` invisible, FS en lecture seule, agent sans écriture sur la racine), appairage manuel puis tunnel mTLS établi, `kill -9` -> relance avec nouveau PID et reconnexion de l'agent, `systemctl stop` -> `Result=success` + `ExecMainStatus=0` + pas de relance, aucune erreur au journal. Binaires musl vérifiés **static-pie**. Exécuté par la CI (job `systemd`).

## Limites
- Testé sur systemd 255 (Ubuntu 24.04) ; des directives (`ProtectProc`, `ProcSubset`) exigent systemd ≥ 247. Pas testé sur des distributions plus anciennes.
- `IPAddressAllow` non utilisé : le master doit accepter n'importe quel agent, et l'agent joint un master dont l'adresse est une configuration.
- Pas d'unité pour le client torrent : hors périmètre (l'agent peut s'y ordonner via `After=`).
