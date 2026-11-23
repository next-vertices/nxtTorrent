# 0023 — Comptes d'administration multiples

## Contexte

Le master n'acceptait qu'un compte, créé au premier démarrage. Plusieurs personnes (le propriétaire et un proche, deux
administrateurs d'une même équipe) ne pouvaient pas se connecter séparément : mot de passe partagé, journal incapable
de distinguer qui a fait quoi, une seule double authentification.

## Décisions

### Tous les comptes ont les mêmes droits

Pas de rôles. Un rôle « lecture seule » a été envisagé et écarté : il demande de masquer ou désactiver les boutons dans
chaque page (et de maintenir cette liste à jour), pour un besoin non exprimé. L'attribution des actions à leur auteur
(`actor_label` dans le journal, déjà en place) couvre le besoin de traçabilité. Si un rôle limité devient nécessaire,
la table et les routes sont prêtes à porter une colonne `role` ; c'est l'interface qui demanderait le plus de travail.

### Un administrateur crée le compte, avec un mot de passe provisoire

`POST /api/users` : le nom (3 à 32 caractères, comme à l'initialisation) et un mot de passe PROVISOIRE choisi par
l'administrateur, soumis aux mêmes règles de robustesse que n'importe quel mot de passe. L'interface en propose un tiré
du générateur sécurisé du navigateur (`crypto.getRandomValues`, alphabet sans caractères ambigus, tirage sans biais de
modulo). Le compte est marqué `must_change`.

L'administrateur qui l'a choisi le connaît : il ne peut donc pas rester le mot de passe du compte. Tant que `must_change`
est vrai, **toute route sauf `/api/me`, `/api/logout` et `/api/password` répond 403** (`authed` côté serveur ; un
contournement par l'interface serait sans effet). L'interface affiche à la place un écran de changement de mot de
passe. Le changement lève l'obligation sans nouvelle connexion.

### Garde-fous

- **Un nom est unique sans tenir compte de la casse** (« Alice » et « alice » seraient deux comptes indiscernables pour
  un humain). Le test et l'insertion sont une seule requête : deux créations simultanées ne réussissent pas.
- **Au plus 20 comptes.**
- **Un administrateur ne peut ni supprimer son propre compte, ni réinitialiser son propre mot de passe ou sa double
  authentification par ces routes** : « Mon compte » exige le mot de passe actuel. Cela garantit aussi qu'il reste
  toujours au moins un compte (le sien).
- **Réinitialiser un mot de passe ou une double authentification, ou supprimer un compte, ferme toutes les sessions de
  ce compte** (`remove_user_except`) et remet à zéro son compteur d'échecs de connexion.
- **Jamais de mot de passe dans le journal** : seuls le nom du compte et l'action y figurent (`user_created`,
  `user_deleted`, `user_password_reset`, `user_totp_reset` ; ce dernier est mis en évidence comme événement de
  sécurité). La liste des comptes n'expose aucun condensat.
- La récupération locale `master admin set-password` (par le propriétaire, sur la machine du master) crée toujours un
  compte et lève l'obligation de changement : c'est lui qui choisit le mot de passe.

### Double authentification

Inchangée : elle reste **par compte** (secret chiffré et codes de secours propres à chacun). Un administrateur peut
désactiver celle d'un autre compte (téléphone perdu) sans ouvrir de terminal sur le master, l'équivalent de
`master admin disable-totp`.

## Limites

- Les mots de passe provisoires doivent être transmis par un canal sûr : le master ne les envoie nulle part.
- Pas d'invitation par lien ni d'envoi de courriel : le master n'a pas de service de courrier.
- Les limites anti brute-force par compte et par adresse s'appliquent à chaque compte comme avant.
