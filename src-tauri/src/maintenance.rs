use crate::{error, lock, Operation, Result, Shared};
use serde::Serialize;
use std::{
    collections::HashMap,
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tauri::{Emitter, State};
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub active: bool,
    pub phase: String,
    pub bytes: u64,
    pub total: u64,
    pub backup: Option<String>,
    pub restarted: bool,
    pub error: Option<String>,
}
pub struct Job {
    token: uuid::Uuid,
    cancel: Arc<AtomicBool>,
    pub status: Status,
}
pub type Jobs = HashMap<String, Job>;
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub backup: Option<String>,
    pub restarted: bool,
    pub error: Option<String>,
}
fn update(shared: &Shared, id: &str, token: uuid::Uuid, phase: &str, bytes: u64, total: u64) {
    if let Ok(mut inner) = lock(shared) {
        if let Some(job) = inner.maintenance.get_mut(id).filter(|j| j.token == token) {
            job.status.phase = phase.into();
            job.status.bytes = bytes;
            job.status.total = total;
        }
    }
}
pub fn cancel_in(inner: &crate::Inner, id: &str) -> bool {
    if let Some(job) = inner.maintenance.get(id).filter(|j| j.status.active) {
        job.cancel.store(true, Ordering::Relaxed);
        true
    } else {
        false
    }
}
struct Reservation {
    operation: Operation,
    session: Option<String>,
    custom: String,
    token: uuid::Uuid,
    cancel: Arc<AtomicBool>,
}
fn reserve(shared: &Shared, id: &str, allow_stop: bool) -> Result<Reservation> {
    let mut inner = lock(shared)?;
    crate::load(&inner, id)?;
    if inner.busy.contains("java-install") || inner.busy.contains(id) {
        return Err("Wait for current profile/Java operations".into());
    }
    let (session, custom) = if let Some(r) = inner.running.get(id) {
        if !allow_stop {
            return Err("Stop the server or explicitly allow a maintenance backup".into());
        }
        if r.intent != crate::lifecycle::Intent::None {
            return Err("Server is already stopping/restarting".into());
        }
        (Some(r.session.clone()), r.custom_path.clone())
    } else {
        (None, String::new())
    };
    let token = uuid::Uuid::new_v4();
    let cancel = Arc::new(AtomicBool::new(false));
    inner.busy.insert(id.into());
    inner.maintenance.insert(
        id.into(),
        Job {
            token,
            cancel: cancel.clone(),
            status: Status {
                active: true,
                phase: if session.is_some() {
                    "stopping"
                } else {
                    "archiving"
                }
                .into(),
                ..Default::default()
            },
        },
    );
    Ok(Reservation {
        operation: Operation {
            shared: shared.clone(),
            id: id.into(),
        },
        session,
        custom,
        token,
        cancel,
    })
}
pub fn execute(
    shared: &Shared,
    id: &str,
    allow_stop: bool,
    restart: bool,
    external: Option<&AtomicBool>,
    restore: impl FnOnce(Operation, String) -> Result<()>,
) -> Result<Outcome> {
    let Reservation {
        operation,
        session,
        custom,
        token,
        cancel,
    } = reserve(shared, id, allow_stop)?;
    let was_running = session.is_some();
    let mut operation = Some(operation);
    let mut clean_stop = !was_running;
    let cancelled =
        || cancel.load(Ordering::Relaxed) || external.is_some_and(|c| c.load(Ordering::Relaxed));
    let result = (|| -> Result<String> {
        if let Some(session) = &session {
            {
                let mut inner = lock(shared)?;
                let r = inner
                    .running
                    .get_mut(id)
                    .ok_or("Server exited before maintenance stop")?;
                if &r.session != session {
                    return Err("Server session changed".into());
                }
                r.intent = crate::lifecycle::Intent::Stop;
                let mut input = r.input.lock().map_err(error)?;
                writeln!(input, "stop").map_err(error)?;
                input.flush().map_err(error)?;
            }
            let deadline = Instant::now() + Duration::from_secs(180);
            loop {
                if cancelled() {
                    return Err("Maintenance cancelled; no automatic restart will be made".into());
                }
                let inner = lock(shared)?;
                if let Some(r) = inner.running.get(id) {
                    if &r.session != session {
                        return Err("Server session changed during maintenance".into());
                    }
                } else {
                    if inner.lifecycle.get(id).and_then(|c| c.last_exit) != Some(0) {
                        return Err("Java did not exit cleanly; consistent backup and automatic resume were refused".into());
                    }
                    clean_stop = true;
                    break;
                }
                drop(inner);
                if Instant::now() >= deadline {
                    return Err("Java did not stop within 180 seconds. No forced kill, backup or replacement process was attempted.".into());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if cancelled() {
            return Err("Maintenance cancelled".into());
        }
        let (server, root, dir) = {
            let inner = lock(shared)?;
            (
                crate::load(&inner, id)?,
                inner.root.clone(),
                crate::directory(&inner, id)?,
            )
        };
        update(shared, id, token, "archiving", 0, 0);
        let path = crate::backups::create_progress(&root, &server, &dir, &|done, total| {
            if cancelled() {
                return Err("Backup cancelled; partial ZIP removed".into());
            }
            update(shared, id, token, "archiving", done, total);
            Ok(())
        })?;
        path.file_name()
            .and_then(|v| v.to_str())
            .map(str::to_string)
            .ok_or("Invalid backup filename".into())
    })();
    let mut outcome = Outcome {
        backup: result.as_ref().ok().cloned(),
        restarted: false,
        error: result.err(),
    };
    if was_running && restart && clean_stop && !cancelled() {
        update(shared, id, token, "starting", 0, 0);
        let owned = operation.take().ok_or("Missing maintenance reservation")?;
        if let Err(e) = restore(owned, custom) {
            outcome.error = Some(format!(
                "{}Server could not be restarted: {e}",
                outcome
                    .error
                    .as_ref()
                    .map(|s| format!("{s}; "))
                    .unwrap_or_default()
            ));
        } else {
            outcome.restarted = true;
        }
    }
    if let Ok(mut inner) = lock(shared) {
        if let Some(job) = inner.maintenance.get_mut(id).filter(|j| j.token == token) {
            job.status.active = false;
            job.status.phase = if cancelled() {
                "cancelled"
            } else if outcome.error.is_some() {
                "failed"
            } else {
                "done"
            }
            .into();
            job.status.backup = outcome.backup.clone();
            job.status.restarted = outcome.restarted;
            job.status.error = outcome.error.clone();
        }
    }
    drop(operation);
    Ok(outcome)
}
pub fn capture(
    shared: &Shared,
    app: tauri::AppHandle,
    id: &str,
    allow_stop: bool,
    external: Option<&AtomicBool>,
) -> Result<Outcome> {
    let result = execute(
        shared,
        id,
        allow_stop,
        true,
        external,
        |operation, custom| crate::lifecycle::launch(operation, app.clone(), custom, None),
    );
    let _ = app.emit("servers-changed", ());
    result
}
#[tauri::command]
pub async fn maintenance_backup(
    state: State<'_, Shared>,
    app: tauri::AppHandle,
    id: String,
    consent: bool,
    restart: bool,
) -> Result<Outcome> {
    if !consent {
        return Err("Approve stop → ZIP backup → optional restart, including player disconnection and executing installed server code again".into());
    }
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = execute(&shared, &id, true, restart, None, |op, custom| {
            crate::lifecycle::launch(op, app.clone(), custom, None)
        });
        let _ = app.emit("servers-changed", ());
        result
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub fn maintenance_status(state: State<'_, Shared>, id: String) -> Result<Status> {
    let inner = lock(&state)?;
    crate::load(&inner, &id)?;
    Ok(inner
        .maintenance
        .get(&id)
        .map(|j| j.status.clone())
        .unwrap_or_default())
}
#[tauri::command]
pub fn maintenance_cancel(state: State<'_, Shared>, id: String) -> Result<()> {
    let inner = lock(&state)?;
    crate::load(&inner, &id)?;
    cancel_in(&inner, &id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stopped_backup_cancellation_leaves_no_partial_zip_and_releases_profile() {
        let stage = crate::archive::Stage::new(&std::env::temp_dir()).unwrap();
        let shared = crate::tests::memory_state();
        let id = {
            let mut inner = lock(&shared).unwrap();
            inner.root = stage.0.clone();
            let id: String = inner
                .db
                .query_row("SELECT id FROM servers", [], |r| r.get(0))
                .unwrap();
            std::fs::create_dir_all(inner.root.join("servers").join(&id)).unwrap();
            std::fs::write(
                inner.root.join("servers").join(&id).join("data.txt"),
                b"stable",
            )
            .unwrap();
            id
        };
        let cancel = AtomicBool::new(true);
        let result = execute(&shared, &id, false, true, Some(&cancel), |_, _| {
            panic!("Stopped servers must not be started by backup")
        })
        .unwrap();
        assert!(result.error.is_some());
        assert!(result.backup.is_none());
        assert!(crate::begin(&shared, &id).is_ok());
        let result = execute(&shared, &id, false, true, None, |_, _| {
            panic!("No restart for a previously stopped server")
        })
        .unwrap();
        assert!(result.error.is_none());
        let name = result.backup.unwrap();
        let zip = stage.0.join("backups").join(name);
        assert!(crate::backups::preview(&zip).is_ok());
        assert!(!result.restarted);
    }
    #[test]
    fn real_child_exits_before_zip_and_resume_callback() {
        use std::process::{Command, Stdio};
        use std::sync::Mutex;
        let stage = crate::archive::Stage::new(&std::env::temp_dir()).unwrap();
        let shared = crate::tests::memory_state();
        let mut child = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$line=[Console]::ReadLine();if($line -eq 'stop'){exit 0}else{exit 1}",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let session = uuid::Uuid::new_v4().to_string();
        let id = {
            let mut inner = lock(&shared).unwrap();
            inner.root = stage.0.clone();
            inner.db.execute_batch(crate::lifecycle::SCHEMA).unwrap();
            inner.db.execute_batch(crate::history::SCHEMA).unwrap();
            let id: String = inner
                .db
                .query_row("SELECT id FROM servers", [], |r| r.get(0))
                .unwrap();
            let dir = inner.root.join("servers").join(&id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("data.txt"), b"flushed fixture").unwrap();
            inner.running.insert(
                id.clone(),
                crate::Running {
                    input: Arc::new(Mutex::new(child.stdin.take().unwrap())),
                    ram: 512,
                    player_query: None,
                    players: None,
                    connected: Default::default(),
                    session: session.clone(),
                    process: None,
                    pid: child.id(),
                    started: Instant::now(),
                    intent: crate::lifecycle::Intent::None,
                    custom_path: "fixture".into(),
                    tick_query: None,
                    last_tick_query: None,
                },
            );
            id
        };
        let child_state = shared.clone();
        let child_id = id.clone();
        let watcher = std::thread::spawn(move || {
            let status = child.wait().unwrap();
            crate::lifecycle::after_exit(
                &child_state,
                &child_id,
                &session,
                status.code(),
                !status.success(),
                Duration::ZERO,
            )
            .unwrap();
        });
        let mut resumed = false;
        let result = execute(&shared, &id, true, true, None, |operation, custom| {
            assert_eq!(custom, "fixture");
            assert!(lock(&shared).unwrap().running.is_empty());
            assert!(crate::begin(&shared, &id).is_err());
            resumed = true;
            drop(operation);
            Ok(())
        })
        .unwrap();
        watcher.join().unwrap();
        assert!(resumed && result.restarted && result.error.is_none());
        assert!(result.backup.is_some());
        assert!(crate::begin(&shared, &id).is_ok());
    }
}
