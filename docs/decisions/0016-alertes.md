# 0016 — Alertes configurables

## Contexte
Le master n'avait qu'une notification de présence (nœud hors ligne/en ligne) vers un webhook réglé en ligne de commande.
Pour un cluster qu'on ne regarde pas en permanence, il faut être prévenu des vrais problèmes, sans spam.

## Décisions
- **Règles** (page « Alertes ») : nœud hors ligne, port d'écoute fermé, espace disque sous un seuil, torrents en erreur ;
  chacune activable avec son délai de confirmation (30 s à 24 h) ; option « prévenir aussi du rétablissement ».
- **Anti-battement** : une condition doit durer le délai choisi avant d'alerter ; une alerte n'est envoyée qu'**une fois**
  tant que le problème dure ; sa disparition produit un message de rétablissement ; une panne plus courte que le délai
  ne produit rien (ni alerte ni rétablissement). Machine à états pure, testée (`alerts::AlertState`).
- **Pas de fausse alerte sur de l'inconnu** : un nœud « jamais connecté » n'est pas une panne ; un espace libre inconnu
  (0) ou une joignabilité « inconnue » ne déclenchent rien.
- **Canaux** : webhook JSON générique et ntfy (5 au plus). Envoi sans redirection, délai de 10 s. Un « Tester » par canal.
- **L'URL d'un canal est un secret** (token ntfy, webhook privé) : chiffrée au repos (même clé que le TOTP), **jamais**
  renvoyée par l'API (seul `schéma://hôte:port/…` est affiché), jamais journalisée, jamais dans un message d'erreur.
- **Journal** : `alert_fired` / `alert_resolved` sont inscrits même sans canal (« Alerte : … », « Rétabli : … »).
- **État en mémoire** : un redémarrage du master repart de zéro et renotifie ce qui est toujours anormal après le délai.
- Le notifier de présence en ligne de commande (`--webhook-url`) est conservé tel quel pour compatibilité.

## Limites
- Pas de canal e-mail/SMS/Telegram natif (un webhook vers un relais couvre ces cas).
- Un canal http(s) quelconque est accepté (ntfy auto-hébergé sur le réseau local) : seul l'administrateur authentifié peut
  en définir, mais il pourrait faire émettre des POST depuis le master vers son réseau interne.
