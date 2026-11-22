//! Création de `.torrent` depuis des fichiers d'un NŒUD (le master ne détient pas les données) et exploration de ses dossiers.
//!
//! Le nœud lit et hache les fichiers sous sa racine locale (commandes fermées `ListDir`, `CreateTorrent`, `CancelJob`) ; le
//! master ajoute ensuite, HORS du dictionnaire `info` (l'infohash ne change pas), les adresses de trackers, le commentaire et la
//! date, puis tient le résultat à disposition (en mémoire, une heure). Aucune adresse de tracker ne transite par le protocole
//! de l'agent, et aucune n'est journalisée : celle d'un tracker privé contient un passkey.
use crate::manage::ManageError;
use crate::reconcile::Phase;
use crate::store::{Role, TorrentDef};
use crate::Core;
use proto::pb::{
    command::Kind, master_msg::M as MasterM, CancelJobCmd, Command, CreateTorrentCmd, DirListing,
    ListDirCmd,
};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// Durée de conservation d'une tâche terminée.
const RETAIN_SECS: i64 = 3600;
/// Sans nouvelle du nœud (la progression arrive chaque seconde) : la tâche est déclarée perdue.
const STALE_SECS: i64 = 180;
/// Tâches (en cours ou terminées) gardées au plus en mémoire.
const MAX_JOBS: usize = 20;
/// Délai d'attente d'un listage de dossier.
const BROWSE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error("{0}")]
    Invalid(String),
    #[error("le nœud est hors ligne")]
    Offline,
    #[error("le nœud n'a pas répondu à temps")]
    Timeout,
    #[error("tâche inconnue")]
    Unknown,
    #[error("{0}")]
    Busy(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    Running,
    Done,
    Failed(String),
    Cancelled,
}

/// Ce que l'admin a demandé (jamais l'adresse d'un tracker dans un journal ou un message d'erreur).
#[derive(Clone, Debug, Default)]
pub struct CreateRequest {
    pub node_id: String,
    /// Fichier ou dossier du nœud, relatif à sa racine.
    pub path: String,
    /// Trackers par paliers.
    pub tiers: Vec<Vec<String>>,
    pub comment: String,
    pub private: bool,
    pub source: String,
    /// 0 = automatique.
    pub piece_length: u32,
    /// Ajouter le torrent au nœud (il vérifie les fichiers présents et partage).
    pub add_to_node: bool,
}

pub struct Job {
    pub id: String,
    pub req: CreateRequest,
    pub state: JobState,
    pub done: u64,
    pub total: u64,
    pub created_at: i64,
    pub updated_at: i64,
    /// Résultat (fichier .torrent complet) quand `state == Done`.
    pub file: Option<Vec<u8>>,
    pub infohash: String,
    pub name: String,
    pub size: u64,
    /// `Some(true)` ajouté au nœud ; `Some(false)` ajout refusé (raison dans `add_error`) ; `None` non demandé.
    pub added: Option<bool>,
    pub add_error: String,
}

/// Vue d'une tâche, sans le contenu du fichier.
#[derive(Clone, Debug, serde::Serialize)]
pub struct JobView {
    pub id: String,
    pub node_id: String,
    pub path: String,
    /// running | done | failed | cancelled
    pub state: &'static str,
    pub error: String,
    pub done: u64,
    pub total: u64,
    pub infohash: String,
    pub name: String,
    pub size: u64,
    pub magnet: String,
    pub private: bool,
    pub add_requested: bool,
    pub added: Option<bool>,
    pub add_error: String,
}

#[derive(Default)]
pub struct Jobs {
    jobs: Mutex<HashMap<String, Job>>,
    /// req_id -> (nœud interrogé, canal de la réponse).
    browse: Mutex<HashMap<String, (String, tokio::sync::oneshot::Sender<DirListing>)>>,
}

fn now() -> i64 {
    crate::store::unix_now()
}

/// Dossier qui contient `path` (relatif) : c'est là que le client cherchera le contenu (`save_path`).
pub fn parent_dir(path: &str) -> String {
    let parts: Vec<&str> = path
        .split(['/', '\\'])
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    match parts.split_last() {
        Some((_, head)) => head.join("/"),
        None => String::new(),
    }
}

