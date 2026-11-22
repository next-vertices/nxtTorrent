# ADR 0004 — Réconciliation, scheduler et réplication (phase 4)

## Modèle
- **État désiré** (SQLite, master) : `torrents` (magnet, `save_subdir`, `requires`, `replicate_group`, `size_hint`, pause, complété, supprimé) et `assignments` (torrent × nœud, rôle `primary|replica`, phase, tombstone). `desired_rev` par nœud, croissant, incrémenté à chaque changement de l'état désiré du nœud.
- **État observé** : `FullState` (envoyé à chaque connexion, puis à chaque changement de signature : apparition/disparition, statut, complétion). La progression seule ne déclenche pas de FullState.
- **`reconcile::plan`** (pure) : diff désiré/observé -> commandes manquantes (idempotentes, clé = infohash) + transitions de phase. Le master pousse `DesiredState{rev, commands}` ; l'agent exécute, acquitte chaque commande (`Ack`), puis acquitte la révision via `applied_rev` (uniquement si tout a réussi ; persisté dans `applied_rev`, repris au redémarrage).

## Décisions
1. **Le master ne touche jamais à ce qu'il ne gère pas.** Un torrent présent sur un nœud mais absent de l'état désiré est ignoré. Seul un **tombstone** explicite (suppression admin, réassignation) déclenche `Remove` ; une fois le torrent absent, l'assignation est purgée.
2. **Cycle de réplication** : `Adding` (ajout **en pause**, même `save_subdir`) -> `Rechecking` (`Recheck` envoyé) -> `Active` (resume quand la vérification est terminée et les données complètes). Données incomplètes après vérification : le réplica reste en pause (aucune boucle de recheck). Les transitions de phase ne sont persistées que si la commande est effectivement partie, sinon un réplica resterait bloqué en `Rechecking` sans `Recheck` envoyé.
3. **Scheduler** (`scheduler::pick_node`, pure) : éligible = `Online` ∧ tous les labels `requires` ∧ `disk_free >= size_hint + disk_reserve` ; choix = le moins chargé (nombre d'assignations actives), départage par id. Aucun éligible -> `Pending` (aucune assignation). Jamais de nœud non conforme.
4. **Réplicas** : éligibilité = `Online` + label du groupe cible + espace disque. Les `requires` ne s'appliquent **pas** aux réplicas : ils conditionnent l'acquisition (ex. passer par un VPN) ; un réplica copie et seed. Un nœud qui détient déjà une ligne (même tombstone) pour ce torrent n'est jamais réutilisé automatiquement.
5. **Réassignation** : un primaire `Revoked`, ou `Offline` depuis plus que `offline_grace`, est remplacé par un nœud conforme (le tombstone n'est posé qu'une fois un remplaçant trouvé). Si l'ancien nœud revient, il retire sa copie. Un balayage périodique (`sweep_interval`) couvre aussi les nœuds arrivés en ligne plus tard.
6. **Événements fiables** : l'agent met chaque événement dans une **outbox persistante** (fichier, protobuf hex, réécriture atomique) AVANT envoi, et la **surveillance du client torrent est indépendante du tunnel** : une complétion pendant une coupure est donc détectée puis rejouée. Le master dédoublonne par `event_id` en base (exactement-une-fois pour la réplication) et répond `EventAck` ; l'agent purge alors son outbox. Outbox plafonnée à 10 000 événements. Une complétion survenue pendant l'arrêt du **processus** agent est déduite du `FullState` à la reconnexion (cf. ADR 0005).
7. **Concurrence** : un verrou unique sérialise ordonnancement et réconciliation côté master (sinon deux déclencheurs simultanés pourraient assigner deux fois). Côté driver qBittorrent, les ré-authentifications concurrentes sont sérialisées (génération d'authentification).
8. **Idempotence des Desired** : `cmd_id = r<rev>-<type>-<infohash8>` ; renvoyer un `Desired` déjà appliqué est sans effet.

## Défauts trouvés par les tests et corrigés
- **Login concurrent** : deux requêtes simultanées se ré-authentifiaient en parallèle et invalidaient mutuellement leur session (apparu avec le watcher). -> login sérialisé, une seule ré-authentification par expiration.
- **Signature de FullState** : une vérification terminée sans changement de statut (Paused -> Paused, progression à 100 %) n'était pas signalée ; le réplica ne seedait jamais. -> le critère « complet » entre dans la signature.

## Limites connues
- L'état observé du master est en mémoire (perdu au redémarrage du master, reconstruit au `FullState` de reconnexion de chaque agent).
- Pas encore d'API admin : `Core::add_torrent / remove_torrent / set_torrent_paused` sont l'interface (phase suivante).
- Pas de pondération par taille dans le « moins chargé » (simple compte d'assignations).
