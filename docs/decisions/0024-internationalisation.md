# 0024 — Internationalisation de l'interface

**Statut :** accepté (rc.3)

## Contexte

L'interface d'administration était écrite uniquement en français. Le produit s'adresse à des utilisateurs de VPS, NAS
et homelabs de toutes langues ; on veut au minimum : français, anglais, espagnol, italien, portugais, russe, chinois.

## Décisions

1. **Aucune dépendance.** Un module d'une centaine de lignes (`ui/src/lib/i18n.js`, JavaScript pur : langue,
   dictionnaire et fonction `tr(texte, variables)`) ; le chargement des fichiers de langue est à part
   (`lang-loader.js`, propre à Vite) pour que le reste reste importable par les tests Node. Les bibliothèques i18n (ICU, formatage de messages…)
   alourdiraient le binaire embarqué pour un besoin simple ; on s'appuie sur `Intl` (pluriels, nombres, dates).
2. **La clé d'un texte est son texte français** (`tr('Créer un compte')`). Le français, langue de référence, n'a donc
   pas de fichier ; un texte oublié reste lisible au lieu d'afficher une clé technique ; le code reste lisible. Les
   autres langues sont des fichiers `ui/src/locales/<code>.json` `{ "texte français": "traduction" }`.
3. **Chargement à la demande** : chaque langue est un chunk séparé (≈ 25 à 30 Ko compressés), chargé avant l'affichage
   (pas de flash en français). Choix mémorisé dans le navigateur (`nxt-lang`), sinon langue du navigateur, sinon anglais.
4. **Variables** `{nom}` ; **pluriels** : une clé `un|autres` (formes du français) a pour traduction un objet
   `{ one, few, many, other }` selon les règles de la langue cible (`Intl.PluralRules`) ; le chinois n'a qu'une forme.
   Une variable numérique `{n}` est écrite avec les séparateurs de la langue.
5. **Mise en forme dans une phrase** : le composant `Rich` rend `**gras**` et `` `code` `` d'un texte traduit **sans
   jamais interpréter de HTML** (segments rendus un à un). Une phrase reste ainsi traduisible d'un seul bloc au lieu
   d'être coupée autour des balises.
6. **Dates, nombres, jours de la semaine, unités** : `Intl` avec la langue active (`format.js`). Les unités d'octets
   suivent l'usage (français : o, Ko, Mo ; autres : B, KB, MB).
7. **Changement de langue = rechargement de la page.** Certains libellés (menus, colonnes) sont construits une fois au
   démarrage de leur composant ; recharger est plus simple et plus sûr que de les rendre tous réactifs. L'état de
   langue n'a donc pas besoin d'être réactif.

## Hors périmètre (limites connues)

- **Les textes produits par le master restent en français** : messages d'erreur de l'API (`{"error": "…"}`), titres du
  journal d'audit, corps des alertes envoyées par ntfy/webhook. Les convertir en codes stables traduits côté interface
  est un chantier distinct (les tests Rust attendent aujourd'hui ces phrases).
- **Les traductions sont de bonne qualité mais n'ont pas été relues par des locuteurs natifs** (en particulier chinois
  et russe) : une relecture est recommandée avant de les annoncer comme définitives. Corriger un texte = éditer le JSON.
- Les langues s'écrivant de droite à gauche (arabe, hébreu) ne sont pas gérées (mise en page à adapter).

## Garde-fous

`ui/e2e/i18n.mjs` (lancé en CI) :

1. **couverture statique** : chaque texte employé dans le code existe dans chaque langue, sans texte périmé ; les
   variables `{x}`, le gras `**` et le code `` ` `` sont identiques au français ; formes de pluriel valides ;
2. **rendu réel** : les 10 pages sont affichées en français puis dans d'autres langues ; toute ligne **identique** au
   français (hors noms propres, nombres et traductions volontairement identiques) est signalée comme texte oublié.

Les autres tests de bout en bout forcent `--lang=fr-FR` (variable `E2E_LANG` pour changer) : ils vérifient du texte français.

## Ajouter une langue

1. Ajouter `{ code, name, locale }` à `LANGS` (`i18n.js`) ;
2. copier `ui/src/locales/en.json` en `<code>.json` et traduire les valeurs (pluriel : objet des formes CLDR de la langue) ;
3. `node ui/e2e/i18n.mjs` doit être vert.

## Ajouter un texte

Écrire `tr('Texte en français')` dans le code, puis l'ajouter aux fichiers de langue ; le test signale tout oubli.
