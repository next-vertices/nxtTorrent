# nxtTorrent (POC)

A multi-instance torrent client orchestrator: one **master** (the source of truth) and **agents** (one per machine)
connected by an mTLS gRPC tunnel. Agents drive an existing client — **qBittorrent, Transmission, Deluge or ruTorrent** —
and never open a control port.

> **Status:** pre-release (`0.1.0-rc.x`). It works end to end and is tested against real clients, but it has not had
> field use yet. Expect rough edges and read [SECURITY.md](SECURITY.md) before exposing anything.

## Quick start

```bash
# Master: the CA and the database are created on first run (--san = name/IP the agents use to reach it)
master --data-dir ./data pair-code --label ssd --label vpn-ch     # one-time code (15 min), labels set by the admin
master --data-dir ./data serve --listen 0.0.0.0:7443 --san master.example.org

# On each node: one-time pairing, then the tunnel
agent --data-dir ./agent-data pair master.example.org:7443 CODE-XXXX...
AGENT_CLIENT_PASSWORD=... agent --data-dir ./agent-data run --root /downloads --client transmission
```

`--client`: `qbittorrent` (default, `:8080`), `transmission` (`:9091`), `deluge` (`:8112`), `rutorrent` (`:8080`).
The client's web interface must be on the **loopback interface** (`--client-url` for another port).
Lost identity or expired certificate: `agent pair --force …` with a new code.

## Web interface

Served by the master on the admin API address (`http://127.0.0.1:7080/` by default) and embedded in the binary
(in-house design system, light/dark/system theme, no component library). Pages: **Overview** (cluster KPIs, bandwidth
and torrent history from 1 h to 30 d, attention points), **Torrents** (table filtered, sorted and paginated
server-side, built for thousands of rows; bulk selection, detail panel, adding by drag-and-drop `.torrent` files or
magnet links), **Nodes** (editable name, labels and location, network health with a diagnosis — is the listening
port reachable? —, per-node work comparison), **Pairing**, **Log** (audit events as readable sentences, by category
and severity). Updates arrive live over WebSocket (automatic fallback to a refresh every 5 s). To build it (Node 22):

```bash
cd ui && npm ci && npm run build && cd .. && cargo build --release -p master   # without this, a fallback page is shown
cd ui && npm run dev                                                           # development (proxy to 127.0.0.1:7080)
cargo build -p master -p agent && cargo build -p testkit --example mock_qbit
node ui/e2e/smoke.mjs                                                          # end-to-end test in a real browser (400 simulated torrents)
```

## Admin API

A listener separate from the agents' one, on **127.0.0.1:7080** by default (`--admin-listen` / `MASTER_ADMIN_LISTEN`);
a non-local address is refused unless `--admin-allow-remote` is given. The account is created on first start; the
strong password is hashed with Argon2id; brute-force protection (per IP and per account), short sessions, CSRF
protection, every action audited. Details: [docs/decisions/0010-admin-api.md](docs/decisions/0010-admin-api.md).

```bash
master admin set-password --username admin                      # (or POST /api/setup from the local machine)
H='-H X-Requested-With:nxt -H Content-Type:application/json'     # anti-CSRF header + JSON
curl -c jar $H -d '{"username":"admin","password":"..."}' http://127.0.0.1:7080/api/login
curl -b jar http://127.0.0.1:7080/api/nodes                      # nodes, state, labels, disk...
curl -b jar $H -d '{"name":"Living-room NAS","labels":["ssd"]}' http://127.0.0.1:7080/api/pairing-codes   # pairing code (shown once); name and labels optional
curl -b jar $H -d '{"name":"Seedbox 2"}' http://127.0.0.1:7080/api/nodes/<id>/rename                      # rename a node
curl -b jar $H -d '{"infohash":"<40 hex>","save_subdir":"movies","requires":["vpn-ch"]}' http://127.0.0.1:7080/api/torrents
curl -b jar $H -H 'Content-Type: application/x-bittorrent' --data-binary @movie.torrent \
  'http://127.0.0.1:7080/api/torrents/file?save_subdir=movies'    # .torrent file (4 MB max; required for private trackers)
curl -b jar 'http://127.0.0.1:7080/api/torrents?q=ubuntu&status=seeding&sort=size&dir=desc&limit=50&offset=0'   # paginated/filtered list
curl -b jar $H -d '{"action":"pause","infohashes":["<40 hex>"]}' http://127.0.0.1:7080/api/torrents/bulk       # bulk pause/resume/remove
curl -b jar 'http://127.0.0.1:7080/api/torrents?tracker=tracker.example.org&sort=peers&dir=desc'   # filter/sort by tracker, peers, swarm
curl -b jar 'http://127.0.0.1:7080/api/nodes/stats?range=24h'    # node comparison (volumes, rates, availability)
curl -b jar http://127.0.0.1:7080/api/stats                      # cluster KPIs; /api/metrics?range=24h: history
curl -b jar 'http://127.0.0.1:7080/api/audit?limit=50&category=security'   # readable log (title, severity, actor_label…)
```

