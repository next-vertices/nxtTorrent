//! L'installateur (service root) : revérification, bascule, retour arrière. systemd est remplacé par un faux ; le test réel
//! sous systemd est `deploy/systemd/test/self-update.sh`.
use master::selfupdate::{apply, restore_snapshot, ApplyConfig, Host, Outcome};
use proto::version::Version;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::TempDir;
use testkit::signed::Signer;

const ASSET: &str = "nxt-master-linux-amd64";
const OLD: &[u8] = b"ancien binaire 1.0.0";
const NEW: &[u8] = b"nouveau binaire 1.1.0";

struct World {
    _tmp: TempDir,
    data: PathBuf,
    state: PathBuf,
    target: PathBuf,
    signer: Signer,
}

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let state = tmp.path().join("state");
    let bin = tmp.path().join("bin");
    for d in [&data.join("update/staged"), &state, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    let target = bin.join("nxt-master");
    std::fs::write(&target, OLD).unwrap();
    World {
        _tmp: tmp,
        data,
        state,
        target,
        signer: Signer::new(),
    }
}

impl World {
    fn stage(&self, version: &str, binary: &[u8]) {
        let (m, s) = self.signer.release(version, &[(ASSET, binary)]);
        let d = self.data.join("update/staged");
        std::fs::write(d.join("manifest.json"), m).unwrap();
        std::fs::write(d.join("manifest.json.sig"), s).unwrap();
        std::fs::write(d.join("nxt-master"), binary).unwrap();
    }
    fn config(&self) -> ApplyConfig {
        ApplyConfig {
            data_dir: self.data.clone(),
            target: self.target.clone(),
            state_dir: self.state.clone(),
            keys: vec![self.signer.key],
            installed: Version::parse("1.0.0").unwrap(),
            asset: ASSET.to_owned(),
            health_timeout: Duration::from_millis(300),
            poll: Duration::from_millis(20),
        }
    }
    fn status(&self) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(self.state.join("status.json")).unwrap()).unwrap()
    }
    fn target(&self) -> Vec<u8> {
        std::fs::read(&self.target).unwrap()
    }
    fn assert_untouched(&self, host: &Fake) {
        assert_eq!(self.target(), OLD, "binaire inchangé");
        assert!(
            !host.calls.iter().any(|c| c == "stop"),
            "service jamais arrêté : {:?}",
            host.calls
        );
        assert!(!self.target.with_extension("new").exists(), "aucun résidu");
    }
}

#[derive(Default)]
struct Fake {
    /// Version annoncée par `--version` du nouveau binaire.
    probe: Option<String>,
    /// Le nouveau binaire se déclare sain au démarrage.
    healthy: bool,
    fail_stop: bool,
    fail_start_of_new: bool,
    fail_restore: bool,
    data: PathBuf,
    target: PathBuf,
    calls: Vec<String>,
}

fn fake(w: &World) -> Fake {
    Fake {
        probe: Some("nxt-master 1.1.0".to_owned()),
        healthy: true,
        data: w.data.clone(),
        target: w.target.clone(),
        ..Fake::default()
    }
}

