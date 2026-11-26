# 0028 — Mise à jour des agents Linux

**Statut :** accepté. Suite des ADR 0026 (signature) et 0027 (master).

## Décisions

1. **Nouvelle commande, écart assumé au jeu fermé** : `UpdateAgentCmd` ne porte qu'une **version** (ni URL, ni chemin, ni
   commande). L'agent n'installe qu'une release signée par une clé compilée dans son binaire et plus récente que lui.
   Un master compromis ne peut donc rien faire exécuter d'autre.
2. **Cible = version du master.** Le master télécharge les binaires d'agent de sa version (déclenché par un
   administrateur, jamais par un nœud), les vérifie, les garde en cache et les sert par `FetchUpdate` sur la connexion mTLS
   (certificat requis, nœud non révoqué, quatre transferts au plus). L'agent ne contacte jamais GitHub.
3. **Installateur commun** au master et à l'agent (`proto::install`) : service root déclenché par fichier, revérification,
   retour arrière. Dossier d'état de l'agent : `/var/lib/nxt-agent-update`.
4. **« Sain » = reconnecté au master** pendant 15 s, pas seulement démarré. Sans cela après 3 minutes, l'ancien binaire
   revient. Si le master tombe pendant la fenêtre, un bon agent est aussi annulé : choix prudent.
5. **Compatibilité** : champs et messages ajoutés sans changer `protocol_version` ; un ancien agent reste connecté et se
   met à jour à la main.
6. **Activation** : `nxt-agent self-update enable` (fait par le script d'installation). Sans elle, le master refuse.
7. Déclenchement : `POST /api/nodes/{id}/update`, mot de passe et second facteur redemandés, journalisé
   (`node_update_requested`, puis le résultat rapporté par le nœud, une fois).

## Essais

`master/tests/tunnel.rs` (distribution et parcours complet), `master/tests/self_update.rs` (installateur),
`deploy/systemd/test/self-update.sh` (vrai systemd : agent en panne annulé, puis mise à jour réussie ; non lancé en CI).

## Reste à faire

Le bouton par nœud et la suspension des alertes « hors ligne » (fenêtre de 10 minutes) sont livrés. Restent :
agent Windows, mises à jour par vagues. Une ancienne version doit rester capable de relire les données écrites par la
nouvelle (certificat renouvelé, événements en attente) : à vérifier à chaque release.