### Install an agent with one command

"Pairing" page → "Install on a Linux server": choose the server's torrent client, copy the generated command and paste
it over SSH. It creates an unprivileged system account, writes the configuration with mode 0600, installs the hardened
systemd service, pairs the agent and starts it; the client's password is asked for on the server. For the script to
download the agent itself (with SHA-256 verification), set `MASTER_AGENT_BINARY_URL` and `MASTER_AGENT_BINARY_SHA256`
on the master; otherwise copy the binary and add `NXT_AGENT_BIN=/path/nxt-agent`.
For a **mixed** fleet (amd64 VPS, arm64 Raspberry Pi), the URL contains `{arch}` and the checksum is a table
`amd64=<sha>,arm64=<sha>`: `sh deploy/release/master-env.sh SHA256SUMS <release URL>` writes those two lines for you.
Details: [docs/decisions/0015-installation-agent-une-commande.md](docs/decisions/0015-installation-agent-une-commande.md).

### Backup and restore

"My account" → Backup: an **encrypted** file (passphrase, never recoverable) with the database, the CA and the keys.
From the command line, scriptable: `MASTER_BACKUP_PASSPHRASE=… master --data-dir … admin backup --out master.nxtbak`
then, with the master stopped, `master --data-dir … admin restore --from master.nxtbak [--force]`. Agents reconnect
unchanged (same CA). Details: [docs/decisions/0017-sauvegarde-restauration.md](docs/decisions/0017-sauvegarde-restauration.md).

### Alerts

"Alerts" page: add an **ntfy** topic or a JSON **webhook**, then choose when to be notified (node offline, port closed,
disk almost full, torrents in error) with an anti-flapping delay; a recovery message follows. A channel's address is
encrypted and never shown again. Details: [docs/decisions/0016-alertes.md](docs/decisions/0016-alertes.md).

### Two-factor authentication

"My account" page: TOTP (Aegis, Google Authenticator, 1Password, Bitwarden…) with 10 single-use backup codes. The secret
is encrypted at rest (key `totp.key`, mode 0600, next to `master.db`: **back both up together**). Phone AND backup codes
lost: `master --data-dir … admin disable-totp --username admin` from the master's machine. Details:
[docs/decisions/0014-double-authentification.md](docs/decisions/0014-double-authentification.md).

## Replication between nodes (no shared storage)

With `--tracker-listen 0.0.0.0:7444` (`MASTER_TRACKER_LISTEN`), the master hosts a **private BitTorrent tracker**:
the copies of a torrent (target group) download **from the other nodes** over the BitTorrent protocol, then switch
back to the original torrent (cross-seed). Nodes only contact the master to index themselves. The torrent must have
been added as a **`.torrent` file**, and the nodes must be able to reach each other. Without this option, a copy
assumes the files are already present (the previous behaviour). See
[docs/decisions/0018-replication-par-swarm-prive.md](docs/decisions/0018-replication-par-swarm-prive.md).

## Create a torrent