impl Host for Fake {
    fn stop(&mut self) -> Result<(), String> {
        self.calls.push("stop".into());
        if self.fail_stop {
            Err("refusé".into())
        } else {
            Ok(())
        }
    }
    fn start(&mut self) -> Result<(), String> {
        self.calls.push("start".into());
        let new_in_place = std::fs::read(&self.target).unwrap() == NEW;
        if new_in_place && self.fail_start_of_new {
            return Err("échec au démarrage".into());
        }
        if new_in_place && self.healthy {
            std::fs::write(self.data.join("update/healthy"), "1.1.0\n").unwrap();
        }
        Ok(())
    }
    fn probe_version(&mut self, binary: &Path) -> Result<String, String> {
        self.calls.push("probe".into());
        assert_eq!(
            std::fs::read(binary).unwrap(),
            NEW,
            "on sonde les octets vérifiés"
        );
        self.probe
            .clone()
            .ok_or_else(|| "ne démarre pas".to_owned())
    }
    fn restore_snapshot(&mut self, binary: &Path) -> Result<(), String> {
        self.calls.push("restore".into());
        assert_eq!(
            std::fs::read(binary).unwrap(),
            OLD,
            "restauration par l'ANCIEN binaire"
        );
        if self.fail_restore {
            Err("disque plein".into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn a_genuine_newer_release_replaces_the_binary_and_keeps_the_old_one() {
    let w = world();
    w.stage("1.1.0", NEW);
    let mut h = fake(&w);
    assert_eq!(apply(&w.config(), &mut h), Outcome::Installed);
    assert_eq!(w.target(), NEW);
    assert_eq!(std::fs::read(w.state.join("previous")).unwrap(), OLD);
    assert_eq!(h.calls, ["probe", "stop", "start"]);
    let s = w.status();
    assert_eq!(
        (
            s["state"].as_str(),
            s["version"].as_str(),
            s["from"].as_str()
        ),
        (Some("installed"), Some("1.1.0"), Some("1.0.0"))
    );
}

#[test]
fn nothing_is_touched_when_the_staged_files_cannot_be_trusted() {
    // Binaire modifié après la signature (le dépôt appartient au compte non privilégié).
    let w = world();
    w.stage("1.1.0", NEW);
    std::fs::write(w.data.join("update/staged/nxt-master"), b"cheval de Troie").unwrap();
    let mut h = fake(&w);
    assert!(matches!(apply(&w.config(), &mut h), Outcome::Rejected(_)));
    w.assert_untouched(&h);
    assert_eq!(w.status()["state"], "rejected");

    // Signé par une autre clé.
    let w = world();
    let evil = Signer::new();
    let (m, s) = evil.release("9.9.9", &[(ASSET, NEW)]);
    std::fs::write(w.data.join("update/staged/manifest.json"), m).unwrap();
    std::fs::write(w.data.join("update/staged/manifest.json.sig"), s).unwrap();
    std::fs::write(w.data.join("update/staged/nxt-master"), NEW).unwrap();
    let mut h = fake(&w);
    assert!(matches!(apply(&w.config(), &mut h), Outcome::Rejected(_)));
    w.assert_untouched(&h);

    // Release authentique mais plus ancienne, ou identique : jamais de retour en arrière.
    for old in ["0.9.0", "1.0.0"] {
        let w = world();
        w.stage(old, NEW);
        let mut h = fake(&w);
        let r = apply(&w.config(), &mut h);
        assert!(
            matches!(&r, Outcome::Rejected(why) if why.contains("pas plus récente")),
            "{old} : {r:?}"
        );
        w.assert_untouched(&h);
    }

    // Rien de déposé.
    let w = world();
    let mut h = fake(&w);
    assert!(matches!(apply(&w.config(), &mut h), Outcome::Rejected(_)));
    w.assert_untouched(&h);

    // Un dossier à la place du binaire.
    let w = world();
    w.stage("1.1.0", NEW);
    std::fs::remove_file(w.data.join("update/staged/nxt-master")).unwrap();
    std::fs::create_dir(w.data.join("update/staged/nxt-master")).unwrap();
    let mut h = fake(&w);
    assert!(matches!(apply(&w.config(), &mut h), Outcome::Rejected(_)));
    w.assert_untouched(&h);
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_in_the_staging_area_is_refused() {
    let w = world();
    w.stage("1.1.0", NEW);
    let p = w.data.join("update/staged/nxt-master");
    std::fs::remove_file(&p).unwrap();
    std::os::unix::fs::symlink("/dev/zero", &p).unwrap();
    let mut h = fake(&w);
    assert!(matches!(apply(&w.config(), &mut h), Outcome::Rejected(_)));
    w.assert_untouched(&h);
}

#[test]
fn a_binary_that_does_not_run_or_a_service_that_does_not_stop_changes_nothing() {
    for probe in [None, Some("nxt-master 2.0.0".to_owned())] {
        let w = world();
        w.stage("1.1.0", NEW);
        let mut h = fake(&w);
        h.probe = probe;
        assert!(matches!(apply(&w.config(), &mut h), Outcome::Rejected(_)));
        w.assert_untouched(&h);
    }
    let w = world();
    w.stage("1.1.0", NEW);
    let mut h = fake(&w);
    h.fail_stop = true;
    assert!(matches!(apply(&w.config(), &mut h), Outcome::Rejected(_)));
    assert_eq!(w.target(), OLD);
    assert_eq!(h.calls.last().unwrap(), "start", "le service est relancé");
}

#[test]
fn a_new_version_that_never_becomes_healthy_is_rolled_back_binary_and_database() {
    let w = world();
    w.stage("1.1.0", NEW);
    let mut h = fake(&w);
    h.healthy = false;
    assert!(matches!(apply(&w.config(), &mut h), Outcome::RolledBack(_)));
    assert_eq!(w.target(), OLD);
    assert_eq!(
        h.calls,
        ["probe", "stop", "start", "stop", "restore", "start"]
    );
    assert_eq!(w.status()["state"], "rolled_back");

    // Même chose si elle ne démarre pas du tout.
    let w = world();
    w.stage("1.1.0", NEW);
    let mut h = fake(&w);
    h.fail_start_of_new = true;
    assert!(matches!(apply(&w.config(), &mut h), Outcome::RolledBack(_)));
    assert_eq!(w.target(), OLD);
}

#[test]
fn a_health_marker_left_by_an_earlier_attempt_does_not_count() {
    let w = world();
    w.stage("1.1.0", NEW);
    let marker = w.data.join("update/healthy");
    std::fs::write(&marker, "1.1.0\n").unwrap();
    let past = std::time::SystemTime::now() - Duration::from_secs(600);
    std::fs::File::options()
        .write(true)
        .open(&marker)
        .unwrap()
        .set_modified(past)
        .unwrap();
    let mut h = fake(&w);
    h.healthy = false;
    assert!(matches!(apply(&w.config(), &mut h), Outcome::RolledBack(_)));
}

#[test]
fn an_impossible_rollback_is_reported_and_the_service_is_still_started() {
    let w = world();
    w.stage("1.1.0", NEW);
    let mut h = fake(&w);
    h.healthy = false;
    h.fail_restore = true;
    assert!(matches!(apply(&w.config(), &mut h), Outcome::Failed(_)));
    assert_eq!(h.calls.last().unwrap(), "start");
    assert_eq!(w.status()["state"], "failed");
}

#[test]
fn restoring_the_snapshot_replaces_the_database_and_drops_its_journals() {
    let d = tempfile::tempdir().unwrap();
    assert!(restore_snapshot(d.path()).is_err(), "aucun instantané");
    std::fs::create_dir(d.path().join("update")).unwrap();
    std::fs::write(d.path().join("update/snapshot.db"), b"avant").unwrap();
    for f in ["master.db", "master.db-wal", "master.db-shm"] {
        std::fs::write(d.path().join(f), b"apres").unwrap();
    }
    restore_snapshot(d.path()).unwrap();
    assert_eq!(std::fs::read(d.path().join("master.db")).unwrap(), b"avant");
    assert!(!d.path().join("master.db-wal").exists() && !d.path().join("master.db-shm").exists());
}
