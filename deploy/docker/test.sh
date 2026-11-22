#!/bin/sh
# Teste les images Docker construites (nxt-master:dev, nxt-agent:dev) comme en production :
# non-root, système de fichiers en lecture seule, aucune capacité.
#   docker build --target master -t nxt-master:dev . && docker build --target agent -t nxt-agent:dev .
#   sh deploy/docker/test.sh
export MSYS_NO_PATHCONV=1   # Git Bash (Windows) ne doit pas convertir /nxt-master en chemin Windows
IMG_M="${IMG_M:-nxt-master:dev}"
IMG_A="${IMG_A:-nxt-agent:dev}"
NET=nxt-t-net
PASS=0; FAIL=0
ok()  { echo "  [OK]   $*"; PASS=$((PASS+1)); }
bad() { echo "  [FAIL] $*"; FAIL=$((FAIL+1)); }
check() { what="$1"; shift; if "$@" >/dev/null 2>&1; then ok "$what"; else bad "$what"; fi; }
wait_for() { what="$1"; secs="$2"; shift 2; i=0; while [ $i -lt $((secs*2)) ]; do "$@" >/dev/null 2>&1 && { ok "$what"; return 0; }; sleep 0.5; i=$((i+1)); done; bad "$what (délai ${secs}s)"; return 1; }

cleanup() {
  docker rm -f nxt-t-master nxt-t-agent >/dev/null 2>&1
  docker network rm $NET >/dev/null 2>&1
  docker volume rm nxt-t-mdata nxt-t-adata >/dev/null 2>&1
}
cleanup
trap cleanup EXIT
docker network create $NET >/dev/null

HARDEN="--read-only --cap-drop ALL --security-opt no-new-privileges"
health() { [ "$(docker inspect -f '{{.State.Health.Status}}' "$1" 2>/dev/null)" = "healthy" ]; }
# uid 65532, aucune capacité, NoNewPrivs : lus dans /proc du PID 1 du conteneur (espace de PID partagé).
check_isolated() {
  got=$(docker run --rm --pid=container:"$1" alpine sh -c "grep -E '^(Uid|CapEff|NoNewPrivs):' /proc/1/status | tr -s '[:space:]' ' '")
  if [ "$got" = "Uid: 65532 65532 65532 65532 CapEff: 0000000000000000 NoNewPrivs: 1 " ]; then
    ok "$2 : uid 65532, aucune capacité, NoNewPrivs"
  else
    bad "$2 : isolation inattendue ($got)"
  fi
}
logs_count() { docker logs "$1" 2>&1 | grep -c "$2"; }

echo "== 1. Images"
for img in "$IMG_M" "$IMG_A"; do
  size=$(docker image inspect -f '{{.Size}}' "$img")
  [ "$size" -lt 31457280 ] && ok "$img : $((size/1024/1024)) Mo (< 30 Mo)" || bad "$img trop lourde : $((size/1024/1024)) Mo"
  check "$img : utilisateur non-root 65532" test "$(docker image inspect -f '{{.Config.User}}' "$img")" = "65532:65532"
  check "$img : aucun shell dans l'image" sh -c "! docker run --rm --entrypoint /bin/sh '$img' -c true"
done

echo "== 2. Master"
docker run -d --name nxt-t-master --network $NET --restart on-failure:5 $HARDEN \
  --health-interval 2s --health-timeout 2s --health-retries 3 \
  -e MASTER_SANS=nxt-t-master -v nxt-t-mdata:/data "$IMG_M" >/dev/null
wait_for "master healthy (HEALTHCHECK)" 40 health nxt-t-master
check "données : dossier 0700 et clé de CA 0600, propriété 65532" \
  test "$(docker run --rm -v nxt-t-mdata:/data alpine stat -c '%a %u' /data /data/ca.key | tr '\n' ' ')" = "700 65532 600 65532 "
check_isolated nxt-t-master "master"
docker run --rm --entrypoint /nxt-master "$IMG_M" serve >/dev/null 2>&1 && bad "serve sans MASTER_SANS doit échouer" || ok "serve sans MASTER_SANS échoue franchement"

echo "== 3. Appairage"
CODE=$(docker exec nxt-t-master /nxt-master pair-code --label ssd 2>/dev/null | tail -1)
[ -n "$CODE" ] && ok "code d'appairage généré" || bad "code d'appairage"
docker run --rm --network $NET -v nxt-t-adata:/data "$IMG_A" pair nxt-t-master:7443 "$CODE" >/dev/null 2>&1 && ok "agent appairé" || bad "appairage de l'agent"
check "clé de l'agent 0600, propriété 65532" \
  test "$(docker run --rm -v nxt-t-adata:/data alpine stat -c '%a %u' /data/agent.key)" = "600 65532"

echo "== 4. Agent"
docker run -d --name nxt-t-agent --network $NET --restart on-failure:5 $HARDEN \
  -e AGENT_ROOT=/downloads -e AGENT_CLIENT_URL=http://127.0.0.1:18080 \
  -v nxt-t-adata:/data "$IMG_A" >/dev/null
wait_for "tunnel mTLS établi (journal du master)" 30 sh -c "[ \$(docker logs nxt-t-master 2>&1 | grep -c 'tunnel établi') -ge 1 ]"
check_isolated nxt-t-agent "agent"
check "le code d'appairage n'apparaît dans aucun journal" sh -c "! docker logs nxt-t-master 2>&1 | grep -qF '$CODE' && ! docker logs nxt-t-agent 2>&1 | grep -qF '$CODE'"

