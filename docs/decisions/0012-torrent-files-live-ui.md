# 0012 — Fichiers .torrent, tableau de bord en direct, journal lisible

## Contexte
Usage visé : de grosses seedboxes (centaines à milliers de torrents), trackers privés compris. Un lien magnet ne
suffit pas pour un tracker privé (les métadonnées ne se récupèrent pas par le DHT) ; l'interface doit rester
fluide à cette échelle et se mettre à jour sans rechargement ; le journal doit se lire sans connaître les codes
internes.

## Décisions

**Fichiers .torrent.**
- `proto::metainfo` analyse le fichier **côté master avant tout stockage** (bencode écrit à la main, profondeur ≤ 32,
  aucune allocation dimensionnée par une longueur annoncée, 4 Mio max, infohash v1 = SHA-1 du dictionnaire `info`
  tel qu'il apparaît dans le fichier). Les torrents purs v2 sont refusés (leur infohash SHA-256 n'est pas géré).
- Le contenu est stocké en base (`torrents.torrent_file`), jamais dans l'état désiré chargé en mémoire : il n'est
  lu que pour construire une commande `Add`. Le protocole gagne `AddCmd.torrent_file` (`oneof` avec magnet et
  infohash) : le jeu de commandes reste fermé, sans URL ni chemin libre. L'agent ré-analyse le fichier (il ne fait
  pas confiance au master pour l'infohash) et le pousse au client via son API (4 drivers).
