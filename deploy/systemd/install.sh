#!/bin/sh
# Installe les unités systemd de l'orchestrateur (à exécuter en root).
#   ./install.sh [--bin-dir DIR] master|agent|both
# DIR contient les binaires `master` et `agent` (défaut : ../../target/release).
set -eu

BIN_DIR="$(dirname "$0")/../../target/release"
if [ "${1:-}" = "--bin-dir" ]; then BIN_DIR="$2"; shift 2; fi
WHAT="${1:-}"
case "$WHAT" in master|agent|both) ;; *) echo "usage: $0 [--bin-dir DIR] master|agent|both" >&2; exit 2 ;; esac
[ "$(id -u)" -eq 0 ] || { echo "à exécuter en root" >&2; exit 1; }

HERE="$(cd "$(dirname "$0")" && pwd)"
install -d -m 0755 /etc/nxt
install -d -m 0755 /etc/sysusers.d
install -m 0644 "$HERE/nxt.sysusers.conf" /etc/sysusers.d/nxt.conf
systemd-sysusers /etc/sysusers.d/nxt.conf

install_one() {
  name="$1"
  [ -f "$BIN_DIR/$name" ] || { echo "binaire introuvable : $BIN_DIR/$name" >&2; exit 1; }
  install -m 0755 "$BIN_DIR/$name" "/usr/local/bin/nxt-$name"
  # Dossier d'état créé ici (et non seulement par StateDirectory= au premier démarrage) : l'appairage
  # manuel `sudo -u nxt-<x> …` y écrit AVANT que le service n'existe.
  install -d -m 0700 -o "nxt-$name" -g "nxt-$name" "/var/lib/nxt-$name"
  install -m 0644 "$HERE/nxt-$name.service" "/etc/systemd/system/nxt-$name.service"
  # Ne jamais écraser une configuration existante ; fichier secret : 0600 root:root.
  if [ ! -e "/etc/nxt/$name.env" ]; then
    install -m 0600 -o root -g root "$HERE/$name.env.example" "/etc/nxt/$name.env"
    echo "créé /etc/nxt/$name.env : à éditer avant le démarrage"
  fi
}

case "$WHAT" in
  master) install_one master ;;
  agent)  install_one agent ;;
  both)   install_one master; install_one agent ;;
esac
systemctl daemon-reload
echo "Installé. Ensuite :"
case "$WHAT" in master|both)
  echo "  éditer /etc/nxt/master.env puis : systemctl enable --now nxt-master"
  echo "  code d'appairage : sudo -u nxt-master nxt-master --data-dir /var/lib/nxt-master pair-code --label ssd" ;;
esac
case "$WHAT" in agent|both)
  echo "  appairage : sudo -u nxt-agent nxt-agent --data-dir /var/lib/nxt-agent pair <master:7443> <CODE>"
  echo "  éditer /etc/nxt/agent.env puis : systemctl enable --now nxt-agent" ;;
esac
