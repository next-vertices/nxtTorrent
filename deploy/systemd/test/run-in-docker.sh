#!/bin/sh
# Exerce les unités systemd dans un conteneur dont systemd est le PID 1.
# Prérequis : binaires statiques dans dist/linux (master, agent) et Docker.
#   cargo build --release --target x86_64-unknown-linux-musl -p master -p agent
#   mkdir -p dist/linux && cp target/x86_64-unknown-linux-musl/release/master target/x86_64-unknown-linux-musl/release/agent dist/linux/
#   sh deploy/systemd/test/run-in-docker.sh
set -eu
cd "$(dirname "$0")/../../.."
[ -f dist/linux/master ] && [ -f dist/linux/agent ] || { echo "binaires absents dans dist/linux" >&2; exit 1; }

docker build -q -t nxt-systemd-test deploy/systemd/test >/dev/null
docker rm -f nxt-systemd >/dev/null 2>&1 || true
trap 'docker rm -f nxt-systemd >/dev/null 2>&1 || true' EXIT
docker run -d --name nxt-systemd --privileged --cgroupns=host \
  -v /sys/fs/cgroup:/sys/fs/cgroup:rw --tmpfs /run --tmpfs /tmp \
  -v "$PWD:/src:ro" nxt-systemd-test >/dev/null

# Attente du démarrage de systemd.
for _ in $(seq 1 60); do
  s=$(docker exec nxt-systemd systemctl is-system-running 2>/dev/null || true)
  case "$s" in running|degraded) break ;; esac
  sleep 1
done
docker exec nxt-systemd bash /src/deploy/systemd/test/run-tests.sh
