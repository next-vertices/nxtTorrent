# 0017 — Sauvegarde et restauration du master

## Contexte
Perdre le disque du master = perdre la CA : tous les nœuds devraient être ré-appairés, et l'historique (torrents, nœuds,
comptes, alertes) disparaît.

## Décisions
- **Contenu** : instantané cohérent de la base (`VACUUM INTO`, possible pendant que le master tourne), `ca.crt`, `ca.key`,
  `totp.key` (clé qui déchiffre les secrets TOTP et les URL d'alertes), `webhook_url` s'il existe.
- **Toujours chiffrée** : une sauvegarde contient la clé privée de la CA. Phrase de passe (12 caractères minimum) →
  Argon2id → AES-256-GCM (authentifié : toute altération est détectée). La phrase de passe n'est jamais stockée et ne
  peut pas être récupérée ; l'interface le dit. Testé : l'archive ne contient rien de lisible.
- **Téléchargement depuis l'interface** (Mon compte) : exige le mot de passe actuel ET le code de double authentification
  si elle est activée (une session volée ne doit pas livrer la CA) ; limiteur d'échecs commun ; journalisé (sans secret).
- **En ligne de commande** (automatisable : cron, timer systemd) : `master admin backup --out F` et
  `master admin restore --from F [--force]`, phrase de passe lue dans `MASTER_BACKUP_PASSPHRASE` ou sur l'entrée standard
  (jamais en argument). Sortie en 0600, jamais d'écrasement d'une sauvegarde existante.
- **Restauration sûre** : liste fermée de noms de fichiers (aucune entrée ne peut écrire hors du dossier de données :
  `../`, chemins absolus, noms inconnus, doublons refusés), contrôle d'intégrité de la base AVANT toute écriture, secrets
  en 0600, journaux WAL/SHM périmés supprimés, refus d'écraser des données existantes sans `--force` (master arrêté).

## Limites
- Les agents ne sont pas sauvegardés par le master (leur identité est locale) : après restauration du master, ils se
  reconnectent tels quels, car la CA est la même.
- Pas de rotation automatique ni de chiffrement à clé publique : la phrase de passe est le seul secret.
