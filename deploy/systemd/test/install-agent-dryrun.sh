#!/bin/sh
# Exécute le script d'installation d'agent (master/assets/install-agent.sh) dans un conteneur Debian jetable, avec un
# binaire d'agent et un systemctl FACTICES : on vérifie ce que le script écrit et appelle (compte, fichiers, droits,
# appairage, démarrage) sans toucher à la machine hôte. Prérequis : Docker.
#   sh deploy/systemd/test/install-agent-dryrun.sh
set -eu
cd "$(dirname "$0")/../../.."
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

# Gabarit -> script (mêmes remplacements que master/src/admin/install.rs).
python3 - "$OUT" <<'PY'
import sys
t = open('master/assets/install-agent.sh', encoding='utf-8').read()
unit = open('deploy/systemd/nxt-agent.service', encoding='utf-8').read().rstrip('\n')
for k, v in {
    'MASTER_ADDR': 'master.exemple.org:7443', 'CODE': 'CODE-TEST-1234', 'CLIENT': 'transmission',
    'CLIENT_URL': 'http://127.0.0.1:9091', 'CLIENT_USER': 'admin', 'ROOT': '/srv/mes downloads',
    'BIN_URL': '', 'BIN_SHA256': '',
}.items():
    t = t.replace('@@%s@@' % k, v)
t = t.replace('@@UNIT@@', unit)
open(sys.argv[1] + '/install.sh', 'w', encoding='utf-8', newline='\n').write(t)
PY

cat > "$OUT/inside.sh" <<'IN'
set -eu
mkdir -p /stub
: > /tmp/calls.log && chmod 666 /tmp/calls.log   # écrit aussi par le compte nxt-agent (pair)
# Agent factice : `--help` réussit, `pair` journalise ses arguments, tout le reste est ignoré.
cat > /stub/nxt-agent <<'EOF'
#!/bin/sh
case "$*" in
  *--help*) exit 0 ;;
  *" pair "*) echo "PAIR user=$(id -un) args=$*" >> /tmp/calls.log; exit 0 ;;
esac
EOF
chmod +x /stub/nxt-agent
# systemctl factice : journalise les appels.
cat > /usr/local/bin/systemctl <<'EOF'
#!/bin/sh
echo "SYSTEMCTL $*" >> /tmp/calls.log
case "$*" in *is-active*) exit 0 ;; esac
EOF
chmod +x /usr/local/bin/systemctl

export NXT_AGENT_BIN=/stub/nxt-agent NXT_CLIENT_PASSWORD='pa"ss\wo$rd'
sh /t/install.sh

echo "---- vérifications"
fail() { echo "ÉCHEC : $1" >&2; exit 1; }
id nxt-agent >/dev/null || fail "compte nxt-agent absent"
[ "$(getent passwd nxt-agent | cut -d: -f7)" = "/usr/sbin/nologin" ] || fail "le compte doit avoir /usr/sbin/nologin"
[ -x /usr/local/bin/nxt-agent ] || fail "binaire non installé"
[ "$(stat -c '%a %U' /etc/nxt/agent.env)" = "600 root" ] || fail "agent.env doit être 0600 root : $(stat -c '%a %U' /etc/nxt/agent.env)"
[ "$(stat -c '%a %U' /var/lib/nxt-agent)" = "700 nxt-agent" ] || fail "dossier d'état : $(stat -c '%a %U' /var/lib/nxt-agent)"
grep -q '^AGENT_ROOT="/srv/mes downloads"$' /etc/nxt/agent.env || fail "AGENT_ROOT"
grep -q '^AGENT_CLIENT_URL="http://127.0.0.1:9091"$' /etc/nxt/agent.env || fail "AGENT_CLIENT_URL"
grep -qF 'AGENT_CLIENT_PASSWORD="pa\"ss\\wo$rd"' /etc/nxt/agent.env || fail "mot de passe mal échappé : $(grep PASSWORD /etc/nxt/agent.env)"
grep -q '^User=nxt-agent$' /etc/systemd/system/nxt-agent.service || fail "unité systemd"
grep -q '^NoNewPrivileges=yes$' /etc/systemd/system/nxt-agent.service || fail "durcissement de l'unité"
grep -q 'PAIR user=nxt-agent args=.*--data-dir /var/lib/nxt-agent pair master.exemple.org:7443 CODE-TEST-1234' /tmp/calls.log || fail "appairage : $(cat /tmp/calls.log)"
grep -q 'SYSTEMCTL enable --now nxt-agent' /tmp/calls.log || fail "démarrage"
# Rejouer le script ne casse rien (compte déjà présent).
sh /t/install.sh >/dev/null
echo "OK : script d'installation vérifié (compte, droits 0600/0700, échappement du mot de passe, unité, appairage sous nxt-agent, démarrage, idempotence)"
IN

# Git Bash (Windows) : chemin hôte natif pour Docker et pas de conversion automatique de /t.
HOST_OUT="$OUT"
if command -v cygpath >/dev/null 2>&1; then HOST_OUT="$(cygpath -m "$OUT")"; fi
MSYS_NO_PATHCONV=1 docker run --rm -v "$HOST_OUT:/t:ro" nxt-systemd-test bash /t/inside.sh
