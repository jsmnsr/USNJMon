//! SCM hosting and process-lifetime console cancellation, unavailable in std.

use anyhow::{Context, Result};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use usnjmon_core::stop::Stop;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{
    self, ServiceControlHandlerResult, ServiceStatusHandle,
};
use windows_service::{define_windows_service, service_dispatcher};
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_SHUTDOWN_EVENT, SetConsoleCtrlHandler,
};

pub const SERVICE: &str = "USNJMon";
static PATHS: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
static CONSOLE_STOP: OnceLock<Stop> = OnceLock::new();

define_windows_service!(service_entry, service_main);

pub fn dispatch(config: PathBuf, state: PathBuf) -> Result<()> {
    PATHS
        .set((config, state))
        .map_err(|_| anyhow::anyhow!("service dispatcher already initialized"))?;
    service_dispatcher::start(SERVICE, service_entry)
        .context("SCM dispatcher (use foreground outside SCM)")
}

fn status(state: ServiceState, code: ServiceExitCode) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: code,
        checkpoint: if matches!(
            state,
            ServiceState::StartPending | ServiceState::StopPending
        ) {
            1
        } else {
            0
        },
        wait_hint: if matches!(
            state,
            ServiceState::StartPending | ServiceState::StopPending
        ) {
            Duration::from_secs(30)
        } else {
            Duration::ZERO
        },
        process_id: None,
    }
}

fn control(control: ServiceControl, stop: &Stop) -> ServiceControlHandlerResult {
    match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            stop.request();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    }
}

fn service_main(_: Vec<OsString>) {
    if let Err(error) = run_service() {
        eprintln!("SCM service failure: {error:#}");
    }
}

fn run_service() -> Result<()> {
    let stop = Stop::default();
    let handler_stop = stop.clone();
    let shared: Arc<Mutex<Option<ServiceStatusHandle>>> = Arc::new(Mutex::new(None));
    let handler_status = shared.clone();
    let handle = service_control_handler::register(SERVICE, move |event| {
        let locked = handler_status.lock().expect("SCM status lock poisoned");
        let result = control(event, &handler_stop);
        if handler_stop.requested() {
            if let Some(handle) = &*locked {
                if let Err(error) = handle.set_service_status(status(
                    ServiceState::StopPending,
                    ServiceExitCode::Win32(0),
                )) {
                    eprintln!("cannot report SCM stop pending: {error}");
                }
            }
        }
        result
    })?;
    handle.set_service_status(status(
        ServiceState::StartPending,
        ServiceExitCode::Win32(0),
    ))?;
    *shared.lock().expect("SCM status lock poisoned") = Some(handle);
    let ready_stop = stop.clone();
    let result = match PATHS.get() {
        Some((config, state)) => super::collector::run(config, state, stop, || {
            let locked = shared.lock().expect("SCM status lock poisoned");
            if !ready_stop.requested() {
                locked
                    .as_ref()
                    .expect("SCM status handle registered before collector startup")
                    .set_service_status(status(ServiceState::Running, ServiceExitCode::Win32(0)))?;
            }
            Ok(())
        }),
        None => Err(anyhow::anyhow!("missing service paths")),
    };
    let code = if result.is_ok() {
        ServiceExitCode::Win32(0)
    } else {
        ServiceExitCode::ServiceSpecific(1)
    };
    let mut locked = shared.lock().expect("SCM status lock poisoned");
    locked
        .take()
        .expect("SCM status handle retained until shutdown")
        .set_service_status(status(ServiceState::Stopped, code))?;
    result
}

unsafe extern "system" fn console_control(code: u32) -> i32 {
    if matches!(
        code,
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_SHUTDOWN_EVENT
    ) {
        if let Some(stop) = CONSOLE_STOP.get() {
            stop.request();
            return 1;
        }
    }
    0
}

pub fn foreground(config: PathBuf, state: PathBuf) -> Result<()> {
    let stop = console_stop()?;
    super::collector::run(&config, &state, stop, || Ok(()))
}

pub fn console_stop() -> Result<Stop> {
    let stop = Stop::default();
    CONSOLE_STOP
        .set(stop.clone())
        .map_err(|_| anyhow::anyhow!("console handler already initialized"))?;
    // SAFETY: Handler has the required ABI and accesses only process-lifetime synchronized state.
    if unsafe { SetConsoleCtrlHandler(Some(console_control), 1) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("register console cancellation handler");
    }
    Ok(stop)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scm_stop_and_shutdown_cancel_without_scm_registration() {
        for event in [ServiceControl::Stop, ServiceControl::Shutdown] {
            let stop = Stop::default();
            assert!(matches!(
                control(event, &stop),
                ServiceControlHandlerResult::NoError
            ));
            assert!(stop.wait(Duration::from_secs(30)));
        }
        let stop = Stop::default();
        assert!(matches!(
            control(ServiceControl::Interrogate, &stop),
            ServiceControlHandlerResult::NoError
        ));
        assert!(!stop.requested());
    }
}
