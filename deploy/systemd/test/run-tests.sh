#!/bin/bash
# Exécuté DANS le conteneur systemd (root) : installe les unités et les exerce pour de vrai.
#   /src = dépôt (lecture seule) ; /src/dist/linux = binaires musl statiques.
set -uo pipefail
PASS=0; FAIL=0
ok()   { echo "  [OK]   $*"; PASS=$((PASS+1)); }
bad()  { echo "  [FAIL] $*"; FAIL=$((FAIL+1)); }
check() { local what="$1"; shift; if "$@" >/dev/null 2>&1; then ok "$what"; else bad "$what"; fi; }
wait_for() { local what="$1" secs="$2"; shift 2; for _ in $(seq 1 $((secs*2))); do "$@" >/dev/null 2>&1 && { ok "$what"; return 0; }; sleep 0.5; done; bad "$what (délai ${secs}s)"; return 1; }
active()  { [ "$(systemctl is-active "$1")" = "active" ]; }
listening() { ss -ltn "sport = :$1" | grep -q LISTEN; }
mainpid() { systemctl show -p MainPID --value "$1"; }

echo "== 1. Vérification statique des unités"
sh /src/deploy/systemd/install.sh --bin-dir /src/dist/linux both >/dev/null || { bad "install.sh"; exit 1; }
ok "install.sh"
check "systemd-analyze verify (master)" systemd-analyze verify /etc/systemd/system/nxt-master.service
check "systemd-analyze verify (agent)"  systemd-analyze verify /etc/systemd/system/nxt-agent.service
echo "-- exposition (systemd-analyze security) :"
for u in nxt-master nxt-agent; do
  systemd-analyze security --offline=true --no-pager "/etc/systemd/system/$u.service" 2>&1 | tail -1
done

echo "== 2. Configuration et démarrage du master"
sed -i 's|^MASTER_LISTEN=.*|MASTER_LISTEN=127.0.0.1:7443|; s|^MASTER_SANS=.*|MASTER_SANS=127.0.0.1|' /etc/nxt/master.env
mkdir -p /srv/downloads
sed -i 's|^AGENT_ROOT=.*|AGENT_ROOT=/srv/downloads|; s|^AGENT_CLIENT_URL=.*|AGENT_CLIENT_URL=http://127.0.0.1:18080|' /etc/nxt/agent.env
check "env master en 0600 root" test "$(stat -c '%a %U' /etc/nxt/master.env)" = "600 root"
check "env agent en 0600 root"  test "$(stat -c '%a %U' /etc/nxt/agent.env)" = "600 root"
systemctl start nxt-master
wait_for "master actif" 20 active nxt-master
wait_for "master à l'écoute (127.0.0.1:7443)" 15 listening 7443
check "API admin à l'écoute sur 127.0.0.1:7080 uniquement (pas sur toutes les interfaces)" bash -c "ss -ltn 'sport = :7080' | grep -q '127.0.0.1:7080' && ! ss -ltn 'sport = :7080' | grep -qE '(0\.0\.0\.0|\*|\[::\]):7080'"
check "dossier d'état 0700 nxt-master" test "$(stat -c '%a %U' /var/lib/nxt-master)" = "700 nxt-master"
check "clé de CA en 0600" test "$(stat -c '%a' /var/lib/nxt-master/ca.key)" = "600"

echo "== 3. Isolation du processus (lue dans /proc)"
P=$(mainpid nxt-master)
check "NoNewPrivs=1"            grep -q '^NoNewPrivs:[[:space:]]*1' /proc/$P/status
check "aucune capacité (CapEff=0)" grep -q '^CapEff:[[:space:]]*0000000000000000' /proc/$P/status
check "aucune capacité (CapBnd=0)" grep -q '^CapBnd:[[:space:]]*0000000000000000' /proc/$P/status
check "filtre seccomp actif"    grep -q '^Seccomp:[[:space:]]*2' /proc/$P/status
check "s'exécute sous nxt-master" test "$(ps -o user= -p "$P" | tr -d ' ')" = "nxt-master"
check "ne voit pas /home (ProtectHome)" bash -c "nsenter -t $P -m ls /home | wc -l | grep -qx 0"
check "système de fichiers en lecture seule" bash -c "! nsenter -t $P -m -- sh -c 'touch /usr/x' 2>/dev/null"

