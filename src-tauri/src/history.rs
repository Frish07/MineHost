use crate::{error, lock, players, Inner, Result, Shared};
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::State;
pub const SCHEMA:&str="CREATE TABLE IF NOT EXISTS connection_history(event_id INTEGER PRIMARY KEY AUTOINCREMENT,server_id TEXT NOT NULL,session TEXT NOT NULL,player TEXT NOT NULL,kind TEXT NOT NULL,at INTEGER NOT NULL); CREATE INDEX IF NOT EXISTS history_server ON connection_history(server_id,event_id);";
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn parse(line: &str) -> Option<(String, &'static str)> {
    let body = line.split_once("]: ").map(|(_, v)| v).unwrap_or(line);
    for (suffix, kind) in [(" joined the game", "join"), (" left the game", "leave")] {
        if let Some(name) = body.strip_suffix(suffix) {
            if players::username(name).is_ok() {
                return Some((name.into(), kind));
            }
        }
    }
    None
}
fn insert(inner: &Inner, id: &str, session: &str, name: &str, kind: &str) -> Result<()> {
    inner.db.execute("INSERT INTO connection_history(server_id,session,player,kind,at) VALUES(?1,?2,?3,?4,?5)",rusqlite::params![id,session,name,kind,now()]).map_err(error)?;
    inner.db.execute("DELETE FROM connection_history WHERE server_id=?1 AND event_id NOT IN (SELECT event_id FROM connection_history WHERE server_id=?1 ORDER BY event_id DESC LIMIT 10000)",[id]).map_err(error)?;
    Ok(())
}
pub fn record(inner: &mut Inner, id: &str, line: &str) -> Result<()> {
    let Some((name, kind)) = parse(line) else {
        return Ok(());
    };
    let Some(running) = inner.running.get_mut(id) else {
        return Ok(());
    };
    let changed = if kind == "join" {
        running.connected.insert(name.clone())
    } else {
        running.connected.remove(&name)
    };
    let session = running.session.clone();
    if changed {
        insert(inner, id, &session, &name, kind)?;
    }
    Ok(())
}
pub fn end(inner: &Inner, id: &str) -> Result<()> {
    if let Some(running) = inner.running.get(id) {
        for name in &running.connected {
            insert(inner, id, &running.session, name, "server_exit")?;
        }
    }
    Ok(())
}
#[derive(Serialize)]
pub struct Event {
    event_id: i64,
    player: String,
    kind: String,
    at: i64,
    session: String,
}
#[tauri::command]
pub fn connection_history(state: State<'_, Shared>, id: String, offset: u32) -> Result<Vec<Event>> {
    if offset > 10000 {
        return Err("Invalid history offset".into());
    }
    let inner = lock(&state)?;
    crate::load(&inner, &id)?;
    let mut statement=inner.db.prepare("SELECT event_id,player,kind,at,session FROM connection_history WHERE server_id=?1 ORDER BY event_id DESC LIMIT 100 OFFSET ?2").map_err(error)?;
    let result = statement
        .query_map(rusqlite::params![id, offset], |row| {
            Ok(Event {
                event_id: row.get(0)?,
                player: row.get(1)?,
                kind: row.get(2)?,
                at: row.get(3)?,
                session: row.get(4)?,
            })
        })
        .map_err(error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(error)?;
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stores_events_across_database_reopen_and_bounds_history() {
        let stage = crate::archive::Stage::new(&std::env::temp_dir()).unwrap();
        let path = stage.0.join("history.sqlite");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(SCHEMA).unwrap();
        db.execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<10001) INSERT INTO connection_history(server_id,session,player,kind,at) SELECT 'server','session','Steve','join',x FROM n;").unwrap();
        let inner = crate::Inner {
            db,
            root: stage.0.clone(),
            running: Default::default(),
            busy: Default::default(),
            logs: Default::default(),
            agent: None,
            claim: None,
            drive: Default::default(),
            lifecycle: Default::default(),
        };
        insert(&inner, "server", "session", "Steve", "leave").unwrap();
        drop(inner);
        let db = rusqlite::Connection::open(&path).unwrap();
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM connection_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 10000);
        let kind: String = db
            .query_row(
                "SELECT kind FROM connection_history ORDER BY event_id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kind, "leave");
    }
    #[test]
    fn ignores_chat_and_localized_non_events() {
        assert_eq!(
            parse("[Server thread/INFO]: Steve joined the game"),
            Some(("Steve".into(), "join"))
        );
        assert!(parse("[Server thread/INFO]: <Alex> Steve joined the game").is_none());
        assert!(parse("[Server thread/INFO]: <Alex> : Steve joined the game").is_none());
        assert!(parse("Player list unavailable").is_none());
    }
}
