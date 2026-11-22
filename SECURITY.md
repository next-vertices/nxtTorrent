# Politique de sécurité

nxtTorrent pilote des clients torrent sur des machines distantes : une faille peut donner un accès à des fichiers ou à des
identifiants. Merci de la signaler avec discrétion.

## Signaler une vulnérabilité

**Ne l'ouvrez pas en « issue » publique.** Utilisez le signalement privé de GitHub :
onglet **Security → Report a vulnerability** de ce dépôt.

Indiquez : la version (`master --version`, `agent --version`), le composant concerné, les étapes pour reproduire et l'impact
que vous imaginez. Ne joignez **jamais** de vrai passkey de tracker, de clé privée, de mot de passe ou de `.torrent` privé :
remplacez-les par des valeurs factices.

Les signalements sont traités au mieux, par une personne, sans délai garanti. Une correction est publiée avec une version,
puis l'avis est rendu public ; les personnes qui le souhaitent sont créditées.

## Versions prises en charge

Seule la **dernière version publiée** reçoit des correctifs (projet en préversion : `0.x`).

## Ce qui est dans le périmètre

- Contournement de l'authentification de l'API d'administration (compte, 2FA, session, protection anti force brute).
- Contournement du tunnel mTLS, usurpation de l'identité d'un nœud, appairage sans code valide.
- Un agent compromis qui pourrait agir hors du jeu de commandes fermé, sortir de sa racine locale (chemins, liens),
  exécuter une commande ou faire tomber le master.
- Fuite de secrets : codes d'appairage, clés privées, mots de passe, passkeys, clés d'API des indexeurs, sauvegardes.
- Injection dans le script d'installation d'agent (il est exécuté en root).
- Faille dans le tracker privé de réplication ou dans la création de fichiers `.torrent`.

## Hors périmètre

- Les failles des clients torrent pilotés (qBittorrent, Transmission, Deluge, ruTorrent) : signalez-les à leurs projets.
- Un master exposé sans proxy TLS alors que la documentation demande de ne pas le faire (`MASTER_ADMIN_LISTEN`).
- Un accès physique à la machine, ou un compte root local déjà compromis.
- Les attaques par déni de service volumétriques contre un port exposé sur Internet.

## Bonnes pratiques de déploiement

L'API d'administration n'écoute que sur `127.0.0.1` par défaut ; ne l'exposez que derrière un proxy TLS. Activez la
double authentification. Conservez les sauvegardes chiffrées (`*.nxtbak`) hors de la machine, avec leur phrase de passe.
Vérifiez ce que vous installez : les versions publiées fournissent un fichier `SHA256SUMS` et une attestation de provenance
(`gh attestation verify <fichier> --repo <utilisateur>/<dépôt>`).
