//! Faux qBittorrent peuplé de centaines de torrents aux états et débits variables, pour voir
//! l'interface d'administration « en vrai » (captures d'écran, démonstration) sans client réel.
//!   cargo run -p testkit --example mock_qbit -- <dossier-racine> [nombre] [connected|firewalled]
//! Par défaut le faux client se dit « firewalled » (port non joignable) pour montrer le diagnostic réseau.
//! Affiche `URL=http://127.0.0.1:<port>` puis tourne jusqu'à interruption.
use testkit::qbit::{MockQbit, MockTorrent};

const NAMES: [&str; 12] = [
    "Ubuntu 24.04 Desktop amd64",
    "Debian 12 netinst",
    "Fedora Workstation 40",
    "Arch Linux 2025.01",
    "Big Buck Bunny 4K",
    "Sintel 1080p",
    "Tears of Steel",
    "Blender Open Movie Archive",
    "OpenStreetMap planet extract",
    "Wikipedia dump FR",
    "Linux kernel sources",
    "Raspberry Pi OS Lite",
];

const TRACKERS: [&str; 4] = [
    "tracker.linuxiso.org",
    "bt.archive-mirror.net",
    "open.publictracker.io",
    "announce.seedbox.example",
];

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().unwrap_or_else(|| "/downloads".into());
    let count: usize = args.next().and_then(|n| n.parse().ok()).unwrap_or(400);
    let net = args.next().unwrap_or_else(|| "firewalled".into());
    let q = MockQbit::start().await;
    q.set_connection(&net, 180);
    q.set_free(1_800_000_000_000); // 1,8 To libres : au-dessus de la réserve disque du scheduler
    let mut rng = Lcg(42);
    let subs = ["films", "series", "isos", "archives"];
    let mut hashes = Vec::new();
    for i in 0..count {
        let hash = format!("{:040x}", 0xA000_0000_0000u64 + i as u64 * 7919);
        let r = rng.next() % 100;
        let (state, progress) = match r {
            0..=69 => ("stalledUP", 1.0),
            70..=84 => ("downloading", (rng.next() % 95) as f64 / 100.0),
            85..=91 => ("pausedUP", 1.0),
            92..=95 => ("checkingUP", 0.4),
            96..=97 => ("error", 0.2),
            _ => ("queuedDL", 0.0),
        };
        let sub = subs[rng.next() as usize % subs.len()];
        let mut t = MockTorrent::new(&hash, state, progress, &format!("{root}/{sub}"));
        t.name = format!("{} #{:03}", NAMES[i % NAMES.len()], i);
        t.size = (50 + rng.next() % 40_000) * 1_048_576;
        // Trackers d'exemple ; l'URL porte un faux passkey : seul l'hôte doit jamais sortir du nœud.
        let host = TRACKERS[i % TRACKERS.len()];
        t.tracker = format!("https://{host}/SECRETPASSKEY{i}/announce");
        t.num_complete = (20 + rng.next() % 800) as i64;
        t.num_incomplete = (rng.next() % 120) as i64;
        if matches!(state, "stalledUP" | "downloading") {
            t.num_seeds = (rng.next() % 12) as u32;
            t.num_leechs = (rng.next() % 25) as u32;
        }
        hashes.push(hash);
        q.insert(t);
    }
    println!("URL={}", q.url());
    // Les débits évoluent chaque seconde pour que les courbes vivent.
    loop {
        for h in &hashes {
            if let Some(mut t) = q.get(h) {
                let busy = rng.next() % 100;
                t.upspeed = if t.state == "stalledUP" || t.state == "uploading" {
                    if busy < 20 {
                        (rng.next() % 4_000) * 1024
                    } else {
                        0
                    }
                } else {
                    0
                };
                t.dlspeed = if t.state == "downloading" {
                    (200 + rng.next() % 6_000) * 1024
                } else {
                    0
                };
                q.insert(t);
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}
