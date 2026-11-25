# 0026 — Signature des releases (étape 2 des mises à jour)

**Statut :** accepté (rc.3). La vérification est livrée et testée ; **rien ne l'utilise encore** pour installer quoi que ce soit
(c'est l'étape 3).

## Contexte

Une mise à jour d'agent revient à lui faire exécuter un nouveau binaire. Si l'agent ne se fiait qu'au master (« voici le
binaire, son empreinte est X »), un master compromis pourrait faire exécuter du code à **tous les nœuds**. On veut donc une
**signature indépendante du master** : la clé privée n'est jamais sur le master, et l'agent ne fait confiance qu'aux clés
publiques compilées dans son binaire.

## Décisions

1. **Ed25519, signature des octets exacts de `manifest.json`** (publiée à côté : `manifest.json.sig`, 64 octets bruts).
   Ed25519 : petit, déterministe, sans paramètre à mal choisir. Signature faite par `openssl pkeyutl -sign -rawin`
   (présent sur les runners GitHub : aucun outil tiers dans la CI) ; vérification par `ring` (déjà compilé pour rustls :
   aucune brique nouvelle dans le binaire). On authentifie les octets AVANT de les interpréter.
2. **Le manifeste** (`proto::release`) : `purpose` (empêche de rejouer une signature obtenue pour autre chose), `schema`,
   `repository`, `key_id`, `version` (forme canonique), `tag` (= `v` + version), `issued_at`, `expires_at`, et pour
   chaque binaire (`nxt-*`) son SHA-256 et sa taille. Il est fabriqué par `release-tool manifest` à partir des binaires de
   **la même exécution du workflow** que ceux publiés.
3. **Ce que la vérification refuse** : signature absente ou invalide, signée par une clé non compilée (même si le
   manifeste revendique le bon `key_id`), `key_id` incohérent avec la clé qui a signé, autre dépôt ou autre but, schéma
   inconnu, daté du futur (au-delà d'un jour), expiré, validité de plus de 400 jours, nom de fichier suspect (`../`,
   caché), empreinte mal formée, plus de 64 fichiers, manifeste de plus de 64 Kio. Étapes distinctes, appelées par
   l'installateur : `require_newer_than` (**jamais de retour en arrière, ni la même version**) et `check_asset` (taille et
   SHA-256 du fichier reçu).
4. **Clés de confiance = `proto::release::TRUSTED_KEYS`**, une liste : lors d'une rotation on **ajoute** la nouvelle clé
   AVANT de retirer l'ancienne, pour que les agents déjà installés acceptent la transition. L'identifiant de clé est
   l'empreinte (8 premiers hexadécimaux) du SubjectPublicKeyInfo, la même que `openssl pkey -pubin -outform DER | sha256sum`.
   Clé actuelle : `1c0224a8` (empreinte complète `1c0224a857fa577446915a5ff32a9e1732b8ecc907d3710d0da5d69da18f3a74`,
   vérifiée par un test unitaire).
5. **Où vit la clé privée** : secret d'**environnement** GitHub `release` (`RELEASE_SIGNING_KEY` chiffrée par une
   passphrase, `RELEASE_SIGNING_PASSPHRASE`), avec relecteur obligatoire et restriction aux étiquettes `v*` ; étiquettes
   `v*` protégées. **Copie chiffrée hors GitHub obligatoire.** Les secrets ne sont donnés qu'à l'étape « Signature » du job
   `sign` : ni la compilation de `release-tool`, ni les autres étapes ne les voient.
6. **Le job `sign` s'exécute APRÈS la publication** de la release et y ajoute les deux fichiers. Une approbation refusée
   ou un échec laisse donc une release sans signature : les agents l'ignoreront (aucun manifeste, aucune mise à jour). Rien
   de dangereux, et la signature peut être relancée.
7. **Le dernier garde-fou est dans le job lui-même** : après la signature, `release-tool verify` rejoue la vérification
   de l'agent avec les clés **compilées**. Si le secret n'est pas la sœur de la clé publique compilée, le job échoue avant
   d'ajouter quoi que ce soit.
8. **Répétition sans secret** (`sign-rehearsal`, lancement manuel sans publication) : clé jetable chiffrée comme la
   vraie, signature OpenSSL, vérification avec cette clé, et vérification que les clés **compilées refusent** cette
   signature. L'interopérabilité OpenSSL → `ring` est aussi un test (`release-tool/tests/openssl_interop.rs`).

## Procédure : avant d'approuver une signature

Le relecteur (vous) lit ce qu'il approuve, car le workflow exécuté est celui du commit pointé par l'étiquette :
- le tag pointe sur un commit que vous avez fait ou relu ;
- `git diff <ancien tag>..<tag> -- .github/` ne contient que ce que vous attendez (aucun nouveau `run:` qui touche aux
  secrets hors de l'étape « Signature », aucune action tierce qui change de SHA sans raison) ;
- la répétition (`workflow_dispatch` sans publication) a réussi sur ce commit.

## Rotation et perte de clé

- **Rotation prévue** : générer une nouvelle paire, ajouter sa clé publique à `TRUSTED_KEYS`, publier une release signée
  par l'ANCIENNE clé qui l'embarque, attendre que les agents l'aient installée, puis seulement basculer le secret et
  retirer l'ancienne.
- **Fuite de la clé privée** : retirer la clé de `TRUSTED_KEYS` ne suffit pas pour les agents déjà installés (ils ne
  connaissent que ce qui est compilé) ; il faudra une release signée par une clé de secours embarquée d'avance, ou
  réinstaller. D'où l'intérêt d'embarquer tôt une **clé de secours** (hors ligne) dans `TRUSTED_KEYS`.
- **Perte du secret GitHub** : restaurer depuis la copie chiffrée hors GitHub.

## Limites

- La sécurité repose sur la protection du secret GitHub et de votre compte (double authentification forte). L'approbation
  rend une fuite visible et difficile à faire en silence ; elle ne protège pas d'un compte pris.
- Rien n'est encore installé par ce mécanisme : le manifeste est vérifié par `release-tool` en CI, l'agent n'a que la
  bibliothèque. L'installation, le retour arrière et les vagues arrivent à l'étape 3.
