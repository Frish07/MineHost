use crate::{backups, drive, drive_auth, error, lock, safety, Result, Shared};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::State;
pub const SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS backup_schedules(server_id TEXT PRIMARY KEY,data TEXT NOT NULL);";
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub enabled: bool,
    pub interval_hours: u32,
    pub keep_local: usize,
    pub cloud: bool,
    pub keep_cloud: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_hours: 24,
            keep_local: 7,
            cloud: false,
            keep_cloud: 7,
        }
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub settings: Settings,
    pub next_due: u64,
    pub last_success: u64,
    pub last_attempt: u64,
    pub message: String,
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn validate(s: &Settings) -> Result<()> {
    if !(1..=168).contains(&s.interval_hours)
        || !(1..=100).contains(&s.keep_local)
        || !(1..=100).contains(&s.keep_cloud)
    {
        return Err("Interval: 1–168 hours; retention: 1–100 copies".into());
    }
    Ok(())
}
fn read(shared: &Shared, id: &str) -> Result<Plan> {
    let inner = lock(shared)?;
    crate::load(&inner, id)?;
    let rows = inner.db.query_row(
        "SELECT data FROM backup_schedules WHERE server_id=?1",
        [id],
        |r| r.get::<_, String>(0),
    );
    match rows {
        Ok(v) => serde_json::from_str(&v).map_err(error),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Plan::default()),
        Err(e) => Err(error(e)),
    }
}
fn write(shared: &Shared, id: &str, p: &Plan) -> Result<()> {
    let inner = lock(shared)?;
    crate::load(&inner, id)?;
    inner.db.execute("INSERT INTO backup_schedules(server_id,data) VALUES(?1,?2) ON CONFLICT(server_id) DO UPDATE SET data=excluded.data",rusqlite::params![id,serde_json::to_string(p).map_err(error)?]).map_err(error)?;
    Ok(())
}
#[tauri::command]
pub fn backup_schedule(state: State<'_, Shared>, id: String) -> Result<Plan> {
    read(&state, &id)
}
#[tauri::command]
pub fn save_backup_schedule(
    state: State<'_, Shared>,
    id: String,
    settings: Settings,
    consent: bool,
) -> Result<Plan> {
    let (_operation, _cancel) = drive::job(&state)?;
    validate(&settings)?;
    if settings.enabled && !consent {
        return Err(
            "Confirm automatic backup creation, retention and (if selected) Google upload".into(),
        );
    }
    if settings.enabled && settings.cloud && !drive_auth::linked()? {
        return Err("Connect Google Drive before enabling cloud schedule".into());
    }
    if lock(&state)?.busy.contains(&id) {
        return Err("Wait for the current profile operation".into());
    }
    let mut plan = read(&state, &id)?;
    plan.next_due = if settings.enabled {
        now() + u64::from(settings.interval_hours) * 3600
    } else {
        0
    };
    plan.settings = settings;
    plan.last_attempt = 0;
    plan.message =
        "Schedule saved; runs only while MineHost is open and the server is stopped".into();
    write(&state, &id, &plan)?;
    Ok(plan)
}
fn automatic_name(id: &str, name: &str) -> bool {
    name.strip_prefix(&format!("{id}-auto-"))
        .and_then(|s| s.strip_suffix(".zip"))
        .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
}
fn retain_local(root: &Path, id: &str, keep: usize) -> Result<()> {
    let folder = root.join("backups");
    safety::reject_links(&folder)?;
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(folder).map_err(error)? {
        let entry = entry.map_err(error)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if automatic_name(id, &name) {
            safety::reject_links(&entry.path())?;
            let m = entry.metadata().map_err(error)?;
            if m.is_file() {
                entries.push((m.modified().unwrap_or(UNIX_EPOCH), name, entry.path()));
            }
        }
    }
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    for (_, _, path) in entries.into_iter().skip(keep) {
        safety::reject_links(&path)?;
        std::fs::remove_file(path).map_err(error)?;
    }
    Ok(())
}
fn perform(shared: &Shared, id: &str, p: &Plan, cancel: &AtomicBool) -> Result<()> {
    let (server, root, profile) = {
        let inner = lock(shared)?;
        (
            crate::load(&inner, id)?,
            inner.root.clone(),
            crate::directory(&inner, id)?,
        )
    };
    if p.settings.cloud {
        drive_auth::access_token()?;
    }
    let resumed = if p.settings.cloud {
        drive::resume_scheduled(shared, cancel, id)?
    } else {
        false
    };
    if !resumed {
        drive::progress(shared, "archiving", 0, 0);
        let source = backups::create(&root, &server, &profile)?;
        let name = format!("{id}-auto-{}.zip", uuid::Uuid::new_v4());
        let path = root.join("backups").join(&name);
        std::fs::rename(source, &path).map_err(error)?;
        retain_local(&root, id, p.settings.keep_local)?;
        if p.settings.cloud {
            drive::upload(shared, cancel, id, &name, "scheduled", false)?;
        }
    }
    if p.settings.cloud {
        drive::retain_cloud(
            &drive_auth::client()?,
            &drive_auth::access_token()?,
            id,
            p.settings.keep_cloud,
            cancel,
        )?;
    }
    retain_local(&root, id, p.settings.keep_local)
}
fn due(p: &Plan, time: u64) -> bool {
    p.settings.enabled
        && p.next_due <= time
        && (p.last_attempt == 0 || time.saturating_sub(p.last_attempt) >= 300)
}
fn tick(shared: &Shared, id: &str) -> Result<()> {
    let mut p = read(shared, id)?;
    if !due(&p, now()) {
        return Ok(());
    }
    {
        let inner = lock(shared)?;
        if inner.running.contains_key(id)
            || inner.busy.contains(id)
            || inner.busy.contains("google-drive")
            || inner.busy.contains("java-install")
        {
            drop(inner);
            p.message = "Waiting: stop the server and finish current operations".into();
            write(shared, id, &p)?;
            return Ok(());
        }
    }
    let (_job, cancel) = drive::job(shared)?;
    let _profile = crate::begin(shared, id)?;
    p.last_attempt = now();
    p.message = "Creating scheduled backup".into();
    write(shared, id, &p)?;
    let result = perform(shared, id, &p, &cancel);
    drive::finish(shared, &result);
    if result.is_ok() {
        p.last_success = now();
        p.next_due = p.last_success + u64::from(p.settings.interval_hours) * 3600;
        p.message = "Scheduled backup complete".into();
    } else {
        p.message = result.as_ref().err().cloned().unwrap_or_default();
    }
    write(shared, id, &p)?;
    result
}
pub fn start(shared: &Shared) {
    let weak = Arc::downgrade(shared);
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(30));
        let Some(shared) = weak.upgrade() else { break };
        let ids = (|| -> Result<Vec<String>> {
            let inner = lock(&shared)?;
            let mut statement = inner
                .db
                .prepare("SELECT server_id FROM backup_schedules")
                .map_err(error)?;
            let rows = statement
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(error)?;
            rows.map(|r| r.map_err(error)).collect()
        })();
        if let Ok(ids) = ids {
            for id in ids {
                let _ = tick(&shared, &id);
            }
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schedule_and_retention_protect_manual_copies() {
        let stage = crate::archive::Stage::new(&std::env::temp_dir()).unwrap();
        let root = &stage.0;
        std::fs::create_dir(root.join("backups")).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let manual = format!("{id}-{}.zip", uuid::Uuid::new_v4());
        std::fs::write(root.join("backups").join(&manual), b"manual").unwrap();
        for _ in 0..3 {
            std::fs::write(
                root.join("backups")
                    .join(format!("{id}-auto-{}.zip", uuid::Uuid::new_v4())),
                b"auto",
            )
            .unwrap();
        }
        retain_local(root, &id, 1).unwrap();
        assert!(root.join("backups").join(manual).is_file());
        assert_eq!(std::fs::read_dir(root.join("backups")).unwrap().count(), 2);
        let mut p = Plan::default();
        assert!(!due(&p, 100));
        p.settings.enabled = true;
        p.next_due = 100;
        assert!(due(&p, 100));
        p.last_attempt = 99;
        assert!(!due(&p, 100));
        assert!(due(&p, 399));
        p.settings.keep_local = 0;
        assert!(validate(&p.settings).is_err());
    }
}