impl Jobs {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Job>> {
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Retire les tâches périmées et déclare perdues celles qui ne donnent plus signe de vie.
    fn sweep(&self) {
        let t = now();
        let mut m = self.lock();
        for j in m.values_mut() {
            if j.state == JobState::Running && t - j.updated_at > STALE_SECS {
                j.state =
                    JobState::Failed("plus de nouvelles du nœud : création perdue".to_owned());
            }
        }
        m.retain(|_, j| j.state == JobState::Running || t - j.updated_at <= RETAIN_SECS);
        while m.len() > MAX_JOBS {
            // On abandonne d'abord la plus ancienne terminée.
            let oldest = m
                .values()
                .filter(|j| j.state != JobState::Running)
                .min_by_key(|j| j.updated_at)
                .map(|j| j.id.clone());
            match oldest {
                Some(id) => {
                    m.remove(&id);
                }
                None => break,
            }
        }
    }

    pub fn view(&self, id: &str) -> Option<JobView> {
        self.sweep();
        self.lock().get(id).map(to_view)
    }

    pub fn list(&self) -> Vec<JobView> {
        self.sweep();
        let mut v: Vec<JobView> = self.lock().values().map(to_view).collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// Fichier .torrent d'une tâche terminée : (nom de fichier sûr, contenu).
    pub fn file(&self, id: &str) -> Option<(String, Vec<u8>)> {
        self.sweep();
        let m = self.lock();
        let j = m.get(id)?;
        let bytes = j.file.clone()?;
        let safe: String = j
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                    c
                } else {
                    '_'
                }
            })
            .take(100)
            .collect();
        Some((format!("{}.torrent", safe.trim().trim_matches('.')), bytes))
    }
}

fn to_view(j: &Job) -> JobView {
    let (state, error) = match &j.state {
        JobState::Running => ("running", String::new()),
        JobState::Done => ("done", String::new()),
        JobState::Failed(e) => ("failed", e.clone()),
        JobState::Cancelled => ("cancelled", String::new()),
    };
    JobView {
        id: j.id.clone(),
        node_id: j.req.node_id.clone(),
        path: j.req.path.clone(),
        state,
        error,
        done: j.done,
        total: j.total,
        infohash: j.infohash.clone(),
        name: j.name.clone(),
        size: j.size,
        magnet: if j.infohash.is_empty() {
            String::new()
        } else {
            format!("magnet:?xt=urn:btih:{}", j.infohash)
        },
        private: j.req.private,
        add_requested: j.req.add_to_node,
        added: j.added,
        add_error: j.add_error.clone(),
    }
}

/// Validation des valeurs d'une demande, AVANT de déranger le nœud.
pub fn validate_request(r: &CreateRequest) -> Result<(), JobError> {
    let bad = |m: &str| Err(JobError::Invalid(m.to_owned()));
    if proto::relpath::split_relative(&r.path).map_or(true, |p| p.is_empty()) {
        return bad("chemin invalide : choisissez un fichier ou un dossier sous la racine du nœud");
    }
    let all: Vec<&String> = r.tiers.iter().flatten().collect();
    if r.tiers.iter().any(Vec::is_empty)
        || all.len() > proto::metainfo::MAX_TRACKERS_CREATED
        || !all.iter().all(|u| proto::metainfo::valid_tracker_url(u))
    {
        return bad("adresse de tracker invalide (http, https ou udp ; 20 au plus)");
    }
    if r.private && all.is_empty() {
        return bad("un torrent privé exige au moins un tracker");
    }
    if r.comment.len() > 500 || r.comment.chars().any(char::is_control) {
        return bad("commentaire invalide (500 caractères au plus, sans retour à la ligne)");
    }
    if !r.source.is_empty() && !r.private {
        return bad("l'étiquette « source » n'a de sens que pour un torrent privé");
    }
    if r.source.len() > 64 || r.source.chars().any(char::is_control) {
        return bad("étiquette « source » invalide (64 caractères au plus)");
    }
    if r.piece_length != 0
        && !(proto::create::MIN_PIECE..=proto::create::MAX_PIECE).contains(&r.piece_length)
        || r.piece_length != 0 && !r.piece_length.is_power_of_two()
    {
        return bad("taille de pièce : puissance de deux de 16 Kio à 16 Mio");
    }
    Ok(())
}

