# 0020 — Distribution : binaires statiques, images multi-architecture, installation d'un parc mixte

Statut : **implémenté** ; le workflow de publication n'a **jamais tourné sur GitHub** (voir « Non vérifié »).

## Objectif
Un utilisateur non expert doit pouvoir installer le master et ses agents sans compiler, sur des machines de
architectures différentes (VPS amd64, NAS ou Raspberry Pi arm64), en pouvant **vérifier ce qu'il installe**.

## Décisions
1. **Binaires statiques (musl)**, un par architecture, compilés **nativement** (runners amd64 et arm64) par le **même
   Dockerfile** que les images : une étape `artifacts` (`docker buildx build --target artifacts --output type=local,dest=…`)
   extrait `nxt-master` et `nxt-agent`. Un seul chemin de compilation pour les binaires et les images, `cargo build
   --release --locked`. Le workflow vérifie qu'ils sont statiques (`file`, `ldd`) et qu'ils s'exécutent.
2. **Fichiers publiés** par version `vX.Y.Z` : `nxt-{master,agent}-linux-{amd64,arm64}`, `nxt-{master,agent}-windows-amd64.exe`,
   **`SHA256SUMS`**, et une **attestation de provenance** (`actions/attest-build-provenance`) : `gh attestation verify
   <fichier> --repo <dépôt>` prouve quel workflow, depuis quel commit, a construit le fichier.
3. **Images GHCR** `nxt-master` et `nxt-agent` : construites par architecture (natives), poussées **par condensat**, puis
   réunies en un seul tag (`X.Y.Z`, et `latest` hors préversions) : `docker pull` prend la bonne architecture seul.
4. **Garde-fou de version** : l'étiquette doit être égale à `version` du `Cargo.toml` racine, sinon le workflow échoue.
   Une version contenant `-` (ex. `1.0.0-rc1`) est publiée comme préversion, sans tag `latest`.
5. **Répétition générale** : le déclenchement manuel (`workflow_dispatch`, `publish` faux) construit tout et dépose les
   fichiers en artefact, **sans** release ni push.
6. **Parc mixte** pour l'installation d'agent en une commande : `MASTER_AGENT_BINARY_URL` peut contenir `{arch}` et
   `MASTER_AGENT_BINARY_SHA256` devient `amd64=<sha>,arm64=<sha>`. Le script détecte l'architecture (`uname -m` :
   x86_64 → amd64, aarch64 → arm64, sinon refus net), choisit l'URL et **l'empreinte de cette architecture**, et refuse tout
   binaire dont l'empreinte diffère (rien n'est installé). La forme historique (une URL, une empreinte) reste valide.
   `deploy/release/master-env.sh SHA256SUMS <URL>` produit ces deux lignes depuis une release.
7. **Sûreté du gabarit** : `{arch}` est le seul gabarit accepté dans l'URL ; toute autre accolade, toute apostrophe, un schéma
   autre que https, une architecture inconnue ou en double, une empreinte en majuscules ou trop courte sont refusés
   (`valid_binary`), puisque ces valeurs sont écrites dans un script exécuté en root.

## Vérifié (sur cette machine)
- Compilation musl complète via Docker : `nxt-master` (13,7 Mo, interface embarquée) et `nxt-agent` (8,4 Mo) sont des
  exécutables **`static-pie`**, qui s'exécutent sur Alpine et sur une glibc minimale.
- Les images (`master`, `agent`) passent `deploy/docker/test.sh` : 33 vérifications (non-root, sans capacité, sans shell,
  appairage, plantage puis redémarrage, arrêt propre).
- Le script d'installation : 6 scénarios (`deploy/systemd/test/install-agent-arch.sh`, conteneurs jetables, `curl` et
  `uname` factices) : arm64, amd64, architecture non prise en charge, empreinte absente pour l'architecture, empreinte
  fausse (rien d'installé), forme historique ; et le test d'installation existant, inchangé.
- `valid_binary` / `render` (tests unitaires), `master-env.sh` (cas valides et refus), syntaxe YAML des workflows ;
  correction d'un piège de matrice GitHub (un `include` d'architecture s'appliquerait à toutes les combinaisons).

## Non vérifié
- **Le workflow `release.yml` n'a jamais été exécuté** : ni la publication, ni GHCR, ni l'attestation, ni le job Windows
  (construction de l'interface puis `cargo build --release`). Faire d'abord une **répétition** (`workflow_dispatch`).
- **arm64** : jamais compilé ici (machine amd64) ; il l'est sur un runner arm64 natif au moment de la publication.
  Ces runners (`ubuntu-24.04-arm`) sont gratuits pour les dépôts publics seulement.
- Les actions sont fixées par **version majeure**, pas par condensat de commit : à durcir pour une chaîne
  d'approvisionnement stricte.
- **Pas de signature de code Windows** (SmartScreen avertira), pas de SBOM, pas de binaire macOS ni ARM 32 bits
  (Raspberry Pi sous système 32 bits : armv7 non couvert), pas de paquets `.deb`/`.rpm`.
