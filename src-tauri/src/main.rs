#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod addons;
mod archive;
mod backup_schedule;
mod backups;
mod catalog;
mod curseforge;
mod dimensions;
mod drive;
mod drive_auth;
mod files;
mod forge;
mod history;
mod lifecycle;
mod maintenance;
mod modpacks;
mod modrinth;
mod monitor;
mod network;
mod players;
mod playit;
mod properties;
mod runtime;
mod safety;
mod software;
mod tick_metrics;
mod win;
mod worlds;

use runtime::Runtime;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{ChildStdin, Command},
    sync::{Arc, Mutex},
};
use tauri::{Emitter, Manager, State};

type Result<T> = std::result::Result<T, String>;
type Shared = Arc<Mutex<Inner>>;
const LOG_LIMIT: usize = 2000;
const RAM_BUDGET: u32 = 12288;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Server {
    id: String,
    name: String,
    version: String,
    port: u16,
    ram_mb: u32,
    eula: bool,
    installed: bool,
    java_major: u32,
    #[serde(default)]
    software: software::Software,
    #[serde(default)]
    loader_version: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerView {
    #[serde(flatten)]
    server: Server,
    running: bool,
    busy: bool,
}

struct Running {
    input: Arc<Mutex<ChildStdin>>,
    ram: u32,
    player_query: Option<uuid::Uuid>,
    players: Option<Vec<String>>,
    connected: HashSet<String>,
    session: String,
    process: Option<monitor::Process>,
    pid: u32,
    started: std::time::Instant,
    intent: lifecycle::Intent,
    custom_path: String,
    tick_query: Option<tick_metrics::Query>,
    last_tick_query: Option<std::time::Instant>,
}

struct Inner {
    db: rusqlite::Connection,
    root: PathBuf,
    running: HashMap<String, Running>,
    busy: HashSet<String>,
    logs: HashMap<String, VecDeque<String>>,
    agent: Option<playit::ManagedAgent>,
    claim: Option<playit::Claim>,
    drive: drive::Session,
    lifecycle: HashMap<String, lifecycle::Control>,
    maintenance: maintenance::Jobs,
}

struct Operation {
    shared: Shared,
    id: String,
}
impl Drop for Operation {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.shared.lock() {
            inner.busy.remove(&self.id);
        }
    }
}

fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn lock(shared: &Shared) -> Result<std::sync::MutexGuard<'_, Inner>> {
    shared.lock().map_err(error)
}
fn load(inner: &Inner, id: &str) -> Result<Server> {
    let json: String = inner
        .db
        .query_row("SELECT data FROM servers WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .map_err(error)?;
    serde_json::from_str(&json).map_err(error)
}
fn save(inner: &Inner, server: &Server) -> Result<()> {
    inner.db.execute("INSERT INTO servers(id,port,data) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET port=excluded.port,data=excluded.data", rusqlite::params![server.id, server.port, serde_json::to_string(server).map_err(error)?]).map_err(error)?;
    Ok(())
}
fn directory(inner: &Inner, id: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(id).map_err(error)?;
    let path = inner.root.join("servers").join(id);
    safety::reject_links(&path)?;
    Ok(path)
}
fn begin(shared: &Shared, id: &str) -> Result<Operation> {
    let mut inner = lock(shared)?;
    load(&inner, id)?;
    if inner.busy.contains("java-install")
        || inner.running.contains_key(id)
        || !inner.busy.insert(id.to_string())
    {
        return Err("Stop the server and wait for current operations to finish".into());
    }
    Ok(Operation {
        shared: shared.clone(),
        id: id.into(),
    })
}

async fn profile_task<T: Send + 'static>(
    shared: Shared,
    id: String,
    stopped: bool,
    task: impl FnOnce(Server, PathBuf) -> Result<T> + Send + 'static,
) -> Result<T> {
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = if stopped {
            Some(begin(&shared, &id)?)
        } else {
            None
        };
        let (server, root) = profile_path(&shared, &id)?;
        task(server, root)
    })
    .await
    .map_err(error)?
}

