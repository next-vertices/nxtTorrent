#!/bin/sh
# nxtTorrent : installation d'un agent sur cette machine (Linux + systemd).
# Script généré par le master. Il contient un code d'appairage À USAGE UNIQUE, valable 15 minutes :
# ne le publiez pas et ne le conservez pas.
#
# Il : installe l'agent, crée un compte système dédié sans privilège, écrit la configuration (0600),
# installe l'unité systemd durcie, appaire l'agent au master (en vérifiant son identité par l'empreinte
# contenue dans le code), puis démarre le service.
set -eu

MASTER='@@MASTER_ADDR@@'
CODE='@@CODE@@'
CLIENT='@@CLIENT@@'
CLIENT_URL='@@CLIENT_URL@@'
CLIENT_USER='@@CLIENT_USER@@'
ROOT='@@ROOT@@'
BIN_URL='@@BIN_URL@@'
BIN_SHA256='@@BIN_SHA256@@'

say() { printf '\n==> %s\n' "$1"; }
die() { printf '\nErreur : %s\n' "$1" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "exécutez ce script en root (sudo)."
[ "$(uname -s)" = "Linux" ] || die "ce script ne gère que Linux ; voir le README pour Windows."
command -v systemctl >/dev/null 2>&1 || die "systemd est requis (systemctl introuvable)."

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# ---- 1. Le binaire de l'agent -------------------------------------------------------------------
say "Binaire de l'agent"
BIN=""
if [ -n "${NXT_AGENT_BIN:-}" ]; then
  BIN="$NXT_AGENT_BIN"
elif [ -n "$BIN_URL" ]; then
  [ -n "$BIN_SHA256" ] || die "URL de téléchargement sans empreinte SHA-256 : refus (le binaire ne serait pas vérifié)."
  # Architecture de CETTE machine : l'URL peut contenir {arch} et l'empreinte être une table `amd64=...,arm64=...`.
  case "$(uname -m)" in
    x86_64|amd64) ARCH=amd64 ;;
    aarch64|arm64) ARCH=arm64 ;;
    *) die "architecture non prise en charge ($(uname -m)) : amd64 et arm64 seulement." ;;
  esac
  URL="$(printf '%s' "$BIN_URL" | sed "s/{arch}/$ARCH/g")"
  case "$BIN_SHA256" in
    *=*)
      WANT=""
      OLDIFS="$IFS"; IFS=','
      for e in $BIN_SHA256; do
        [ "${e%%=*}" = "$ARCH" ] && WANT="${e#*=}"
      done
      IFS="$OLDIFS"
      [ -n "$WANT" ] || die "aucune empreinte SHA-256 pour l'architecture $ARCH : binaire refusé."
      ;;
    *) WANT="$BIN_SHA256" ;;
  esac
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --proto '=https' --tlsv1.2 -o "$TMP/nxt-agent" "$URL" || die "téléchargement impossible."
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$TMP/nxt-agent" "$URL" || die "téléchargement impossible."
  else
    die "ni curl ni wget : installez l'un des deux, ou fournissez le binaire avec NXT_AGENT_BIN=/chemin/nxt-agent."
  fi
  GOT="$(sha256sum "$TMP/nxt-agent" | cut -d' ' -f1)"
  [ "$GOT" = "$WANT" ] || die "empreinte SHA-256 incorrecte (attendue $WANT, obtenue $GOT) : binaire refusé."
  BIN="$TMP/nxt-agent"
  echo "binaire téléchargé et vérifié (SHA-256 conforme)."
else
  for c in ./nxt-agent ./agent /tmp/nxt-agent; do
    if [ -x "$c" ]; then BIN="$c"; break; fi
  done
  [ -n "$BIN" ] || die "aucun binaire d'agent. Copiez-le sur cette machine (scp), puis relancez avec : NXT_AGENT_BIN=/chemin/nxt-agent"
fi
[ -x "$BIN" ] || chmod +x "$BIN" 2>/dev/null || die "binaire non exécutable : $BIN"
"$BIN" --help >/dev/null 2>&1 || die "ce binaire ne s'exécute pas sur cette machine (architecture ?) : $BIN"

