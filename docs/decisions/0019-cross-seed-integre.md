# 0019 — Cross-seed intégré

Statut : **implémenté** (voir « État de l'implémentation » en fin de document pour les écarts avec la conception).

## Objectif
Un client « tout-en-un » : en plus de piloter les nœuds, le master optimise les seedbox à la manière de
[cross-seed](https://github.com/cross-seed/cross-seed). Pour un contenu déjà complet sur un nœud, il retrouve le **même
contenu sur d'autres trackers** et l'ajoute sur ce même nœud, pointé sur les fichiers existants : le client revérifie,
puis partage. Aucun téléchargement de nouvelles données, donc du ratio gagné sur plusieurs trackers avec un seul stockage.

À ne pas confondre avec le « rebranchement » de l'ADR 0018 : lui garde le **même infohash** (même torrent, autre
annonce) pour finir une copie entre nœuds. Le cross-seed ajoute un **autre torrent** (autre infohash, autre tracker).
Les deux sont complémentaires ; le cross-seed n'a aucun rôle dans le transfert.

## Principe
1. **Recherche.** L'admin déclare des indexeurs **Torznab** (Prowlarr, Jackett : URL + clé d'API). Pour un torrent complet,
   le master interroge avec son nom et sa taille, et rien d'autre.
2. **Appariement strict.** Chaque candidat est téléchargé, puis comparé par **liste de fichiers** : mêmes chemins
   relatifs et mêmes tailles (multi-ensemble trié). Le nom seul ne suffit jamais. Il faut étendre `proto::metainfo`
   (liste des fichiers, bornée) et une clé de contenu (SHA-256 des couples chemin/taille triés).
3. **Ajout.** Commande `Add` existante : même `save_subdir`, **en pause**, puis `Recheck`. À 100 % : partage.
   Moins de 100 % : le torrent est **retiré** (`Remove` sans les fichiers). On ne télécharge jamais de données.
4. **Disposition différente** (dossier racine renommé) : renommage par l'**API du client** (qBittorrent, Transmission en
   ont une), jamais de lien physique ni de commande système : le jeu de commandes reste fermé. Les torrents
   mono-fichier au nom différent sont hors périmètre de la première version.
5. **Garde-fous.** Mode **simulation par défaut** (affiche ce qui serait ajouté, sans rien faire) ; limite de requêtes par
   indexeur ; liste blanche de trackers ; un torrent déjà connu (même infohash) n'est jamais ré-ajouté ; journal d'audit
   de chaque recherche et de chaque ajout.

## Interface : une ligne par contenu (exigence)
Un contenu partagé sur N trackers ne doit **jamais** apparaître N fois dans la liste.

- **Modèle.** Un torrent cross-seedé est rattaché à son torrent d'origine (`xseed_of` = infohash du parent, plus la clé de
  contenu). Le parent est la ligne affichée.
- **Tableau des torrents.** Une ligne par contenu. Débits et pairs sont **additionnés** sur les membres du groupe ; le
  statut et la progression sont ceux du parent (un enfant en erreur est signalé par un badge d'alerte sur la ligne).
  Badge « +3 trackers » ; un clic déplie un sous-détail par tracker : hôte, statut, progression, envoi, ratio.
- **Serveur.** Le regroupement est fait dans `torrent_rows` : filtres, tri et **pagination comptent les groupes**, pas
  les torrents. Filtrer par tracker retrouve le groupe si l'un de ses membres correspond.
- **Actions.** Pause, reprise, revérification, suppression sur la ligne s'appliquent au groupe ; la suppression demande
  s'il faut retirer aussi les torrents cross-seedés (par défaut : oui, sans supprimer les fichiers tant qu'un membre les
  utilise). Le sous-détail permet d'agir sur un seul membre.
- **Torrents non gérés** (déjà présents dans le client, ajoutés hors nxtTorrent) : leur liste de fichiers n'est pas
  connue de l'agent, ils ne sont donc pas regroupés dans la première version.
- **Page « Cross-seed ».** Indexeurs (ajout, test, activation), mode simulation/actif, historique (trouvé, ajouté,
  refusé et pourquoi), ratio par tracker.

## Sécurité
- Clés d'API des indexeurs **chiffrées** comme les autres secrets du master (clé `totp.key`, sauvegardées avec elle).
- Les `.torrent` de trackers privés contiennent un **passkey** : stockés chiffrés, jamais journalisés ni affichés, jamais
  renvoyés par l'API d'administration. L'agent ne reçoit que le contenu nécessaire à l'`Add` (déjà le cas des torrents ajoutés
  par fichier).