Page **Torrents → Create a torrent**: pick an online node, browse its folders, enter the trackers (one tier per block,
one address per line), private or not, then start. **The node reads and hashes its own files** (the master does not
have the data); you download the `.torrent` and the magnet link, and can **add it to the node** so it starts sharing
at once. Tracker addresses (which contain a passkey) are neither stored nor logged; the file is only kept for the
duration of the window. Without a master, the same thing from the command line:

```bash
agent create-torrent /srv/media/Movie.2024 -o movie.torrent   --tracker https://tracker.example.org/announce?passkey=… --private --source MyTracker
```

See [docs/decisions/0021-creation-de-torrents.md](docs/decisions/0021-creation-de-torrents.md).

## Cross-seed

For content already complete on a node, the master can find the **same content at other indexers** (Prowlarr or
Jackett, Torznab) and add it to that node, pointed at the existing files: several trackers, a single copy on disk, no
new download. **Cross-seed** page: add an indexer (test it), leave the mode on **Dry run** (the default) to see what
would be added, then switch to **Active**. Matching is done on the file list; a candidate whose verification is
incomplete is removed. **Each indexer has its own caution settings** (delay between requests, hourly and daily budgets)
so your account does not get banned; an indexer asking to slow down is paused, and a refused access disables it
automatically. Another tracker that names the folder or the files differently is handled with **hard links** created
by the agent (no copy; the agent must be able to write in its downloads folder, as the same user as the client). In
the torrent list, content shared across several trackers stays **a single row** (expandable). See
[docs/decisions/0019-cross-seed-integre.md](docs/decisions/0019-cross-seed-integre.md).

## Install a published release

Every version (tag `vX.Y.Z`) publishes on GitHub:

| Asset | What it is |
|---|---|
| `nxt-master-linux-{amd64,arm64}`, `nxt-agent-linux-{amd64,arm64}` | **static** binaries (musl), no dependencies |
| `nxt-{master,agent}-windows-amd64.exe` | Windows binaries |
| `SHA256SUMS` | checksums of all of the above |
| build provenance attestation | which workflow built what, from which commit |

and on GHCR the `nxt-master` and `nxt-agent` images (amd64 and arm64 under a single tag). Versions containing a `-`
(such as `0.1.0-rc.1`) are marked **pre-release** and do not receive the `latest` image tag.

```bash
# Verify a download
sha256sum -c SHA256SUMS --ignore-missing
gh attestation verify nxt-agent-linux-amd64 --repo next-vertices/nxtTorrent     # who built it, from which commit

# Images
docker pull ghcr.io/next-vertices/nxt-master:0.1.0-rc.1
```

The release does not contain the systemd units: they are in the repository ([deploy/systemd/](deploy/systemd/)). The
installer expects files named `master` and `agent`, so rename the downloaded binaries (see the *Linux (systemd)*
section).

**Publishing a version** (maintainers): bump `version` in the root `Cargo.toml`, push to `main`, wait for the `ci`
workflow to be green, then tag `vX.Y.Z` and push the tag. The `release` workflow refuses a tag that does not match the
version, and a real publication is only possible **from a tag**. Running `release` manually (with *publish* unchecked)
performs a complete dry run without publishing anything. Details:
[docs/decisions/0020-distribution.md](docs/decisions/0020-distribution.md).

## Docker

Minimal images (`scratch`, ~15 MB, static binary, non-root user, health check):

```bash
docker build --target master -t nxt-master .
docker build --target agent  -t nxt-agent  .
docker compose -f deploy/docker/compose.example.yml up -d      # master + qBittorrent + agent
```

Quick trial with **Transmission** (master + Transmission + agent, all in Docker, nothing to install): see the commands
at the top of [deploy/docker/compose.transmission-test.yml](deploy/docker/compose.transmission-test.yml)
(dashboard on `http://127.0.0.1:17080`).

The agent shares the network stack of the client's container (`network_mode: "service:qbittorrent"`): the client's API
stays on the loopback interface and is published nowhere. See [deploy/docker/](deploy/docker/).

## Linux (systemd)

Static musl binaries (`cargo build --release --target x86_64-unknown-linux-musl -p master -p agent`), or the ones from a
release. Hardened units (dedicated accounts, no capabilities, read-only filesystem, seccomp filter, restart after a
crash, clean stop on SIGTERM) in [deploy/systemd/](deploy/systemd/):