#[tauri::command]
fn list_servers(state: State<'_, Shared>) -> Result<Vec<ServerView>> {
    let inner = lock(&state)?;
    let mut stmt = inner
        .db
        .prepare("SELECT data FROM servers ORDER BY rowid")
        .map_err(error)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(error)?;
    rows.map(|row| {
        let server: Server = serde_json::from_str(&row.map_err(error)?).map_err(error)?;
        Ok(ServerView {
            running: inner.running.contains_key(&server.id),
            busy: inner.busy.contains(&server.id),
            server,
        })
    })
    .collect()
}

#[tauri::command]
async fn create_server(
    state: State<'_, Shared>,
    name: String,
    version: String,
    port: u16,
    ram_mb: u32,
    software: software::Software,
    loader_version: Option<String>,
) -> Result<String> {
    if name.trim().is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
        return Err("Name must contain 1–80 printable characters".into());
    }
    safety::version(&version)?;
    if let Some(loader) = loader_version.as_ref().filter(|v| !v.is_empty()) {
        safety::version(loader)?;
    }
    if port < 1024 || !(512..=RAM_BUDGET).contains(&ram_mb) {
        return Err("Port must be 1024–65535; RAM 512–12288 MiB".into());
    }
    let java_major = software::required_java(&version).await?;
    let inner = lock(&state)?;
    let server = Server {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.trim().into(),
        version,
        port,
        ram_mb,
        eula: false,
        installed: false,
        java_major,
        software,
        loader_version,
    };
    save(&inner, &server)?;
    let dir = directory(&inner, &server.id)?;
    std::fs::create_dir_all(&dir).map_err(error)?;
    if let Ok(addon_dir) = software.addon_directory() {
        std::fs::create_dir_all(dir.join(addon_dir)).map_err(error)?;
    }
    std::fs::write(dir.join("server.properties"), format!("server-port={}\nonline-mode=true\nmax-players=20\ndifficulty=normal\npvp=true\nview-distance=10\nsimulation-distance=8\nmotd=MineHost Server\n", port)).map_err(error)?;
    Ok(server.id)
}

#[tauri::command]
async fn software_versions(software: software::Software) -> Result<Vec<String>> {
    software.versions().await
}

#[tauri::command]
async fn install_software(
    state: State<'_, Shared>,
    id: String,
    custom_path: String,
    installer_consent: bool,
    on_progress: tauri::ipc::Channel<runtime::DownloadProgress>,
) -> Result<()> {
    let operation = begin(&state, &id)?;
    let (server, dir) = {
        let inner = lock(&state)?;
        (load(&inner, &id)?, directory(&inner, &id)?)
    };
    let report = |mut progress: runtime::DownloadProgress| {
        if progress.phase == runtime::DownloadPhase::Complete {
            progress.phase = runtime::DownloadPhase::Installing;
        }
        let _ = on_progress.send(progress);
    };
    let (major, size) = if matches!(
        server.software,
        software::Software::Forge | software::Software::Neoforge
    ) {
        if !installer_consent {
            return Err("Explicit permission to execute the official Minecraft loader installer is required".into());
        }
        let major = software::required_java(&server.version).await?;
        let root = { lock(&state)?.root.clone() };
        let java = runtime::detect_java(&root, major, &custom_path)
            .map_err(|e| format!("First detect or download Java {major} in Server & Java. {e}"))?;
        let size = forge::install(
            server.software,
            &server.version,
            server.loader_version.as_deref(),
            &dir,
            Path::new(&java.path),
            &report,
        )
        .await?;
        (major, size)
    } else {
        let major = server
            .software
            .install_pinned(
                &server.version,
                &dir,
                server.loader_version.as_deref(),
                &report,
            )
            .await
            .map_err(|e| format!("{:?} {}: {e}", server.software, server.version))?;
        let size = std::fs::metadata(dir.join(server.software.launcher()))
            .map_err(error)?
            .len();
        (major, size)
    };
    finish_paper_install(operation, major)?;
    let _ = on_progress.send(runtime::DownloadProgress::new(
        runtime::DownloadPhase::Complete,
        size,
        Some(size),
    ));
    Ok(())
}