impl Core {
    /// Contenu d'un dossier du nœud (15 s au plus).
    pub async fn browse_node(&self, node_id: &str, path: &str) -> Result<DirListing, JobError> {
        if proto::relpath::split_relative(path).is_err() {
            return Err(JobError::Invalid("chemin invalide".to_owned()));
        }
        let req_id = uuid::Uuid::new_v4().simple().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.torrent_jobs
            .browse
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(req_id.clone(), (node_id.to_owned(), tx));
        let sent = self
            .sessions
            .send(
                node_id,
                MasterM::Cmd(Command {
                    cmd_id: format!("ls-{}", &req_id[..8]),
                    kind: Some(Kind::ListDir(ListDirCmd {
                        req_id: req_id.clone(),
                        path: path.to_owned(),
                    })),
                }),
            )
            .await;
        let result = if sent {
            tokio::time::timeout(BROWSE_TIMEOUT, rx)
                .await
                .map_err(|_| JobError::Timeout)
        } else {
            Err(JobError::Offline)
        };
        self.torrent_jobs
            .browse
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&req_id);
        match result? {
            Ok(l) if l.error.is_empty() => Ok(l),
            Ok(l) => Err(JobError::Invalid(l.error)),
            Err(_) => Err(JobError::Timeout),
        }
    }

    /// Réponse d'un nœud à un listage : seul le nœud interrogé peut y répondre.
    pub fn on_dir_listing(&self, node_id: &str, l: DirListing) {
        let waiter = {
            let mut m = self
                .torrent_jobs
                .browse
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match m.get(&l.req_id) {
                Some((n, _)) if n == node_id => m.remove(&l.req_id),
                _ => None,
            }
        };
        if let Some((_, tx)) = waiter {
            let _ = tx.send(l);
        }
    }

    /// Lance la création d'un torrent sur un nœud ; renvoie l'identifiant de la tâche.
    pub async fn start_torrent_job(
        &self,
        actor: &str,
        req: CreateRequest,
    ) -> Result<String, JobError> {
        validate_request(&req)?;
        if !self.sessions.is_connected(&req.node_id) {
            return Err(JobError::Offline);
        }
        self.torrent_jobs.sweep();
        let id = uuid::Uuid::new_v4().simple().to_string();
        {
            let mut m = self.torrent_jobs.lock();
            let running_here = m
                .values()
                .filter(|j| j.state == JobState::Running && j.req.node_id == req.node_id)
                .count();
            if running_here >= 2 {
                return Err(JobError::Busy(
                    "deux créations sont déjà en cours sur ce nœud : attendez la fin de l'une d'elles".to_owned(),
                ));
            }
            if m.len() >= MAX_JOBS {
                return Err(JobError::Busy(
                    "trop de tâches gardées en mémoire : supprimez les anciennes".to_owned(),
                ));
            }
            let t = now();
            m.insert(
                id.clone(),
                Job {
                    id: id.clone(),
                    req: req.clone(),
                    state: JobState::Running,
                    done: 0,
                    total: 0,
                    created_at: t,
                    updated_at: t,
                    file: None,
                    infohash: String::new(),
                    name: String::new(),
                    size: 0,
                    added: None,
                    add_error: String::new(),
                },
            );
        }
        let sent = self
            .sessions
            .send(
                &req.node_id,
                MasterM::Cmd(Command {
                    cmd_id: format!("ct-{}", &id[..8]),
                    kind: Some(Kind::CreateTorrent(CreateTorrentCmd {
                        job_id: id.clone(),
                        path: req.path.clone(),
                        name: String::new(),
                        piece_length: req.piece_length,
                        private: req.private,
                        source: req.source.clone(),
                    })),
                }),
            )
            .await;
        if !sent {
            self.torrent_jobs.lock().remove(&id);
            return Err(JobError::Offline);
        }
        // Journal : le chemin et le nœud, JAMAIS les adresses de trackers (passkey possible).
        self.store
            .audit(actor, "torrent_create_started", &req.node_id);
        Ok(id)
    }

    pub fn on_create_progress(&self, node_id: &str, p: &proto::pb::CreateProgress) {
        let mut m = self.torrent_jobs.lock();
        if let Some(j) = m.get_mut(&p.job_id) {
            if j.req.node_id == node_id && j.state == JobState::Running {
                j.done = p.done;
                j.total = p.total;
                j.updated_at = now();
            }
        }
    }

    /// Résultat d'une création : assemble le .torrent (trackers, commentaire), puis l'ajoute au nœud si demandé.
    pub async fn on_torrent_created(&self, node_id: &str, r: proto::pb::TorrentCreated) {
        let req = {
            let m = self.torrent_jobs.lock();
            match m.get(&r.job_id) {
                Some(j) if j.req.node_id == node_id && j.state == JobState::Running => {
                    j.req.clone()
                }
                _ => return, // inconnu, déjà terminé ou annulé, ou venant d'un autre nœud
            }
        };
        let fail = |msg: String| {
            let mut m = self.torrent_jobs.lock();
            if let Some(j) = m.get_mut(&r.job_id) {
                j.state = JobState::Failed(msg.chars().take(300).collect());
                j.updated_at = now();
            }
        };
        if !r.error.is_empty() {
            return fail(r.error.clone());
        }
        let created_by = concat!("nxtTorrent ", env!("CARGO_PKG_VERSION"));
        let full = match proto::metainfo::compose(
            &r.torrent_file,
            &req.tiers,
            (!req.comment.is_empty()).then_some(req.comment.as_str()),
            created_by,
            Some(now()),
        ) {
            Ok(b) => b,
            Err(e) => return fail(format!("fichier .torrent inattendu : {e}")),
        };
        let Ok(meta) = proto::metainfo::parse(&full) else {
            return fail("fichier .torrent inattendu".to_owned());
        };
        {
            let mut m = self.torrent_jobs.lock();
            if let Some(j) = m.get_mut(&r.job_id) {
                j.state = JobState::Done;
                j.updated_at = now();
                j.done = j.total;
                j.infohash = meta.infohash.clone();
                j.name = meta.name.clone();
                j.size = meta.size;
                j.file = Some(full.clone());
            }
        }
        self.store.audit(
            "system",
            "torrent_created",
            &format!("{} {node_id}", meta.infohash),
        );
        if req.add_to_node {
            let res = self
                .add_created_torrent(node_id, &full, &meta, &parent_dir(&req.path))
                .await;
            let mut m = self.torrent_jobs.lock();
            if let Some(j) = m.get_mut(&r.job_id) {
                match res {
                    Ok(()) => j.added = Some(true),
                    Err(e) => {
                        j.added = Some(false);
                        j.add_error = e.to_string().chars().take(200).collect();
                    }
                }
            }
        }
        self.bump_live();
    }

    /// Ajoute au nœud le torrent qu'il vient de créer : les fichiers y sont déjà, le client les vérifie puis partage.
    async fn add_created_torrent(
        &self,
        node_id: &str,
        bytes: &[u8],
        meta: &proto::metainfo::Metainfo,
        save_subdir: &str,
    ) -> Result<(), ManageError> {
        let _g = self.reconcile_lock.lock().await;
        if self.store.get_torrent(&meta.infohash)?.is_some() {
            return Err(ManageError::Invalid(
                "ce torrent est déjà géré par nxtTorrent".into(),
            ));
        }
        let def = TorrentDef {
            infohash: meta.infohash.clone(),
            magnet: format!("magnet:?xt=urn:btih:{}", meta.infohash),
            save_subdir: save_subdir.to_owned(),
            requires: Vec::new(),
            replicate_group: None,
            size_hint: meta.size,
            paused: false,
            completed: false,
            deleted: false,
            name: meta.name.clone(),
            has_file: true,
        };
        self.store.insert_torrent(&def, Some(bytes))?;
        self.store.insert_assignment(
            &meta.infohash,
            node_id,
            Role::Primary,
            Phase::Active.as_str(),
        )?;
        self.store.bump_rev(node_id)?;
        self.store.audit("system", "torrent_added", &meta.infohash);
        self.reconcile_locked(node_id).await;
        Ok(())
    }

    /// Annule une tâche en cours (le nœud arrête de lire) ; sans effet sur une tâche déjà terminée.
    pub async fn cancel_torrent_job(&self, id: &str) -> Result<(), JobError> {
        let node = {
            let mut m = self.torrent_jobs.lock();
            let j = m.get_mut(id).ok_or(JobError::Unknown)?;
            if j.state == JobState::Running {
                j.state = JobState::Cancelled;
                j.updated_at = now();
                Some(j.req.node_id.clone())
            } else {
                None
            }
        };
        if let Some(n) = node {
            let _ = self
                .sessions
                .send(
                    &n,
                    MasterM::Cmd(Command {
                        cmd_id: format!("cj-{}", &id[..8.min(id.len())]),
                        kind: Some(Kind::CancelJob(CancelJobCmd {
                            job_id: id.to_owned(),
                        })),
                    }),
                )
                .await;
        }
        Ok(())
    }

    /// Oublie une tâche terminée (libère la mémoire).
    pub fn forget_torrent_job(&self, id: &str) -> Result<(), JobError> {
        let mut m = self.torrent_jobs.lock();
        match m.get(id).map(|j| j.state.clone()) {
            None => Err(JobError::Unknown),
            Some(JobState::Running) => Err(JobError::Busy(
                "tâche en cours : annulez-la d'abord".to_owned(),
            )),
            Some(_) => {
                m.remove(id);
                Ok(())
            }
        }
    }

    /// Le tunnel d'un nœud est tombé : ses créations en cours sont perdues (le nœud n'a plus personne à qui répondre).
    pub fn fail_torrent_jobs_of(&self, node_id: &str) {
        let mut m = self.torrent_jobs.lock();
        for j in m.values_mut() {
            if j.state == JobState::Running && j.req.node_id == node_id {
                j.state =
                    JobState::Failed("le nœud s'est déconnecté pendant la création".to_owned());
                j.updated_at = now();
            }
        }
    }

    /// Injecte une tâche TERMINÉE (tests automatisés de l'API de téléchargement, sans tunnel ni agent).
    #[doc(hidden)]
    pub fn insert_finished_job_for_tests(&self, id: &str, node_id: &str, file: Vec<u8>) {
        let Ok(meta) = proto::metainfo::parse(&file) else {
            return;
        };
        let t = now();
        self.torrent_jobs.lock().insert(
            id.to_owned(),
            Job {
                id: id.to_owned(),
                req: CreateRequest {
                    node_id: node_id.to_owned(),
                    path: "x".into(),
                    ..CreateRequest::default()
                },
                state: JobState::Done,
                done: meta.size,
                total: meta.size,
                created_at: t,
                updated_at: t,
                file: Some(file),
                infohash: meta.infohash,
                name: meta.name,
                size: meta.size,
                added: None,
                add_error: String::new(),
            },
        );
    }

    pub fn torrent_job(&self, id: &str) -> Option<JobView> {
        self.torrent_jobs.view(id)
    }

    pub fn torrent_jobs_list(&self) -> Vec<JobView> {
        self.torrent_jobs.list()
    }

    pub fn torrent_job_file(&self, id: &str) -> Option<(String, Vec<u8>)> {
        self.torrent_jobs.file(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_req() -> CreateRequest {
        CreateRequest {
            node_id: "n1".into(),
            path: "movies/Film".into(),
            tiers: vec![vec!["https://t.test/announce".into()]],
            ..CreateRequest::default()
        }
    }

    #[test]
    fn the_parent_directory_is_where_the_client_will_look_for_the_content() {
        assert_eq!(parent_dir("movies/Film"), "movies");
        assert_eq!(parent_dir("movies/tv/Serie"), "movies/tv");
        assert_eq!(parent_dir("Film"), "");
        assert_eq!(parent_dir("a//b/./c"), "a/b");
        assert_eq!(parent_dir(""), "");
    }

    #[test]
    fn requests_are_validated_before_the_node_is_disturbed() {
        assert!(validate_request(&ok_req()).is_ok());
        let mut r = ok_req();
        r.tiers.clear();
        assert!(
            validate_request(&r).is_ok(),
            "torrent public sans tracker : DHT"
        );
        r.private = true;
        assert!(validate_request(&r).is_err(), "privé sans tracker");
        type Mutation = Box<dyn Fn(&mut CreateRequest)>;
        let cases: Vec<(&str, Mutation)> = vec![
            ("chemin vide", Box::new(|r| r.path = String::new())),
            ("racine", Box::new(|r| r.path = ".".into())),
            ("..", Box::new(|r| r.path = "../x".into())),
            ("absolu", Box::new(|r| r.path = "/etc".into())),
            ("lecteur", Box::new(|r| r.path = "C:\\x".into())),
            (
                "schéma",
                Box::new(|r| r.tiers = vec![vec!["ftp://x.test/a".into()]]),
            ),
            ("palier vide", Box::new(|r| r.tiers = vec![vec![]])),
            ("commentaire", Box::new(|r| r.comment = "a\nb".into())),
            ("source sans privé", Box::new(|r| r.source = "X".into())),
            ("pièce", Box::new(|r| r.piece_length = 1000)),
            (
                "pièce non puissance de deux",
                Box::new(|r| r.piece_length = 3 * 16384),
            ),
        ];
        for (what, f) in cases {
            let mut r = ok_req();
            f(&mut r);
            assert!(validate_request(&r).is_err(), "{what}");
        }
        let mut r = ok_req();
        r.piece_length = 65536;
        r.private = true;
        r.source = "MonTracker".into();
        assert!(validate_request(&r).is_ok());
    }
}
