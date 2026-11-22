#!/bin/sh
# Teste le CHOIX D'ARCHITECTURE du script d'installation d'agent (master/assets/install-agent.sh) dans des conteneurs
# jetables : un `curl` et un `uname` FACTICES permettent de vérifier, sans réseau, quelle URL est demandée, quelle
# empreinte est exigée, et que tout binaire douteux est refusé. Prérequis : Docker et l'image nxt-systemd-test.
#   sh deploy/systemd/test/install-agent-arch.sh
set -eu
cd "$(dirname "$0")/../../.."
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

# Deux « binaires » distincts (un par architecture) et leurs empreintes.
printf '#!/bin/sh\ncase "$*" in *--help*) exit 0 ;; *" pair "*) exit 0 ;; esac\n# amd64\n' > "$OUT/agent-amd64"
printf '#!/bin/sh\ncase "$*" in *--help*) exit 0 ;; *" pair "*) exit 0 ;; esac\n# arm64\n' > "$OUT/agent-arm64"
SHA_AMD="$(sha256sum "$OUT/agent-amd64" | cut -d' ' -f1)"
SHA_ARM="$(sha256sum "$OUT/agent-arm64" | cut -d' ' -f1)"
BAD="$(printf '%064d' 0)"

# Gabarit -> script, pour une table d'empreintes donnée (mêmes remplacements que master/src/admin/install.rs).
render() { # $1 = fichier de sortie, $2 = URL, $3 = empreintes
python3 - "$1" "$2" "$3" <<'PY'
import sys
out, url, sha = sys.argv[1:4]
t = open('master/assets/install-agent.sh', encoding='utf-8').read()
unit = open('deploy/systemd/nxt-agent.service', encoding='utf-8').read().rstrip('\n')
for k, v in {
    'MASTER_ADDR': 'master.exemple.org:7443', 'CODE': 'CODE-TEST-1234', 'CLIENT': 'transmission',
    'CLIENT_URL': 'http://127.0.0.1:9091', 'CLIENT_USER': 'admin', 'ROOT': '/srv/dl',
    'BIN_URL': url, 'BIN_SHA256': sha,
}.items():
    t = t.replace('@@%s@@' % k, v)
t = t.replace('@@UNIT@@', unit)
open(out, 'w', encoding='utf-8', newline='\n').write(t)
PY
}
URL='https://exemple.org/releases/nxt-agent-linux-{arch}'
render "$OUT/ok.sh"      "$URL" "amd64=$SHA_AMD,arm64=$SHA_ARM"
render "$OUT/missing.sh" "$URL" "amd64=$SHA_AMD"
render "$OUT/badsha.sh"  "$URL" "amd64=$SHA_AMD,arm64=$BAD"
render "$OUT/single.sh"  "https://exemple.org/releases/nxt-agent-linux-amd64" "$SHA_AMD"

cat > "$OUT/inside.sh" <<'IN'
set -eu
# `uname -m` configurable ; `curl` factice qui sert le binaire de l'architecture demandée et journalise l'URL ; systemctl factice.
cat > /usr/local/bin/uname <<'EOF'
#!/bin/sh
if [ "${1:-}" = "-m" ]; then echo "$FAKE_ARCH"; else /usr/bin/uname "$@"; fi
EOF
cat > /usr/local/bin/curl <<'EOF'
#!/bin/sh
URL=""; OUTF=""
while [ $# -gt 0 ]; do case "$1" in -o) OUTF="$2"; shift 2 ;; http*) URL="$1"; shift ;; *) shift ;; esac; done
echo "CURL $URL" >> /tmp/calls.log
case "$URL" in
  *amd64) cat /t/agent-amd64 > "$OUTF" ;;
  *arm64) cat /t/agent-arm64 > "$OUTF" ;;
  *) exit 22 ;;
