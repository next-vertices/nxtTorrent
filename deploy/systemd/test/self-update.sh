#!/bin/sh
# Essai RÉEL de la mise à jour automatique du master sous systemd, dans un conteneur jetable.
#
# On ne peut pas signer avec la clé de production : les binaires d'essai sont construits à partir d'une COPIE du code où
# seules trois constantes changent (clé publique de confiance, adresse des releases, adresse de téléchargement). Le code
# exécuté est par ailleurs exactement celui du dépôt. Trois binaires : la version installée, une « suivante » saine, et
# une « suivante » en panne (pour exercer le retour arrière).
#
# Prérequis : Docker, openssl, git. Durée : plusieurs minutes (compilation Linux, puis deux installations).
#   sh deploy/systemd/test/self-update.sh
set -eu
# Git Bash : pas de conversion de chemins pour Docker seulement (openssl, lui, en a besoin).
d() { MSYS_NO_PATHCONV=1 docker "$@"; }
cd "$(dirname "$0")/../../.."
ROOT="$PWD"
WORK="$(mktemp -d)"
native() { if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi; }
cleanup() {
    d rm -f nxt-selfupdate >/dev/null 2>&1 || true
    rm -rf "$WORK" 2>/dev/null || true
}
trap cleanup EXIT

CUR="$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p}' Cargo.toml | tr -d '\r')"
GOOD="90.0.0"
BAD="91.0.0"
echo "== version installée $CUR ; suivante saine $GOOD ; suivante en panne $BAD"

echo "== copie du code et clé de test"
mkdir -p "$WORK/src" "$WORK/out" "$WORK/site/dl"
git ls-files -z --cached --others --exclude-standard | tar --null -T - -cf - | tar -xf - -C "$WORK/src"
[ -d ui/dist ] && mkdir -p "$WORK/src/ui" && cp -r ui/dist "$WORK/src/ui/dist"
openssl genpkey -algorithm ED25519 -out "$WORK/test.key" 2>/dev/null
openssl pkey -in "$WORK/test.key" -pubout -outform DER > "$WORK/pub.der" 2>/dev/null
BYTES="$(tail -c 32 "$WORK/pub.der" | od -An -v -tx1 | tr -s ' \n' ' ' | sed 's/^ *//; s/ *$//; s/ /, 0x/g; s/^/0x/')"
KEYID="$(sha256sum "$WORK/pub.der" | cut -c1-8)"
# Les trois seules différences avec le dépôt.
sed -i "/^const PUBLIC_KEY_2026/,/^];/c\\const PUBLIC_KEY_2026: [u8; 32] = [$BYTES];" "$WORK/src/proto/src/release.rs"
sed -i 's#https://api.github.com/repos/next-vertices/nxtTorrent/releases?per_page=30#http://127.0.0.1:8099/releases.json#' "$WORK/src/master/src/updates.rs"
sed -i 's#https://github.com/next-vertices/nxtTorrent/releases/download/#http://127.0.0.1:8099/dl/#' "$WORK/src/master/src/selfupdate.rs"

cat > "$WORK/build.sh" <<'EOF'
set -eu
apk add --no-cache build-base >/dev/null
cd /work/src
export CARGO_TARGET_DIR=/target CARGO_PROFILE_RELEASE_LTO=false CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-3}
version() { sed -i "/^\[workspace\.package\]/,/^rust-version/s/^version = \".*\"/version = \"$1\"/" Cargo.toml; }
cargo build --release -q -p master -p release-tool
cp /target/release/master /work/out/master-current
cp /target/release/release-tool /work/out/release-tool
version "$GOOD"
cargo build --release -q -p master
cp /target/release/master /work/out/master-good
version "$BAD"
# Panne volontaire : la version « en panne » refuse de servir.
sed -i 's/^) -> anyhow::Result<()> {\r\{0,1\}$/&\n    if env!("CARGO_PKG_VERSION") == "91.0.0" { anyhow::bail!("panne volontaire"); }/' master/src/main.rs
grep -q 'panne volontaire' master/src/main.rs
cargo build --release -q -p master
cp /target/release/master /work/out/master-bad
EOF