echo "== 4. Appairage manuel puis démarrage de l'agent"
CODE=$(sudo -u nxt-master nxt-master --data-dir /var/lib/nxt-master pair-code --label ssd 2>/dev/null)
[ -n "$CODE" ] && ok "code d'appairage généré" || bad "code d'appairage"
sudo -u nxt-agent nxt-agent --data-dir /var/lib/nxt-agent pair 127.0.0.1:7443 "$CODE" >/dev/null 2>&1 && ok "agent appairé" || bad "appairage de l'agent"
check "clé de l'agent en 0600" test "$(stat -c '%a' /var/lib/nxt-agent/agent.key)" = "600"
systemctl start nxt-agent
wait_for "agent actif" 20 active nxt-agent
wait_for "tunnel établi (journal du master)" 20 bash -c "journalctl -u nxt-master --no-pager | grep -q 'tunnel établi'"
AP=$(mainpid nxt-agent)
check "agent: aucune capacité"  grep -q '^CapEff:[[:space:]]*0000000000000000' /proc/$AP/status
check "agent: ne peut pas écrire dans le dossier de téléchargement (lecture seule)" bash -c "! nsenter -t $AP -m -- sh -c 'touch /srv/downloads/x' 2>/dev/null"

echo "== 5. Plantage -> redémarrage automatique (kill -9)"
OLD=$(mainpid nxt-master); kill -9 "$OLD"
wait_for "master relancé avec un nouveau PID" 30 bash -c "[ \"\$(systemctl is-active nxt-master)\" = active ] && [ \"\$(systemctl show -p MainPID --value nxt-master)\" != \"$OLD\" ] && [ \"\$(systemctl show -p MainPID --value nxt-master)\" != 0 ]"
wait_for "master de nouveau à l'écoute" 15 listening 7443
wait_for "l'agent rétablit son tunnel" 40 bash -c "[ \$(journalctl -u nxt-master --no-pager | grep -c 'tunnel établi') -ge 2 ]"
check "agent resté actif" active nxt-agent
OLDA=$(mainpid nxt-agent); kill -9 "$OLDA"
wait_for "agent relancé avec un nouveau PID" 30 bash -c "[ \"\$(systemctl is-active nxt-agent)\" = active ] && [ \"\$(systemctl show -p MainPID --value nxt-agent)\" != \"$OLDA\" ]"

echo "== 6. Arrêt demandé : propre (SIGTERM, code 0) et PAS relancé"
systemctl stop nxt-master
R=$(systemctl show -p Result --value nxt-master); S=$(systemctl show -p ExecMainStatus --value nxt-master)
[ "$R" = "success" ] && ok "Result=success" || bad "Result=$R"
[ "$S" = "0" ] && ok "ExecMainStatus=0 (sortie propre sur SIGTERM)" || bad "ExecMainStatus=$S"
sleep 8
[ "$(systemctl is-active nxt-master)" = "inactive" ] && ok "pas de redémarrage après un arrêt volontaire" || bad "relancé après stop"
systemctl stop nxt-agent
[ "$(systemctl show -p Result --value nxt-agent)" = "success" ] && ok "agent: arrêt propre" || bad "agent: arrêt non propre ($(systemctl show -p Result,ExecMainStatus nxt-agent | tr '\n' ' '))"

echo "== 7. Journaux"
check "aucune erreur dans le journal du master" bash -c "! journalctl -u nxt-master --no-pager -p err | grep -v '^-- ' | grep -q ."

echo
echo "Résultat : $PASS réussis, $FAIL échecs"
[ "$FAIL" -eq 0 ]
