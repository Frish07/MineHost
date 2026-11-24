use crate::{error, files, lock, safety, Result, Shared};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
    time::{Duration, Instant},
};
use tauri::State;

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Player {
    name: String,
    uuid: Option<String>,
    whitelist: bool,
    operator: bool,
    banned: bool,
}
#[derive(Deserialize)]
struct Record {
    name: String,
    uuid: Option<String>,
}
pub fn username(value: &str) -> Result<()> {
    if !(3..=16).contains(&value.len())
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err("Minecraft username must contain 3–16 letters, digits or underscores".into());
    }
    Ok(())
}
fn records(root: &Path) -> Result<Vec<Player>> {
    let mut result: BTreeMap<String, Player> = BTreeMap::new();
    for (file, kind) in [
        ("usercache.json", 0),
        ("whitelist.json", 1),
        ("ops.json", 2),
        ("banned-players.json", 3),
    ] {
        let path = files::root_path(root, file)?;
        if !path.exists() {
            continue;
        }
        let file = std::fs::File::open(path).map_err(error)?;
        let mut bytes = Vec::new();
        file.take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(error)?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err("Player list exceeds 2 MiB".into());
        }
        let rows: Vec<Record> = serde_json::from_slice(&bytes).map_err(error)?;
        for row in rows {
            if username(&row.name).is_err() {
                continue;
            }
            let item = result
                .entry(row.name.to_ascii_lowercase())
                .or_insert_with(|| Player {
                    name: row.name,
                    ..Default::default()
                });
            if let Some(uuid) = row.uuid.filter(|v| uuid::Uuid::parse_str(v).is_ok()) {
                item.uuid = Some(uuid);
            }
            match kind {
                1 => item.whitelist = true,
                2 => item.operator = true,
                3 => item.banned = true,
                _ => (),
            };
            if result.len() > 10000 {
                return Err("Player list exceeds 10000 players".into());
            }
        }
    }
    Ok(result.into_values().collect())
}
pub fn online_line(line: &str) -> Option<Vec<String>> {
    let body = line.split_once("]: ").map(|(_, v)| v).unwrap_or(line);
    let body = body.strip_prefix("There are ")?;
    let (count, rest) = body.split_once(" of a max of ")?;
    let count = count.parse::<usize>().ok()?;
    let (maximum, names) = rest.split_once(" players online:")?;
    maximum.parse::<usize>().ok()?;
    if count > 10000 {
        return None;
    }
    let values = if names.trim().is_empty() {
        Vec::new()
    } else {
        names
            .trim()
            .split(", ")
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    if values.len() != count || values.iter().any(|v| username(v).is_err()) {
        return None;
    }
    Some(values)
}
fn action_command(action: &str, name: &str, text: &str) -> Result<String> {
    if action == "say" {
        safety::command(text)?;
        return Ok(format!("say {text}"));
    }
    if action == "whitelist_on" {
        return Ok("whitelist on".into());
    }
    if action == "whitelist_off" {
        return Ok("whitelist off".into());
    }
    username(name)?;
    let command = match action {
        "whitelist_add" => "whitelist add",
        "whitelist_remove" => "whitelist remove",
        "op" => "op",
        "deop" => "deop",
        "ban" => "ban",
        "pardon" => "pardon",
        "kick" => "kick",
        _ => return Err("Unknown player action".into()),
    };
    Ok(format!("{command} {name}"))
}
#[tauri::command]
pub async fn player_records(state: State<'_, Shared>, id: String) -> Result<Vec<Player>> {
    crate::profile_task(state.inner().clone(), id, false, move |_, root| {
        records(&root)
    })
    .await
}
#[tauri::command]
pub fn player_action(
    state: State<'_, Shared>,
    id: String,
    action: String,
    name: String,
    text: String,
) -> Result<()> {
    crate::send_command(state, id, action_command(&action, &name, &text)?)
}
struct Query {
    shared: Shared,
    id: String,
    token: uuid::Uuid,
}
impl Drop for Query {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.shared.lock() {
            if let Some(running) = inner.running.get_mut(&self.id) {
                if running.player_query == Some(self.token) {
                    running.player_query = None;
                    running.players = None;
                }
            }
        }
    }
}
#[tauri::command]
pub async fn query_players(state: State<'_, Shared>, id: String) -> Result<Vec<String>> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move|| {
        let token=uuid::Uuid::new_v4();
        let input={let mut inner=lock(&shared)?; let running=inner.running.get_mut(&id).ok_or("Server is not running")?;
            if running.player_query.is_some() {return Err("A player query is already in progress".into());}
            running.player_query=Some(token);running.players=None;running.input.clone()
        };
        let _query=Query{shared:shared.clone(),id:id.clone(),token};
        {let mut input=input.lock().map_err(error)?;writeln!(input,"list").map_err(error)?;input.flush().map_err(error)?;}
        let deadline=Instant::now()+Duration::from_secs(10);
        loop {
            {let mut inner=lock(&shared)?; let running=inner.running.get_mut(&id).ok_or("Server stopped while querying players")?;
                if running.player_query!=Some(token) {return Err("Server session changed".into());}
                if let Some(players)=running.players.take() {return Ok(players);}
            }
            if Instant::now()>=deadline {return Err("No standard response to list within 10 seconds. Check the console; a plugin may have replaced the command.".into());}
            std::thread::sleep(Duration::from_millis(100));
        }
    }).await.map_err(error)?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn player_actions_cannot_inject_commands_or_selectors() {
        for name in ["@a", "abc\nstop", "name other", "../x", ""] {
            assert!(action_command("op", name, "").is_err());
        }
        assert_eq!(action_command("op", "Player_1", "").unwrap(), "op Player_1");
        assert!(action_command("say", "", "hello\nstop").is_err());
        assert!(action_command("execute", "Player_1", "").is_err());
    }
    #[test]
    fn list_parser_rejects_incomplete_or_localized_responses() {
        assert_eq!(
            online_line(
                "[Server thread/INFO]: There are 2 of a max of 20 players online: Steve, Alex"
            )
            .unwrap(),
            vec!["Steve", "Alex"]
        );
        assert!(online_line("There are 0 of a max of 20 players online:")
            .unwrap()
            .is_empty());
        assert!(online_line("There are 2 of a max of 20 players online: Steve").is_none());
        assert!(online_line("Players: unknown").is_none());
        assert!(online_line(
            "[Server thread/INFO]: <Alex> There are 0 of a max of 20 players online:"
        )
        .is_none());
    }
    #[test]
    fn roster_merges_actual_files_and_validates_uuids() {
        let root = std::env::temp_dir().join(format!("minehost-players-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("usercache.json"),
            r#"[{"name":"Steve","uuid":"invalid"}]"#,
        )
        .unwrap();
        std::fs::write(
            root.join("ops.json"),
            r#"[{"name":"Steve","uuid":"00000000-0000-0000-0000-000000000001"}]"#,
        )
        .unwrap();
        let rows = records(&root).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].operator && rows[0].uuid.is_some());
    }
}
