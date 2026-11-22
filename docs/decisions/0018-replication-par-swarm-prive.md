# 0018 — Réplication entre nœuds par le protocole BitTorrent (tracker privé du master)

Statut : **implémentée** (remplace la piste « transfert direct » de l'ADR 0013, qui reste reportée).

## Contexte
Jusqu'ici, une « copie » (ADR 0004) ajoutait le torrent en pause puis le revérifiait : elle supposait que les
fichiers étaient déjà présents (stockage partagé, rsync…). Avec des VPS indépendants, la copie restait à 0 %.

Contraintes de l'utilisateur : les nœuds ne contactent **que le master** pour s'indexer, puis tout passe par le
protocole BitTorrent ; la copie doit rester **privée** (pas de DHT, ni de pairs ni de trackers extérieurs) ; une fois
terminée, on doit pouvoir **revenir au torrent d'origine** (cross-seed).

## Décision
Le master héberge un **mini-tracker BitTorrent privé** (`master/src/tracker.rs`, option `--tracker-listen`).

1. **Copie (phase `Transfer`)** : le master envoie le `.torrent` **réécrit** (`proto::metainfo::rewrite_announce`) :
   même dictionnaire `info` donc **même infohash**, mais `announce` = tracker privé, et `announce-list`, `nodes`,
   `url-list`, `httpseeds` supprimés. Le torrent d'origine n'est jamais contacté pendant la copie.
2. **Source** : un nœud qui a le contenu complet reçoit `AddTracker` (nouvelle commande fermée) pour annoncer **aussi**
   au tracker privé, sans toucher à ses trackers d'origine. L'agent revalide l'URL : `http://`, **même hôte que son
   master**, chemin `/announce/…`, ASCII imprimable, sans identifiants, requête ni fragment. Le driver demande une
   annonce immédiate (Transmission n'annonce pas sinon un tracker ajouté après coup — constaté en test).
3. **Rebranchement (phase `Retarget`)** : copie terminée → `Remove` **sans** les fichiers ; puis, quand le client
   a fini de retirer, `Add` de l'**original** en pause, `Recheck`, partage (cycle existant `Adding` → `Rechecking` →
   `Active`). Le nœud partage alors sur les trackers d'origine, comme un cross-seed.

Sans tracker activé, le comportement historique est conservé (stockage partagé : ajout en pause + `Recheck`).

### Torrents ajoutés par magnet
Le master n'a pas leur `.torrent`. Dès qu'un nœud a le contenu complet, le master lui envoie `FetchMetainfo` ; l'agent
exporte le `.torrent` par l'API du client et le renvoie (`Metainfo`). Le master n'accepte la réponse que s'il l'a
**demandée à ce nœud**, que le fichier est valide (≤ 4 Mio) et que son infohash est celui du torrent ; le premier
fichier reçu gagne. Les copies attendent ce fichier. Si le client ne sait pas exporter (échec d'`Ack`), le master
n'attend plus et retombe sur le cycle à stockage partagé.

### Plafond de débit des copies
Réglage du master (`Réplication` dans l'interface, `POST /api/replication`) : plafond de téléchargement **par
torrent** appliqué à la copie pendant `Transfer` (commande fermée `TorrentLimits`). Il disparaît de lui-même au
rebranchement (le torrent est retiré puis ré-ajouté). Les sources ne sont pas bridées.

## Sécurité
- Le tracker écoute sur un port séparé, **HTTP clair** : les clients torrent ne font pas confiance à notre CA. Chaque URL
  d'annonce est liée à **un torrent et un nœud** par un jeton (HMAC-SHA-256 d'un secret dérivé de la clé du master, donc
  sauvegardé avec elle). Un jeton intercepté ne permet d'annoncer que ce torrent-là pour ce nœud-là.
- Un nœud ne reçoit que les **autres nœuds du même torrent**. L'adresse proposée à un pair est celle **vue par le
  master** sur le tunnel, jamais celle déclarée par le client.
- Aucune URL libre dans le protocole : `AddTracker` n'accepte que le tracker du master (hôte imposé par l'agent).
- Bornes : taille de requête, nombre de pairs par torrent, nombre de torrents suivis ; pairs périmés oubliés.

## Limites connues
- Les nœuds doivent pouvoir se joindre entre eux (même réseau, ports du client ouverts). Derrière un NAT sans
  redirection, la copie peut ne pas démarrer ; le master n'est **pas** un relais de données.
- Après le rebranchement, le nœud annonce aux trackers d'origine (et au DHT si le torrent est public). C'est voulu, mais
  un torrent public cesse d'être « privé » à cet instant.
- **Clients** (vérifiés avec de vrais clients Docker, tests `#[ignore]` `replication_proto` et `replication_qbit`) :

  | Client | Copie | Source (`AddTracker`) | Export du `.torrent` (magnet) | Plafond par torrent |
  |---|---|---|---|---|
  | qBittorrent 5 | vérifié | vérifié | vérifié | vérifié |
  | Transmission 4.1.4 | vérifié | vérifié | **non supporté** (l'API ne donne pas le fichier) | vérifié |
  | Deluge | **non testé** | implémenté, **non testé** | non supporté | implémenté, **non testé** |
  | ruTorrent | non vérifié | non supporté | non supporté | non supporté |

  Les torrents magnet dont les nœuds sources sont Transmission, Deluge ou ruTorrent retombent donc sur le cycle à
  stockage partagé ; ajoutez-les par fichier `.torrent` pour la copie entre nœuds.
- qBittorrent valide l'en-tête `Host` : le port du WebUI doit être le même des deux côtés d'une redirection de port.
