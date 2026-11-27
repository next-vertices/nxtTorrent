# ADR 0010 — API d'administration et audit (point 8 de la spec)

Fichiers : `master/src/admin/` (`api.rs`, `password.rs`, `session.rs`), `master/src/tlsaccept.rs`, tests `master/tests/admin_api.rs`.

## Exposition
1. **Deux listeners séparés** : agents (mTLS obligatoire, `--listen`, 7443) et administration (HTTP/JSON, `--admin-listen`, **127.0.0.1:7080** par défaut).
2. **Adresse non locale refusée au démarrage** (`check_bind`) sauf `--admin-allow-remote` / `MASTER_ADMIN_ALLOW_REMOTE=1`. Ce choix explicite est destiné aux conteneurs (où 127.0.0.1 est inaccessible depuis l'hôte : l'image écoute sur le conteneur et le Compose ne publie le port que sur `127.0.0.1` de l'hôte) ou à un déploiement derrière un proxy TLS. Le cookie reçoit `Secure` seulement quand un proxy indique `X-Forwarded-Proto: https` (le décider d'après la configuration cassait la session en HTTP simple, ex. Docker).
3. L'API n'a pas de TLS propre : la confidentialité du réseau est la responsabilité du proxy ou de la boucle locale.

## Compte et mot de passe
4. **Compte créé au premier démarrage** (`POST /api/setup`) **uniquement depuis la machine locale**, même si l'API est exposée : personne d'autre ne peut s'emparer d'un master neuf. La création est atomique (`INSERT … WHERE NOT EXISTS`) : deux créations simultanées ne peuvent pas réussir toutes les deux (testé). Récupération / provisionnement : `master admin set-password` (accès au dossier de données = confiance ; mot de passe par `MASTER_ADMIN_PASSWORD` ou stdin, jamais en argument).
5. **Politique** : 12 à 128 caractères ; au moins 3 classes parmi minuscules/majuscules/chiffres/symboles (ou 20 caractères et plus : phrase de passe) ; refus si le nom d'utilisateur y figure, si trop répétitif, ou dans une courte liste de mots de passe courants.
6. **Argon2id** (19 Mio, 2 itérations, 1 voie ; sel aléatoire ; format PHC), exécuté hors du runtime asynchrone (`spawn_blocking`). Un compte inconnu coûte le même temps qu'un mauvais mot de passe (hachage factice) et reçoit exactement la même réponse : pas d'énumération.

## Anti brute-force
7. Fenêtre glissante de 15 min : **5 échecs par IP** et **10 par compte** (toutes IP confondues) -> `429` + `Retry-After`, même avec le bon mot de passe pendant le blocage. Le seuil par compte est plus haut que par IP pour qu'un attaquant ne puisse pas verrouiller durablement l'administrateur légitime depuis n'importe où (compromis assumé : il peut le gêner, pas l'empêcher indéfiniment). Une connexion réussie remet les compteurs à zéro. Une seule entrée d'audit « bloqué » par IP et par fenêtre.

## Sessions et CSRF
8. **Sessions bornées** : 1 h d'inactivité (glissante) ET 12 h de durée absolue, en mémoire (un redémarrage déconnecte : voulu). Case « Rester connecté », décochée par défaut : 7 jours d'inactivité et 30 jours au maximum, enregistrée en base (table `admin_sessions`, 20 par compte) pour survivre aux redémarrages et mises à jour ; elle est fermée par la déconnexion, « Déconnecter mes autres appareils », tout changement ou réinitialisation du mot de passe ou de la double authentification (interface ou ligne de commande) et la suppression du compte ; les sauvegardes ne l'emportent pas. Pas de *refresh token* : le master vérifie chaque session et peut la fermer à tout moment, un second secret n'apporterait rien. Limite : un retour arrière de mise à jour restaure la base et peut ranimer une session fermée pendant ces quelques minutes. (Avant la rc.7 : 15 min / 8 h.) Jeton aléatoire 256 bits, seul son condensat SHA-256 est conservé. Cookie `HttpOnly; SameSite=Strict; Path=/; Max-Age`. Le changement de mot de passe ferme toutes les AUTRES sessions du compte.
9. **CSRF** : toute requête qui modifie l'état exige l'en-tête `X-Requested-With: nxt` (un site tiers ne peut pas l'envoyer sans préalable CORS, jamais accordé) ET, si le navigateur fournit `Origin`, une origine identique à `Host`. Le corps doit être du JSON ; 64 Kio maximum. Réponses : `Cache-Control: no-store`, `nosniff`, CSP `default-src 'none'`.

## Fonctions
10. `GET /api/nodes` (état, tunnel, labels, révisions, client, disque, débit), `POST /api/nodes/{id}/revoke`, `POST /api/pairing-codes` (le code n'est montré qu'à cette réponse), `GET|POST /api/torrents`, `POST …/{ih}/pause|resume`, `DELETE …/{ih}?delete_files=`, `GET /api/audit` (pagination `before`, filtre `action`, `limit` borné 1-500), `POST /api/login|logout|password`, `GET /api/health`. Les entrées sont revalidées par la couche métier (magnet, infohash, sous-dossier relatif, labels).

## Audit
11. **Chaque action d'administration est attribuée** (`admin:<utilisateur>`) via des variantes `*_as(actor, …)` du `Core` (les anciennes méthodes restent, acteur `admin`). Événements : création du compte, connexion, échec, blocage, déconnexion, changement de mot de passe, codes d'appairage créés (labels, jamais le code), révocation, torrents ajoutés/retirés/pausés, appairages, refus de commande, doublons de session, etc. L'audit ne contient jamais de code d'appairage ni de mot de passe (testé) ; les valeurs fournies par un client sont nettoyées (contrôles, 64 caractères).
12. **Échecs mTLS** (certificat expiré, CA inconnue, trafic non-TLS) : tonic faisait la poignée de main TLS avant tout code applicatif, donc rien n'était journalisé. Le master fait désormais l'acceptation TLS lui-même (`tlsaccept.rs`, rustls + `tokio-rustls`) et passe les flux validés à tonic avec leurs informations de connexion (IP, chaîne de certificats client) dans les extensions de requête. Une entrée `mtls_failed` (IP + raison) au plus par IP et par minute ; **256 poignées de main simultanées** au plus et délai de 10 s : un scanner ou des connexions muettes ne saturent pas le master (testé : 30 connexions parasites = 1 entrée ; 50 connexions muettes n'empêchent pas un vrai agent de se connecter).

## Hors périmètre / limites
- **Interface web** : premier jet livré, cf. [ADR 0011](0011-web-ui.md).
- **TOTP / passkey** : phase ultérieure, comme prévu par la spec. Un seul rôle (administrateur) ; création de comptes supplémentaires via l'API non prévue.
- Les sessions sont en mémoire : pas de révocation ciblée depuis l'API (la déconnexion et le changement de mot de passe suffisent pour le POC).
- Derrière un proxy, l'IP vue est celle du proxy (pas de confiance accordée à `X-Forwarded-For`) : l'anti brute-force par IP s'y réduit à une limite globale ; le seuil par compte reste efficace.
