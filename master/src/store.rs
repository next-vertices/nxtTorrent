//! Persistance SQLite : codes d'appairage (hachés), nœuds, journal d'audit.
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("base de données: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("clé de chiffrement: {0}")]
    Key(String),
}

/// Secret TOTP déchiffré et dernier pas de temps accepté.
pub type TotpSecret = (Vec<u8>, Option<i64>);

pub struct Store {
    conn: Mutex<Connection>,
    /// Clé AES-256 qui protège les secrets TOTP au repos (fichier 0600 à côté de la base).
    key: [u8; 32],
}

/// Charge (ou crée, en 0600) la clé de chiffrement des secrets. Une clé illisible ou de mauvaise taille est une
/// erreur franche : la régénérer en silence rendrait les secrets TOTP existants indéchiffrables.
fn load_or_create_key(path: &Path) -> Result<[u8; 32], StoreError> {
    if path.exists() {
        let bytes = std::fs::read(path).map_err(|e| StoreError::Key(e.to_string()))?;
        return bytes
            .try_into()
            .map_err(|_| StoreError::Key("fichier de clé invalide (32 octets attendus)".into()));
    }
    let key = crate::admin::totp::generate_secret_32();
    proto::fsutil::write_secret(path, &key).map_err(|e| StoreError::Key(e.to_string()))?;
    Ok(key)
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub struct NodeInfo {
    pub state: String,
    pub last_seen: Option<i64>,
    pub offline_since: Option<i64>,
}

pub struct NodeRecord {
    pub id: String,
    /// Nom d'affichage choisi par l'admin (None = on affiche le nom d'hôte).
    pub name: Option<String>,
    pub hostname: String,
    pub agent_version: String,
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TorrentDef {
    pub infohash: String,
    pub magnet: String,
    pub save_subdir: String,
    pub requires: Vec<String>,
    /// Label du groupe cible de réplication (None = pas de réplication).
    pub replicate_group: Option<String>,
    pub size_hint: u64,
    pub paused: bool,
    pub completed: bool,
    pub deleted: bool,
    /// Nom lisible (issu du .torrent ou du paramètre `dn` du magnet) ; vide si inconnu.
    pub name: String,
    /// Le contenu du .torrent est conservé (envoyé aux nœuds à la place du magnet : trackers privés).
    pub has_file: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TorrentFlag {
    Paused,
    Completed,
    Deleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Primary,
    Replica,
    /// Torrent ajouté par le cross-seed : autre torrent (autre tracker) sur des fichiers déjà présents.
    Xseed,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Replica => "replica",
            Self::Xseed => "xseed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct AssignRow {
    pub infohash: String,
    pub node_id: String,
    pub role: String,
    pub phase: String,
    pub removed: bool,
    pub delete_files: bool,
}

/// Canal de notification (l'URL peut contenir un secret : chiffrée en base, jamais exposée par l'API).
#[derive(Clone, Debug)]
pub struct ChannelRow {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub url: String,
    pub enabled: bool,
}

/// Indexeur Torznab (la clé d'API est chiffrée en base et n'apparaît jamais ici).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexerRow {
    pub id: String,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    /// Délai minimal entre deux requêtes à cet indexeur (ms).
    pub min_interval_ms: u64,
    /// Budgets glissants de requêtes (recherches ET téléchargements de .torrent).
    pub max_per_hour: u32,
    pub max_per_day: u32,
    /// Résultats téléchargés au plus par recherche.
    pub max_candidates: u32,
    /// Pause imposée (secondes Unix) : aucune requête avant. 0 = aucune.
    pub paused_until: i64,
    /// Échecs consécutifs (remis à 0 au premier succès).
    pub fail_count: u32,
    pub last_error: String,
    pub last_error_ts: i64,
    /// Non vide : désactivé AUTOMATIQUEMENT (accès refusé…), avec la raison.
    pub disabled_reason: String,
}

/// Réglages de prudence par défaut : un indexeur privé peut bannir un compte pour des requêtes trop fréquentes.
pub const INDEXER_DEFAULTS: (u64, u32, u32, u32) = (3000, 60, 500, 10);

/// Une ligne de l'historique du cross-seed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XseedRow {
    pub ts: i64,
    pub parent: String,
    pub node_id: String,
    pub indexer: String,
    pub candidate: String,
    pub title: String,
    /// Hôte du tracker du candidat (jamais l'URL : elle porte un passkey).
    pub tracker: String,
    /// found (simulation) | adding | seeding | refused | removed
    pub state: String,
    pub detail: String,
}

/// Ce que porte un code d'appairage (fixé par l'admin) : labels et nom d'affichage du futur nœud.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeGrant {
    pub labels: Vec<String>,
    pub name: Option<String>,
}

/// Un compte d'administration tel que l'API le liste (sans aucun secret).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminUser {
    pub username: String,
    pub created_at: i64,
    pub last_login_at: Option<i64>,
    pub totp_enabled: bool,
    pub must_change: bool,
}

/// Un volume horaire (voir `Store::traffic_rows`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrafficRow {
    pub hour: i64,
    pub node_id: String,
    pub tracker: String,
    pub xseed: bool,
    pub up: u64,
    pub down: u64,
}

/// Un échantillon de l'état du cluster (une ligne par minute, voir `Core::sample_metrics`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetricPoint {
    pub ts: i64,
    pub nodes_online: i64,
    pub nodes_total: i64,
    pub torrents_total: i64,
    pub torrents_seeding: i64,
    pub torrents_downloading: i64,
    pub dl_rate: i64,
    pub ul_rate: i64,
    pub disk_free: i64,
}

#[derive(Clone, Debug)]
pub struct AuditRow {
    pub id: i64,
    pub ts: i64,
    pub actor: String,
    pub action: String,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct NodeDetail {
    pub id: String,
    pub name: Option<String>,
    pub hostname: String,
    pub agent_version: String,
    pub labels: Vec<String>,
    pub state: String,
    pub created_at: i64,
    pub last_seen: Option<i64>,
    pub offline_since: Option<i64>,
    pub desired_rev: u64,
    /// Localisation saisie par l'admin (texte libre : « Hetzner · Falkenstein »).
    pub location: Option<String>,
    /// Dernière adresse source vue par le master (celle du VPS, ou d'un NAT/proxy/réseau Docker).
    pub last_ip: Option<String>,
    /// Limites de débit voulues (octets/s ; 0 = illimité), appliquées par le master à chaque connexion.
    /// `limits_managed` : tant que l'admin n'y a jamais touché, le master n'écrase pas les réglages locaux du client.
    pub limits_managed: bool,
    pub dl_limit: u64,
    pub ul_limit: u64,
}

/// Un échantillon par nœud et par période d'échantillonnage.
#[derive(Clone, Debug, Default)]
pub struct NodeMetric {
    pub ts: i64,
    pub node_id: String,
    pub online: bool,
    pub dl_rate: i64,
    pub ul_rate: i64,
    pub torrents: i64,
    pub peers: i64,
}

/// Synthèse d'un nœud sur une période.
#[derive(Clone, Debug, Default)]
pub struct NodeSummary {
    pub node_id: String,
    pub samples: i64,
    pub online_samples: i64,
    pub avg_dl: f64,
    pub avg_ul: f64,
    pub max_dl: i64,
    pub max_ul: i64,
    pub avg_peers: f64,
    /// Octets estimés (somme des débits échantillonnés × période).
    pub up_bytes: f64,
    pub down_bytes: f64,
}

#[derive(Clone, Debug)]
pub struct NodeRow {
    pub id: String,
    pub state: String,
    pub labels: Vec<String>,
    pub offline_since: Option<i64>,
}

fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
        .collect()
}