esac
EOF
printf '#!/bin/sh\necho "SYSTEMCTL $*" >> /tmp/calls.log\ncase "$*" in *is-active*) exit 0 ;; esac\n' > /usr/local/bin/systemctl
chmod +x /usr/local/bin/uname /usr/local/bin/curl /usr/local/bin/systemctl
: > /tmp/calls.log && chmod 666 /tmp/calls.log
fail() { echo "ÉCHEC ($SCENARIO) : $1" >&2; cat /tmp/calls.log >&2; exit 1; }
unset NXT_AGENT_BIN || true
export NXT_CLIENT_PASSWORD=x

case "$SCENARIO" in
  arm64)   # machine arm64 : URL arm64, empreinte arm64, binaire arm64 installé
    FAKE_ARCH=aarch64 sh /t/ok.sh >/dev/null 2>&1 || fail "l'installation aurait dû réussir"
    grep -q 'CURL https://exemple.org/releases/nxt-agent-linux-arm64$' /tmp/calls.log || fail "URL arm64 attendue"
    cmp -s /usr/local/bin/nxt-agent /t/agent-arm64 || fail "le binaire arm64 devait être installé" ;;
  amd64)
    FAKE_ARCH=x86_64 sh /t/ok.sh >/dev/null 2>&1 || fail "l'installation aurait dû réussir"
    grep -q 'CURL https://exemple.org/releases/nxt-agent-linux-amd64$' /tmp/calls.log || fail "URL amd64 attendue"
    cmp -s /usr/local/bin/nxt-agent /t/agent-amd64 || fail "le binaire amd64 devait être installé" ;;
  unsupported)   # armv7l : refus net, aucun téléchargement
    if FAKE_ARCH=armv7l sh /t/ok.sh >/tmp/out.txt 2>&1; then fail "une architecture non prise en charge doit être refusée"; fi
    grep -q "architecture non prise en charge" /tmp/out.txt || fail "message attendu : $(cat /tmp/out.txt)"
    ! grep -q CURL /tmp/calls.log || fail "rien ne doit être téléchargé"
    [ ! -e /usr/local/bin/nxt-agent ] || fail "rien ne doit être installé" ;;
  missing)       # empreinte absente pour cette architecture : refus AVANT tout téléchargement
    if FAKE_ARCH=aarch64 sh /t/missing.sh >/tmp/out.txt 2>&1; then fail "empreinte absente : refus attendu"; fi
    grep -q "aucune empreinte SHA-256 pour l'architecture arm64" /tmp/out.txt || fail "message attendu : $(cat /tmp/out.txt)"
    ! grep -q CURL /tmp/calls.log || fail "rien ne doit être téléchargé sans empreinte"
    [ ! -e /usr/local/bin/nxt-agent ] || fail "rien ne doit être installé" ;;
  badsha)        # binaire servi != empreinte attendue : refusé, non installé
    if FAKE_ARCH=aarch64 sh /t/badsha.sh >/tmp/out.txt 2>&1; then fail "empreinte fausse : refus attendu"; fi
    grep -q "empreinte SHA-256 incorrecte" /tmp/out.txt || fail "message attendu : $(cat /tmp/out.txt)"
    [ ! -e /usr/local/bin/nxt-agent ] || fail "un binaire non vérifié ne doit JAMAIS être installé" ;;
  single)        # forme historique : une URL, une empreinte
    FAKE_ARCH=x86_64 sh /t/single.sh >/dev/null 2>&1 || fail "la forme à une seule architecture doit rester valide"
    cmp -s /usr/local/bin/nxt-agent /t/agent-amd64 || fail "binaire attendu" ;;
esac
echo "OK : $SCENARIO"
IN

HOST_OUT="$OUT"
if command -v cygpath >/dev/null 2>&1; then HOST_OUT="$(cygpath -m "$OUT")"; fi
for s in arm64 amd64 unsupported missing badsha single; do
  MSYS_NO_PATHCONV=1 docker run --rm -e SCENARIO="$s" -v "$HOST_OUT:/t:ro" nxt-systemd-test bash /t/inside.sh
done
echo "OK : choix d'architecture du script d'installation vérifié (6 scénarios)"
