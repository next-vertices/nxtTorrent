# 0025 — Notification des nouvelles versions (étape 1 des mises à jour)

**Statut :** accepté (rc.3)

## Contexte

On veut à terme des mises à jour automatiques du master et des agents. Une mise à jour d'agent revient à lui faire
exécuter un nouveau binaire : c'est le geste le plus risqué du produit (jeu de commandes fermé, « un master compromis ne
doit pas pouvoir faire exécuter du code arbitraire aux agents »). Elle exigera une **signature indépendante du master**
(clé de release, `key_id`, manifeste avec expiration et version minimale), prévue aux étapes suivantes.

Cette première étape ne fait que **prévenir** : elle n'installe rien, ne télécharge rien, et n'exige aucune clé.

## Décisions

1. **Désactivée tant que l'administrateur n'a pas dit oui.** Le master ne contacte rien de lui-même. À la première
   connexion, l'interface propose une fois d'activer (« Activer » / « Non merci », le refus est mémorisé côté master).
   Interrupteur et vérification à la main dans « Mon compte ». Activation et désactivation vont au journal d'audit.
2. **Adresse fixe dans le code** (`updates::RELEASES_URL`, API publique des releases GitHub du dépôt) : ni option, ni
   variable d'environnement, ni route d'API ne la change (pas de SSRF). Seul un test peut la remplacer
   (`Core::set_update_source`), d'où l'absence de test de bout en bout du côté navigateur : la logique est couverte par
   `master/tests/admin_api.rs` avec un faux serveur de releases (`testkit::releases`), et l'interface par
   `ui/e2e/update.mjs` (routes `/api/update*` simulées).
3. **Rythme** : au plus une vérification toutes les 12 h ; une heure après un échec ; une vérification à la main au plus
   par minute (le clic vaut accord pour cet appel, sans changer le réglage). Passage de fond toutes les 30 min, sans
   effet si la vérification n'est pas activée.
4. **Rien sur l'installation ne sort** : User-Agent générique `nxtTorrent-update-check` (sans version), aucun cookie ni
   en-tête applicatif. Le master révèle seulement son adresse IP à GitHub, comme toute requête.
5. **Canal** : une pré-version (`-rc.N`) voit pré-versions et versions finales ; une version finale ne voit **que** les
   finales (étiquette `-pre` ou pastille « pre-release » de GitHub). Comparaison semver complète (`rc.3 < rc.10 < 0.1.0`).
6. **La réponse n'est jamais digne de confiance** : taille bornée (1 Mio), 50 entrées lues, délai de 15 s, redirections
   limitées, brouillons et étiquettes aux caractères inattendus ignorés, et **le lien de la release est reconstruit** à
   partir de l'étiquette validée (jamais lu dans la réponse). Lien ouvert avec `rel="noopener noreferrer"`.
7. **Une panne n'a aucune autre conséquence** qu'un message discret dans « Mon compte » : code d'erreur stable
   (`unreachable`, `rate_limited`, `http_error`, `bad_response`) que l'interface traduit ; la dernière version connue est
   conservée. Un réseau fermé fonctionne exactement comme avant.
8. `available` est recalculé à chaque lecture : après une mise à jour du master, le bandeau disparaît sans nouvelle
   vérification. « Ignorer » mémorise la version vue dans le navigateur (`localStorage`), pas côté master.

## Suite prévue (non faite)

Signature des releases (clé dans un secret d'environnement GitHub protégé, copie chiffrée hors ligne), vérification
côté agent, mise à jour d'un agent à la demande, puis par vagues avec retour arrière ; pour le master, sauvegarde
automatique avant mise à jour, toujours déclenchée par un administrateur. Les installations Docker restent gérées par
leur orchestrateur.
