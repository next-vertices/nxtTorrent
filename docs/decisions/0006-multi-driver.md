# ADR 0006 — Drivers Transmission, Deluge et ruTorrent (en plus de qBittorrent)

La spec initiale plaçait le multi-driver hors POC ; il est désormais demandé. Le trait `TorrentClient` n'a pas changé : chaque driver l'implémente, la fabrique `drivers::build` choisit selon `agent run --client {qbittorrent|transmission|rutorrent|deluge}`.

## Décisions communes
- **Loopback obligatoire** (`client::parse_local_url`) pour tous les drivers : l'agent ne joint jamais l'API d'un client à distance. Les mots de passe ne figurent dans aucun `Debug`/log ; les URL sont retirées des messages d'erreur.
- **Mot de passe par variable d'environnement** `AGENT_CLIENT_PASSWORD` uniquement (jamais en argument de ligne de commande, visible dans `ps`).
- **Chemins** : toujours validés AVANT le driver par `Executor` (relatifs, sans `..`, symlinks canonicalisés). Le driver ne reçoit qu'un chemin déjà sous la racine, et ne renvoie au master que des chemins relatifs (`save_subdir`) ; hors racine -> `"?"`.
- **Idempotence** : ajout d'un doublon, suppression d'un torrent absent = succès, dans les quatre drivers.

## Particularités par client
| | Transmission | Deluge | ruTorrent (rTorrent) |
|---|---|---|---|
| Protocole | JSON `/transmission/rpc` | JSON-RPC `/json` (Deluge 2.x) | XML-RPC via `plugins/httprpc/action.php` |
| Auth | HTTP Basic (optionnelle) + handshake `X-Transmission-Session-Id` (409) | mot de passe seul, cookie `_session_id` ; `web.connect` auto | HTTP Basic (optionnelle) |
| Pause / reprise | `torrent-stop` / `torrent-start-now` | `pause_torrents` / `resume_torrents` | `d.stop` / `d.start` |
| Limites | kB/s (0 = illimité) | KiB/s (-1 = illimité) | kB/s (0 = illimité) |
| Espace disque | `free-space` | `core.get_free_space` | plugin `diskspace` (JSON) |
| `delete_files=true` | oui | oui | **refusé** (erreur explicite) |

- **rTorrent / suppression des fichiers** : rTorrent n'efface pas les données ; le faire passerait par l'exécution d'une commande côté client, ce que l'agent s'interdit (principe « aucune exécution arbitraire »). L'agent répond donc par un `Ack` négatif explicite plutôt que de conserver les fichiers en silence.
- **rTorrent / `save_subdir`** : `d.directory` inclut le nom du torrent pour les multi-fichiers ; le dossier demandé à l'ajout est mémorisé dans un champ personnalisé (`d.custom.set=nxt_save,…`) et relu à la place.
- **rTorrent / injection de commande** : les arguments sont des commandes rTorrent ; tout chemin contenant `,` `"` `'` `{` `}` `$` `\` `;` ou un contrôle est refusé (pas d'échappement).
- **Codec XML-RPC** : crate locale `xmlrpc-lite` (sous-ensemble : chaînes, entiers, booléens, flottants, tableaux, structures ; pas de DTD ni d'entité externe, profondeur bornée) plutôt qu'une dépendance externe.
- **Deluge** : version minimale 2.x (méthodes au pluriel). Le Web UI doit connaître un démon (le cas par défaut).

## Défauts trouvés en validant contre de VRAIS clients (Docker)
- **qBittorrent 5.3** répond **401** (et non 403) sans session, et le login réussi renvoie **204 sans corps** (et non `200 "Ok."`). Le driver ne se ré-authentifiait pas et refusait un login valide. Corrigé ; le faux serveur sait imiter ce comportement.

## Tests
- `agent/tests/common` : parcours de conformité unique (ajout en pause, statuts, resume/pause, recheck, limites, suppression, idempotence), exécuté contre :
  - les **faux serveurs** (`testkit`, sans Docker) : `mock_drivers.rs` ;
  - les **vrais clients** (`testkit/docker/compose.yml`, `#[ignore]`) : `real_clients.rs`. Versions validées : qBittorrent 5.3.0, Transmission 4.1.4, Deluge 2.2.0, ruTorrent/rTorrent 0.16.25.
- Bout en bout master -> agent -> client pour Transmission, Deluge et ruTorrent.
