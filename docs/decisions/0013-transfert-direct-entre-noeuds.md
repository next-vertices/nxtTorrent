# 0013 — Transfert de fichiers entre nœuds (PROPOSITION, non implémentée)

Statut : **reporté** (la réplication sans stockage partagé est traitée par l'[ADR 0018](0018-replication-par-swarm-prive.md)). Ce document fixe l'idée, les contraintes et le découpage pour une session future.

## Contexte
La réplication actuelle (ADR 0004) ajoute la copie **en pause**, lance une vérification, puis passe en partage si
les données sont complètes. Elle **ne transfère aucun fichier** : elle suppose qu'ils sont déjà présents au même
chemin relatif sur le nœud cible (stockage partagé, Syncthing, rsync…). Avec plusieurs VPS indépendants, sans
stockage commun, la copie reste en pause à 0 % (vérification qui ne trouve rien).

## Options étudiées
1. **La copie télécharge depuis le swarm** (option par torrent, désactivée par défaut). Rapide à faire (quelques
   heures). Coût : N × la bande passante ; risque avec les trackers privés (une seule IP par passkey, ratio).
2. **Transfert direct de nœud à nœud** (cet ADR). Évite de re-télécharger, ménage le tracker. Chantier lourd.
3. **Outil externe** (Syncthing, rclone, rsync) : robuste, mais à configurer par l'utilisateur, contraire à
   l'objectif « sans configuration ».

Décision : l'option 2 est jugée la bonne cible à long terme ; l'option 1 pourrait la précéder.

## Contrainte d'architecture
Un agent **n'écoute sur aucun port** (principe 2). Un transfert A → B suppose pourtant qu'un des deux accepte une
connexion. Trois façons :
- **Relais par le master** *(recommandé)* : les deux agents se connectent au master (flux sortants, mTLS), qui
  fait suivre les octets. Cohérent avec tous les principes ; coût : le trafic transite par le master (dimensionner
  sa bande passante).
- **Port d'écoute côté agent** : le plus efficace, mais casse le principe 2 et impose une configuration réseau.
- **Traversée de NAT** (hole punching) : complexe et peu fiable sur des VPS aux réseaux variés. Écartée.

## Conception proposée (relais par le master)
- **Protocole** : nouveau flux gRPC `Transfer` (offre, blocs, accusés, reprise par décalage), tailles de blocs et
  de messages bornées, tous champs validés (le master ne fait jamais confiance à l'agent).
- **Jeu de commandes fermé conservé** : `SendFiles{infohash, transfer_id}` / `ReceiveFiles{infohash, transfer_id}`.
  Aucun chemin, aucune URL, aucune commande arbitraire dans le protocole ; les chemins viennent du client
  torrent local de chaque agent.
- **Drivers** : nouvelle méthode `files(infohash)` du trait `TorrentClient` (liste des fichiers relatifs et
  tailles) pour les 4 clients.
- **Agent source** : lit uniquement sous sa racine (canonicalisation + vérification du préfixe), refuse les
  liens symboliques sortants, limite de débit configurable, reprise après coupure.
- **Agent cible** : écrit uniquement sous sa racine, vérifie l'espace disque avant, écrit dans un fichier
  temporaire puis renomme, rejette tout chemin absolu ou contenant `..`, puis déclenche la vérification
  (`Recheck`) existante : **l'intégrité est garantie par les hachages du torrent**, pas par le transfert.
- **Master** : appaire les deux flux (identités = SAN des certificats), plafonne le débit total et le nombre de
  transferts simultanés, journalise début, fin et échec (audit lisible).
- **Scheduler / réconciliation** : nouvelle phase de réplica `Transferring` entre `Adding` et `Rechecking`.
- **Interface** : progression du transfert, erreurs expliquées (disque plein, nœud hors ligne, reprise).

## Tests exigés
Transfert complet entre deux vrais agents, coupure puis reprise, chemin malveillant rejeté des deux côtés,
lien symbolique sortant refusé, disque plein, nœud révoqué en cours de transfert, deux transferts simultanés,
trafic limité.

## Ordre de grandeur
Environ 2 000 à 3 000 lignes de Rust plus les tests (estimation, non mesurée). La partie sécurité des chemins et
les tests de coupure sont les plus coûteux.

## Questions ouvertes
- Dimensionnement et plafond de bande passante du master en mode relais.
- Compression ou non (les fichiers de torrents sont déjà souvent incompressibles).
- Faut-il autoriser le port d'écoute en option pour les déploiements qui le souhaitent ?
- Faut-il réutiliser le canal pour d'autres usages (déplacer un torrent d'un nœud à un autre) ?
