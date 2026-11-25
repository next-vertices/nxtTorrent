# 0027 — Mise à jour du master depuis l'interface

**Statut :** accepté (rc.4). Linux avec systemd seulement.

## Contexte

Suite des ADR 0025 (notification) et 0026 (signature). Le master tourne sous un compte sans privilège, avec
`ProtectSystem=strict` : il **ne peut pas** remplacer son propre binaire, et c'est voulu.

## Décisions

1. **Jamais silencieuse.** Un administrateur clique « Installer la version X » et redonne son mot de passe (et son code de
   double authentification) ; les échecs sont limités et journalisés. Seule la version annoncée par la dernière
   vérification peut être demandée.
2. **Deux rôles.** Le master (non privilégié) télécharge manifeste, signature et binaire depuis une adresse **fixe dans le
   code**, les vérifie (`proto::release`, version strictement plus récente), prend un **instantané de la base**, dépose le
   tout dans `<data>/update/staged/` puis écrit `<data>/update/request`. Une unité `.path` réveille un service **root**
   (`nxt-master apply-update`).
3. **Le service root ne fait confiance à rien du dépôt** : il lit des fichiers ordinaires de taille bornée (ni lien, ni
   tube), **revérifie** signature, version et empreinte avec ses clés compilées, puis écrit **les octets qu'il a
   vérifiés** (pas de course entre vérification et copie). Un master compromis ne peut installer qu'une release signée et
   plus récente. Il n'écrit jamais dans le dossier de données ; la restauration de la base et l'essai `--version` sont
   exécutés **sous le compte du master**.
4. **Retour arrière automatique** : l'ancien binaire est copié dans `/var/lib/nxt-update/previous`. La nouvelle version
   doit écrire `<data>/update/healthy` après 15 s de fonctionnement avec ses deux écoutes ouvertes ; sans cela au bout de
   120 s, l'ancien binaire **et la base d'avant** sont rétablis (une nouvelle version peut avoir migré le schéma).
   Conséquence assumée : ce qui s'est produit entre l'instantané et le retour arrière (journal, mesures) est perdu.
5. **Activation explicite**, une fois, en root : `nxt-master self-update enable` écrit `nxt-master-update.path`,
   `nxt-master-update.service` et un drop-in `MASTER_SELF_UPDATE=1`, après avoir vérifié que le binaire et son dossier ne
   sont modifiables que par root. `self-update disable` les retire. Sans cela, l'interface explique la commande.
6. **Résultat** écrit par root dans `/var/lib/nxt-update/status.json`, affiché dans « Mon compte » et reporté une fois au
   journal d'audit (`update_installed`, `update_rolled_back`, `update_rejected`, `update_failed`). Les sessions étant en
   mémoire, le redémarrage demande une reconnexion.

## Essais

- `master/tests/self_update.rs` : l'installateur avec un systemd factice (refus, bascule, retour arrière, échecs).
- `master/tests/admin_api.rs` : préparation et demande (signature, altération, mauvaise version, mot de passe).
- `ui/e2e/update.mjs` : l'interface.
- `deploy/systemd/test/self-update.sh` : **sous un vrai systemd** (conteneur), à partir d'une copie du code où seules la
  clé de confiance et les adresses changent : installation réussie, puis version en panne et retour arrière. Non lancé en
  CI (plusieurs minutes, conteneur privilégié) : à lancer avant toute modification de ce mécanisme.

## Limites

- Linux + systemd. Docker : mettre à jour l'image. Windows : manuel pour l'instant. Les **agents** ne sont pas encore
  concernés.
- La première mise à jour réelle ne peut avoir lieu qu'entre **deux releases signées contenant ce mécanisme** (rc.4 →
  rc.5). La rc.4 s'installe à la main.
- Un défaut de l'installateur d'une version se corrige à la main : l'installateur utilisé est celui de la version en place.
- Pas encore de clé de secours (cf. ADR 0026).
