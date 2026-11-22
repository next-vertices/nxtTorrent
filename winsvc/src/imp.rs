//! Implémentation Windows (crate `windows-service`).
// Confiné : la macro `define_windows_service!` produit le `extern "system" fn` appelé par le SCM.
#![allow(unsafe_code)]

use super::{Error, InstallSpec};
use std::ffi::OsString;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::Notify;
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
    ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{
    self, ServiceControlHandlerResult, ServiceStatusHandle,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

type Entry = Box<dyn FnOnce(Arc<Notify>) -> Result<(), String> + Send>;

static ENTRY: Mutex<Option<Entry>> = Mutex::new(None);
static NAME: OnceLock<String> = OnceLock::new();
static HANDLE: OnceLock<ServiceStatusHandle> = OnceLock::new();

const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;
/// Code d'erreur Windows 1063 : le processus n'a pas été lancé par le SCM.
const ERROR_FAILED_SERVICE_CONTROLLER_CONNECT: i32 = 1063;
/// Code d'erreur Windows 5 : accès refusé (droits administrateur requis).
const ERROR_ACCESS_DENIED: i32 = 5;
/// Code d'erreur Windows 1062 : le service n'est pas démarré.
const ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;

fn werr(e: windows_service::Error) -> Error {
    // Le message utile (ex. « Accès refusé ») est dans la source d'erreur système.
    let detail = std::error::Error::source(&e)
        .map(ToString::to_string)
        .unwrap_or_default();
    let mut msg = if detail.is_empty() {
        e.to_string()
    } else {
        format!("{e}: {detail}")
    };
    if os_code(&e) == Some(ERROR_ACCESS_DENIED) {
        msg.push_str(" — ouvrez un terminal en tant qu'administrateur");
    }
    Error::Windows(msg)
}

fn os_code(e: &windows_service::Error) -> Option<i32> {
    std::error::Error::source(e)
        .and_then(|s| s.downcast_ref::<std::io::Error>())
        .and_then(std::io::Error::raw_os_error)
}

fn status_of(
    state: ServiceState,
    exit: ServiceExitCode,
    accept: ServiceControlAccept,
    hint: Duration,
) -> ServiceStatus {
    ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: state,
        controls_accepted: accept,
        exit_code: exit,
        checkpoint: 0,
        wait_hint: hint,
        process_id: None,
    }
}

// ---------- Gestion (installation, démarrage…) ----------

pub fn install(spec: &InstallSpec) -> Result<(), Error> {
    let manager =
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CREATE_SERVICE)
            .map_err(werr)?;
    let info = ServiceInfo {
        name: OsString::from(&spec.name),
        display_name: OsString::from(&spec.display_name),
        service_type: SERVICE_TYPE,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: spec.exe.clone(),
        launch_arguments: spec.args.clone(),
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let service = manager
        .create_service(
            &info,
            ServiceAccess::CHANGE_CONFIG | ServiceAccess::QUERY_STATUS | ServiceAccess::START,
        )
        .map_err(werr)?;
    service.set_description(&spec.description).map_err(werr)?;
    // Démarrage différé : laisse le réseau et les clients torrent démarrer d'abord.
    service.set_delayed_auto_start(true).map_err(werr)?;
    // Redémarrage automatique après un plantage.
    let restart = |secs| ServiceAction {
        action_type: ServiceActionType::Restart,
        delay: Duration::from_secs(secs),
    };
    service
        .update_failure_actions(ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
            reboot_msg: None,
            command: None,
            actions: Some(vec![restart(5), restart(30), restart(60)]),
        })
        .map_err(werr)?;
    service
        .set_failure_actions_on_non_crash_failures(true)
        .map_err(werr)?;
    Ok(())
}

fn open(name: &str, access: ServiceAccess) -> Result<windows_service::service::Service, Error> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(werr)?;
    manager.open_service(name, access).map_err(werr)
}

pub fn status(name: &str) -> Result<String, Error> {
    let s = open(name, ServiceAccess::QUERY_STATUS)?;
    let st = s.query_status().map_err(werr)?;
    // Le PID permet de vérifier (et de tester) le redémarrage automatique après plantage.
    let pid = st
        .process_id
        .map_or_else(|| "-".to_owned(), |p| p.to_string());
    Ok(format!("{:?} (pid {pid})", st.current_state))
}

pub fn start(name: &str) -> Result<(), Error> {
    open(name, ServiceAccess::START)?
        .start::<OsString>(&[])
        .map_err(werr)
}

pub fn stop(name: &str) -> Result<(), Error> {
    let s = open(name, ServiceAccess::STOP | ServiceAccess::QUERY_STATUS)?;
    match s.stop() {
        Ok(_) => {}
        Err(e) if os_code(&e) == Some(ERROR_SERVICE_NOT_ACTIVE) => return Ok(()),
        Err(e) => return Err(werr(e)),
    }
    for _ in 0..60 {
        if s.query_status().map_err(werr)?.current_state == ServiceState::Stopped {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(Error::Windows(
        "le service ne s'est pas arrêté dans les 30 s".into(),
    ))
}

pub fn uninstall(name: &str) -> Result<(), Error> {
    // Arrêt best-effort (un service déjà arrêté n'est pas une erreur), puis suppression.
    let _ = stop(name);
    open(name, ServiceAccess::DELETE)?.delete().map_err(werr)
}

// ---------- Exécution sous le SCM ----------

define_windows_service!(ffi_service_main, service_main);

pub fn run(name: &str, entry: Entry) -> Result<(), Error> {
    let _ = NAME.set(name.to_owned());
    *ENTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(entry);
    service_dispatcher::start(name, ffi_service_main).map_err(|e| {
        if os_code(&e) == Some(ERROR_FAILED_SERVICE_CONTROLLER_CONNECT) {
            Error::NotUnderScm
        } else {
            werr(e)
        }
    })
}

fn service_main(_args: Vec<OsString>) {
    let code = match run_service() {
        Ok(()) => ServiceExitCode::Win32(0),
        Err(_) => ServiceExitCode::ServiceSpecific(1),
    };
    if let Some(h) = HANDLE.get() {
        let _ = h.set_service_status(status_of(
            ServiceState::Stopped,
            code,
            ServiceControlAccept::empty(),
            Duration::ZERO,
        ));
    }
}

fn run_service() -> Result<(), String> {
    let name = NAME.get().cloned().ok_or("nom de service absent")?;
    let entry = ENTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .ok_or("point d'entrée absent")?;
    let stop = Arc::new(Notify::new());
    let stop_for_handler = stop.clone();
    let handle = service_control_handler::register(&name, move |event| match event {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            // `notify_one` mémorise le signal : aucun arrêt perdu même si l'entrée n'attend pas encore.
            stop_for_handler.notify_one();
            if let Some(h) = HANDLE.get() {
                let pending = status_of(
                    ServiceState::StopPending,
                    ServiceExitCode::Win32(0),
                    ServiceControlAccept::empty(),
                    Duration::from_secs(15),
                );
                let _ = h.set_service_status(pending);
            }
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })
    .map_err(|e| e.to_string())?;
    let _ = HANDLE.set(handle);
    handle
        .set_service_status(status_of(
            ServiceState::Running,
            ServiceExitCode::Win32(0),
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            Duration::ZERO,
        ))
        .map_err(|e| e.to_string())?;
    entry(stop)
}