# ---- 2. Mot de passe de l'interface du client torrent --------------------------------------------
PASS="${NXT_CLIENT_PASSWORD:-}"
if [ -z "$PASS" ] && [ -t 0 ]; then
  printf "Mot de passe de l'interface de %s (utilisateur « %s »), vide si aucun : " "$CLIENT" "$CLIENT_USER"
  stty -echo 2>/dev/null || true
  read -r PASS || PASS=""
  stty echo 2>/dev/null || true
  printf '\n'
fi

# ---- 3. Compte système dédié -------------------------------------------------------------------------
say "Compte système « nxt-agent » (sans shell, sans mot de passe)"
if ! id nxt-agent >/dev/null 2>&1; then
  if command -v useradd >/dev/null 2>&1; then
    useradd --system --no-create-home --home-dir /var/lib/nxt-agent --shell /usr/sbin/nologin nxt-agent
  elif command -v adduser >/dev/null 2>&1; then
    adduser --system --no-create-home --home /var/lib/nxt-agent --shell /usr/sbin/nologin nxt-agent
  else
    die "impossible de créer le compte nxt-agent (ni useradd ni adduser)."
  fi
fi

install -m 0755 -o root -g root "$BIN" /usr/local/bin/nxt-agent
install -d -m 0700 -o nxt-agent -g nxt-agent /var/lib/nxt-agent
install -d -m 0755 /etc/nxt

# ---- 4. Configuration (secrets en 0600 root:root) --------------------------------------------------
say "Configuration : /etc/nxt/agent.env (0600)"
esc() { printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'; }
(
  umask 077
  {
    printf 'AGENT_ROOT="%s"\n' "$(esc "$ROOT")"
    printf 'AGENT_CLIENT="%s"\n' "$(esc "$CLIENT")"
    printf 'AGENT_CLIENT_URL="%s"\n' "$(esc "$CLIENT_URL")"
    printf 'AGENT_CLIENT_USER="%s"\n' "$(esc "$CLIENT_USER")"
    printf 'AGENT_CLIENT_PASSWORD="%s"\n' "$(esc "$PASS")"
  } > /etc/nxt/agent.env
)
chown root:root /etc/nxt/agent.env
chmod 0600 /etc/nxt/agent.env

# ---- 5. Unité systemd durcie ------------------------------------------------------------------------------
say "Service systemd : nxt-agent"
cat > /etc/systemd/system/nxt-agent.service <<'NXT_UNIT_EOF'
@@UNIT@@
NXT_UNIT_EOF
chmod 0644 /etc/systemd/system/nxt-agent.service
systemctl daemon-reload

# ---- 6. Appairage ---------------------------------------------------------------------------------------------
say "Appairage avec le master ($MASTER)"
if command -v runuser >/dev/null 2>&1; then
  RUNAS="runuser -u nxt-agent --"
else
  RUNAS="su -s /bin/sh nxt-agent -c"
fi
if [ "$RUNAS" = "su -s /bin/sh nxt-agent -c" ]; then
  su -s /bin/sh nxt-agent -c "/usr/local/bin/nxt-agent --data-dir /var/lib/nxt-agent pair '$MASTER' '$CODE'" \
    || die "appairage refusé (code expiré ou déjà utilisé ? master injoignable ?). Générez un nouveau code dans l'interface."
else
  # shellcheck disable=SC2086
  $RUNAS /usr/local/bin/nxt-agent --data-dir /var/lib/nxt-agent pair "$MASTER" "$CODE" \
    || die "appairage refusé (code expiré ou déjà utilisé ? master injoignable ?). Générez un nouveau code dans l'interface."
fi

# ---- 7. Démarrage ------------------------------------------------------------------------------------------------
say "Démarrage"
systemctl enable --now nxt-agent
sleep 3
if systemctl is-active --quiet nxt-agent; then
  echo "L'agent tourne. Il apparaît dans l'interface du master (page Nœuds) dans quelques secondes."
else
  echo "L'agent ne tourne pas. Diagnostic : journalctl -u nxt-agent -e" >&2
  exit 1
fi
echo
echo "Terminé. Le code d'appairage est consommé : vous pouvez supprimer ce script."