fn finish_paper_install(operation: Operation, java_major: u32) -> Result<()> {
    // A named guard is dropped BEFORE Operation::drop locks the state again.
    // Never use save(&*lock(...)?, ...) as a tail expression here (Rust 2021).
    let inner = lock(&operation.shared)?;
    let mut server = load(&inner, &operation.id)?;
    server.java_major = java_major;
    server.installed = true;
    save(&inner, &server)
}

#[tauri::command]
fn accept_eula(state: State<'_, Shared>, id: String, consent: bool) -> Result<()> {
    if !consent {
        return Err("Explicit Minecraft EULA consent is required".into());
    }
    let _operation = begin(&state, &id)?;
    let inner = lock(&state)?;
    let mut server = load(&inner, &id)?;
    safety::atomic_write(&directory(&inner, &id)?.join("eula.txt"), b"eula=true\n")?;
    server.eula = true;
    save(&inner, &server)
}

#[tauri::command]
async fn java_status(state: State<'_, Shared>, id: String, custom_path: String) -> Result<Runtime> {
    let (server, root) = {
        let inner = lock(&state)?;
        (load(&inner, &id)?, inner.root.clone())
    };
    let major = if server.installed {
        server.java_major
    } else {
        software::required_java(&server.version).await?
    };
    runtime::detect_java(&root, major, &custom_path)
}

#[tauri::command]
async fn install_java(state: State<'_, Shared>, id: String, consent: bool) -> Result<Runtime> {
    if !consent {
        return Err("Explicit Java runtime download consent is required".into());
    }
    // Serialize managed runtime changes across all servers.
    let root = {
        let mut inner = lock(&state)?;
        if !inner.running.is_empty() || !inner.busy.is_empty() {
            return Err("Stop servers and finish downloads before installing Java".into());
        }
        load(&inner, &id)?;
        inner.busy.insert("java-install".into());
        inner.root.clone()
    };
    let _operation = Operation {
        shared: state.inner().clone(),
        id: "java-install".into(),
    };
    let major = {
        let inner = lock(&state)?;
        load(&inner, &id)?.version
    };
    runtime::install_java(&root, software::required_java(&major).await?).await
}

#[derive(Clone, Serialize)]
struct LogEvent {
    id: String,
    line: String,
}
fn append_log(shared: &Shared, app: &tauri::AppHandle, id: &str, line: String) {
    append_session_log(shared, app, id, line, None);
}
fn append_session_log(
    shared: &Shared,
    app: &tauri::AppHandle,
    id: &str,
    line: String,
    session: Option<&str>,
) {
    let line: String = line.chars().take(8192).collect();
    if let Ok(mut inner) = shared.lock() {
        if session.is_some_and(|session| inner.running.get(id).is_none_or(|r| r.session != session))
        {
            return;
        }
        if let Err(e) = history::record(&mut inner, id, &line) {
            eprintln!("Connection history: {e}");
        }
        if let Some(running) = inner.running.get_mut(id) {
            tick_metrics::observe(running, &line);
            if running.player_query.is_some() {
                if let Some(names) = players::online_line(&line) {
                    running.players = Some(names);
                }
            }
        }
        let logs = inner.logs.entry(id.into()).or_default();
        if logs.len() >= LOG_LIMIT {
            logs.pop_front();
        }
        logs.push_back(line.clone());
    }
    let _ = app.emit(
        "server-log",
        LogEvent {
            id: id.into(),
            line,
        },
    );
}
fn read_pipe(
    pipe: impl std::io::Read + Send + 'static,
    shared: Shared,
    app: tauri::AppHandle,
    id: String,
    session: String,
) -> std::sync::mpsc::Receiver<()> {
    let (finished, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // A malicious plugin cannot grow an unbounded line buffer.
        let mut reader = BufReader::new(pipe);
        loop {
            let mut buffer = Vec::new();
            match std::io::Read::take(&mut reader, 8192).read_until(b'\n', &mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(_) => append_session_log(
                    &shared,
                    &app,
                    &id,
                    String::from_utf8_lossy(&buffer).trim_end().to_string(),
                    Some(&session),
                ),
            }
        }
        let _ = finished.send(());
    });
    receiver
}

