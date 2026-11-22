# 0014 — Double authentification de l'administrateur (TOTP)

## Contexte
Le tableau de bord pilote tout le cluster (ajout/suppression de torrents, révocation de nœuds). Un mot de passe volé
ne doit pas suffire. La spec prévoyait « TOTP/passkey en phase ultérieure ».

## Décisions
- **TOTP RFC 6238** (SHA-1, 6 chiffres, 30 s), compatible avec toutes les applications courantes ; implémenté sur
  `ring::hmac` (aucune dépendance de plus pour le calcul), validé sur les vecteurs de test de la RFC. Tolérance ±1 pas.
- **Secret chiffré au repos** (AES-256-GCM, nonce aléatoire) avec une clé aléatoire dans `totp.key` (0600, à côté de
  la base) : une copie de la base seule ne permet pas de générer des codes. Clé illisible ou tronquée = erreur franche
  (jamais de régénération silencieuse qui rendrait les secrets existants inutilisables).
- **Pas de rejeu** : un pas de temps ne sert qu'une fois, réservé par un `UPDATE` atomique (deux requêtes simultanées
  avec le même code ne passent pas toutes les deux). Le code qui confirme l'activation ne peut pas servir à se connecter.
- **Connexion en deux étapes** : le mot de passe valide produit un *ticket* (5 min, 5 essais, lié à l'adresse source),
  PAS de session. Les compteurs anti-brute-force ne sont remis à zéro qu'à la réussite complète : connaître le mot de
  passe ne donne pas un nombre illimité d'essais de codes (test dédié).
- **10 codes de secours** à usage unique (50 bits chacun), affichés une seule fois, stockés hachés (SHA-256).
- **Désactivation** : mot de passe ET code exigés, le mot de passe d'abord (un mauvais mot de passe ne consomme ni
  code de secours ni pas TOTP — défaut trouvé par les tests). Activation et désactivation ferment les autres sessions.
- **Récupération** : `master admin disable-totp --username X`, qui exige un accès local à la machine du master.
- **Journal** : activation, désactivation, échecs, usage d'un code de secours (jamais le secret ni les codes).
- **Passkeys/WebAuthn** : non faits (le TOTP couvre le besoin avec bien moins de surface) ; possibles ensuite.
