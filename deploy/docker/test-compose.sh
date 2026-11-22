#!/bin/sh
# Rejoue, sur le VRAI Compose (master + Transmission + agent), le parcours d'un utilisateur :
#   up -d  ->  création du compte  ->  code d'appairage  ->  appairage  ->  nœud en ligne  ->  torrent dans Transmission
#   docker build --target master -t nxt-master:latest . && docker build --target agent -t nxt-agent:latest .
#   sh deploy/docker/test-compose.sh
export MSYS_NO_PATHCONV=1   # Git Bash (Windows) ne doit pas convertir les chemins des commandes
C="docker compose -f deploy/docker/compose.transmission-test.yml"
A=http://127.0.0.1:17080
H='-H X-Requested-With:nxt -H Content-Type:application/json'
PW='Correct-Horse-9'
PY=$(command -v python3 || command -v python)
COOKIE=
PASS=0; FAIL=0
ok()  { echo "  [OK]   $*"; PASS=$((PASS+1)); }
bad() { echo "  [FAIL] $*"; FAIL=$((FAIL+1)); }
wait_for() { what="$1"; secs="$2"; shift 2; i=0; while [ $i -lt $((secs*2)) ]; do "$@" >/dev/null 2>&1 && { ok "$what"; return 0; }; sleep 0.5; i=$((i+1)); done; bad "$what (délai ${secs}s)"; return 1; }
json() { $PY -c "import sys,json; d=json.load(sys.stdin); print($1)"; }
# Vérifications JSON : de vraies fonctions (pas de sh -c imbriqué, dont l'échappement est fragile).
api_get() { curl -s -H "Cookie: $COOKIE" "$A$1"; }
node_online() { api_get /api/nodes | $PY -c "import sys,json; d=json.load(sys.stdin); sys.exit(0 if len(d)==1 and d[0]['connected'] else 1)"; }
node_is_transmission() { api_get /api/nodes | $PY -c "import sys,json; d=json.load(sys.stdin); n=d[0]; sys.exit(0 if len(d)==1 and n['connected'] and n['client_kind']=='transmission' and n['hostname']=='transmission-1' and n['labels']==[] else 1)"; }
# `/api/torrents` renvoie une PAGE : {total, items, counts, trackers}. Un torrent est « assigné » dès qu'un nœud le porte.
torrent_assigned() { api_get /api/torrents | $PY -c "import sys,json; d=json.load(sys.stdin); i=d['items']; sys.exit(0 if len(i)==1 and len(i[0]['nodes'])==1 else 1)"; }

cleanup() { $C down -v >/dev/null 2>&1; }
cleanup
trap cleanup EXIT

echo "== 1. Démarrage d'un seul coup"
$C up -d >/dev/null 2>&1 && ok "docker compose up -d" || { bad "up -d"; exit 1; }
wait_for "tableau de bord joignable" 60 curl -sf $A/api/health
wait_for "Transmission joignable" 150 sh -c "curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:19091/ | grep -qE '^(200|401)'"
[ "$(curl -s $A/api/health | json "d['setup_required']")" = "True" ] && ok "premier démarrage : initialisation requise" || bad "setup_required"
wait_for "l'agent non appairé ATTEND (il n'a pas quitté)" 30 sh -c "$C logs agent 2>&1 | grep -q 'non appairé' && [ \"\$($C ps --status running --format '{{.Service}}' | grep -c '^agent\$')\" = 1 ]"

echo "== 2. Création du compte depuis le navigateur (source = réseau Docker, non locale)"
[ "$(curl -s -o /dev/null -w '%{http_code}' -X POST $H -d '{"username":"admin","password":"court"}' $A/api/setup)" = "400" ] && ok "mot de passe faible refusé" || bad "mot de passe faible"
[ "$(curl -s -o /dev/null -w '%{http_code}' -X POST $H -d "{\"username\":\"admin\",\"password\":\"$PW\"}" $A/api/setup)" = "201" ] && ok "compte créé" || bad "création du compte"
login() { curl -s -i $H -d "{\"username\":\"admin\",\"password\":\"$PW\"}" $A/api/login | grep -i '^set-cookie' | sed 's/^[^:]*: *//; s/;.*//' | tr -d ''; }
COOKIE=$(login)
[ -n "$COOKIE" ] && ok "connexion" || bad "connexion"

echo "== 3. Code d'appairage puis appairage de l'agent"
CODE=$(curl -s -H "Cookie: $COOKIE" $H -d '{"labels":[]}' $A/api/pairing-codes | json "d['code']")
case "$CODE" in CODE-*) ok "code généré (sans label : facultatif)";; *) bad "code : $CODE";; esac
$C run --rm --no-deps agent pair master:7443 "$CODE" 2>&1 | grep -q "Appairé" && ok "agent appairé" || bad "appairage"

echo "== 4. Le nœud apparaît seul"
wait_for "nœud « transmission-1 » en ligne, client Transmission, sans label" 60 node_is_transmission

echo "== 5. Un torrent ajouté depuis le tableau de bord arrive dans Transmission"
IH=c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00
[ "$(curl -s -o /dev/null -w '%{http_code}' -H "Cookie: $COOKIE" $H -d "{\"infohash\":\"$IH\",\"save_subdir\":\"films\"}" $A/api/torrents)" = "201" ] && ok "torrent ajouté" || bad "ajout"
wait_for "torrent assigné au nœud" 30 torrent_assigned
SID=$(curl -s -D - -o /dev/null -u admin:adminadmin http://127.0.0.1:19091/transmission/rpc | grep -i '^x-transmission-session-id' | tr -d '\r\n' | sed 's/^[^:]*: *//')
wait_for "présent dans Transmission, dans /downloads/films" 30 sh -c "curl -s -u admin:adminadmin -H 'X-Transmission-Session-Id: $SID' -d '{\"method\":\"torrent-get\",\"arguments\":{\"fields\":[\"hashString\",\"downloadDir\"]}}' http://127.0.0.1:19091/transmission/rpc | grep -q '\"downloadDir\":\"/downloads/films\"'"

echo "== 6. Redémarrage de toute la pile : rien à refaire"
$C restart >/dev/null 2>&1
wait_for "tableau de bord de retour" 60 curl -sf $A/api/health
[ "$(curl -s $A/api/health | json "d['setup_required']")" = "False" ] && ok "le compte a survécu" || bad "compte perdu"
COOKIE=$(login)
[ -n "$COOKIE" ] && ok "reconnexion avec le même mot de passe" || bad "reconnexion"
wait_for "le nœud se reconnecte tout seul" 60 node_online

echo; echo "Résultat : $PASS réussis, $FAIL échecs"
[ "$FAIL" -eq 0 ]