#[tauri::command]
async fn start_server(
    state: State<'_, Shared>,
    app: tauri::AppHandle,
    id: String,
    custom_path: String,
) -> Result<()> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        lifecycle::manual_start(&shared, app, &id, custom_path)
    })
    .await
    .map_err(error)?
}
#[tauri::command]
fn send_command(state: State<'_, Shared>, id: String, command: String) -> Result<()> {
    lifecycle::command(state.inner(), &id, &command)
}
#[tauri::command]
fn stop_server(state: State<'_, Shared>, id: String) -> Result<()> {
    lifecycle::command(state.inner(), &id, "stop")
}
#[tauri::command]
fn server_logs(state: State<'_, Shared>, id: String) -> Result<Vec<String>> {
    let inner = lock(&state)?;
    load(&inner, &id)?;
    Ok(inner
        .logs
        .get(&id)
        .map(|logs| logs.iter().cloned().collect())
        .unwrap_or_default())
}

#[tauri::command]
fn read_properties(state: State<'_, Shared>, id: String) -> Result<String> {
    let inner = lock(&state)?;
    load(&inner, &id)?;
    let file = directory(&inner, &id)?.join("server.properties");
    safety::reject_links(&file)?;
    std::fs::read_to_string(file).map_err(error)
}

#[tauri::command]
fn save_properties(state: State<'_, Shared>, id: String, content: String) -> Result<()> {
    let _operation = begin(&state, &id)?;
    let inner = lock(&state)?;
    let server = load(&inner, &id)?;
    safety::properties(&content, server.port)?;
    let file = directory(&inner, &id)?.join("server.properties");
    safety::reject_links(&file)?;
    safety::reject_links(&file.with_extension("properties.bak"))?;
    std::fs::copy(&file, file.with_extension("properties.bak")).map_err(error)?;
    safety::atomic_write(&file, content.as_bytes())
}

#[tauri::command]
fn import_addon(state: State<'_, Shared>, id: String, source: String, consent: bool) -> Result<()> {
    if !consent {
        return Err("Confirm third-party plugin execution risk".into());
    }
    let _operation = begin(&state, &id)?;
    let (server, dir) = profile_path(&state, &id)?;
    if !server.installed {
        return Err("Install the selected server software before importing addons".into());
    }
    addons::import(&dir, server.software, &server.version, Path::new(&source))
}

fn profile_path(shared: &Shared, id: &str) -> Result<(Server, PathBuf)> {
    let inner = lock(shared)?;
    Ok((load(&inner, id)?, directory(&inner, id)?))
}

#[tauri::command]
fn list_addons(state: State<'_, Shared>, id: String) -> Result<Vec<addons::Installed>> {
    let (server, dir) = profile_path(&state, &id)?;
    addons::installed(&dir, server.software)
}

#[tauri::command]
fn set_addon_enabled(
    state: State<'_, Shared>,
    id: String,
    file_name: String,
    enabled: bool,
) -> Result<()> {
    let _operation = begin(&state, &id)?;
    let (server, dir) = profile_path(&state, &id)?;
    addons::set_enabled(&dir, server.software, &file_name, enabled)
}

#[tauri::command]
fn remove_addon(
    state: State<'_, Shared>,
    id: String,
    file_name: String,
    confirmation: String,
) -> Result<()> {
    if confirmation != file_name {
        return Err("Confirm the addon filename before removal".into());
    }
    let _operation = begin(&state, &id)?;
    let (server, dir) = profile_path(&state, &id)?;
    addons::remove(&dir, server.software, &file_name)
}

#[tauri::command]
async fn search_addons(
    state: State<'_, Shared>,
    id: String,
    query: String,
    offset: u32,
    sort: String,
    category: Option<String>,
) -> Result<modrinth::SearchResult> {
    let (server, _) = profile_path(&state, &id)?;
    modrinth::search(
        server.software,
        &server.version,
        &query,
        offset,
        &sort,
        category.as_deref(),
    )
    .await
}

#[tauri::command]
async fn addon_versions(
    state: State<'_, Shared>,
    id: String,
    project_id: String,
) -> Result<Vec<modrinth::Version>> {
    let (server, _) = profile_path(&state, &id)?;
    modrinth::versions(&project_id, server.software, &server.version).await
}