fn assign_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<AssignRow> {
    Ok(AssignRow {
        infohash: r.get(0)?,
        node_id: r.get(1)?,
        role: r.get(2)?,
        phase: r.get(3)?,
        removed: r.get::<_, i64>(4)? != 0,
        delete_files: r.get::<_, i64>(5)? != 0,
    })
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let key = load_or_create_key(&path.with_file_name("totp.key"))?;
        Self::init(Connection::open(path)?, key)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(
            Connection::open_in_memory()?,
            crate::admin::totp::generate_secret_32(),
        )
    }

    fn init(conn: Connection, key: [u8; 32]) -> Result<Self, StoreError> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS pairing_codes(
                 hash TEXT PRIMARY KEY, expires_at INTEGER NOT NULL, used INTEGER NOT NULL DEFAULT 0,
                 labels TEXT NOT NULL DEFAULT '', name TEXT);
             CREATE TABLE IF NOT EXISTS nodes(
                 id TEXT PRIMARY KEY, hostname TEXT NOT NULL, agent_version TEXT NOT NULL,
                 labels TEXT NOT NULL, state TEXT NOT NULL, created_at INTEGER NOT NULL,
                 last_seen INTEGER, offline_since INTEGER, desired_rev INTEGER NOT NULL DEFAULT 0, name TEXT);
             CREATE TABLE IF NOT EXISTS audit(
                 id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER NOT NULL,
                 actor TEXT NOT NULL, action TEXT NOT NULL, detail TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS torrents(
                 infohash TEXT PRIMARY KEY, magnet TEXT NOT NULL, save_subdir TEXT NOT NULL,
                 requires TEXT NOT NULL, replicate_group TEXT, size_hint INTEGER NOT NULL DEFAULT 0,
                 paused INTEGER NOT NULL DEFAULT 0, completed INTEGER NOT NULL DEFAULT 0,
                 deleted INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL,
                 name TEXT NOT NULL DEFAULT '', torrent_file BLOB);
             CREATE TABLE IF NOT EXISTS assignments(
                 infohash TEXT NOT NULL, node_id TEXT NOT NULL, role TEXT NOT NULL, phase TEXT NOT NULL,
                 removed INTEGER NOT NULL DEFAULT 0, delete_files INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY(infohash, node_id));
             CREATE TABLE IF NOT EXISTS settings(k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS indexers(
                 id TEXT PRIMARY KEY, name TEXT NOT NULL, url TEXT NOT NULL, api_key BLOB NOT NULL,
                 enabled INTEGER NOT NULL DEFAULT 1, created_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS indexer_requests(
                 indexer_id TEXT NOT NULL, ts_ms INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS indexer_requests_idx ON indexer_requests(indexer_id, ts_ms);
             CREATE TABLE IF NOT EXISTS xseed_seen(key TEXT PRIMARY KEY, ts INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS xseed_log(
                 parent TEXT NOT NULL, node_id TEXT NOT NULL, candidate TEXT NOT NULL, indexer TEXT NOT NULL,
                 title TEXT NOT NULL, tracker TEXT NOT NULL, state TEXT NOT NULL, detail TEXT NOT NULL,
                 ts INTEGER NOT NULL, PRIMARY KEY(parent, node_id, candidate));
             CREATE TABLE IF NOT EXISTS xseed_checked(
                 parent TEXT NOT NULL, node_id TEXT NOT NULL, ts INTEGER NOT NULL, PRIMARY KEY(parent, node_id));
             CREATE TABLE IF NOT EXISTS copy_limited(
                 infohash TEXT NOT NULL, node_id TEXT NOT NULL, dl_limit INTEGER NOT NULL,
                 PRIMARY KEY(infohash, node_id));
             CREATE TABLE IF NOT EXISTS tracker_added(
                 infohash TEXT NOT NULL, node_id TEXT NOT NULL, PRIMARY KEY(infohash, node_id));
             CREATE TABLE IF NOT EXISTS admin_users(
                 username TEXT PRIMARY KEY, password_hash TEXT NOT NULL, created_at INTEGER NOT NULL,
                 totp_secret BLOB, totp_enabled INTEGER NOT NULL DEFAULT 0, totp_last_step INTEGER,
                 recovery_codes TEXT NOT NULL DEFAULT '');
             CREATE TABLE IF NOT EXISTS metrics(
                 ts INTEGER PRIMARY KEY, nodes_online INTEGER NOT NULL, nodes_total INTEGER NOT NULL,
                 torrents_total INTEGER NOT NULL, torrents_seeding INTEGER NOT NULL,
                 torrents_downloading INTEGER NOT NULL, dl_rate INTEGER NOT NULL, ul_rate INTEGER NOT NULL,
                 disk_free INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS alert_rules(k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS alert_channels(
                 id TEXT PRIMARY KEY, kind TEXT NOT NULL, label TEXT NOT NULL, url BLOB NOT NULL,
                 enabled INTEGER NOT NULL DEFAULT 1, created_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS node_metrics(
                 ts INTEGER NOT NULL, node_id TEXT NOT NULL, online INTEGER NOT NULL,
                 dl_rate INTEGER NOT NULL, ul_rate INTEGER NOT NULL, torrents INTEGER NOT NULL,
                 peers INTEGER NOT NULL, PRIMARY KEY(ts, node_id));
             CREATE TABLE IF NOT EXISTS torrent_totals(
                 node_id TEXT NOT NULL, infohash TEXT NOT NULL, up INTEGER NOT NULL, down INTEGER NOT NULL,
                 last_up_ts INTEGER NOT NULL, first_seen_ts INTEGER NOT NULL, PRIMARY KEY(node_id, infohash));
             CREATE TABLE IF NOT EXISTS traffic_hourly(
                 hour INTEGER NOT NULL, node_id TEXT NOT NULL, tracker TEXT NOT NULL, xseed INTEGER NOT NULL,
                 up INTEGER NOT NULL, down INTEGER NOT NULL, PRIMARY KEY(hour, node_id, tracker, xseed));
             CREATE TABLE IF NOT EXISTS torrent_hourly(
                 hour INTEGER NOT NULL, node_id TEXT NOT NULL, infohash TEXT NOT NULL,
                 up INTEGER NOT NULL, down INTEGER NOT NULL, PRIMARY KEY(hour, node_id, infohash));
             CREATE TABLE IF NOT EXISTS disk_hourly(
                 hour INTEGER NOT NULL, node_id TEXT NOT NULL, free INTEGER NOT NULL, PRIMARY KEY(hour, node_id));
             CREATE TABLE IF NOT EXISTS events(
                 event_id TEXT PRIMARY KEY, node_id TEXT NOT NULL, kind TEXT NOT NULL,
                 infohash TEXT NOT NULL, ts INTEGER NOT NULL);",
        )?;
        // Bases créées par une version précédente : ajout tolérant de la colonne.
        let _ = conn.execute(
            "ALTER TABLE nodes ADD COLUMN desired_rev INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE pairing_codes ADD COLUMN labels TEXT NOT NULL DEFAULT ''",
            [],
        );
        // Bases créées par une version précédente : colonnes ajoutées plus tard (erreur ignorée si présentes).
        for ddl in [
            "ALTER TABLE pairing_codes ADD COLUMN name TEXT",
            "ALTER TABLE nodes ADD COLUMN name TEXT",
            "ALTER TABLE torrents ADD COLUMN name TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE torrents ADD COLUMN torrent_file BLOB",
            "ALTER TABLE torrents ADD COLUMN xseed_of TEXT",
            "ALTER TABLE torrents ADD COLUMN xseed_linked INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE indexers ADD COLUMN min_interval_ms INTEGER NOT NULL DEFAULT 3000",
            "ALTER TABLE indexers ADD COLUMN max_per_hour INTEGER NOT NULL DEFAULT 60",
            "ALTER TABLE indexers ADD COLUMN max_per_day INTEGER NOT NULL DEFAULT 500",
            "ALTER TABLE indexers ADD COLUMN max_candidates INTEGER NOT NULL DEFAULT 10",
            "ALTER TABLE indexers ADD COLUMN paused_until INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE indexers ADD COLUMN fail_count INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE indexers ADD COLUMN last_error TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE indexers ADD COLUMN last_error_ts INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE indexers ADD COLUMN disabled_reason TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE nodes ADD COLUMN location TEXT",
            "ALTER TABLE nodes ADD COLUMN last_ip TEXT",
            "ALTER TABLE nodes ADD COLUMN limits_managed INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE nodes ADD COLUMN dl_limit INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE nodes ADD COLUMN ul_limit INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE admin_users ADD COLUMN totp_secret BLOB",
            "ALTER TABLE admin_users ADD COLUMN totp_enabled INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE admin_users ADD COLUMN totp_last_step INTEGER",
            "ALTER TABLE admin_users ADD COLUMN recovery_codes TEXT NOT NULL DEFAULT ''",
            // Comptes multiples : mot de passe provisoire à changer dès la première connexion, dernière connexion.
            "ALTER TABLE admin_users ADD COLUMN must_change INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE admin_users ADD COLUMN last_login_at INTEGER",
        ] {
            let _ = conn.execute(ddl, []);
        }
        Ok(Self {
            conn: Mutex::new(conn),
            key,
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        // Un panic pendant une requête ne doit pas bloquer définitivement le master.
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Les labels du futur nœud sont fixés ICI, par l'admin : l'agent ne choisit pas les siens.
    pub fn insert_code(
        &self,
        hash: &str,
        expires_at: i64,
        labels: &[String],
        name: Option<&str>,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO pairing_codes(hash, expires_at, labels, name) VALUES (?1, ?2, ?3, ?4)",
            params![hash, expires_at, labels.join(","), name],
        )?;
        Ok(())
    }

    /// Consomme atomiquement un code valide (non utilisé, non expiré) et rend ses labels.
    pub fn consume_code(&self, hash: &str, now: i64) -> Result<Option<CodeGrant>, StoreError> {
        let c = self.conn();
        let n = c.execute(
            "UPDATE pairing_codes SET used=1 WHERE hash=?1 AND used=0 AND expires_at>?2",
            params![hash, now],
        )?;
        if n != 1 {
            return Ok(None);
        }
        let (labels, name): (String, Option<String>) = c.query_row(
            "SELECT labels, name FROM pairing_codes WHERE hash=?1",
            params![hash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(Some(CodeGrant {
            labels: split_csv(&labels),
            name,
        }))
    }

    /// Au démarrage du master, aucun tunnel n'existe : tout nœud « Online » est en réalité Offline.
    /// (Sans cela, un nœud resté Online en base ne serait jamais détecté comme perdu.)
    pub fn reset_presence(&self, now: i64) -> Result<usize, StoreError> {
        Ok(self.conn().execute(
            "UPDATE nodes SET state='Offline', offline_since=?1 WHERE state='Online'",
            params![now],
        )?)
    }

    pub fn insert_node(&self, n: &NodeRecord) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO nodes(id, hostname, agent_version, labels, state, created_at, name)
             VALUES (?1, ?2, ?3, ?4, 'Pending', ?5, ?6)",
            params![
                n.id,
                n.hostname,
                n.agent_version,
                n.labels.join(","),
                unix_now(),
                n.name
            ],
        )?;
        Ok(())
    }

    /// Met à jour l'état. `Revoked` est terminal : toute autre écriture est ignorée.
    pub fn set_state(&self, id: &str, state: &str, now: i64) -> Result<(), StoreError> {
        let since: Option<i64> = (state == "Offline").then_some(now);
        self.conn().execute(
            "UPDATE nodes SET state=?2, offline_since=?3 WHERE id=?1 AND (state!='Revoked' OR ?2='Revoked')",
            params![id, state, since],
        )?;
        Ok(())
    }

    pub fn touch(&self, id: &str, now: i64) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE nodes SET last_seen=?2 WHERE id=?1",
            params![id, now],
        )?;
        Ok(())
    }

    pub fn node_info(&self, id: &str) -> Result<Option<NodeInfo>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare("SELECT state, last_seen, offline_since FROM nodes WHERE id=?1")?;
        let mut rows = st.query(params![id])?;
        Ok(match rows.next()? {
            Some(r) => Some(NodeInfo {
                state: r.get(0)?,
                last_seen: r.get(1)?,
                offline_since: r.get(2)?,
            }),
            None => None,
        })
    }

    pub fn node_state(&self, id: &str) -> Result<Option<String>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare("SELECT state FROM nodes WHERE id=?1")?;
        let mut rows = st.query(params![id])?;
        Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
    }

    // ---------- État désiré : torrents gérés par le master ----------

    pub fn insert_torrent(&self, t: &TorrentDef, file: Option<&[u8]>) -> Result<bool, StoreError> {
        let n = self.conn().execute(
            "INSERT OR IGNORE INTO torrents(infohash, magnet, save_subdir, requires, replicate_group, size_hint, created_at, name, torrent_file)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                t.infohash,
                t.magnet,
                t.save_subdir,
                t.requires.join(","),
                t.replicate_group,
                i64::try_from(t.size_hint).unwrap_or(i64::MAX),
                unix_now(),
                t.name,
                file
            ],
        )?;
        Ok(n == 1)
    }

    /// Enregistre le .torrent d'un torrent ajouté par magnet, SEULEMENT s'il n'en a pas encore. `true` si écrit.
    pub fn set_torrent_file_if_missing(&self, ih: &str, bytes: &[u8]) -> Result<bool, StoreError> {
        let n = self.conn().execute(
            "UPDATE torrents SET torrent_file=?2 WHERE infohash=?1 AND torrent_file IS NULL",
            params![ih, bytes],
        )?;
        Ok(n == 1)
    }

    /// Contenu du .torrent conservé pour ce torrent (chargé à la demande : il peut peser plusieurs centaines de Kio).
    pub fn torrent_file(&self, ih: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare("SELECT torrent_file FROM torrents WHERE infohash=?1")?;
        let mut rows = st.query(params![ih])?;
        Ok(match rows.next()? {
            Some(r) => r.get::<_, Option<Vec<u8>>>(0)?,
            None => None,
        })
    }

    fn torrent_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<TorrentDef> {
        let requires: String = r.get(3)?;
        Ok(TorrentDef {
            infohash: r.get(0)?,
            magnet: r.get(1)?,
            save_subdir: r.get(2)?,
            requires: split_csv(&requires),
            replicate_group: r.get(4)?,
            size_hint: u64::try_from(r.get::<_, i64>(5)?).unwrap_or(0),
            paused: r.get::<_, i64>(6)? != 0,
            completed: r.get::<_, i64>(7)? != 0,
            deleted: r.get::<_, i64>(8)? != 0,
            name: r.get(9)?,
            has_file: r.get::<_, i64>(10)? != 0,
        })
    }

    const TORRENT_COLS: &'static str =
        "infohash, magnet, save_subdir, requires, replicate_group, size_hint, paused, completed, deleted, name, (torrent_file IS NOT NULL)";

    pub fn get_torrent(&self, ih: &str) -> Result<Option<TorrentDef>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(&format!(
            "SELECT {} FROM torrents WHERE infohash=?1",
            Self::TORRENT_COLS
        ))?;
        let mut rows = st.query_map(params![ih], Self::torrent_from)?;
        Ok(rows.next().transpose()?)
    }

    pub fn list_torrents(&self) -> Result<Vec<TorrentDef>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(&format!(
            "SELECT {} FROM torrents WHERE deleted=0 ORDER BY created_at, infohash",
            Self::TORRENT_COLS
        ))?;
        let v = st
            .query_map([], Self::torrent_from)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// Remplace les labels requis et le groupe de réplication d'un torrent.
    pub fn set_torrent_placement(
        &self,
        ih: &str,
        requires: &[String],
        group: Option<&str>,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE torrents SET requires=?2, replicate_group=?3 WHERE infohash=?1",
            params![ih, requires.join(","), group],
        )?;
        Ok(())
    }

    pub fn set_torrent_flag(
        &self,
        ih: &str,
        column: TorrentFlag,
        value: bool,
    ) -> Result<(), StoreError> {
        let col = match column {
            TorrentFlag::Paused => "paused",
            TorrentFlag::Completed => "completed",
            TorrentFlag::Deleted => "deleted",
        };
        self.conn().execute(
            &format!("UPDATE torrents SET {col}=?2 WHERE infohash=?1"),
            params![ih, i64::from(value)],
        )?;
        Ok(())
    }

    // ---------- Assignations torrent -> nœud ----------

    /// Crée l'assignation si elle n'existe pas. `true` si créée.
    pub fn insert_assignment(
        &self,
        ih: &str,
        node: &str,
        role: Role,
        phase: &str,
    ) -> Result<bool, StoreError> {
        let n = self.conn().execute(
            "INSERT OR IGNORE INTO assignments(infohash, node_id, role, phase) VALUES (?1, ?2, ?3, ?4)",
            params![ih, node, role.as_str(), phase],
        )?;
        Ok(n == 1)
    }

    pub fn all_assignments(&self) -> Result<Vec<AssignRow>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT infohash, node_id, role, phase, removed, delete_files FROM assignments",
        )?;
        let v = st
            .query_map([], assign_from)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn assignments_for_torrent(&self, ih: &str) -> Result<Vec<AssignRow>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT infohash, node_id, role, phase, removed, delete_files FROM assignments WHERE infohash=?1 ORDER BY node_id",
        )?;
        let v = st
            .query_map(params![ih], assign_from)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// Vue désirée d'un nœud (assignations + définition du torrent).
    pub fn desired_for_node(&self, node: &str) -> Result<Vec<(AssignRow, TorrentDef)>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT a.infohash, a.node_id, a.role, a.phase, a.removed, a.delete_files,
                    t.infohash, t.magnet, t.save_subdir, t.requires, t.replicate_group, t.size_hint, t.paused, t.completed, t.deleted, t.name, (t.torrent_file IS NOT NULL)
             FROM assignments a JOIN torrents t ON t.infohash = a.infohash
             WHERE a.node_id=?1 ORDER BY a.infohash",
        )?;
        let v = st
            .query_map(params![node], |r| {
                let a = assign_from(r)?;
                // Colonnes torrent : décalées de 6.
                let requires: String = r.get(9)?;
                let t = TorrentDef {
                    infohash: r.get(6)?,
                    magnet: r.get(7)?,
                    save_subdir: r.get(8)?,
                    requires: split_csv(&requires),
                    replicate_group: r.get(10)?,
                    size_hint: u64::try_from(r.get::<_, i64>(11)?).unwrap_or(0),
                    paused: r.get::<_, i64>(12)? != 0,
                    completed: r.get::<_, i64>(13)? != 0,
                    deleted: r.get::<_, i64>(14)? != 0,
                    name: r.get(15)?,
                    has_file: r.get::<_, i64>(16)? != 0,
                };
                Ok((a, t))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn set_phase(&self, ih: &str, node: &str, phase: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE assignments SET phase=?3 WHERE infohash=?1 AND node_id=?2",
            params![ih, node, phase],
        )?;
        Ok(())
    }

    /// Pose un tombstone : le nœud devra retirer ce torrent.
    pub fn mark_removed(&self, ih: &str, node: &str, delete_files: bool) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE assignments SET removed=1, delete_files=?3 WHERE infohash=?1 AND node_id=?2",
            params![ih, node, i64::from(delete_files)],
        )?;
        Ok(())
    }

    pub fn delete_assignment(&self, ih: &str, node: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "DELETE FROM assignments WHERE infohash=?1 AND node_id=?2",
            params![ih, node],
        )?;
        self.conn().execute(
            "DELETE FROM tracker_added WHERE infohash=?1 AND node_id=?2",
            params![ih, node],
        )?;
        self.conn().execute(
            "DELETE FROM copy_limited WHERE infohash=?1 AND node_id=?2",
            params![ih, node],
        )?;
        Ok(())
    }

    // ---------- Cross-seed ----------

    pub fn indexer_add(
        &self,
        id: &str,
        name: &str,
        url: &str,
        api_key: &str,
    ) -> Result<(), StoreError> {
        let sealed = crate::admin::totp::seal(&self.key, api_key.as_bytes());
        self.conn().execute(
            "INSERT INTO indexers(id, name, url, api_key, enabled, created_at) VALUES (?1,?2,?3,?4,1,?5)",
            params![id, name, url, sealed, unix_now()],
        )?;
        Ok(())
    }

    pub fn indexers(&self) -> Result<Vec<IndexerRow>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT id, name, url, enabled, min_interval_ms, max_per_hour, max_per_day, max_candidates,
                    paused_until, fail_count, last_error, last_error_ts, disabled_reason
             FROM indexers ORDER BY created_at, id",
        )?;
        let v = st
            .query_map([], |r| {
                let u = |i: usize| -> rusqlite::Result<u64> {
                    Ok(u64::try_from(r.get::<_, i64>(i)?).unwrap_or(0))
                };
                Ok(IndexerRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    url: r.get(2)?,
                    enabled: r.get::<_, i64>(3)? != 0,
                    min_interval_ms: u(4)?,
                    max_per_hour: u32::try_from(u(5)?).unwrap_or(u32::MAX),
                    max_per_day: u32::try_from(u(6)?).unwrap_or(u32::MAX),
                    max_candidates: u32::try_from(u(7)?).unwrap_or(u32::MAX),
                    paused_until: r.get(8)?,
                    fail_count: u32::try_from(u(9)?).unwrap_or(u32::MAX),
                    last_error: r.get(10)?,
                    last_error_ts: r.get(11)?,
                    disabled_reason: r.get(12)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// Clé d'API déchiffrée (usage interne : requêtes). `None` si inconnu ou illisible (clé du master perdue).
    pub fn indexer_key(&self, id: &str) -> Result<Option<String>, StoreError> {
        let sealed: Option<Vec<u8>> = self
            .conn()
            .query_row(
                "SELECT api_key FROM indexers WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(sealed
            .and_then(|s| crate::admin::totp::open(&self.key, &s))
            .and_then(|b| String::from_utf8(b).ok()))
    }

    pub fn indexer_delete(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self
            .conn()
            .execute("DELETE FROM indexers WHERE id=?1", params![id])?
            == 1)
    }

    /// Active/désactive à la main. Activer efface aussi pause, échecs et raison d'une désactivation automatique :
    /// c'est une décision explicite de l'admin.
    pub fn indexer_enable(&self, id: &str, enabled: bool) -> Result<bool, StoreError> {
        let sql = if enabled {
            "UPDATE indexers SET enabled=1, paused_until=0, fail_count=0, last_error='', disabled_reason='' WHERE id=?1"
        } else {
            "UPDATE indexers SET enabled=0 WHERE id=?1"
        };
        Ok(self.conn().execute(sql, params![id])? == 1)
    }

    /// Réglages de prudence (bornes vérifiées par l'appelant).
    pub fn indexer_set_limits(
        &self,
        id: &str,
        min_interval_ms: u64,
        max_per_hour: u32,
        max_per_day: u32,
        max_candidates: u32,
    ) -> Result<bool, StoreError> {
        Ok(self.conn().execute(
            "UPDATE indexers SET min_interval_ms=?2, max_per_hour=?3, max_per_day=?4, max_candidates=?5 WHERE id=?1",
            params![
                id,
                i64::try_from(min_interval_ms).unwrap_or(i64::MAX),
                i64::from(max_per_hour),
                i64::from(max_per_day),
                i64::from(max_candidates)
            ],
        )? == 1)
    }

    /// Une requête vient d'être émise (compte dans les budgets). Les lignes de plus de 25 h sont purgées.
    pub fn indexer_record_request(&self, id: &str, now_ms: i64) -> Result<(), StoreError> {
        let c = self.conn();
        c.execute(
            "INSERT INTO indexer_requests(indexer_id, ts_ms) VALUES (?1, ?2)",
            params![id, now_ms],
        )?;
        c.execute(
            "DELETE FROM indexer_requests WHERE indexer_id=?1 AND ts_ms < ?2",
            params![id, now_ms - 25 * 3600 * 1000],
        )?;
        Ok(())
    }

    /// Requêtes émises depuis `since_ms`.
    pub fn indexer_requests_since(&self, id: &str, since_ms: i64) -> Result<u32, StoreError> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM indexer_requests WHERE indexer_id=?1 AND ts_ms >= ?2",
            params![id, since_ms],
            |r| r.get(0),
        )?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    pub fn indexer_last_request_ms(&self, id: &str) -> Result<Option<i64>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT MAX(ts_ms) FROM indexer_requests WHERE indexer_id=?1",
                params![id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Un échec : compte, mémorise le message (court) et impose une pause jusqu'à `pause_until` (secondes Unix).
    pub fn indexer_failure(
        &self,
        id: &str,
        error: &str,
        pause_until: i64,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE indexers SET fail_count=fail_count+1, last_error=?2, last_error_ts=?3, paused_until=MAX(paused_until, ?4) WHERE id=?1",
            params![id, error.chars().take(200).collect::<String>(), unix_now(), pause_until],
        )?;
        Ok(())
    }

    /// Succès : plus d'échec consécutif ni de pause.
    pub fn indexer_success(&self, id: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE indexers SET fail_count=0, paused_until=0 WHERE id=?1",
            params![id],
        )?;
        Ok(())
    }

    /// Désactivation AUTOMATIQUE (accès refusé, adresse invalide…) : on cesse toute requête jusqu'à décision de l'admin.
    pub fn indexer_auto_disable(&self, id: &str, reason: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE indexers SET enabled=0, disabled_reason=?2, last_error=?2, last_error_ts=?3 WHERE id=?1",
            params![id, reason.chars().take(200).collect::<String>(), unix_now()],
        )?;
        Ok(())
    }

    /// Crée ou met à jour la ligne d'historique (parent, nœud, candidat).
    #[allow(clippy::too_many_arguments)]
    pub fn xseed_upsert(
        &self,
        parent: &str,
        node: &str,
        candidate: &str,
        indexer: &str,
        title: &str,
        tracker: &str,
        state: &str,
        detail: &str,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO xseed_log(parent, node_id, candidate, indexer, title, tracker, state, detail, ts)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(parent, node_id, candidate) DO UPDATE SET
                 state=excluded.state, detail=excluded.detail, ts=excluded.ts",
            params![parent, node, candidate, indexer, title, tracker, state, detail, unix_now()],
        )?;
        Ok(())
    }

    pub fn xseed_set_state(
        &self,
        parent: &str,
        node: &str,
        candidate: &str,
        state: &str,
        detail: &str,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE xseed_log SET state=?4, detail=?5, ts=?6 WHERE parent=?1 AND node_id=?2 AND candidate=?3",
            params![parent, node, candidate, state, detail, unix_now()],
        )?;
        Ok(())
    }

    pub fn xseed_known(
        &self,
        parent: &str,
        node: &str,
        candidate: &str,
    ) -> Result<bool, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT 1 FROM xseed_log WHERE parent=?1 AND node_id=?2 AND candidate=?3",
                params![parent, node, candidate],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn xseed_log(&self, limit: usize) -> Result<Vec<XseedRow>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT ts, parent, node_id, indexer, candidate, title, tracker, state, detail
             FROM xseed_log ORDER BY ts DESC, rowid DESC LIMIT ?1",
        )?;
        let v = st
            .query_map(params![i64::try_from(limit).unwrap_or(200)], |r| {
                Ok(XseedRow {
                    ts: r.get(0)?,
                    parent: r.get(1)?,
                    node_id: r.get(2)?,
                    indexer: r.get(3)?,
                    candidate: r.get(4)?,
                    title: r.get(5)?,
                    tracker: r.get(6)?,
                    state: r.get(7)?,
                    detail: r.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// Ce résultat de recherche a-t-il déjà été examiné depuis `since` ?
    pub fn xseed_seen(&self, key: &str, since: i64) -> Result<bool, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT 1 FROM xseed_seen WHERE key=?1 AND ts>=?2",
                params![key, since],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn xseed_mark_seen(&self, key: &str) -> Result<(), StoreError> {
        let c = self.conn();
        c.execute(
            "INSERT INTO xseed_seen(key, ts) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET ts=excluded.ts",
            params![key, unix_now()],
        )?;
        // Purge : on ne garde que ce qui reste utile.
        c.execute(
            "DELETE FROM xseed_seen WHERE ts < ?1",
            params![unix_now() - 31 * 24 * 3600],
        )?;
        Ok(())
    }

    /// Dernière recherche pour ce (torrent, nœud) : évite d'interroger les indexeurs en boucle.
    pub fn xseed_checked_at(&self, parent: &str, node: &str) -> Result<Option<i64>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT ts FROM xseed_checked WHERE parent=?1 AND node_id=?2",
                params![parent, node],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn xseed_mark_checked(&self, parent: &str, node: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO xseed_checked(parent, node_id, ts) VALUES (?1,?2,?3)
             ON CONFLICT(parent, node_id) DO UPDATE SET ts=excluded.ts",
            params![parent, node, unix_now()],
        )?;
        Ok(())
    }

    /// Rattache un torrent au torrent d'origine dont il partage le contenu (regroupement dans l'interface).
    pub fn set_xseed_of(&self, ih: &str, parent: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE torrents SET xseed_of=?2 WHERE infohash=?1",
            params![ih, parent],
        )?;
        Ok(())
    }

    /// Ce torrent cross-seedé vit sur des LIENS PHYSIQUES vers les fichiers de l'original (disposition différente).
    pub fn set_xseed_linked(&self, ih: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE torrents SET xseed_linked=1 WHERE infohash=?1",
            params![ih],
        )?;
        Ok(())
    }

    /// Torrents cross-seedés (non supprimés) qui vivent sur des liens physiques.
    pub fn xseed_linked_set(&self) -> Result<std::collections::HashSet<String>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT infohash FROM torrents WHERE xseed_linked=1 AND xseed_of IS NOT NULL AND deleted=0",
        )?;
        let v = st
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Torrent d'origine d'un torrent cross-seedé.
    pub fn xseed_parent(&self, ih: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT xseed_of FROM torrents WHERE infohash=?1",
                params![ih],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// enfant -> parent, pour tous les torrents cross-seedés non supprimés.
    pub fn xseed_children(&self) -> Result<std::collections::HashMap<String, String>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT infohash, xseed_of FROM torrents WHERE xseed_of IS NOT NULL AND deleted=0",
        )?;
        let v = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    pub fn setting_get(&self, k: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn()
            .query_row("SELECT v FROM settings WHERE k=?1", params![k], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn setting_set(&self, k: &str, v: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO settings(k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            params![k, v],
        )?;
        Ok(())
    }

    /// Plafond de débit de copie déjà appliqué à ce torrent sur ce nœud (`None` : jamais appliqué).
    pub fn copy_limit_applied(&self, ih: &str, node: &str) -> Result<Option<u64>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT dl_limit FROM copy_limited WHERE infohash=?1 AND node_id=?2",
                params![ih, node],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|v| u64::try_from(v).unwrap_or(0)))
    }

    pub fn mark_copy_limit(&self, ih: &str, node: &str, limit: u64) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO copy_limited(infohash, node_id, dl_limit) VALUES (?1, ?2, ?3)
             ON CONFLICT(infohash, node_id) DO UPDATE SET dl_limit=excluded.dl_limit",
            params![ih, node, i64::try_from(limit).unwrap_or(i64::MAX)],
        )?;
        Ok(())
    }

    /// Nombre de copies en cours de transfert entre nœuds.
    pub fn count_transfers(&self) -> Result<usize, StoreError> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM assignments WHERE removed=0 AND phase IN ('Transfer','Retarget')",
            [],
            |r| r.get(0),
        )?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Le nœud annonce-t-il déjà ce torrent au tracker du master (commande `AddTracker` envoyée) ?
    pub fn tracker_added(&self, ih: &str, node: &str) -> Result<bool, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT 1 FROM tracker_added WHERE infohash=?1 AND node_id=?2",
                params![ih, node],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn mark_tracker_added(&self, ih: &str, node: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT OR IGNORE INTO tracker_added(infohash, node_id) VALUES (?1, ?2)",
            params![ih, node],
        )?;
        Ok(())
    }

    /// Une copie de ce torrent est-elle en cours de transfert (une source doit alors annoncer au tracker) ?
    pub fn has_transfer(&self, ih: &str) -> Result<bool, StoreError> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM assignments WHERE infohash=?1 AND removed=0 AND phase='Transfer'",
            params![ih],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Secret dérivé de la clé du master (HMAC-SHA-256 de `label`) : sauvegardé avec elle, aucun fichier de plus.
    pub fn derived_key(&self, label: &str) -> [u8; 32] {
        let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &self.key);
        let tag = ring::hmac::sign(&k, format!("nxt-derive|{label}").as_bytes());
        let mut out = [0u8; 32];
        out.copy_from_slice(tag.as_ref());
        out
    }

    /// Charge du nœud : assignations actives (hors tombstones).
    pub fn active_load(&self, node: &str) -> Result<usize, StoreError> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM assignments WHERE node_id=?1 AND removed=0",
            params![node],
            |r| r.get(0),
        )?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    // ---------- Révisions désirées et nœuds ----------

    pub fn bump_rev(&self, node: &str) -> Result<u64, StoreError> {
        let c = self.conn();
        c.execute(
            "UPDATE nodes SET desired_rev = desired_rev + 1 WHERE id=?1",
            params![node],
        )?;
        let r: i64 = c.query_row(
            "SELECT desired_rev FROM nodes WHERE id=?1",
            params![node],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(r).unwrap_or(0))
    }

    pub fn node_rev(&self, node: &str) -> Result<u64, StoreError> {
        let r: i64 = self
            .conn()
            .query_row(
                "SELECT desired_rev FROM nodes WHERE id=?1",
                params![node],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(u64::try_from(r).unwrap_or(0))
    }

    pub fn list_nodes(&self) -> Result<Vec<NodeRow>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare("SELECT id, state, labels, offline_since FROM nodes ORDER BY id")?;
        let v = st
            .query_map([], |r| {
                let labels: String = r.get(2)?;
                Ok(NodeRow {
                    id: r.get(0)?,
                    state: r.get(1)?,
                    labels: split_csv(&labels),
                    offline_since: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    // ---------- Événements (dédoublonnage persistant) ----------

    /// Enregistre l'événement ; `false` s'il était déjà connu (rejeu après coupure).
    pub fn record_event(
        &self,
        event_id: &str,
        node: &str,
        kind: &str,
        ih: &str,
    ) -> Result<bool, StoreError> {
        let n = self.conn().execute(
            "INSERT OR IGNORE INTO events(event_id, node_id, kind, infohash, ts) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![event_id, node, kind, ih, unix_now()],
        )?;
        Ok(n == 1)
    }

    // ---------- Comptes administrateur ----------

    /// Crée le premier compte, ATOMIQUEMENT et seulement s'il n'en existe aucun (le test et
    /// l'insertion sont une seule requête : deux créations simultanées ne peuvent pas réussir toutes les deux).
    pub fn create_first_admin(
        &self,
        username: &str,
        password_hash: &str,
    ) -> Result<bool, StoreError> {
        let n = self.conn().execute(
            "INSERT INTO admin_users(username, password_hash, created_at)
             SELECT ?1, ?2, ?3 WHERE NOT EXISTS (SELECT 1 FROM admin_users)",
            params![username, password_hash, unix_now()],
        )?;
        Ok(n == 1)
    }

    pub fn admin_count(&self) -> Result<usize, StoreError> {
        let n: i64 = self
            .conn()
            .query_row("SELECT COUNT(*) FROM admin_users", [], |r| r.get(0))?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    pub fn admin_hash(&self, username: &str) -> Result<Option<String>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare("SELECT password_hash FROM admin_users WHERE username=?1")?;
        let mut rows = st.query(params![username])?;
        Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
    }

    /// Instantané cohérent de la base dans un NOUVEAU fichier (`VACUUM INTO`), même pendant les écritures.
    pub fn snapshot_into(&self, path: &Path) -> Result<(), StoreError> {
        let p = path.to_string_lossy().into_owned();
        self.conn().execute("VACUUM INTO ?1", params![p])?;
        Ok(())
    }

    // ---------- Alertes : règles et canaux (URL chiffrées au repos) ----------

    pub fn alert_rules_get(&self) -> Result<Option<String>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare("SELECT v FROM alert_rules WHERE k='rules'")?;
        let mut rows = st.query([])?;
        Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
    }

    pub fn alert_rules_set(&self, json: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO alert_rules(k, v) VALUES ('rules', ?1) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            params![json],
        )?;
        Ok(())
    }

    pub fn alert_channel_insert(
        &self,
        id: &str,
        kind: &str,
        label: &str,
        url: &str,
    ) -> Result<(), StoreError> {
        let sealed = crate::admin::totp::seal(&self.key, url.as_bytes());
        self.conn().execute(
            "INSERT INTO alert_channels(id, kind, label, url, enabled, created_at) VALUES (?1,?2,?3,?4,1,?5)",
            params![id, kind, label, sealed, unix_now()],
        )?;
        Ok(())
    }

    /// Canaux avec leur URL déchiffrée (usage interne : envoi). Une URL illisible (clé perdue) donne une chaîne vide.
    pub fn alert_channels(&self) -> Result<Vec<ChannelRow>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT id, kind, label, url, enabled FROM alert_channels ORDER BY created_at, id",
        )?;
        let v = st
            .query_map([], |r| {
                let sealed: Vec<u8> = r.get(3)?;
                Ok(ChannelRow {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    label: r.get(2)?,
                    url: crate::admin::totp::open(&self.key, &sealed)
                        .and_then(|b| String::from_utf8(b).ok())
                        .unwrap_or_default(),
                    enabled: r.get::<_, i64>(4)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn alert_channel_delete(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self
            .conn()
            .execute("DELETE FROM alert_channels WHERE id=?1", params![id])?
            == 1)
    }

    pub fn alert_channel_enable(&self, id: &str, enabled: bool) -> Result<bool, StoreError> {
        Ok(self.conn().execute(
            "UPDATE alert_channels SET enabled=?2 WHERE id=?1",
            params![id, i64::from(enabled)],
        )? == 1)
    }

    // ---------- Double authentification (TOTP) ----------

    pub fn admin_totp_enabled(&self, username: &str) -> Result<bool, StoreError> {
        let c = self.conn();
        let mut st = c.prepare("SELECT totp_enabled FROM admin_users WHERE username=?1")?;
        let mut rows = st.query(params![username])?;
        Ok(rows
            .next()?
            .map(|r| r.get::<_, i64>(0))
            .transpose()?
            .is_some_and(|v| v != 0))
    }

    /// Secret TOTP déchiffré et dernier pas accepté. `None` si la double authentification n'est pas activée (ou si
    /// le secret est illisible : on ne contourne pas, l'appelant refuse la connexion).
    pub fn admin_totp_secret(&self, username: &str) -> Result<Option<TotpSecret>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT totp_secret, totp_last_step FROM admin_users WHERE username=?1 AND totp_enabled=1",
        )?;
        let mut rows = st.query(params![username])?;
        let Some(r) = rows.next()? else {
            return Ok(None);
        };
        let sealed: Option<Vec<u8>> = r.get(0)?;
        let last: Option<i64> = r.get(1)?;
        Ok(sealed
            .and_then(|s| crate::admin::totp::open(&self.key, &s))
            .map(|s| (s, last)))
    }

    /// Active la double authentification : secret chiffré + condensats des codes de secours.
    pub fn admin_totp_enable(
        &self,
        username: &str,
        secret: &[u8],
        recovery_hashes: &[String],
    ) -> Result<bool, StoreError> {
        let sealed = crate::admin::totp::seal(&self.key, secret);
        let n = self.conn().execute(
            "UPDATE admin_users SET totp_secret=?2, totp_enabled=1, totp_last_step=NULL, recovery_codes=?3
             WHERE username=?1",
            params![username, sealed, recovery_hashes.join(",")],
        )?;
        Ok(n == 1)
    }

    pub fn admin_totp_disable(&self, username: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE admin_users SET totp_secret=NULL, totp_enabled=0, totp_last_step=NULL, recovery_codes=''
             WHERE username=?1",
            params![username],
        )?;
        Ok(())
    }

    /// Réserve un pas de temps de façon atomique : `false` s'il a déjà été utilisé (y compris par une requête
    /// concurrente) : un même code ne peut servir qu'une fois, même sous requêtes simultanées.
    pub fn admin_totp_claim_step(&self, username: &str, step: i64) -> Result<bool, StoreError> {
        let n = self.conn().execute(
            "UPDATE admin_users SET totp_last_step=?2
             WHERE username=?1 AND (totp_last_step IS NULL OR totp_last_step < ?2)",
            params![username, step],
        )?;
        Ok(n == 1)
    }

    /// Consomme un code de secours (par son condensat). `false` s'il est inconnu ou déjà utilisé.
    pub fn admin_recovery_consume(&self, username: &str, hash: &str) -> Result<bool, StoreError> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let codes: String = tx
            .query_row(
                "SELECT recovery_codes FROM admin_users WHERE username=?1",
                params![username],
                |r| r.get(0),
            )
            .unwrap_or_default();
        let mut list: Vec<&str> = codes.split(',').filter(|s| !s.is_empty()).collect();
        let Some(i) = list.iter().position(|h| *h == hash) else {
            return Ok(false);
        };
        list.remove(i);
        tx.execute(
            "UPDATE admin_users SET recovery_codes=?2 WHERE username=?1",
            params![username, list.join(",")],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn admin_recovery_remaining(&self, username: &str) -> Result<usize, StoreError> {
        let codes: String = self
            .conn()
            .query_row(
                "SELECT recovery_codes FROM admin_users WHERE username=?1",
                params![username],
                |r| r.get(0),
            )
            .unwrap_or_default();
        Ok(codes.split(',').filter(|s| !s.is_empty()).count())
    }

    /// Comptes d'administration, du plus ancien au plus récent (jamais le condensat du mot de passe).
    pub fn admin_users(&self) -> Result<Vec<AdminUser>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT username, created_at, last_login_at, totp_enabled, must_change FROM admin_users
             ORDER BY created_at, username",
        )?;
        let v = st
            .query_map([], |r| {
                Ok(AdminUser {
                    username: r.get(0)?,
                    created_at: r.get(1)?,
                    last_login_at: r.get(2)?,
                    totp_enabled: r.get::<_, i64>(3)? != 0,
                    must_change: r.get::<_, i64>(4)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// Crée un compte avec un mot de passe PROVISOIRE (`must_change`). `false` si le nom existe déjà, sans tenir
    /// compte de la casse : « Alice » et « alice » seraient deux comptes impossibles à distinguer pour un humain.
    /// Le test et l'insertion sont une seule requête (deux créations simultanées du même nom ne réussissent pas).
    pub fn create_admin_user(
        &self,
        username: &str,
        password_hash: &str,
        must_change: bool,
    ) -> Result<bool, StoreError> {
        let n = self.conn().execute(
            "INSERT INTO admin_users(username, password_hash, created_at, must_change)
             SELECT ?1, ?2, ?3, ?4 WHERE NOT EXISTS (SELECT 1 FROM admin_users WHERE lower(username) = lower(?1))",
            params![username, password_hash, unix_now(), i64::from(must_change)],
        )?;
        Ok(n == 1)
    }

    /// Remplace le mot de passe d'un compte EXISTANT. `false` si le compte n'existe pas.
    pub fn set_admin_password(
        &self,
        username: &str,
        password_hash: &str,
        must_change: bool,
    ) -> Result<bool, StoreError> {
        let n = self.conn().execute(
            "UPDATE admin_users SET password_hash=?2, must_change=?3 WHERE username=?1",
            params![username, password_hash, i64::from(must_change)],
        )?;
        Ok(n == 1)
    }

    /// Supprime un compte. `false` s'il n'existe pas.
    pub fn delete_admin_user(&self, username: &str) -> Result<bool, StoreError> {
        let n = self.conn().execute(
            "DELETE FROM admin_users WHERE username=?1",
            params![username],
        )?;
        Ok(n == 1)
    }

    /// Le compte doit-il changer son mot de passe provisoire avant toute autre action ?
    pub fn admin_must_change(&self, username: &str) -> Result<bool, StoreError> {
        let c = self.conn();
        let mut st = c.prepare("SELECT must_change FROM admin_users WHERE username=?1")?;
        let mut rows = st.query(params![username])?;
        Ok(rows
            .next()?
            .map(|r| r.get::<_, i64>(0))
            .transpose()?
            .is_some_and(|v| v != 0))
    }

    pub fn touch_admin_login(&self, username: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE admin_users SET last_login_at=?2 WHERE username=?1",
            params![username, unix_now()],
        )?;
        Ok(())
    }

    /// Crée ou remplace le mot de passe d'un compte (récupération locale par la CLI).
    pub fn upsert_admin(&self, username: &str, password_hash: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO admin_users(username, password_hash, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(username) DO UPDATE SET password_hash=excluded.password_hash, must_change=0",
            params![username, password_hash, unix_now()],
        )?;
        Ok(())
    }

    // ---------- Lecture de l'audit et des nœuds ----------

    /// Entrées du journal, les plus récentes d'abord ; `before` = pagination par identifiant.
    pub fn audit_list(
        &self,
        limit: usize,
        before: Option<i64>,
        action: Option<&str>,
    ) -> Result<Vec<AuditRow>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT id, ts, actor, action, detail FROM audit
             WHERE (?1 IS NULL OR id < ?1) AND (?2 IS NULL OR action = ?2)
             ORDER BY id DESC LIMIT ?3",
        )?;
        let v = st
            .query_map(
                params![before, action, i64::try_from(limit).unwrap_or(100)],
                |r| {
                    Ok(AuditRow {
                        id: r.get(0)?,
                        ts: r.get(1)?,
                        actor: r.get(2)?,
                        action: r.get(3)?,
                        detail: r.get(4)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// Renomme un nœud (None ou chaîne vide = retour au nom d'hôte). `false` si le nœud n'existe pas.
    pub fn set_node_name(&self, id: &str, name: Option<&str>) -> Result<bool, StoreError> {
        let n = self.conn().execute(
            "UPDATE nodes SET name=?2 WHERE id=?1",
            params![id, name.filter(|n| !n.is_empty())],
        )?;
        Ok(n == 1)
    }

    pub fn set_node_agent_version(&self, id: &str, version: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE nodes SET agent_version=?2 WHERE id=?1 AND agent_version<>?2",
            params![id, version],
        )?;
        Ok(())
    }

    /// Remplace les labels d'un nœud (fixés par l'admin uniquement). `false` si le nœud n'existe pas.
    pub fn set_node_labels(&self, id: &str, labels: &[String]) -> Result<bool, StoreError> {
        let n = self.conn().execute(
            "UPDATE nodes SET labels=?2 WHERE id=?1",
            params![id, labels.join(",")],
        )?;
        Ok(n == 1)
    }

    // ---------- Historique de métriques ----------

    pub fn insert_metric(&self, m: &MetricPoint) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT OR REPLACE INTO metrics(ts, nodes_online, nodes_total, torrents_total, torrents_seeding,
                 torrents_downloading, dl_rate, ul_rate, disk_free) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![m.ts, m.nodes_online, m.nodes_total, m.torrents_total, m.torrents_seeding,
                    m.torrents_downloading, m.dl_rate, m.ul_rate, m.disk_free],
        )?;
        Ok(())
    }

    /// Points depuis `since`, regroupés en tranches de `bucket_secs` (moyenne) pour borner la taille de la réponse.
    pub fn metrics_since(
        &self,
        since: i64,
        bucket_secs: i64,
    ) -> Result<Vec<MetricPoint>, StoreError> {
        let b = bucket_secs.max(1);
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT (ts / ?2) * ?2 AS bucket, CAST(ROUND(AVG(nodes_online)) AS INTEGER), CAST(ROUND(AVG(nodes_total)) AS INTEGER),
                    CAST(ROUND(AVG(torrents_total)) AS INTEGER), CAST(ROUND(AVG(torrents_seeding)) AS INTEGER),
                    CAST(ROUND(AVG(torrents_downloading)) AS INTEGER), CAST(ROUND(AVG(dl_rate)) AS INTEGER),
                    CAST(ROUND(AVG(ul_rate)) AS INTEGER), CAST(ROUND(AVG(disk_free)) AS INTEGER)
             FROM metrics WHERE ts >= ?1 GROUP BY bucket ORDER BY bucket",
        )?;
        let v = st
            .query_map(params![since, b], |r| {
                Ok(MetricPoint {
                    ts: r.get(0)?,
                    nodes_online: r.get(1)?,
                    nodes_total: r.get(2)?,
                    torrents_total: r.get(3)?,
                    torrents_seeding: r.get(4)?,
                    torrents_downloading: r.get(5)?,
                    dl_rate: r.get(6)?,
                    ul_rate: r.get(7)?,
                    disk_free: r.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn prune_metrics(&self, before: i64) -> Result<usize, StoreError> {
        // Un seul verrou pour les deux suppressions : deux `self.conn()` dans la même expression
        // garderaient le premier verrou pendant la prise du second (blocage de toute la base).
        let c = self.conn();
        let a = c.execute("DELETE FROM metrics WHERE ts < ?1", params![before])?;
        let b = c.execute("DELETE FROM node_metrics WHERE ts < ?1", params![before])?;
        Ok(a + b)
    }

    pub fn node_details(&self) -> Result<Vec<NodeDetail>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT id, hostname, agent_version, labels, state, created_at, last_seen, offline_since, desired_rev, name, location, last_ip,
                    limits_managed, dl_limit, ul_limit
             FROM nodes ORDER BY created_at, id",
        )?;
        let v = st
            .query_map([], |r| {
                let labels: String = r.get(3)?;
                Ok(NodeDetail {
                    id: r.get(0)?,
                    hostname: r.get(1)?,
                    agent_version: r.get(2)?,
                    labels: split_csv(&labels),
                    state: r.get(4)?,
                    created_at: r.get(5)?,
                    last_seen: r.get(6)?,
                    offline_since: r.get(7)?,
                    desired_rev: u64::try_from(r.get::<_, i64>(8)?).unwrap_or(0),
                    name: r.get(9)?,
                    location: r.get(10)?,
                    last_ip: r.get(11)?,
                    limits_managed: r.get::<_, i64>(12)? != 0,
                    dl_limit: u64::try_from(r.get::<_, i64>(13)?).unwrap_or(0),
                    ul_limit: u64::try_from(r.get::<_, i64>(14)?).unwrap_or(0),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn set_node_location(&self, id: &str, location: Option<&str>) -> Result<bool, StoreError> {
        let n = self.conn().execute(
            "UPDATE nodes SET location=?2 WHERE id=?1",
            params![id, location.filter(|l| !l.is_empty())],
        )?;
        Ok(n == 1)
    }

    pub fn set_node_limits(&self, id: &str, dl: u64, ul: u64) -> Result<bool, StoreError> {
        let n = self.conn().execute(
            "UPDATE nodes SET limits_managed=1, dl_limit=?2, ul_limit=?3 WHERE id=?1",
            params![
                id,
                i64::try_from(dl).unwrap_or(i64::MAX),
                i64::try_from(ul).unwrap_or(i64::MAX)
            ],
        )?;
        Ok(n == 1)
    }

    pub fn set_node_ip(&self, id: &str, ip: &str) -> Result<(), StoreError> {
        self.conn()
            .execute("UPDATE nodes SET last_ip=?2 WHERE id=?1", params![id, ip])?;
        Ok(())
    }

    pub fn insert_node_metrics(&self, rows: &[NodeMetric]) -> Result<(), StoreError> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        for m in rows {
            tx.execute(
                "INSERT OR REPLACE INTO node_metrics(ts, node_id, online, dl_rate, ul_rate, torrents, peers)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![m.ts, m.node_id, i64::from(m.online), m.dl_rate, m.ul_rate, m.torrents, m.peers],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // ----- Statistiques de trafic (voir `traffic.rs`) -----

    /// Relevés de référence de tous les torrents (rechargés au démarrage : les volumes continuent sans trou).
    pub fn traffic_load_book(&self) -> Result<crate::traffic::Book, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT node_id, infohash, up, down, last_up_ts, first_seen_ts FROM torrent_totals",
        )?;
        let mut book = crate::traffic::Book::new();
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                crate::traffic::Totals {
                    up: r.get::<_, i64>(2)?.max(0) as u64,
                    down: r.get::<_, i64>(3)?.max(0) as u64,
                    last_up_ts: r.get(4)?,
                    first_seen_ts: r.get(5)?,
                },
            ))
        })?;
        for row in rows {
            let (n, h, t) = row?;
            book.entry(n).or_default().insert(h, t);
        }
        Ok(book)
    }

    /// Applique un passage de comptabilité en UNE transaction : volumes de l'heure, relevés modifiés, relevés retirés.
    pub fn traffic_apply(&self, hour: i64, o: &crate::traffic::Outcome) -> Result<(), StoreError> {
        if o.deltas.is_empty()
            && o.per_torrent.is_empty()
            && o.upsert.is_empty()
            && o.removed.is_empty()
        {
            return Ok(());
        }
        let i = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
        let mut c = self.conn();
        let tx = c.transaction()?;
        for d in &o.deltas {
            tx.execute(
                "INSERT INTO traffic_hourly(hour, node_id, tracker, xseed, up, down) VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(hour, node_id, tracker, xseed) DO UPDATE SET up = up + excluded.up, down = down + excluded.down",
                params![hour, d.node_id, d.tracker, i64::from(d.xseed), i(d.up), i(d.down)],
            )?;
        }
        for d in &o.per_torrent {
            tx.execute(
                "INSERT INTO torrent_hourly(hour, node_id, infohash, up, down) VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(hour, node_id, infohash) DO UPDATE SET up = up + excluded.up, down = down + excluded.down",
                params![hour, d.node_id, d.infohash, i(d.up), i(d.down)],
            )?;
        }
        for (n, h, t) in &o.upsert {
            tx.execute(
                "INSERT OR REPLACE INTO torrent_totals(node_id, infohash, up, down, last_up_ts, first_seen_ts)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![n, h, i(t.up), i(t.down), t.last_up_ts, t.first_seen_ts],
            )?;
        }
        for (n, h) in &o.removed {
            tx.execute(
                "DELETE FROM torrent_totals WHERE node_id=?1 AND infohash=?2",
                params![n, h],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Volumes horaires depuis `from_hour` (inclus).
    pub fn traffic_rows(&self, from_hour: i64) -> Result<Vec<TrafficRow>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT hour, node_id, tracker, xseed, up, down FROM traffic_hourly WHERE hour >= ?1 ORDER BY hour",
        )?;
        let v = st
            .query_map(params![from_hour], |r| {
                Ok(TrafficRow {
                    hour: r.get(0)?,
                    node_id: r.get(1)?,
                    tracker: r.get(2)?,
                    xseed: r.get::<_, i64>(3)? != 0,
                    up: r.get::<_, i64>(4)?.max(0) as u64,
                    down: r.get::<_, i64>(5)?.max(0) as u64,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// Volume de chaque torrent par nœud depuis `from_hour` (inclus) : (nœud, infohash, envoyé, téléchargé).
    pub fn torrent_traffic(
        &self,
        from_hour: i64,
    ) -> Result<Vec<(String, String, u64, u64)>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT node_id, infohash, SUM(up), SUM(down) FROM torrent_hourly WHERE hour >= ?1
             GROUP BY node_id, infohash",
        )?;
        let v = st
            .query_map(params![from_hour], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?.max(0) as u64,
                    r.get::<_, i64>(3)?.max(0) as u64,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// Purge des volumes PAR TORRENT (conservés moins longtemps que les volumes par tracker : beaucoup plus de lignes).
    pub fn torrent_traffic_prune(&self, before_hour: i64) -> Result<usize, StoreError> {
        Ok(self.conn().execute(
            "DELETE FROM torrent_hourly WHERE hour < ?1",
            params![before_hour],
        )?)
    }

    /// Plus ancien volume enregistré (heure), s'il y en a : sert à dire « statistiques depuis le … ».
    pub fn traffic_first_hour(&self) -> Result<Option<i64>, StoreError> {
        Ok(self
            .conn()
            .query_row("SELECT MIN(hour) FROM traffic_hourly", [], |r| r.get(0))?)
    }

    pub fn traffic_prune(&self, before_hour: i64) -> Result<usize, StoreError> {
        let c = self.conn();
        let a = c.execute(
            "DELETE FROM traffic_hourly WHERE hour < ?1",
            params![before_hour],
        )?;
        let b = c.execute(
            "DELETE FROM disk_hourly WHERE hour < ?1",
            params![before_hour],
        )?;
        Ok(a + b)
    }

    /// Espace libre d'un nœud : une valeur par heure (la dernière), pour la tendance du disque.
    pub fn disk_record(&self, hour: i64, node_id: &str, free: u64) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT OR REPLACE INTO disk_hourly(hour, node_id, free) VALUES (?1,?2,?3)",
            params![hour, node_id, i64::try_from(free).unwrap_or(i64::MAX)],
        )?;
        Ok(())
    }

    /// Relevés d'espace libre depuis `from_hour` : (nœud, heure, octets libres), du plus ancien au plus récent.
    pub fn disk_series(&self, from_hour: i64) -> Result<Vec<(String, i64, u64)>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT node_id, hour, free FROM disk_hourly WHERE hour >= ?1 ORDER BY node_id, hour",
        )?;
        let v = st
            .query_map(params![from_hour], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)?.max(0) as u64))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// Synthèse par nœud depuis `since` ; `step` = période d'échantillonnage en secondes (pour estimer les octets).
    pub fn node_summaries(&self, since: i64, step: i64) -> Result<Vec<NodeSummary>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT node_id, COUNT(*), SUM(online),
                    COALESCE(AVG(CASE WHEN online=1 THEN dl_rate END), 0),
                    COALESCE(AVG(CASE WHEN online=1 THEN ul_rate END), 0),
                    COALESCE(MAX(dl_rate), 0), COALESCE(MAX(ul_rate), 0),
                    COALESCE(AVG(CASE WHEN online=1 THEN peers END), 0),
                    COALESCE(SUM(ul_rate), 0), COALESCE(SUM(dl_rate), 0)
             FROM node_metrics WHERE ts >= ?1 GROUP BY node_id",
        )?;
        let step = step.max(1) as f64;
        let v = st
            .query_map(params![since], |r| {
                Ok(NodeSummary {
                    node_id: r.get(0)?,
                    samples: r.get(1)?,
                    online_samples: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    avg_dl: r.get(3)?,
                    avg_ul: r.get(4)?,
                    max_dl: r.get(5)?,
                    max_ul: r.get(6)?,
                    avg_peers: r.get(7)?,
                    up_bytes: r.get::<_, i64>(8)? as f64 * step,
                    down_bytes: r.get::<_, i64>(9)? as f64 * step,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// Hooks de test : simulent une base de données défaillante (table des nœuds illisible).
    #[doc(hidden)]
    pub fn break_nodes_table_for_tests(&self) {
        let _ = self
            .conn()
            .execute("ALTER TABLE nodes RENAME TO nodes_broken", []);
    }
    #[doc(hidden)]
    pub fn restore_nodes_table_for_tests(&self) {
        let _ = self
            .conn()
            .execute("ALTER TABLE nodes_broken RENAME TO nodes", []);
    }

    pub fn audit(&self, actor: &str, action: &str, detail: &str) {
        let r = self.conn().execute(
            "INSERT INTO audit(ts, actor, action, detail) VALUES (?1, ?2, ?3, ?4)",
            params![unix_now(), actor, action, detail],
        );
        if let Err(e) = r {
            tracing::error!(error = %e, "écriture du journal d'audit impossible");
        }
    }

    pub fn audit_actions(&self) -> Result<Vec<String>, StoreError> {
        let c = self.conn();
        let mut st = c.prepare("SELECT action FROM audit ORDER BY id")?;
        let v = st
            .query_map([], |r| r.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_accounts_are_unique_regardless_of_case_and_the_recovery_cli_lifts_the_forced_change() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.create_admin_user("alice", "h1", false).unwrap());
        assert!(s.create_admin_user("bob", "h2", true).unwrap());
        assert!(
            !s.create_admin_user("BOB", "h3", true).unwrap(),
            "la casse ne compte pas"
        );
        assert!(!s.create_admin_user("Alice", "h3", true).unwrap());
        assert_eq!(s.admin_users().unwrap().len(), 2);
        assert!(s.admin_must_change("bob").unwrap());
        assert!(!s.admin_must_change("alice").unwrap());
        assert!(
            !s.admin_must_change("inconnu").unwrap(),
            "compte inconnu : rien à changer"
        );

        // `master admin set-password` (récupération locale par le propriétaire) lève l'obligation.
        s.upsert_admin("bob", "h4").unwrap();
        assert!(!s.admin_must_change("bob").unwrap());
        assert_eq!(s.admin_hash("bob").unwrap().as_deref(), Some("h4"));
        // Réinitialisation par un administrateur : provisoire à nouveau ; compte inconnu : refus.
        assert!(s.set_admin_password("bob", "h5", true).unwrap());
        assert!(s.admin_must_change("bob").unwrap());
        assert!(!s.set_admin_password("inconnu", "h", true).unwrap());

        s.touch_admin_login("bob").unwrap();
        let bob = s
            .admin_users()
            .unwrap()
            .into_iter()
            .find(|u| u.username == "bob")
            .unwrap();
        assert!(bob.last_login_at.is_some());
        assert!(s.delete_admin_user("bob").unwrap());
        assert!(!s.delete_admin_user("bob").unwrap());
        assert_eq!(s.admin_count().unwrap(), 1);
    }

    #[test]
    fn totp_secret_survives_a_restart_stays_encrypted_and_is_lost_without_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("master.db");
        {
            let s = Store::open(&db).unwrap();
            s.upsert_admin("alice", "hash").unwrap();
            assert!(s
                .admin_totp_enable(
                    "alice",
                    b"12345678901234567890",
                    &["h1".into(), "h2".into()]
                )
                .unwrap());
        }
        // Le fichier de clé existe à côté de la base ; le secret n'est jamais écrit en clair dans la base.
        assert_eq!(
            std::fs::read(dir.path().join("totp.key")).unwrap().len(),
            32
        );
        let raw = std::fs::read(&db).unwrap();
        assert!(!raw.windows(20).any(|w| w == b"12345678901234567890"));
        let s = Store::open(&db).unwrap();
        let (secret, last) = s.admin_totp_secret("alice").unwrap().unwrap();
        assert_eq!(
            (secret.as_slice(), last),
            (b"12345678901234567890".as_slice(), None)
        );
        // Codes de secours : consommés une seule fois.
        assert!(s.admin_recovery_consume("alice", "h1").unwrap());
        assert!(!s.admin_recovery_consume("alice", "h1").unwrap());
        assert_eq!(s.admin_recovery_remaining("alice").unwrap(), 1);
        // Pas de temps : réservation atomique (le 2ᵉ essai du même pas échoue).
        assert!(s.admin_totp_claim_step("alice", 10).unwrap());
        assert!(!s.admin_totp_claim_step("alice", 10).unwrap());
        assert!(!s.admin_totp_claim_step("alice", 9).unwrap());
        assert!(s.admin_totp_claim_step("alice", 11).unwrap());
        // Clé perdue : on le dit (jamais de régénération silencieuse) ; clé tronquée = erreur franche.
        drop(s);
        std::fs::write(dir.path().join("totp.key"), b"trop court").unwrap();
        assert!(matches!(Store::open(&db), Err(StoreError::Key(_))));
    }

    #[test]
    fn pruning_both_metric_tables_does_not_deadlock_and_node_summaries_add_up() {
        let s = std::sync::Arc::new(Store::open_in_memory().unwrap());
        let m = |ts: i64, online: bool, ul: i64| NodeMetric {
            ts,
            node_id: "n1".into(),
            online,
            dl_rate: 10,
            ul_rate: ul,
            torrents: 3,
            peers: 8,
        };
        s.insert_node_metrics(&[m(100, true, 1000), m(160, true, 3000), m(220, false, 0)])
            .unwrap();
        s.insert_metric(&MetricPoint {
            ts: 100,
            ..MetricPoint::default()
        })
        .unwrap();
        let sum = &s.node_summaries(0, 60).unwrap()[0];
        assert_eq!((sum.samples, sum.online_samples), (3, 2));
        assert_eq!(
            sum.avg_ul, 2000.0,
            "moyenne sur les seuls échantillons en ligne"
        );
        assert_eq!(sum.up_bytes, 4000.0 * 60.0, "somme des débits × période");
        // Dans un fil à part : un blocage ferait échouer le test par délai au lieu de le figer.
        let (tx, rx) = std::sync::mpsc::channel();
        let s2 = s.clone();
        std::thread::spawn(move || tx.send(s2.prune_metrics(150).unwrap()).unwrap());
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
            2
        );
        assert_eq!(s.node_summaries(0, 60).unwrap()[0].samples, 2);
    }

    #[test]
    fn code_single_use_and_ttl() {
        let s = Store::open_in_memory().unwrap();
        s.insert_code("h1", 100, &[], None).unwrap();
        assert!(s.consume_code("h1", 100).unwrap().is_none(), "expiré");
        s.insert_code(
            "h2",
            100,
            &["ssd".to_owned(), "vpn-ch".to_owned()],
            Some("nas-1"),
        )
        .unwrap();
        assert_eq!(
            s.consume_code("h2", 99).unwrap(),
            Some(CodeGrant {
                labels: vec!["ssd".to_owned(), "vpn-ch".to_owned()],
                name: Some("nas-1".to_owned())
            })
        );
        assert!(s.consume_code("h2", 99).unwrap().is_none(), "usage unique");
        assert!(s.consume_code("inconnu", 0).unwrap().is_none());
    }
}
