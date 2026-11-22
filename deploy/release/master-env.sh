#!/bin/sh
# Produit la configuration du master pour l'installation d'agents en une commande, à partir du fichier SHA256SUMS d'une
# version publiée. Les empreintes viennent de la release (vérifiez-la : `gh attestation verify <fichier> --repo <dépôt>`).
#
#   sh deploy/release/master-env.sh SHA256SUMS https://github.com/<utilisateur>/<dépôt>/releases/download/v1.0.0
#
# Sortie : deux lignes à placer dans l'environnement du master (compose, /etc/nxt/master.env…) :
#   MASTER_AGENT_BINARY_URL=<base>/nxt-agent-linux-{arch}
#   MASTER_AGENT_BINARY_SHA256=amd64=<sha>,arm64=<sha>
# `{arch}` vaut amd64 ou arm64 selon la machine où l'agent s'installe : un parc mixte (VPS, Raspberry Pi) fonctionne.
set -eu

[ $# -eq 2 ] || { echo "usage : $0 SHA256SUMS URL_DE_BASE_DE_LA_RELEASE" >&2; exit 2; }
SUMS="$1"
BASE="${2%/}"
[ -r "$SUMS" ] || { echo "fichier illisible : $SUMS" >&2; exit 1; }
case "$BASE" in https://*) ;; *) echo "l'URL doit être en https://" >&2; exit 1 ;; esac
case "$BASE" in *[!A-Za-z0-9:/._~%+-]*) echo "l'URL contient des caractères non autorisés" >&2; exit 1 ;; esac

TABLE=""
for arch in amd64 arm64; do
  # Une ligne `<64 hexadécimaux>  nxt-agent-linux-<arch>` (format de sha256sum).
  SHA="$(awk -v n="nxt-agent-linux-$arch" '$2 == n || $2 == "*" n { print $1 }' "$SUMS" | head -n 1)"
  [ -z "$SHA" ] && continue
  printf '%s' "$SHA" | grep -Eq '^[0-9a-f]{64}$' || { echo "empreinte invalide pour $arch : $SHA" >&2; exit 1; }
  TABLE="${TABLE:+$TABLE,}$arch=$SHA"
done
[ -n "$TABLE" ] || { echo "aucun binaire d'agent Linux (nxt-agent-linux-amd64 ou -arm64) dans $SUMS" >&2; exit 1; }

echo "MASTER_AGENT_BINARY_URL=$BASE/nxt-agent-linux-{arch}"
echo "MASTER_AGENT_BINARY_SHA256=$TABLE"