fn active_projects(dir: &Path, software: software::Software) -> Result<Vec<(String, String)>> {
    Ok(addons::installed(dir, software)?
        .into_iter()
        .filter(|v| v.enabled)
        .filter(|v| v.provider == Some(addons::Provider::Modrinth))
        .filter_map(|v| Some((v.project_id?, v.version_id?)))
        .collect())
}

#[tauri::command]
async fn plan_addon(
    state: State<'_, Shared>,
    id: String,
    version_id: String,
) -> Result<Vec<modrinth::PlanItem>> {
    let (server, dir) = profile_path(&state, &id)?;
    modrinth::plan(
        &version_id,
        server.software,
        &server.version,
        &active_projects(&dir, server.software)?,
    )
    .await
}

#[tauri::command]
async fn install_addon(
    state: State<'_, Shared>,
    id: String,
    version_id: String,
    approved_versions: Vec<String>,
    consent: bool,
    on_progress: tauri::ipc::Channel<runtime::DownloadProgress>,
) -> Result<()> {
    if !consent {
        return Err("Confirm third-party code execution risk before installing addons".into());
    }
    let _operation = begin(&state, &id)?;
    let (server, dir) = profile_path(&state, &id)?;
    let report = |event| {
        let _ = on_progress.send(event);
    };
    if !server.installed {
        return Err("Install the selected server software before installing addons".into());
    }
    report(runtime::DownloadProgress::new(
        runtime::DownloadPhase::Resolving,
        0,
        None,
    ));
    let items = modrinth::plan(
        &version_id,
        server.software,
        &server.version,
        &active_projects(&dir, server.software)?,
    )
    .await?;
    addons::install(
        &dir,
        server.software,
        &server.version,
        &items,
        &approved_versions,
        &report,
    )
    .await
}

#[tauri::command]
fn open_addon_project(project_id: String) -> Result<()> {
    modrinth::identifier(&project_id)?;
    Command::new("explorer.exe")
        .arg(format!("https://modrinth.com/project/{project_id}"))
        .spawn()
        .map_err(error)?;
    Ok(())
}

#[tauri::command]
fn open_folder(state: State<'_, Shared>, id: String) -> Result<()> {
    let inner = lock(&state)?;
    load(&inner, &id)?;
    let path = directory(&inner, &id)?;
    Command::new("explorer.exe")
        .arg(path)
        .spawn()
        .map_err(error)?;
    Ok(())
}

#[tauri::command]
fn delete_server(state: State<'_, Shared>, id: String, confirmation: String) -> Result<()> {
    let _operation = begin(&state, &id)?;
    let mut inner = lock(&state)?;
    if inner
        .agent
        .as_mut()
        .is_some_and(playit::ManagedAgent::alive)
    {
        return Err("Stop the managed playit agent before deleting a server profile".into());
    }
    let server = load(&inner, &id)?;
    if confirmation != server.name {
        return Err("Type the exact server name to confirm deletion".into());
    }
    let dir = directory(&inner, &id)?;
    // Rename to a trash directory instead of recursively following a plugin-created junction.
    let trash = inner.root.join("trash");
    std::fs::create_dir_all(&trash).map_err(error)?;
    safety::reject_links(&trash)?;
    std::fs::rename(dir, trash.join(format!("{}-{}", id, uuid::Uuid::new_v4()))).map_err(error)?;
    let transaction = inner.db.unchecked_transaction().map_err(error)?;
    transaction
        .execute("DELETE FROM connection_history WHERE server_id=?1", [&id])
        .map_err(error)?;
    transaction
        .execute("DELETE FROM servers WHERE id=?1", [&id])
        .map_err(error)?;
    transaction
        .execute("DELETE FROM network_settings WHERE server_id=?1", [&id])
        .map_err(error)?;
    transaction
        .execute("DELETE FROM backup_schedules WHERE server_id=?1", [&id])
        .map_err(error)?;
    transaction
        .execute("DELETE FROM restart_policies WHERE server_id=?1", [&id])
        .map_err(error)?;
    transaction.commit().map_err(error)?;
    inner.lifecycle.remove(&id);
    Ok(())
}

