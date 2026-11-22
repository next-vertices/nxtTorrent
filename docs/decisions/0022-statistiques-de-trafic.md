# 0022 — Statistiques de trafic, ratio, torrents inactifs et prévision du disque

## Contexte

Le master ne recevait que des débits instantanés (`dl_rate`, `ul_rate`) et gardait un historique de débits moyens par
minute. Il ne pouvait répondre ni à « combien ai-je envoyé ce mois-ci ? », ni à « quel est mon ratio sur tel tracker ? »,
ni à « quels torrents ne servent plus à rien ? », ni à « dans combien de temps ce disque sera-t-il plein ? ».

## Décisions

### Source : les compteurs cumulés du client

Chaque `TorrentInfo` porte `uploaded`, `downloaded` (octets depuis l'ajout au client) et `added_on` (secondes Unix).
Les quatre drivers les lisent (qBittorrent `uploaded`/`downloaded`/`added_on`, Transmission
`uploadedEver`/`downloadedEver`/`addedDate`, Deluge `total_uploaded`/`all_time_download`/`time_added`, rTorrent
`d.up.total`/`d.down.total`). rTorrent n'expose pas de date d'ajout fiable : `0` signifie « inconnue ».

Ces champs **ne font pas partie de la signature d'activité** de l'agent (`watcher.rs`) : un compteur qui grossit en
permanence provoquerait sinon un `FullState` à chaque seconde. Le volume d'échanges avec le master ne change pas.

### Des volumes déduits de différences, pas des débits intégrés

Intégrer les débits (`ul_rate × durée`) accumule l'erreur d'échantillonnage et rate les pointes. Le master compare à
chaque échantillon (une minute) les compteurs au relevé précédent et ajoute la **différence** au bucket de l'heure
(`traffic_hourly`, par nœud, tracker et indicateur Multi-trackers). Règles (`traffic.rs`, testées) :

- **le premier relevé est une ligne de base** : l'historique d'avant la surveillance n'est jamais attribué à « aujourd'hui » ;
- **un compteur qui recule** (torrent retiré puis rajouté, statistiques remises à zéro) repart de zéro : la valeur
  courante est comptée en entier, jamais une différence négative ;
- **un torrent disparu** perd sa ligne de base ;
- **un nœud dont l'état complet n'a pas été reçu** (redémarrage du master) est ignoré : sa liste est vide et l'effacer
  ferait perdre les volumes de l'intervalle. Les lignes de base sont persistées (`torrent_totals`) et rechargées au
  démarrage : aucun trou, aucun double comptage.

Seules les lignes modifiées sont réécrites (en une transaction) : un parc au repos ne produit aucune écriture.

### Buckets horaires, jours calculés à l'heure de l'utilisateur

Des buckets quotidiens figeraient des jours UTC (le « jour » d'un utilisateur à Paris commence à 1 h ou 2 h) et
interdiraient un vrai « dernier 24 h ». On enregistre donc **par heure** et l'API regroupe par jour selon le fuseau
passé en paramètre (`tz`, minutes à l'est de UTC, borné à UTC-12 / UTC+14). Les jours sans trafic sont renvoyés à zéro
pour que le graphique ne saute pas. Conservation : **400 jours** (un an et de la marge) ; seules les heures avec du
trafic occupent une ligne.

### Ratio

`envoyé / téléchargé`, ou `envoyé / taille` quand rien n'a été téléchargé (torrent ajouté déjà complet) ; absent si la
taille est inconnue. Une ligne regroupée (Multi-trackers) additionne l'envoi de tous les trackers et le rapporte au
dénominateur du torrent d'origine : sinon la copie, qui ne télécharge rien, diluerait le ratio.

### Torrents sans envoi

Un torrent en partage dont le compteur d'envoi n'a pas augmenté depuis N jours. Le master suit la date de la dernière
augmentation (`last_up_ts`) et la date du premier relevé : « aucun envoi **observé** depuis… ». Un torrent récemment
ajouté ne peut donc pas être signalé, et l'interface le dit.

### Prévision de saturation du disque

Un relevé d'espace libre par heure et par nœud en ligne, régression linéaire sur 14 jours. Seuils : au moins 6 relevés
sur 24 h (sinon « historique insuffisant ») ; variation sous 100 Mo/jour = stable. **Une date n'est annoncée que si
l'ajustement est bon (R² ≥ 0,5)** : une occupation en dents de scie (remplissage puis grand ménage) donnerait une date
fausse et rassurante. L'espace total n'est pas remonté par les clients : on prévoit la saturation à partir du libre.

### Gain de Multi-trackers

Les volumes sont marqués `xseed` pour les torrents ajoutés par la fonction : l'envoi obtenu sur les trackers
supplémentaires se mesure sur la période. L'espace épargné est la taille de ces torrents (liens physiques ou fichiers
communs : aucune copie sur le disque).

## Confidentialité

Seul l'**hôte** du tracker est stocké et exposé, jamais l'URL (celle d'un tracker privé contient le passkey), comme pour
la liste des torrents. Les routes `/api/stats/*` exigent une session admin.

## Limites connues

- Les volumes ne sont mesurés que depuis l'activation de la fonction : l'API expose `tracking_since` et l'interface
  prévient quand la période affichée est plus longue que la mesure.
- Un client dont les compteurs sont remis à zéro sans que le torrent soit retiré est vu comme « repart de zéro » ; la
  valeur courante est comptée en entier (légère surestimation possible).
- Un nœud hors ligne n'envoie plus de relevé : ses volumes pendant la coupure sont comptés à la reconnexion (la
  différence entre deux relevés), dans l'heure de la reconnexion et non dans les heures réelles.
- Les valeurs réelles d'envoi n'ont pas été vérifiées contre un vrai essaim : les tests avec de vrais clients vérifient la
  lecture des champs (noms, unités, date d'ajout), pas les volumes échangés.
