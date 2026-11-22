# ADR 0011 — Interface web d'administration (premier jet)

Dossier `ui/` (Svelte 5 + Vite 8, JavaScript), servie par le listener d'administration du master (ADR 0010).

## Choix
1. **Svelte, comme prévu par la spec** : aucune dépendance d'exécution hors Svelte (pas de bibliothèque de composants ni de CSS tierce). Bundle : ~74 Ko de JS (28 Ko compressé) et ~8 Ko de CSS.
2. **Embarquée dans le binaire** (`rust-embed`, `ui/dist`) : un seul fichier à déployer, aucun Node en production. `build.rs` crée un `ui/dist` vide si l'interface n'est pas construite ; le master sert alors une page de secours explicite au lieu d'échouer (`cargo build` fonctionne sans Node). L'image Docker construit l'interface dans une étape `node:22-alpine` dédiée.
3. **CSP stricte** : `default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'`. Conséquences voulues : aucun script ni style en ligne (la progression utilise `<progress>`, pas de `style=`), pas de CDN, pas de police externe. Vérifié par un test de bout en bout qui écoute les `securitypolicyviolation`.
4. **Cache** : `index.html` en `no-store` ; `/assets/*` (noms avec condensat) en `immutable` un an. Les chemins sans extension (`/torrents`, `/audit`…) renvoient l'application pour que l'actualisation du navigateur fonctionne ; `/api/*` inconnu = 404 JSON ; toute méthode autre que GET/HEAD hors API = 405 ; aucune évasion de chemin (seuls les fichiers embarqués sont servis).
5. **Session** : cookie `HttpOnly` géré par le navigateur, jamais lu par le JavaScript ; l'en-tête anti-CSRF est ajouté par le client d'API ; un `401` ramène à la connexion (session expirée), un `429` affiche le délai d'attente. Un endpoint `GET /api/me` indique si une session existe.
6. **Sobriété** : thème clair/sombre automatique (`prefers-color-scheme`), responsive, `<dialog>` natifs (focus piégé, Échap), rafraîchissement toutes les 5 s sauf onglet masqué. Contrôle `svelte-check` (accessibilité, réactivité) sans avertissement.

## Écrans
- **Connexion / première utilisation** : création du compte (depuis la machine locale uniquement, règle de l'API), indications sur la politique de mot de passe, message dédié si trop d'échecs.
- **Nœuds** : cartes (état, labels, client, espace libre, débit, dernière activité, synchronisation désirée/appliquée), révocation avec confirmation.
- **Torrents** : liste avec état (en attente / assigné), nœuds porteurs (principal / réplica) avec progression, pause/reprise, suppression (avec ou sans fichiers), ajout (magnet ou infohash, sous-dossier, labels requis, groupe de réplication) avec validation côté serveur affichée.
- **Appairage** : labels fixés par l'admin, code affiché une seule fois avec compte à rebours (15 min), bouton copier, commande à lancer ; le code disparaît à l'expiration.
- **Audit** : tableau paginé, filtre par action, événements de sécurité mis en évidence.

## Vérification
`ui/e2e/smoke.mjs` (puppeteer-core + Edge/Chrome) : démarre un vrai master, crée le compte **par l'interface** (mot de passe faible refusé), génère un code, appaire un **vrai agent**, ajoute des torrents (chemin malveillant refusé), pause, audit et filtre, mauvais mot de passe, déconnexion/reconnexion, expiration de session détectée en cours d'usage, aucune erreur JS, aucune violation de CSP ; captures clair/sombre/mobile. Tests Rust : en-têtes et routage statiques (`admin_api.rs`). Job CI `ui`.

## Limites du premier jet
- Pas de changement de mot de passe, ni de gestion des sessions, ni de TOTP/passkey dans l'interface (l'API a le changement de mot de passe).
- Pas de détail d'un nœud (liste de ses torrents), pas de pagination des torrents, pas de recherche.
- Pas de mise à jour en temps réel (interrogation toutes les 5 s) : acceptable à cette échelle ; WebSocket/SSE possible plus tard.
- Textes en français uniquement ; pas de bascule manuelle clair/sombre (suit le système).