- Un `DesiredState` est découpé en messages ≤ 8 Mio ; l'agent accepte jusqu'à 16 Mio par message.
- API : le corps de la requête EST le fichier (`POST /api/torrents/file`, options dans l'URL) : pas de multipart.

**Noms de nœuds.** `nodes.name` (facultatif, ≤ 64 caractères, sans caractère de contrôle), fixé à l'appairage ou
modifié ensuite ; repli sur le nom d'hôte. Purement cosmétique : l'identité reste le SAN du certificat. Les labels
restent facultatifs.

**Liste de torrents à grande échelle.** Filtrage, tri et pagination **côté serveur** (`GET /api/torrents`,
500 lignes max par page) sur une vue qui fusionne l'état désiré (SQLite) et l'état observé (télémétrie). Les
torrents déjà présents sur un nœud mais inconnus du master sont listés (« non géré ») sans jamais être modifiés.
Actions groupées plafonnées à 1000.

**Temps réel.** `GET /api/live` (WebSocket, serveur → navigateur) pousse des instantanés agrégés (KPIs + nœuds)
et un numéro de révision ; le navigateur recharge la page de torrents visible quand la révision change (au plus
toutes les 0,8 s). Garde-fous : session valide + en-tête `Origin` identique à `Host` (sinon un site tiers pourrait
ouvrir le canal avec le cookie de l'admin), messages entrants ignorés et plafonnés à 1 Kio, session revérifiée à
chaque tick. Les rafraîchissements automatiques portent `X-Nxt-Passive` : ils **ne prolongent pas** la session
(ni le WebSocket), l'expiration sur inactivité reste donc effective pour un onglet oublié. Repli automatique sur
une actualisation toutes les 5 s si le WebSocket échoue.

**Historique.** Une ligne par minute (réglable : `MASTER_METRICS_INTERVAL_SECS`) dans `metrics`, conservée 30 jours,
agrégée par tranches à la lecture (~240 points quelle que soit la période).

**Journal lisible.** Le journal reste stocké brut (stable, filtrable). `admin::describe` le traduit à la lecture en
phrase, catégorie et gravité, en remplaçant identifiants par noms de nœuds/torrents actuels ; une action inconnue
reste affichée lisiblement. Le filtre porte sur la version lisible, avec curseur de pagination.

**Interface.** Design system maison (jetons CSS, composants Svelte : Button, Badge, Dialog, Sheet, Menu, Select,
Checkbox, Segmented, Progress, graphiques SVG…) inspiré des conventions courantes mais sans bibliothèque tierce.
Aucun style en ligne (CSP stricte conservée, WebSocket couvert par `connect-src 'self'`).

## Conséquences
- Test e2e navigateur étendu : 400 torrents simulés, WebSocket, envoi de fichiers, sélection groupée, journal.
- Limite connue : le tri/filtre reconstruit la liste complète à chaque requête (O(n), quelques ms pour 10 000
  torrents) ; un index incrémental n'est pas nécessaire à cette échelle.
- Driver Deluge : un Web UI fraîchement démarré répond « Unknown method » (code 2) aux appels non authentifiés ;
  le driver se (re)connecte désormais aussi dans ce cas (constaté contre un vrai Deluge 2.2).

## Addendum — pairs, essaim, trackers, santé réseau, comparaison des nœuds
- **Protocole** : `TorrentInfo` gagne `peers_connected`, `peers_active`, `swarm_seeds`, `swarm_leechers`, `tracker`,
  `message` ; le `Heartbeat` porte un `NodeHealth` (joignabilité du port, port d'écoute, nœuds DHT, connexions).
  Tout est informatif et validé (tailles bornées) côté master.
- **Tracker : l'hôte seul quitte le nœud** (`tracker_host`). L'URL d'un tracker privé contient le passkey ; il n'est
  jamais transmis, stocké, affiché ni journalisé (test dédié sur le driver, test e2e sur l'interface).
- **Couverture par client** : Transmission, qBittorrent et Deluge fournissent pairs, essaim, tracker et santé réseau ;
  rTorrent ne fournit que les pairs connectés (le reste s'affiche « inconnu », jamais inventé).
- **Joignabilité du port sans sonde externe** : déduite d'observations (connexions entrantes vues, statut
  `firewalled` de qBittorrent, `has_incoming_connections` de libtorrent). Sans activité suffisante, « inconnue » :
  on n'alerte pas à tort. Aucun service tiers n'est appelé.
- **Constats** (`master::health`) : port fermé, partage au ralenti (torrents en partage avec pairs en attente qui
  n'envoient rien), téléchargements sans pair. Fonctions pures, testées.
- **Comparaison des nœuds** : un échantillon par nœud et par minute (`node_metrics`, 30 jours) ; volumes estimés
  = somme des débits échantillonnés × période (estimation, pas un compteur exact) ; un nœud est signalé s'il envoie
  moins du quart de la médiane (à partir de 3 nœuds actifs).
- **Localisation** : texte libre saisi par l'admin + dernière adresse source vue par le master. Pas de
  géolocalisation automatique (elle exigerait une base d'adresses embarquée ou un service externe).
- **Défaut corrigé en cours de route** : deux `self.conn()` dans une même expression bloquaient toute la base
  (verrou non réentrant) ; test de non-régression avec délai.

## Addendum — messages d'erreur des clients
- Chaque driver remonte un `message` par torrent : Transmission (`errorString`), Deluge (`message`, état du tracker),
  rTorrent (`d.message`, « Tracker: » = non bloquant), qBittorrent (aucun texte natif : message dérivé de l'état
  `error`/`missingFiles`, et message du tracker lu via `torrents/trackers` — au plus 8 appels par interrogation,
  mis en cache 5 min, uniquement pour les torrents actifs sans tracker qui fonctionne).
- **Aucun texte ne quitte le nœud sans `clean_message`** : toute URL est remplacée par son hôte (le passkey d'un
  tracker privé s'y trouve souvent), contrôles retirés, 300 caractères au plus. Testé sur chaque driver.
- Statut « Erreur » = blocage réel (disque, droits, fichiers). Une erreur ou un avertissement de tracker n'empêche pas
  le client de transférer : le torrent garde son statut et affiche un avertissement.
- L'interface traduit les messages connus (disque plein, droits, fichiers manquants, torrent non enregistré chez le
  tracker, accès refusé/passkey, limite de requêtes, tracker injoignable) ; un message inconnu est affiché tel quel,
  sans interprétation. Table testée par `ui/e2e/explain.mjs`.

## Addendum — revérification et limites de débit par nœud
- **Revérifier** : action admin (menu, panneau, sélection groupée) qui envoie `Recheck` (déjà dans le jeu fermé de
  commandes) aux nœuds connectés qui portent le torrent ; refus clair s'il n'y en a aucun. Journalisé.
- **Limites de débit** : état **voulu** par nœud (`limits_managed`, `dl_limit`, `ul_limit`), envoyé par `SetLimits` dès
  l'enregistrement si le nœud est connecté, puis **à chaque nouvelle session** (le client a pu redémarrer et perdre ses
  limites). Tant que l'admin n'a jamais réglé de limite, le master **ne touche pas** aux réglages locaux du client ;
  « illimité » enregistré = 0/0 explicite. Plafond de saisie 100 Gio/s (faute de frappe).