echo "== compilation Linux (statique)"
d run --rm -e GOOD="$GOOD" -e BAD="$BAD" -v "$(native "$WORK"):/work" \
    -v nxt-selfupdate-target:/target -v nxt-selfupdate-cargo:/usr/local/cargo/registry rust:alpine sh /work/build.sh

echo "== releases signées avec la clé de test"
make_release() { # version binaire
    d="$WORK/site/dl/v$1"; mkdir -p "$d"
    cp "$WORK/out/$2" "$d/nxt-master-linux-amd64"
    d run --rm -v "$(native "$WORK"):/work" alpine:3 \
        /work/out/release-tool manifest --dist "/work/site/dl/v$1" --version "$1" --key-id "$KEYID" --out "/work/site/dl/v$1/manifest.json"
    openssl pkeyutl -sign -inkey "$WORK/test.key" -rawin -in "$d/manifest.json" -out "$d/manifest.json.sig"
}
make_release "$GOOD" master-good
make_release "$BAD" master-bad
echo "[{\"tag_name\":\"v$GOOD\"}]" > "$WORK/site/releases.json"
printf 'FROM nxt-systemd-test\nRUN apt-get update && apt-get install -y --no-install-recommends python3 curl && apt-get clean\n' > "$WORK/Dockerfile"

echo "== machine de test (systemd)"
d build -q -t nxt-systemd-test deploy/systemd/test >/dev/null
d build -q -t nxt-selfupdate-test "$(native "$WORK")" >/dev/null
d rm -f nxt-selfupdate >/dev/null 2>&1 || true
d run -d --name nxt-selfupdate --privileged --cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw --tmpfs /run \
    -v "$(native "$WORK"):/work:ro" -v "$(native "$ROOT"):/repo:ro" nxt-selfupdate-test >/dev/null
for _ in $(seq 1 60); do
    s=$(d exec nxt-selfupdate systemctl is-system-running 2>/dev/null || true)
    case "$s" in running|degraded) break ;; esac
    sleep 1
done

cat > "$WORK/inside.sh" <<'EOF'
set -eu
BIN=/usr/local/bin/nxt-master
DATA=/var/lib/nxt-master
PW='Correct-Horse-9'
fail() { echo "ÉCHEC : $1" >&2; journalctl -u nxt-master -u nxt-master-update --no-pager | tail -60 >&2; cat /var/lib/nxt-update/status.json >&2 2>/dev/null || true; exit 1; }
ok() { echo "✓ $1"; }
api() { # méthode chemin [corps]
    curl -fsS -b /tmp/jar -c /tmp/jar -H 'X-Requested-With: nxt' -H 'Content-Type: application/json' -X "$1" ${3:+-d "$3"} "http://127.0.0.1:7080/api$2"
}
login() {
    for _ in $(seq 1 60); do curl -fsS http://127.0.0.1:7080/api/health >/dev/null 2>&1 && break; sleep 1; done
    rm -f /tmp/jar; api POST /login "{\"username\":\"admin\",\"password\":\"$PW\"}" >/dev/null || fail "connexion"
}
journaled() { # action version : écrite en tâche de fond, quelques secondes de patience
    for _ in $(seq 1 10); do
        api GET "/audit?action=$1" | grep -q "$2" && return 0
        sleep 2
    done
    return 1
}
wait_status() { # état attendu
    for _ in $(seq 1 90); do
        grep -q "\"state\":\"$1\"" /var/lib/nxt-update/status.json 2>/dev/null && return 0
        sleep 3
    done
    fail "état « $1 » jamais atteint"
}

