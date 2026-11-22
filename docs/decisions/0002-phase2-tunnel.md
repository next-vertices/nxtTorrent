# ADR 0002 — Tunnel, présence et renouvellement (phase 2)

1. **Identité = SAN du certificat client validé par TLS** (`peer_certs`). `Hello.node_id` n'est qu'informatif (écart loggé, jamais utilisé). `Connect` sans certificat ou sans SAN `node-<uuid>` → `Unauthenticated`.
2. **Handler `Connect` non bloquant** : il retourne le flux immédiatement ; Hello (timeout 10 s), vérification d'état et boucle tournent dans une tâche. Les refus applicatifs passent par `Disconnect{reason}` (`revoked`, `unknown_node`, `duplicate_session`, `heartbeat_timeout`, `protocol_error`, `protocol_version`).
3. **Un seul tunnel par nœud** : registre `node_id -> session`. Une nouvelle session ferme l'ancienne (`duplicate_session`) et écrit une alerte d'audit. Côté agent, ce `Disconnect` déclenche une attente au backoff maximal (évite le ping-pong de deux clones) ; les raisons `revoked`/`unknown_node` arrêtent l'agent.
4. **Détection hors ligne** : fermeture du tunnel ou silence > `hb_timeout` (30 s ≈ 3 heartbeats). `Revoked` est terminal, garanti aussi en SQL (`set_state`).
5. **Anti-flapping** : chaque changement incrémente une `epoch` par nœud ; la notification n'est émise que si l'état est inchangé après `confirm_delay` ET diffère du dernier état notifié. La première connexion d'un nœud ne notifie pas. Le trait `Notifier` est en place ; le **webhook arrive en phase 3** avec `reqwest` (déjà requis par le driver qBittorrent), pour ne pas ajouter la dépendance deux fois.
6. **Délai de grâce** : `is_reassignable` est une fonction dérivée de `offline_since` ; rien n'est stocké ni supprimé. Le scheduler (phase 4) l'utilisera.
7. **Renouvellement** : l'agent vérifie l'échéance de son certificat (`renew_check`, 1 h) et envoie `CertRenewReq` quand il reste < 1/3 de la validité ; le master applique la même règle sur le certificat présenté, refuse si le nœud est révoqué, impose à nouveau le SAN. L'agent écrit le nouveau certificat (clé inchangée) puis se reconnecte.
8. **Marge d'horloge** des certificats de nœud : `min(60 s, validité/20)` (60 s en production).
9. **Limites** : messages ≤ 8 Mo, `FullState` ≤ 20 000 torrents, champs texte bornés ; toute violation ferme le tunnel et est auditée.

## Limites connues
- La clé privée de l'agent n'est pas renouvelée avec le certificat (rotation de clé = évolution future).
- Un certificat révoqué reste cryptographiquement valide jusqu'à expiration ; le refus est applicatif (conforme à « pas de CRL »).