- Les URL d'indexeur sont saisies par l'admin et utilisées **par le master seulement** : aucune URL libre ne passe par le
  protocole agent. Elles sont validées (http/https, pas d'identifiants dans l'URL) ; attention à la requête vers le réseau
  interne (SSRF) : l'admin est de confiance, mais on borne les redirections et la taille des réponses.
- Limite de débit et délai entre requêtes par indexeur, pour ne pas faire bannir le compte de l'utilisateur.

## Découpage proposé
1. `proto::metainfo` : liste des fichiers et clé de contenu (tests : multi-fichiers, mono-fichier, bornes).
2. Client Torznab (`master`) + faux serveur Torznab dans `testkit` ; indexeurs chiffrés en base.
3. Appariement, simulation, puis ajout et retrait si < 100 % ; machine d'états par candidat (reprise après redémarrage).
4. Regroupement dans `torrent_rows`, API et interface (tableau groupé, sous-détail, page Cross-seed).
5. Test réel avec deux clients Docker (qBittorrent, Transmission) : même contenu, deux `.torrent` d'infohash différents,
   dossier racine renommé.

## Limites connues d'avance
- Aucun test possible contre de vrais trackers privés (comptes de l'utilisateur) : tests sur faux Torznab et vrais clients.
- Un tracker qui exige une règle propre (ratio minimal, une seule IP par passkey) reste de la responsabilité de l'utilisateur.
- Les clients sans renommage (Deluge, ruTorrent) ne géreront que les dispositions identiques.

## État de l'implémentation
Fait, avec les écarts suivants par rapport à la conception :

- **Liste des fichiers** : `proto::metainfo` expose `files` (chemins relatifs sûrs : jamais `..`, `.`, séparateur ni composant
  vide) et `content_key()` (SHA-256 des couples chemin/taille triés). Au-delà de 50 000 fichiers la liste n'est pas
  conservée et le torrent n'est pas éligible.
- **Torznab** (`master/src/torznab.rs`) : lecteur XML minimal qui refuse `<!DOCTYPE`/`<!ENTITY`, réponses bornées à 5 Mio,
  redirections désactivées, **`.torrent` téléchargés uniquement depuis l'hôte (schéma, hôte, port) de l'indexeur**.
  L'adresse d'un indexeur n'accepte ni identifiants, ni requête, ni fragment : la clé d'API est un champ à part.
- **Moteur** (`master/src/crossseed.rs`) : passage toutes les 5 minutes (lot de 20 torrents, une recherche par torrent et
  par jour), ou à la demande. Chaque requête passe par le **garde des indexeurs** (voir « Protection des comptes »). Pré-filtre par taille, puis téléchargement et
  comparaison de la clé de contenu. Refus journalisés : même hôte de tracker que l'original, dossier racine différent.
  Mode **simulation par défaut**. Un candidat dont la vérification se termine à moins de 100 % est retiré (fichiers
  conservés) après un délai de 120 s.
- **Stockage** : un torrent cross-seedé est un torrent géré ordinaire (rôle d'assignation `xseed`) rattaché à son parent
  (`xseed_of`) et **jamais replacé** sur un autre nœud. Le `.torrent` d'un candidat n'est conservé qu'une fois ajouté,
  comme pour tout torrent ajouté par fichier : **en clair dans `master.db`** (fichier 0600, chiffré dans les sauvegardes),
  et non chiffré séparément comme l'écrivait la conception. Seules les **clés d'API** des indexeurs sont chiffrées.
- **Interface** : une ligne par contenu (`group_cross_seeded`), débits et pairs additionnés, sous-lignes par tracker,
  badge « +N trackers », alerte si un membre est en erreur ; filtres, recherche, facettes de tracker et pagination
  comptent les groupes. Pause, reprise, revérification et suppression d'une ligne s'appliquent au groupe (les membres
  partent TOUJOURS sans leurs fichiers) ; le menu d'un membre permet d'agir sur lui seul. Page « Cross-seed » : mode,
  indexeurs (test avant ajout), historique avec la raison de chaque refus ou retrait.

**Non fait** : torrents non gérés (leur liste de fichiers n'est pas connue) ; liste blanche de
trackers ; ratio par tracker.

**Vérifié** : tests unitaires (lecteur Torznab, clé de contenu, regroupement), tests d'intégration avec un faux Torznab HTTP
et de vrais agents (ajout, partage, refus, retrait d'un contenu incomplet, simulation, jamais d'URL de tracker ni de clé dans
l'API ou le journal), et rendu navigateur (`ui/e2e/xseed.mjs`). Les tests `#[ignore]` `xseed_clients` valident
l'hypothèse centrale avec de vrais qBittorrent 5 et Transmission 4.1.4 (Docker) : **confirmée** (un autre infohash sur des
fichiers existants est vérifié à 100 % sans téléchargement ; un contenu différent reste incomplet, 97,9 % dans le test).
Aucun test contre de vrais trackers privés.

## Protection des comptes (rate-limiting et erreurs des indexeurs)
Un indexeur privé peut **bannir un compte** pour des requêtes trop fréquentes, trop nombreuses, ou répétées avec une clé
refusée. Chaque indexeur a ses propres règles : tout est donc réglable **par indexeur**, avec des défauts conservateurs.

**Réglages** (page Cross-seed, bornes vérifiées par le serveur et l'interface) :

| Réglage | Défaut | Bornes |
|---|---|---|
| Délai minimal entre deux requêtes | 3 s | 0,5 s à 1 h |
| Requêtes par heure glissante | 60 | 1 à 1 000 |
| Requêtes par 24 h glissantes | 500 | 1 à 20 000 |
| Résultats examinés par recherche | 10 | 1 à 50 |

Les **recherches ET les téléchargements de `.torrent`** comptent dans les budgets (ce sont les requêtes que les trackers
surveillent le plus). Le budget n'est jamais dépassé, même d'une requête : la requête n'est pas émise et le passage
s'arrête pour cet indexeur. Les budgets sont persistés (un redémarrage ne les remet pas à zéro).

**Économie de requêtes** : pré-filtre par taille annoncée et par infohash annoncé *avant* de télécharger ; un résultat
déjà examiné (indexeur + titre + taille) n'est pas retéléchargé pendant 30 jours ; une recherche par torrent et par jour ;
un torrent dont les indexeurs étaient en pause n'est pas marqué « cherché » et sera retenté à la reprise.

**Réaction aux erreurs** (`indexer_react`) :

| Cause | Réaction |
|---|---|
| HTTP 401/403, erreurs Torznab 100-199 (clé, compte suspendu, accès) | **Désactivation automatique**, raison affichée ; seule une action de l'admin (« Réactiver ») rétablit. On n'insiste jamais avec une clé refusée. |
| HTTP 429, ou 503 avec `Retry-After` | Pause d'au moins ce qui est demandé (bornée de 1 min à 24 h ; 5 min par défaut) |
| Quota annoncé (Torznab 500, 501, 910) | Pause de 12 h |
| Panne, délai dépassé, erreur serveur | Attente croissante : 1 min, 5 min, 15 min, 1 h, puis 6 h ; remise à zéro au premier succès |
| Lien mort ou réponse illisible propre à un résultat | Aucune pénalité pour l'indexeur ; on passe au résultat suivant |

Pendant une pause ou après une désactivation, **plus aucune requête** n'est envoyée, y compris pour un lancement manuel.
L'état (prêt, en pause jusqu'à…, désactivé automatiquement) et la consommation des budgets sont visibles dans l'interface ;
les pauses et désactivations sont dans le journal d'audit, en clair. Le bouton « Tester » est lui aussi espacé (5 s, puis
30 s après un accès refusé), car chaque test avec une mauvaise clé est une tentative d'accès.

**Limites** : le `Retry-After` n'est lu qu'en secondes (la forme « date HTTP » retombe sur l'attente par défaut) ; les
budgets sont ceux du master, pas ceux que l'indexeur applique réellement : à régler selon ses règles ; ces protections
reposent sur des faux serveurs HTTP (aucun test contre de vrais trackers).

## Liens physiques (autre dossier racine, autres noms de fichiers)
Décision : comme le cross-seed original, on crée des **liens physiques (hardlinks)** plutôt que de renommer côté client.
Le renommage ne couvre ni les noms de fichiers différents ni la suppression, et laisse les torrents dépendre du même
dossier. Les liens donnent à chaque torrent SA disposition, sans aucune copie, et une suppression d'un torrent ne casse
pas les autres.

- **Correspondance** (`proto::metainfo::plan_links`) : chaque fichier du candidat reçoit exactement un fichier de l'original
  de même taille : chemin identique d'abord, puis taille unique, puis nom de fichier unique ; au-delà, c'est ambigu et
  le candidat est ignoré. Au plus 5 000 fichiers. La revérification du client tranche ensuite sur le contenu réel.
- **Pas de lien nécessaire** quand le candidat se pose exactement sur les fichiers de l'original (mêmes chemins ET même
  dossier racine) : les deux torrents partagent alors les fichiers tels quels, comme avant.
- **Commande fermée `LinkTree`** (agent) : `source_dir`/`source_name` -> `dest_dir`/`dest_name` + paires de chemins relatifs.
  L'agent **revalide tout** : chemins relatifs sans `..`, sans lecteur ni séparateur dans les noms de racine ; source = fichier
  ordinaire déjà présent sous la racine (jamais un lien symbolique) ; destination sans lien symbolique et dont le dossier
  résolu reste sous la racine (couvre aussi les « junctions » Windows) ; **rien n'est jamais écrasé ni supprimé** ; un lien
  déjà en place (même taille) est accepté (idempotent) ; en cas d'échec, les liens et dossiers créés par CET appel sont
  retirés (tout ou rien). Les opérations tournent hors du fil asynchrone.
- **Déroulement** : phase `Linking` (le torrent n'est PAS encore ajouté au client) -> l'agent confirme -> phase `Adding`
  (ajout en pause) -> `Recheck` -> partage. Si l'agent échoue (autre disque, droits, fichier manquant) ou ne répond pas dans
  les 5 minutes, le candidat est abandonné avec la raison, sans rien laisser.
- **Suppression** : un torrent sur liens est **toujours retiré avec ses liens** (les fichiers de l'original ne sont pas
  touchés, il ne reste qu'un nom en moins) ; supprimer une ligne emporte ses membres. Un candidat lié qui s'avère
  incomplet à la vérification est retiré avec ses liens. Un torrent posé directement sur les fichiers de l'original suit le
  choix de l'utilisateur.
- **Isolation (trouvaille du test réel)** : un candidat dont le contenu diffère est jugé « partiel » à la vérification, et
  **Transmission renomme alors les fichiers qu'il vient de vérifier en `.part`**. Posé directement sur les fichiers de
  l'original, il l'aurait donc cassé (constaté : `film.bin` devenu `film.bin.part`). Règle en conséquence : un candidat
  n'est posé **directement** sur les fichiers de l'original que si les données sont **prouvées identiques** (mêmes chemins,
  même dossier racine ET mêmes empreintes de pièces et même taille de pièce : `Metainfo::pieces_digest`) ; dans tous les
  autres cas il a **son propre dossier de liens**, `<dossier de l'original>/.nxt-xseed/<16 premiers caractères de
  l'infohash>/`, qui est aussi son dossier d'enregistrement. Si le client renomme ses fichiers, seuls SES noms bougent ;
  l'original et son torrent restent intacts (vérifié sur qBittorrent et Transmission). Les trackers qui re-créent les
  torrents avec une autre taille de pièce passent donc par des liens, même à disposition identique.
- **Prérequis** : l'agent doit pouvoir **écrire** sous sa racine et créer des liens physiques : mêmes disque et volume que les
  fichiers (sinon « autre disque »), et **même utilisateur que le client torrent** sous Linux (`fs.protected_hardlinks`),
  d'où le `:ro` du compose d'exemple à retirer pour le cross-seed. Pas de repli en lien symbolique (les chemins vus par un
  client en conteneur diffèrent souvent de ceux de l'agent).

**Vérifié** : tests unitaires de la correspondance, de la validation de l'agent (chemins hostiles, rien d'écrasé, annulation) ;
tests d'intégration avec de vrais agents sur un vrai disque (de vrais hardlinks : une écriture dans l'original se voit dans
le lien) : autre dossier racine, mono-fichier renommé, échec de création, simulation, candidat lié incomplet, suppression.
**Vérifié avec de vrais clients** (`xseed_clients`, qBittorrent et Transmission) : un candidat sur un dossier racine différent
fait de liens est vérifié à 100 % sans téléchargement et partage ; le supprimer avec ses fichiers ne touche pas à l'original
(qBittorrent le place en file d'attente quand ses envois actifs sont au maximum : état « prêt »). Ici `ln` dans le conteneur
joue le rôle de l'agent, dont le code est testé à part.
**Non vérifié** : le test des liens symboliques de l'agent (Windows sans droit de création de liens : il s'exécute en CI
Linux) ; la création de liens par l'agent DANS un conteneur (volume en écriture, même utilisateur) ; les dossiers vides
`.nxt-xseed/<id>` laissés après suppression (le client retire ses fichiers, pas ce dossier).