echo "== 4b. API d'administration (depuis un AUTRE conteneur : ni boucle locale, ni confiance)"
CURL="docker run --rm --network $NET curlimages/curl -s"
JSONH="-H X-Requested-With:nxt -H Content-Type:application/json"
GOODPW='Correct-Horse-9'
ADMIN=http://nxt-t-master:7080
status() { $CURL -o /dev/null -w '%{http_code}' "$@"; }
check_eq() { what="$1"; want="$2"; got="$3"; [ "$got" = "$want" ] && ok "$what" || bad "$what (attendu $want, obtenu $got)"; }

$CURL $ADMIN/api/health | grep -q '"setup_required":true' && ok "health : initialisation requise" || bad "health"
check_eq "initialisation refusée depuis une autre machine" 403 \
  "$(status -X POST $JSONH -d "{\"username\":\"root\",\"password\":\"$GOODPW\"}" $ADMIN/api/setup)"
docker exec -e MASTER_ADMIN_PASSWORD="$GOODPW" nxt-t-master /nxt-master admin set-password --username root >/dev/null 2>&1 \
  && ok "compte créé via la CLI locale" || bad "admin set-password"
check_eq "mauvais mot de passe" 401 \
  "$(status -X POST $JSONH -d '{"username":"root","password":"mauvais-mot-de-passe-1"}' $ADMIN/api/login)"
check_eq "liste des nœuds sans session" 401 "$(status $ADMIN/api/nodes)"
check_eq "requête sans en-tête CSRF" 403 \
  "$(status -X POST -H Content-Type:application/json -d "{\"username\":\"root\",\"password\":\"$GOODPW\"}" $ADMIN/api/login)"
COOKIE=$($CURL -i -X POST $JSONH -d "{\"username\":\"root\",\"password\":\"$GOODPW\"}" $ADMIN/api/login \
  | grep -i '^set-cookie' | sed 's/^[^:]*: *//; s/;.*//' | tr -d '\r')
[ -n "$COOKIE" ] && ok "connexion : cookie de session reçu" || bad "connexion"
NODES=$($CURL -H "Cookie: $COOKIE" $ADMIN/api/nodes)
echo "$NODES" | grep -q '"connected":true' && echo "$NODES" | grep -q '"labels":\["ssd"\]' \
  && ok "l'API voit le nœud appairé : connecté, avec les labels de l'admin" || bad "nœuds : $NODES"
PAGE=$($CURL -i $ADMIN/)
echo "$PAGE" | grep -q '<script type="module"' && echo "$PAGE" | grep -qi "script-src 'self'"   && ok "interface web servie par l'image (assets embarqués, CSP stricte)" || bad "interface web"
$CURL -H "Cookie: $COOKIE" "$ADMIN/api/audit?limit=50" | grep -q '"action":"node_paired"' \
  && ok "journal d'audit consultable" || bad "audit"

echo "== 5. Plantage -> redémarrage (kill -9)"
# `docker kill` compte comme un arrêt MANUEL (la politique de redémarrage ne joue pas) et le noyau ignore
# un SIGKILL envoyé au PID 1 depuis son propre espace de PID. Un vrai plantage : SIGKILL envoyé depuis
# l'espace de PID de l'hôte, au processus du conteneur.
HOSTPID=$(docker inspect -f '{{.State.Pid}}' nxt-t-master)
docker run --rm --pid=host alpine kill -9 "$HOSTPID"
wait_for "le plantage est détecté puis le conteneur relancé (RestartCount >= 1)" 40 sh -c "[ \$(docker inspect -f '{{.RestartCount}}' nxt-t-master) -ge 1 ]"
wait_for "master de nouveau healthy" 40 health nxt-t-master
check "nouveau processus (PID différent)" sh -c "[ \$(docker inspect -f '{{.State.Pid}}' nxt-t-master) != $HOSTPID ]"
wait_for "l'agent rétablit son tunnel" 40 sh -c "[ \$(docker logs nxt-t-master 2>&1 | grep -c 'tunnel établi') -ge 2 ]"

echo "== 6. Arrêt demandé (SIGTERM) : propre et rapide"
start=$(date +%s)
docker stop -t 20 nxt-t-agent >/dev/null; docker stop -t 20 nxt-t-master >/dev/null
elapsed=$(( $(date +%s) - start ))
[ "$(docker inspect -f '{{.State.ExitCode}}' nxt-t-master)" = "0" ] && ok "master : code de sortie 0" || bad "master : code $(docker inspect -f '{{.State.ExitCode}}' nxt-t-master)"
[ "$(docker inspect -f '{{.State.ExitCode}}' nxt-t-agent)" = "0" ] && ok "agent : code de sortie 0" || bad "agent : code $(docker inspect -f '{{.State.ExitCode}}' nxt-t-agent)"
[ "$elapsed" -lt 15 ] && ok "arrêt en ${elapsed}s (SIGTERM géré, pas de SIGKILL après délai)" || bad "arrêt lent : ${elapsed}s"

echo; echo "Résultat : $PASS réussis, $FAIL échecs"
[ "$FAIL" -eq 0 ]