```bash
# From a release: the installer expects files named `master` and `agent`
mkdir bin && cp nxt-master-linux-amd64 bin/master && cp nxt-agent-linux-amd64 bin/agent
sudo sh deploy/systemd/install.sh --bin-dir bin both
# From a source build instead:
#   sudo sh deploy/systemd/install.sh --bin-dir target/x86_64-unknown-linux-musl/release both

sudoedit /etc/nxt/master.env            # MASTER_LISTEN, MASTER_SANS (0600 root:root)
sudo systemctl enable --now nxt-master
sudo -u nxt-master nxt-master --data-dir /var/lib/nxt-master pair-code --label ssd   # pairing code

# On each node:
sudo -u nxt-agent nxt-agent --data-dir /var/lib/nxt-agent pair master.example.org:7443 CODE-XXXX...
sudoedit /etc/nxt/agent.env             # AGENT_ROOT, AGENT_CLIENT, AGENT_CLIENT_PASSWORD (0600 root:root)
sudo systemctl enable --now nxt-agent
```

If the download root is protected, give the agent the right to traverse it (it never writes there):
`systemctl edit nxt-agent` then `[Service]` / `SupplementaryGroups=<torrent client's group>`.

## Windows / Windows Server

`master.exe` and `agent.exe` binaries (target `x86_64-pc-windows-msvc`, C runtime statically linked: nothing to
install). Same commands as elsewhere; the clients supported on Windows are qBittorrent, Transmission and Deluge
(ruTorrent/rTorrent only exist on Unix).

- Secret files (keys, certificates) get a **restricted ACL** (SYSTEM, Administrators and the current user,
  inheritance cut) via `icacls`, the equivalent of Unix `0600`.
- **Native Windows service** (delayed automatic start, LocalSystem account, automatic restart after a crash, clean
  stop on request of the service manager or at Windows shutdown). In an **Administrator** terminal:
  ```powershell
  # Agent: pair first, then install (the client's password goes into a protected file)
  .\agent.exe --data-dir C:\nxt\agent pair master.example.org:7443 CODE-XXXX...
  $env:AGENT_CLIENT_PASSWORD = "..."
  .\agent.exe --data-dir C:\nxt\agent service install --root D:\Downloads --client qbittorrent
  .\agent.exe --data-dir C:\nxt\agent service start

  # Master
  .\master.exe --data-dir C:\nxt\master service install --san master.example.org --listen 0.0.0.0:7443
  .\master.exe --data-dir C:\nxt\master service start
  New-NetFirewallRule -DisplayName nxt-master -Direction Inbound -Protocol TCP -LocalPort 7443 -Action Allow
  ```
  `service stop|start|status|uninstall` drive the service (names: `nxt-agent`, `nxt-master`). Logs: `agent.log` /
  `master.log` in the data directory (rotated at 10 MB). No secret is placed in the service configuration (readable
  by local accounts): the client's password and the webhook URL are in files with a restricted ACL.
- `SYSTEM` account: the agent and the torrent client must have access to the same download folder.

## Tests

```bash
cargo test --workspace                       # unit + integration (fake clients, no Docker)

docker compose -f testkit/docker/compose.yml up -d     # real clients (qBittorrent, Transmission, Deluge, ruTorrent)
# qBittorrent generates a temporary password at each first start; read it in its logs:
#   docker compose -f testkit/docker/compose.yml logs qbittorrent | grep "provided for this session"
QBIT_PASS=<that password> cargo test -p agent --test real_clients -- --ignored --test-threads=1
docker compose -f testkit/docker/compose.yml down -v
```

Architecture decisions: [docs/decisions/](docs/decisions/) (written in French).

## License

Proprietary, source-available — see [LICENSE](LICENSE). In short: **free for individuals**; **free for professionals
with an annual revenue of 50,000 EUR or less, paid above that**; selling, modifying, forking or redistributing the
project requires the author's explicit written authorization. Contact: contact@next-vertices.com. Copyright © 2026 Adrien "ADFuji" Moutet, rights delegated
to the future entity next-vertices.