#[tauri::command]
async fn backup_server(state: State<'_, Shared>, id: String) -> Result<String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = begin(&shared, &id)?;
        let (server, root, dir) = {
            let inner = lock(&shared)?;
            (
                load(&inner, &id)?,
                inner.root.clone(),
                directory(&inner, &id)?,
            )
        };
        Ok(backups::create(&root, &server, &dir)?
            .to_string_lossy()
            .into_owned())
    })
    .await
    .map_err(error)?
}
fn main() {
    let app = tauri::Builder::default().plugin(tauri_plugin_dialog::init()).setup(|app| {
        if let Some(window) = app.get_webview_window("main") {
            let handle = app.handle().clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    let state = handle.state::<Shared>();
                    if let Ok(mut inner) = state.lock() {
                        if !inner.running.is_empty() || !inner.busy.is_empty() || inner.agent.as_mut().is_some_and(playit::ManagedAgent::alive) {
                            api.prevent_close();
                            let _ = handle.emit("exit-blocked", ());
                        }
                    };
                }
            });
        }
        let root = app.path().app_local_data_dir()?;
        for name in ["servers", "backups", "cache", "logs", "config", "runtimes", "trash"] { std::fs::create_dir_all(root.join(name))?; }
        let db = rusqlite::Connection::open(root.join("config/minehost.sqlite"))?;
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS servers(id TEXT PRIMARY KEY,port INTEGER UNIQUE NOT NULL,data TEXT NOT NULL);")?;
        db.execute_batch(history::SCHEMA)?;
        db.execute_batch(network::SCHEMA)?;
        db.execute_batch(backup_schedule::SCHEMA)?;
        db.execute_batch(lifecycle::SCHEMA)?;
        app.manage(Arc::new(Mutex::new(Inner { db, root, running: HashMap::new(), busy: HashSet::new(), logs: HashMap::new(), agent: None, claim: None, drive: drive::Session::default(), lifecycle: HashMap::new(), maintenance: HashMap::new() })));
        backup_schedule::start(app.state::<Shared>().inner(), app.handle().clone());
        Ok(())
    }).invoke_handler(tauri::generate_handler![playit::playit_auto_tunnel, properties::properties_snapshot, properties::properties_save_form, properties::properties_save_raw, maintenance::maintenance_backup, maintenance::maintenance_status, maintenance::maintenance_cancel, curseforge::cf_open_site, curseforge::cf_details, curseforge::cf_status, curseforge::cf_configure, curseforge::cf_forget, curseforge::cf_test, curseforge::cf_categories, curseforge::cf_search, curseforge::cf_project, curseforge::cf_versions, curseforge::cf_plan, curseforge::cf_install, curseforge::cf_open_project, curseforge::cf_help, monitor::server_metrics, lifecycle::restart_server, lifecycle::cancel_restart, lifecycle::restart_status, lifecycle::save_restart_policy, tick_metrics::query_tick_metrics, drive::drive_help, drive::drive_status, drive::drive_cancel, drive::drive_configure, drive::drive_connect, drive::drive_disconnect, drive::drive_discard, drive::drive_list, drive::drive_upload, drive::drive_resume, drive::drive_download, drive::drive_trash, backup_schedule::backup_schedule, backup_schedule::save_backup_schedule, list_servers, create_server, software_versions, install_software, accept_eula, java_status, install_java, start_server, stop_server, send_command, server_logs, read_properties, save_properties, import_addon, list_addons, set_addon_enabled, remove_addon, search_addons, addon_versions, plan_addon, install_addon, open_addon_project, open_folder, delete_server, backup_server, files::list_files, files::read_file, files::save_file, files::file_action, worlds::list_worlds, worlds::configure_world, worlds::import_world, worlds::export_world, worlds::trash_world, players::player_records, players::player_action, players::query_players, history::connection_history, catalog::catalog_categories, catalog::catalog_details, modpacks::inspect_pack, modpacks::install_pack, backups::list_backups, backups::inspect_backup, backups::restore_backup, backups::import_backup, dimensions::transfer_dimension, network::connection_endpoint, network::network_settings, network::save_network_settings, network::network_adapters, network::public_ip, network::local_network_status, network::external_network_status, network::dns_check, network::firewall_status, network::firewall_change, network::upnp_change, playit::playit_install, playit::playit_import_agent, playit::playit_claim, playit::playit_finish_claim, playit::playit_start, playit::playit_stop, playit::playit_forget, playit::playit_status, playit::network_open_link])
        .build(tauri::generate_context!()).expect("Failed to initialize MineHost");
    app.run(|app, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            let state = app.state::<Shared>();
            if let Ok(mut inner) = state.lock() {
                if !inner.running.is_empty()
                    || !inner.busy.is_empty()
                    || inner
                        .agent
                        .as_mut()
                        .is_some_and(playit::ManagedAgent::alive)
                {
                    api.prevent_exit();
                    let _ = app.emit("exit-blocked", ());
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                    }
                }
            };
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn memory_state() -> Shared {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE servers(id TEXT PRIMARY KEY,port INTEGER UNIQUE NOT NULL,data TEXT NOT NULL);").unwrap();
        let server = Server {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Test".into(),
            version: "1.21.11".into(),
            port: 25565,
            ram_mb: 2048,
            eula: false,
            installed: false,
            java_major: 21,
            software: software::Software::Paper,
            loader_version: None,
        };
        let inner = Inner {
            db,
            root: std::env::temp_dir(),
            running: HashMap::new(),
            busy: HashSet::new(),
            logs: HashMap::new(),
            agent: None,
            claim: None,
            drive: drive::Session::default(),
            lifecycle: HashMap::new(),
            maintenance: HashMap::new(),
        };
        save(&inner, &server).unwrap();
        Arc::new(Mutex::new(inner))
    }

    #[test]
    fn sqlite_rejects_port_collisions() {
        let state = memory_state();
        let inner = lock(&state).unwrap();
        let json: String = inner
            .db
            .query_row("SELECT data FROM servers", [], |row| row.get(0))
            .unwrap();
        let mut server: Server = serde_json::from_str(&json).unwrap();
        server.id = uuid::Uuid::new_v4().to_string();
        assert!(save(&inner, &server).is_err());
        server.port += 1;
        assert!(save(&inner, &server).is_ok());
    }

    #[test]
    fn legacy_profile_json_without_software_still_loads_as_paper() {
        let state = memory_state();
        let inner = lock(&state).unwrap();
        let json: String = inner
            .db
            .query_row("SELECT data FROM servers", [], |row| row.get(0))
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value.as_object_mut().unwrap().remove("software");
        let profile: Server = serde_json::from_value(value).unwrap();
        assert_eq!(profile.software, software::Software::Paper);
    }

    #[test]
    fn operations_release_the_guard_after_failure() {
        let state = memory_state();
        let id: String = lock(&state)
            .unwrap()
            .db
            .query_row("SELECT id FROM servers", [], |row| row.get(0))
            .unwrap();
        let operation = begin(&state, &id).unwrap();
        assert!(begin(&state, &id).is_err());
        drop(operation);
        assert!(begin(&state, &id).is_ok());
        lock(&state).unwrap().busy.insert("java-install".into());
        assert!(begin(&state, &id).is_err());
    }

    #[test]
    fn paper_install_finishes_and_releases_state_on_success_and_error() {
        for fail_save in [false, true] {
            let state = memory_state();
            let id: String = lock(&state)
                .unwrap()
                .db
                .query_row("SELECT id FROM servers", [], |row| row.get(0))
                .unwrap();
            let operation = begin(&state, &id).unwrap();
            if fail_save {
                lock(&state)
                    .unwrap()
                    .db
                    .execute_batch("DROP TABLE servers;")
                    .unwrap();
            }
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let result = finish_paper_install(operation, 25);
                let inner = state
                    .try_lock()
                    .expect("Paper completion kept the state locked");
                assert!(!inner.busy.contains(&id));
                if !fail_save {
                    let profile = load(&inner, &id).unwrap();
                    assert!(profile.installed);
                    assert_eq!(profile.java_major, 25);
                }
                sender.send(result).unwrap();
            });
            let result = receiver
                .recv_timeout(std::time::Duration::from_secs(3))
                .expect("Paper completion deadlocked instead of releasing its operation guard");
            assert_eq!(result.is_err(), fail_save);
        }
    }
}
