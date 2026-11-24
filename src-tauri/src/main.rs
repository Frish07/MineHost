#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod addons;
mod modrinth;
mod runtime;
mod safety;
mod software;

use runtime::Runtime;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{ChildStdin, Command, Stdio},
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
}

struct Inner {
    db: rusqlite::Connection,
    root: PathBuf,
    running: HashMap<String, Running>,
    busy: HashSet<String>,
    logs: HashMap<String, VecDeque<String>>,
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
fn create_server(
    state: State<'_, Shared>,
    name: String,
    version: String,
    port: u16,
    ram_mb: u32,
    software: software::Software,
) -> Result<String> {
    if name.trim().is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
        return Err("Name must contain 1–80 printable characters".into());
    }
    safety::version(&version)?;
    if port < 1024 || !(512..=RAM_BUDGET).contains(&ram_mb) {
        return Err("Port must be 1024–65535; RAM 512–12288 MiB".into());
    }
    let inner = lock(&state)?;
    let server = Server {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.trim().into(),
        version,
        port,
        ram_mb,
        eula: false,
        installed: false,
        java_major: 21,
        software,
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
    on_progress: tauri::ipc::Channel<runtime::DownloadProgress>,
) -> Result<()> {
    let operation = begin(&state, &id)?;
    let (server, dir) = {
        let inner = lock(&state)?;
        (load(&inner, &id)?, directory(&inner, &id)?)
    };
    let report = |progress| {
        let _ = on_progress.send(progress);
    };
    let major = server
        .software
        .install(&server.version, &dir, &report)
        .await
        .map_err(|e| format!("{:?} {}: {e}", server.software, server.version))?;
    let size = std::fs::metadata(dir.join(server.software.launcher()))
        .map_err(error)?
        .len();
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
fn java_status(state: State<'_, Shared>, id: String, custom_path: String) -> Result<Runtime> {
    let (major, root) = {
        let inner = lock(&state)?;
        (load(&inner, &id)?.java_major, inner.root.clone())
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
        load(&inner, &id)?.java_major
    };
    runtime::install_java(&root, major).await
}

#[derive(Clone, Serialize)]
struct LogEvent {
    id: String,
    line: String,
}
fn append_log(shared: &Shared, app: &tauri::AppHandle, id: &str, line: String) {
    let line: String = line.chars().take(8192).collect();
    if let Ok(mut inner) = shared.lock() {
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
) {
    std::thread::spawn(move || {
        // A malicious plugin cannot grow an unbounded line buffer.
        let mut reader = BufReader::new(pipe);
        loop {
            let mut buffer = Vec::new();
            match std::io::Read::take(&mut reader, 8192).read_until(b'\n', &mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(_) => append_log(
                    &shared,
                    &app,
                    &id,
                    String::from_utf8_lossy(&buffer).trim_end().to_string(),
                ),
            }
        }
    });
}

#[tauri::command]
fn start_server(
    state: State<'_, Shared>,
    app: tauri::AppHandle,
    id: String,
    custom_path: String,
) -> Result<()> {
    let _operation = begin(&state, &id)?;
    let (server, dir, root) = {
        let inner = lock(&state)?;
        (
            load(&inner, &id)?,
            directory(&inner, &id)?,
            inner.root.clone(),
        )
    };
    if !server.eula || !server.installed {
        return Err(
            "Install server software and explicitly accept the EULA before starting".into(),
        );
    }
    let java = runtime::detect_java(&root, server.java_major, &custom_path)?;
    let mut inner = lock(&state)?;
    if inner.busy.contains("java-install") {
        return Err("Wait for Java installation".into());
    }
    if inner.running.values().map(|r| r.ram).sum::<u32>() + server.ram_mb > RAM_BUDGET {
        return Err("Combined server heap budget exceeds 12 GiB".into());
    }
    let port_probe = std::net::TcpListener::bind(("0.0.0.0", server.port))
        .map_err(|e| format!("Port unavailable: {e}"))?;
    let mut command = Command::new(java.path);
    command
        .args([
            "-Xms256M",
            &format!("-Xmx{}M", server.ram_mb),
            "-jar",
            server.software.launcher(),
            "nogui",
        ])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    runtime::hide_window(&mut command);
    drop(port_probe);
    let mut child = command.spawn().map_err(error)?;
    let input = child.stdin.take().ok_or("Missing child stdin")?;
    inner.running.insert(
        id.clone(),
        Running {
            input: Arc::new(Mutex::new(input)),
            ram: server.ram_mb,
        },
    );
    inner.logs.insert(id.clone(), VecDeque::new());
    drop(inner);
    if let Some(stdout) = child.stdout.take() {
        read_pipe(stdout, state.inner().clone(), app.clone(), id.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        read_pipe(stderr, state.inner().clone(), app.clone(), id.clone());
    }
    let shared = state.inner().clone();
    std::thread::spawn(move || {
        let result = child.wait();
        if let Ok(mut inner) = shared.lock() {
            inner.running.remove(&id);
        }
        append_log(
            &shared,
            &app,
            &id,
            format!("[MineHost] Process exited: {result:?}"),
        );
        let _ = app.emit("servers-changed", ());
    });
    Ok(())
}

#[tauri::command]
fn send_command(state: State<'_, Shared>, id: String, command: String) -> Result<()> {
    safety::command(&command)?;
    let input = lock(&state)?
        .running
        .get(&id)
        .ok_or("Server is not running")?
        .input
        .clone();
    let mut input = input.lock().map_err(error)?;
    writeln!(input, "{}", command.trim_start_matches('/')).map_err(error)?;
    input.flush().map_err(error)
}

#[tauri::command]
fn stop_server(state: State<'_, Shared>, id: String) -> Result<()> {
    send_command(state, id, "stop".into())
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
) -> Result<modrinth::SearchResult> {
    let (server, _) = profile_path(&state, &id)?;
    modrinth::search(server.software, &server.version, &query, offset, &sort).await
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
    let inner = lock(&state)?;
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
    inner
        .db
        .execute("DELETE FROM servers WHERE id=?1", [id])
        .map_err(error)?;
    Ok(())
}

#[tauri::command]
async fn backup_server(state: State<'_, Shared>, id: String) -> Result<String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = begin(&shared, &id)?;
        let (dir, root) = {
            let inner = lock(&shared)?;
            (directory(&inner, &id)?, inner.root.clone())
        };
        let destination = root
            .join("backups")
            .join(format!("{}-{}.zip", id, uuid::Uuid::new_v4()));
        safety::reject_links(&destination)?;
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&destination)
            .map_err(error)?;
        let mut writer = zip::ZipWriter::new(file);
        fn add(writer: &mut zip::ZipWriter<std::fs::File>, root: &Path, dir: &Path) -> Result<()> {
            for entry in std::fs::read_dir(dir).map_err(error)? {
                let path = entry.map_err(error)?.path();
                safety::reject_links(&path)?;
                if path.is_dir() {
                    add(writer, root, &path)?;
                } else if path.is_file() {
                    let name = path
                        .strip_prefix(root)
                        .map_err(error)?
                        .to_string_lossy()
                        .replace('\\', "/");
                    writer
                        .start_file(
                            name,
                            zip::write::SimpleFileOptions::default()
                                .compression_method(zip::CompressionMethod::Deflated),
                        )
                        .map_err(error)?;
                    std::io::copy(&mut std::fs::File::open(&path).map_err(error)?, writer)
                        .map_err(error)?;
                }
            }
            Ok(())
        }
        let result = add(&mut writer, &dir, &dir);
        if let Err(e) = result {
            drop(writer);
            let _ = std::fs::remove_file(&destination);
            return Err(e);
        }
        writer.finish().map_err(error)?.sync_all().map_err(error)?;
        Ok(destination.to_string_lossy().into_owned())
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
                    if let Ok(inner) = state.lock() {
                        if !inner.running.is_empty() || !inner.busy.is_empty() {
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
        app.manage(Arc::new(Mutex::new(Inner { db, root, running: HashMap::new(), busy: HashSet::new(), logs: HashMap::new() })));
        Ok(())
    }).invoke_handler(tauri::generate_handler![list_servers, create_server, software_versions, install_software, accept_eula, java_status, install_java, start_server, stop_server, send_command, server_logs, read_properties, save_properties, import_addon, list_addons, set_addon_enabled, remove_addon, search_addons, addon_versions, plan_addon, install_addon, open_addon_project, open_folder, delete_server, backup_server])
        .build(tauri::generate_context!()).expect("Failed to initialize MineHost");
    app.run(|app, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            let state = app.state::<Shared>();
            if let Ok(inner) = state.lock() {
                if !inner.running.is_empty() || !inner.busy.is_empty() {
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

    fn memory_state() -> Shared {
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
        };
        let inner = Inner {
            db,
            root: std::env::temp_dir(),
            running: HashMap::new(),
            busy: HashSet::new(),
            logs: HashMap::new(),
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
