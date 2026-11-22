# 0021 — Création de fichiers .torrent

Statut : **implémenté**.

## Besoin
Pouvoir produire un `.torrent` à partir de contenu que l'on possède, pour le partager (et, avec le cross-seed, le publier sur
plusieurs trackers). Les données sont **sur les nœuds**, pas sur le master : le calcul se fait donc sur le nœud.

## Décisions
1. **On ne réimplémente pas BitTorrent** : on fabrique seulement les métadonnées (parcours des fichiers, empreintes SHA-1 des
   pièces, bencode) qu'un client existant lira. Module partagé `proto::create`, aussi utilisé par la CLI.
2. **Deux usages** :
   - **Interface** (page Torrents, « Créer un torrent ») : choix d'un nœud en ligne, navigation dans ses dossiers, options, progression,
     téléchargement du `.torrent` et lien magnet, et, si on le souhaite, **ajout au nœud** pour qu'il vérifie les fichiers présents et
     partage aussitôt (le torrent devient un torrent géré, donc répliquable et cross-seedable).
   - **CLI sans master** : `agent create-torrent <chemin> -o fichier.torrent --tracker URL [--private] [--source S] …` (chaque
     `--tracker` forme un palier ; sortie en 0600 car l'adresse d'un tracker privé contient un passkey ; pas d'écrasement sans `--force`).
3. **Commandes fermées** (protocole) : `ListDir`, `CreateTorrent`, `CancelJob`, avec leurs réponses (`DirListing`, `CreateProgress`,
   `TorrentCreated`). Aucun chemin absolu, **aucune URL** : les adresses de trackers sont des clés de premier niveau HORS du dictionnaire
   `info` ; le **master** les ajoute après coup (`metainfo::compose`) sans changer l'infohash. Seuls `private` et `source` sont dans `info` :
   scalaires validés, ils changent l'infohash, ce qui distingue un torrent d'un tracker à l'autre pour les mêmes fichiers.
4. **Tout reste sous la racine locale de l'agent** : chemin relatif sans `..`, symlinks canonicalisés sous la racine (`resolve_under`),
   la **racine entière est refusée** (on ne partage pas tout par mégarde), aucun lien symbolique n'est suivi (refusé avec son nom, jamais
   ignoré en silence : le torrent serait incomplet), les noms non UTF-8 sont refusés, un fichier qui **change pendant la lecture** fait
   échouer l'opération, le dossier interne `.nxt-xseed` (liens du cross-seed) est ignoré.
5. **Bornes** : 50 000 fichiers, `.torrent` ≤ 4 Mio (vérifié **avant** de lire les données), pièces de 16 Kio à 16 Mio (puissance de deux ;
   automatique : environ 2 048 pièces), 2 créations en parallèle par nœud et 20 tâches en mémoire côté master. La lecture est annulable
   et s'arrête si le tunnel tombe.
6. **Tâches en mémoire** côté master (une heure, progression chaque seconde, déclarée perdue après 3 minutes sans nouvelle ou à la
   déconnexion du nœud). Le fichier n'est jamais écrit sur le disque du master ni dans la liste (`GET /api/torrent-jobs/{id}/file` seulement,
   `Cache-Control: no-store`, nom de fichier assaini) ; l'assistant libère la tâche à sa fermeture.
7. **Journal** : « création lancée » (admin, nœud) et « torrent créé » (infohash, nœud) ; **jamais** l'adresse d'un tracker ni le chemin.
8. **Pas de renommage** : le nom du torrent est toujours celui du fichier ou du dossier sur le disque (le client cherche les données sous
   ce nom). Un autre nom exigerait des liens (voir l'ADR 0019) ; la CLI accepte `--name` pour un usage hors nœud.
9. **Ajout au nœud** : le torrent est enregistré avec son `.torrent` (donc éligible à la réplication par swarm et au cross-seed), assigné au
   nœud qui détient les fichiers, dans le **dossier parent** du contenu (c'est là que le client cherche).

## Vérifié
- Tests unitaires : parcours, ordre trié déterministe, pièces à cheval sur plusieurs fichiers (recalculées indépendamment), fichier vide,
  `private`/`source`, taille de pièce, noms et chemins hostiles, dossier interne, liens symboliques, progression et annulation ; assemblage
  des trackers sans changer l'infohash et refus des valeurs dangereuses ; listage (dossiers d'abord, troncature) ; table des tâches.
- Intégration avec un vrai agent sur un vrai disque : exploration, création, résultat identique à une création locale (même infohash),
  ajout au nœud, **aucun passkey dans le journal**, erreurs lisibles sans chemin local, nœud hors ligne, annulation, limite de deux tâches.
- API HTTP : session obligatoire, validation (12 cas), 404/409, téléchargement (en-têtes, nom assaini, contenu exact).
- **Vrais clients** (`created_torrents`, `#[ignore]`, Docker) : qBittorrent et Transmission acceptent les torrents créés (dossier avec sous-dossier
  et fichier vide, fichier seul, privé), les vérifient à **100 % sans téléchargement**, avec le bon nom et la bonne taille ; même infohash des deux côtés.
- Navigateur (`ui/e2e/create.mjs`) : navigation, lien symbolique non sélectionnable, validations avant l'envoi, paliers de trackers, progression,
  résultat, échec, annulation, mobile.

## Non vérifié et limites
- Le test des liens symboliques ne s'exécute pas sous Windows sans droit de création de liens (il tourne en CI Linux).
- Pas de torrents **BitTorrent v2** ni hybrides (v1 uniquement), pas de fichiers « padding ».
- La progression est celle du hachage ; sur un disque réseau lent, la lecture est le facteur limitant.
- Une tâche en cours est perdue si le master redémarre (état en mémoire) ; relancez-la.
- Pas de création en lot ni de modèles de trackers enregistrés (les adresses ne sont volontairement pas conservées).
- La création fonctionne avec le volume en **lecture seule** (`:ro`) du compose d'exemple : l'agent ne fait que lire.