systemd-run --unit=site --quiet python3 -m http.server 8099 --bind 127.0.0.1 --directory /work/site
useradd --system --home-dir "$DATA" --shell /usr/sbin/nologin nxt-master
install -m 0755 -o root -g root /work/out/master-current "$BIN"
install -m 0644 /repo/deploy/systemd/nxt-master.service /etc/systemd/system/nxt-master.service
sed -i 's/\r$//' /etc/systemd/system/nxt-master.service
mkdir -p /etc/nxt && printf 'MASTER_SANS=127.0.0.1\n' > /etc/nxt/master.env
systemctl daemon-reload
systemctl start nxt-master || fail "démarrage"
echo "$PW" | runuser -u nxt-master -- "$BIN" --data-dir "$DATA" admin set-password >/dev/null || fail "compte"
login
api GET /update | grep -q '"supported":false' || fail "sans le service d'installation, la mise à jour doit être indisponible"
ok "master $($BIN --version) démarré ; mise à jour indisponible tant que le service n'est pas installé"

"$BIN" self-update enable || fail "self-update enable"
systemctl restart nxt-master
login
api GET /update | grep -q '"supported":true' || fail "mise à jour attendue disponible"
systemctl is-active --quiet nxt-master-update.path || fail "unité .path inactive"
ok "self-update enable : unités installées, mise à jour disponible"

api POST /update/check '{}' | grep -q '"available":true' || fail "version suivante non annoncée"
api POST /update/install "{\"version\":\"$GOOD\",\"password\":\"$PW\"}" >/dev/null || fail "demande d'installation"
wait_status installed
"$BIN" --version | grep -qw "$GOOD" || fail "binaire non remplacé"
sleep 3
systemctl is-active --quiet nxt-master || fail "service inactif après mise à jour"
login
journaled update_installed "$GOOD" || fail "installation non journalisée"
[ "$(stat -c '%U:%a' "$BIN")" = "root:755" ] || fail "droits du binaire : $(stat -c '%U:%a' "$BIN")"
[ "$(stat -c '%U' "$DATA/master.db")" = nxt-master ] || fail "la base a changé de propriétaire"
ok "installation réelle : $GOOD en service, compte conservé, journalisée"

echo "[{\"tag_name\":\"v$BAD\"},{\"tag_name\":\"v$GOOD\"}]" > /tmp/releases.json
systemctl stop site
cp -r /work/site /tmp/site && cp /tmp/releases.json /tmp/site/releases.json
systemd-run --unit=site2 --quiet python3 -m http.server 8099 --bind 127.0.0.1 --directory /tmp/site
sleep 61 # une vérification à la main par minute au plus
login
api POST /update/check '{}' | grep -q "\"version\":\"$BAD\"" || fail "version en panne non annoncée"
api POST /update/install "{\"version\":\"$BAD\",\"password\":\"$PW\"}" >/dev/null || fail "demande d'installation"
wait_status rolled_back
"$BIN" --version | grep -qw "$GOOD" || fail "l'ancien binaire devait revenir"
sleep 3
systemctl is-active --quiet nxt-master || fail "service inactif après retour arrière"
login
api GET '/audit?action=update_requested' | grep -q "$GOOD" || fail "base restaurée incomplète"
journaled update_rolled_back "$BAD" || fail "retour arrière non journalisé"
[ "$(stat -c '%U' "$DATA/master.db")" = nxt-master ] || fail "la base a changé de propriétaire"
ok "version en panne : retour automatique à $GOOD (binaire et base), journalisé"

"$BIN" self-update disable >/dev/null || fail "self-update disable"
systemctl restart nxt-master
login
api GET /update | grep -q '"supported":false' || fail "désactivation sans effet"
ok "self-update disable : unités retirées"
EOF

echo "== essai"
d exec -e GOOD="$GOOD" -e BAD="$BAD" nxt-selfupdate sh -c "sed 's/\r$//' /work/inside.sh > /tmp/inside.sh && sh /tmp/inside.sh"
echo "Résultat : OK"
