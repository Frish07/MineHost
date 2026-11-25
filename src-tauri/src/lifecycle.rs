use crate::{
    error, forge, lock, monitor, runtime, safety, software, Operation, Result, Running, Shared,
    RAM_BUDGET,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::Write,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{Emitter, State};
pub const SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS restart_policies(server_id TEXT PRIMARY KEY,data TEXT NOT NULL);";
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    None,
    Stop,
    Restart,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Policy {
    pub enabled: bool,
    pub max_attempts: u32,
    pub delay_seconds: u64,
    pub stable_seconds: u64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            max_attempts: 3,
            delay_seconds: 10,
            stable_seconds: 300,
        }
    }
}
impl Policy {
    fn validate(&self) -> Result<()> {
        if !(1..=10).contains(&self.max_attempts)
            || !(5..=300).contains(&self.delay_seconds)
            || !(60..=86400).contains(&self.stable_seconds)
        {
            return Err(
                "Restart policy: 1–10 attempts, delay 5–300s, stability window 60–86400s".into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct Ticket {
    cancel: Arc<AtomicBool>,
    due: Instant,
    custom_path: String,
}
#[derive(Default)]
pub struct Control {
    attempts: u32,
    pending: Option<Ticket>,
    phase: String,
    pub(crate) last_exit: Option<i32>,
    last_error: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    policy: Policy,
    attempts: u32,
    phase: String,
    pending: bool,
    retry_in_seconds: Option<u64>,
    last_exit: Option<i32>,
    last_error: Option<String>,
}
fn read(inner: &crate::Inner, id: &str) -> Result<Policy> {
    crate::load(inner, id)?;
    match inner.db.query_row(
        "SELECT data FROM restart_policies WHERE server_id=?1",
        [id],
        |r| r.get::<_, String>(0),
    ) {
        Ok(s) => serde_json::from_str(&s).map_err(error),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Policy::default()),
        Err(e) => Err(error(e)),
    }
}
fn reserve(
    control: &mut Control,
    policy: &Policy,
    intent: Intent,
    failed: bool,
    uptime: Duration,
    custom_path: String,
) -> Option<Ticket> {
    if intent == Intent::Stop || (intent == Intent::None && (!failed || !policy.enabled)) {
        return None;
    }
    let delay = if intent == Intent::Restart {
        control.attempts = 0;
        Duration::ZERO
    } else {
        if uptime.as_secs() >= policy.stable_seconds {
            control.attempts = 0;
        }
        if control.attempts >= policy.max_attempts {
            control.phase = "limit_reached".into();
            return None;
        }
        let delay = policy
            .delay_seconds
            .saturating_mul(1_u64 << control.attempts.min(10))
            .min(300);
        control.attempts += 1;
        Duration::from_secs(delay)
    };
    let ticket = Ticket {
        cancel: Arc::new(AtomicBool::new(false)),
        due: Instant::now() + delay,
        custom_path,
    };
    control.pending = Some(ticket.clone());
    control.phase = "restart_pending".into();
    Some(ticket)
}
fn wait(ticket: &Ticket) -> Result<()> {
    while Instant::now() < ticket.due {
        if ticket.cancel.load(Ordering::Relaxed) {
            return Err("Restart cancelled".into());
        }
        std::thread::sleep(
            ticket
                .due
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(100)),
        );
    }
    if ticket.cancel.load(Ordering::Relaxed) {
        Err("Restart cancelled".into())
    } else {
        Ok(())
    }
}
fn schedule(operation: Operation, app: tauri::AppHandle, ticket: Ticket) {
    std::thread::spawn(move || {
        let shared = operation.shared.clone();
        let id = operation.id.clone();
        let result = wait(&ticket).and_then(|()| {
            launch(
                operation,
                app.clone(),
                ticket.custom_path.clone(),
                Some(&ticket),
            )
        });
        if let Err(e) = result {
            if let Ok(mut inner) = lock(&shared) {
                if let Some(control) = inner.lifecycle.get_mut(&id) {
                    if control
                        .pending
                        .as_ref()
                        .is_some_and(|p| Arc::ptr_eq(&p.cancel, &ticket.cancel))
                    {
                        control.pending = None;
                        control.phase = if ticket.cancel.load(Ordering::Relaxed) {
                            "cancelled"
                        } else {
                            "start_failed"
                        }
                        .into();
                        control.last_error = Some(e.clone());
                    }
                }
            }
            crate::append_log(
                &shared,
                &app,
                &id,
                format!("[MineHost] Restart stopped: {e}"),
            );
        }
        let _ = app.emit("servers-changed", ());
    });
}
pub fn launch(
    operation: Operation,
    app: tauri::AppHandle,
    custom_path: String,
    ticket: Option<&Ticket>,
) -> Result<()> {
    let shared = operation.shared.clone();
    let id = operation.id.clone();
    let (server, dir, root) = {
        let inner = lock(&shared)?;
        (
            crate::load(&inner, &id)?,
            crate::directory(&inner, &id)?,
            inner.root.clone(),
        )
    };
    if !server.eula || !server.installed {
        return Err(
            "Install server software and explicitly accept the EULA before starting".into(),
        );
    }
    if let Some(ticket) = ticket {
        if ticket.cancel.load(Ordering::Relaxed) {
            return Err("Restart cancelled".into());
        }
    }
    let java = runtime::detect_java(&root, server.java_major, &custom_path)?;
    let mut inner = lock(&shared)?;
    if let Some(ticket) = ticket {
        if ticket.cancel.load(Ordering::Relaxed) {
            return Err("Restart cancelled".into());
        }
    }
    if inner.running.contains_key(&id) || inner.busy.contains("java-install") {
        return Err("Server already running or Java installation active".into());
    }
    if inner.running.values().map(|r| r.ram).sum::<u32>() + server.ram_mb > RAM_BUDGET {
        return Err("Combined server heap budget exceeds 12 GiB".into());
    }
    let port_probe = std::net::TcpListener::bind(("0.0.0.0", server.port))
        .map_err(|e| format!("Port unavailable: {e}"))?;
    let mut command = Command::new(java.path);
    command.args(["-Xms256M", &format!("-Xmx{}M", server.ram_mb)]);
    if matches!(
        server.software,
        software::Software::Forge | software::Software::Neoforge
    ) {
        command.args(forge::launch_arguments(
            &dir,
            server.software,
            &server.version,
        )?);
    } else {
        command.args(["-jar", server.software.launcher(), "nogui"]);
    }
    command
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    runtime::hide_window(&mut command);
    drop(port_probe);
    let mut child = command.spawn().map_err(error)?;
    let input = match child.stdin.take() {
        Some(input) => input,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Missing child stdin".into());
        }
    };
    let process = monitor::Process::capture(&child).ok();
    let pid = child.id();
    let session = uuid::Uuid::new_v4().to_string();
    let started = Instant::now();
    inner.running.insert(
        id.clone(),
        Running {
            input: Arc::new(Mutex::new(input)),
            ram: server.ram_mb,
            player_query: None,
            players: None,
            connected: HashSet::new(),
            session: session.clone(),
            process,
            pid,
            started,
            intent: Intent::None,
            custom_path,
            tick_query: None,
            last_tick_query: None,
        },
    );
    let control = inner.lifecycle.entry(id.clone()).or_default();
    control.pending = None;
    control.phase = "running".into();
    control.last_error = None;
    drop(inner);
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(crate::read_pipe(
            stdout,
            shared.clone(),
            app.clone(),
            id.clone(),
            session.clone(),
        ));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(crate::read_pipe(
            stderr,
            shared.clone(),
            app.clone(),
            id.clone(),
            session.clone(),
        ));
    }
    crate::append_log(
        &shared,
        &app,
        &id,
        format!("[MineHost] Started Java PID {pid}"),
    );
    drop(operation);
    std::thread::spawn(move || {
        let mut reported = false;
        let result = loop {
            match child.wait() {
                Ok(status) => break status,
                Err(e) => {
                    if !reported {
                        let message = format!("Cannot confirm Java exit: {e}. No replacement process will be started until exit is confirmed.");
                        crate::append_log(&shared, &app, &id, format!("[MineHost] {message}"));
                        if let Ok(mut inner) = lock(&shared) {
                            let control = inner.lifecycle.entry(id.clone()).or_default();
                            control.phase = "wait_failed".into();
                            control.last_error = Some(message);
                        }
                        reported = true;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        };
        let code = result.code();
        let failed = !result.success();
        let uptime = started.elapsed();
        for reader in readers {
            let _ = reader.recv_timeout(Duration::from_secs(2));
        }
        let next = after_exit(&shared, &id, &session, code, failed, uptime);
        crate::append_log(
            &shared,
            &app,
            &id,
            format!("[MineHost] Process exited: {result:?}"),
        );
        match next {
            Ok(Some((operation, ticket))) => {
                crate::append_log(
                    &shared,
                    &app,
                    &id,
                    "[MineHost] Restart scheduled; cancel it in server monitoring if needed".into(),
                );
                schedule(operation, app.clone(), ticket);
            }
            Err(e) => crate::append_log(
                &shared,
                &app,
                &id,
                format!("[MineHost] Restart policy error: {e}"),
            ),
            _ => {}
        }
        let _ = app.emit("servers-changed", ());
    });
    Ok(())
}

pub(crate) fn after_exit(
    shared: &Shared,
    id: &str,
    session: &str,
    code: Option<i32>,
    failed: bool,
    uptime: Duration,
) -> Result<Option<(Operation, Ticket)>> {
    let mut inner = lock(shared)?;
    if inner.running.get(id).is_none_or(|r| r.session != session) {
        return Ok(None);
    }
    if let Err(e) = crate::history::end(&inner, id) {
        eprintln!("Connection history: {e}");
    }
    let running = inner.running.remove(id).ok_or("Missing process")?;
    let policy = read(&inner, id)?;
    let control = inner.lifecycle.entry(id.to_string()).or_default();
    control.last_exit = code;
    control.phase = if failed { "crashed" } else { "stopped" }.into();
    let ticket = reserve(
        control,
        &policy,
        running.intent,
        failed,
        uptime,
        running.custom_path,
    );
    if let Some(ticket) = ticket {
        inner.busy.insert(id.to_string());
        Ok(Some((
            Operation {
                shared: shared.clone(),
                id: id.to_string(),
            },
            ticket,
        )))
    } else {
        Ok(None)
    }
}
pub fn manual_start(
    shared: &Shared,
    app: tauri::AppHandle,
    id: &str,
    custom_path: String,
) -> Result<()> {
    let operation = crate::begin(shared, id)?;
    lock(shared)?
        .lifecycle
        .insert(id.into(), Control::default());
    launch(operation, app, custom_path, None)
}
pub fn command(shared: &Shared, id: &str, text: &str) -> Result<()> {
    safety::command(text)?;
    let text = text.trim().trim_start_matches('/');
    let mut inner = lock(shared)?;
    let stop = matches!(text.split_whitespace().next(),Some("stop"|"minecraft:stop"));
    let maintenance_active=inner.maintenance.get(id).is_some_and(|j|j.status.active);
    if maintenance_active {
        if stop {crate::maintenance::cancel_in(&inner,id);if !inner.running.contains_key(id){return Ok(());}}
        else {return Err("Wait for maintenance backup to finish or cancel it before sending commands".into());}
    }
    let running = inner.running.get_mut(id).ok_or("Server is not running")?;
    let mut input = running.input.lock().map_err(error)?;
    if matches!(
        text.split_whitespace().next(),
        Some("stop" | "minecraft:stop")
    ) {
        running.intent = Intent::Stop;
    }
    writeln!(input, "{text}").map_err(error)?;
    input.flush().map_err(error)
}
#[tauri::command]
pub fn restart_server(state: State<'_, Shared>, id: String) -> Result<()> {
    let mut inner = lock(&state)?;
    if inner.busy.contains(&id) {
        return Err("Wait for the profile operation".into());
    }
    let running = inner.running.get_mut(&id).ok_or("Server is not running")?;
    if running.intent != Intent::None {
        return Err("Server is already stopping; wait or cancel its restart".into());
    }
    let mut input = running.input.lock().map_err(error)?;
    running.intent = Intent::Restart;
    if let Err(e) = writeln!(input, "stop").and_then(|()| input.flush()) {
        running.intent = Intent::Stop;
        return Err(error(e));
    }
    Ok(())
}
#[tauri::command]
pub fn cancel_restart(state: State<'_, Shared>, id: String) -> Result<()> {
    let mut inner = lock(&state)?;
    crate::load(&inner, &id)?;
    if let Some(running) = inner.running.get_mut(&id) {
        if running.intent == Intent::Restart {
            running.intent = Intent::Stop;
            return Ok(());
        }
        return Err("Server is running; use Stop".into());
    }
    if let Some(ticket) = inner.lifecycle.get(&id).and_then(|c| c.pending.as_ref()) {
        ticket.cancel.store(true, Ordering::Relaxed);
    }
    Ok(())
}
#[tauri::command]
pub fn restart_status(state: State<'_, Shared>, id: String) -> Result<Status> {
    let inner = lock(&state)?;
    let policy = read(&inner, &id)?;
    let control = inner.lifecycle.get(&id);
    let waiting_stop = inner
        .running
        .get(&id)
        .is_some_and(|r| r.intent == Intent::Restart);
    let phase = if waiting_stop {
        "stopping_for_restart"
    } else if inner
        .running
        .get(&id)
        .is_some_and(|r| r.intent == Intent::Stop)
    {
        "stopping"
    } else {
        control
            .map(|c| c.phase.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("stopped")
    };
    Ok(Status {
        policy,
        attempts: control.map(|c| c.attempts).unwrap_or(0),
        phase: phase.into(),
        pending: waiting_stop || control.is_some_and(|c| c.pending.is_some()),
        retry_in_seconds: control
            .and_then(|c| c.pending.as_ref())
            .map(|p| p.due.saturating_duration_since(Instant::now()).as_secs()),
        last_exit: control.and_then(|c| c.last_exit),
        last_error: control.and_then(|c| c.last_error.clone()),
    })
}
#[tauri::command]
pub fn save_restart_policy(
    state: State<'_, Shared>,
    id: String,
    policy: Policy,
    consent: bool,
) -> Result<()> {
    policy.validate()?;
    if policy.enabled && !consent {
        return Err("Confirm automatic execution of this server after failures".into());
    }
    let inner = lock(&state)?;
    crate::load(&inner, &id)?;
    inner.db.execute("INSERT INTO restart_policies(server_id,data) VALUES(?1,?2) ON CONFLICT(server_id) DO UPDATE SET data=excluded.data",rusqlite::params![id,serde_json::to_string(&policy).map_err(error)?]).map_err(error)?;
    if !policy.enabled {
        if let Some(ticket) = inner.lifecycle.get(&id).and_then(|c| c.pending.as_ref()) {
            ticket.cancel.store(true, Ordering::Relaxed);
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirmed_child_failure_reserves_profile_and_stale_exit_is_ignored() {
        let shared = crate::tests::memory_state();
        let mut child = Command::new("cmd.exe")
            .args(["/d", "/c", "exit 7"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let session = uuid::Uuid::new_v4().to_string();
        let id = {
            let mut inner = lock(&shared).unwrap();
            inner.db.execute_batch(SCHEMA).unwrap();
            inner.db.execute_batch(crate::history::SCHEMA).unwrap();
            let id: String = inner
                .db
                .query_row("SELECT id FROM servers", [], |r| r.get(0))
                .unwrap();
            let policy = Policy {
                enabled: true,
                ..Default::default()
            };
            inner
                .db
                .execute(
                    "INSERT INTO restart_policies(server_id,data) VALUES(?1,?2)",
                    rusqlite::params![id, serde_json::to_string(&policy).unwrap()],
                )
                .unwrap();
            inner.running.insert(
                id.clone(),
                Running {
                    input: Arc::new(Mutex::new(child.stdin.take().unwrap())),
                    ram: 512,
                    player_query: None,
                    players: None,
                    connected: HashSet::new(),
                    session: session.clone(),
                    process: None,
                    pid: child.id(),
                    started: Instant::now(),
                    intent: Intent::None,
                    custom_path: String::new(),
                    tick_query: None,
                    last_tick_query: None,
                },
            );
            id
        };
        let result = child.wait().unwrap();
        assert_eq!(result.code(), Some(7));
        assert!(after_exit(
            &shared,
            &id,
            "old-session",
            result.code(),
            true,
            Duration::ZERO
        )
        .unwrap()
        .is_none());
        assert!(lock(&shared).unwrap().running.contains_key(&id));
        let (operation, ticket) = after_exit(
            &shared,
            &id,
            &session,
            result.code(),
            !result.success(),
            Duration::ZERO,
        )
        .unwrap()
        .unwrap();
        assert!(crate::begin(&shared, &id).is_err());
        assert!(!lock(&shared).unwrap().running.contains_key(&id));
        ticket.cancel.store(true, Ordering::Relaxed);
        assert!(wait(&ticket).is_err());
        drop(operation);
        assert!(crate::begin(&shared, &id).is_ok());
    }
    #[test]
    fn manual_stop_clean_exit_and_retry_limit_are_distinct() {
        let p = Policy {
            enabled: true,
            max_attempts: 2,
            ..Default::default()
        };
        let mut c = Control::default();
        assert!(reserve(
            &mut c,
            &p,
            Intent::Stop,
            true,
            Duration::ZERO,
            String::new()
        )
        .is_none());
        assert!(reserve(
            &mut c,
            &p,
            Intent::None,
            false,
            Duration::ZERO,
            String::new()
        )
        .is_none());
        let first = reserve(
            &mut c,
            &p,
            Intent::None,
            true,
            Duration::ZERO,
            String::new(),
        )
        .unwrap();
        assert!(first.due.saturating_duration_since(Instant::now()) > Duration::from_secs(8));
        assert!(reserve(
            &mut c,
            &p,
            Intent::None,
            true,
            Duration::ZERO,
            String::new()
        )
        .is_some());
        assert!(reserve(
            &mut c,
            &p,
            Intent::None,
            true,
            Duration::ZERO,
            String::new()
        )
        .is_none());
        assert_eq!(c.phase, "limit_reached");
        assert!(reserve(
            &mut c,
            &p,
            Intent::None,
            true,
            Duration::from_secs(300),
            String::new()
        )
        .is_some());
        assert_eq!(c.attempts, 1);
        let mut disabled = p.clone();
        disabled.enabled = false;
        assert!(reserve(
            &mut Control::default(),
            &disabled,
            Intent::None,
            true,
            Duration::ZERO,
            String::new()
        )
        .is_none());
    }
    #[test]
    fn restart_cancellation_interrupts_delay() {
        let ticket = Ticket {
            cancel: Arc::new(AtomicBool::new(false)),
            due: Instant::now() + Duration::from_secs(10),
            custom_path: String::new(),
        };
        let flag = ticket.cancel.clone();
        let worker = std::thread::spawn(move || wait(&ticket));
        flag.store(true, Ordering::Relaxed);
        assert!(worker.join().unwrap().is_err());
    }
}
