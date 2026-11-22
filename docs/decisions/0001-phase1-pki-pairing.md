# ADR 0001 — PKI intégrée et appairage (phase 1)

## Décisions

1. **Code d'appairage** = `CODE-` + base32(secret 80 bits ‖ SHA-256(CA DER) tronqué à 128 bits). Le code est donc long (~42 car.) mais copiable ; l'empreinte tronquée à 128 bits reste résistante (seconde pré-image).
2. **Stockage du code : SHA-256 du secret**, pas argon2. Le secret est aléatoire à 80 bits, TTL 15 min, usage unique, tentatives limitées par IP : un hachage lent n'apporte rien contre un secret non devinable.
3. **Épinglage au premier contact** : vérificateur rustls personnalisé. Le serveur envoie `leaf + CA` ; l'agent exige que la CA de la chaîne ait l'empreinte du code, puis effectue la validation WebPKI complète (chaîne, dates, SAN/hôte, signature) avec cette CA comme seule ancre. La vérification n'est jamais désactivée.
4. **Un seul listener, client-auth TLS optionnel** : `Pair` doit marcher sans certificat client, `Connect` (phase 2) refusera toute requête sans certificat valide. Alternative écartée : deux ports (surface/config en plus).
5. **Identité** : SAN DNS `node-<uuid>` imposé par le master ; le sujet et les extensions du CSR sont écrasés. La preuve de possession de la clé est vérifiée via la signature du CSR.
6. **Certificat serveur régénéré à chaque démarrage** (1 an) : seule la CA est épinglée, donc rotation transparente. Les SAN viennent de `--san` (obligatoire : le master ne devine pas son nom public).
7. **SQLite via `rusqlite` (bundled)** plutôt que `sqlx` : pas de macros/compilation en ligne, build musl statique trivial, accès simple derrière un `Mutex`. À réévaluer si l'API admin devient très concurrente.
8. **Fournisseur crypto `ring`** (rustls/tonic) plutôt qu'`aws-lc` : cross-compilation musl sans toolchain C/CMake.
9. **`protoc-bin-vendored`** : aucune dépendance système à `protoc`.
10. **`build_transport(false)`** dans tonic-build : le RPC `Connect` entre en collision avec l'helper `connect()` généré ; le canal est de toute façon construit à la main (connecteur TLS épinglé).

## Écarts / limites connus
- Pas encore de permissions 0600 vérifiées sous Windows (ACL ignorées) ; effectif sous Unix et testé sous `cfg(unix)`.
- Le journal d'audit n'a que les événements d'appairage ; l'API de consultation arrivera avec l'API admin.
