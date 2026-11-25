use crate::{error, files, safety, Result, Shared};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::Path,
};
use tauri::State;
struct Entry {
    raw: String,
    key: Option<String>,
    value: String,
}
fn space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\u{c}')
}
fn decode(s: &str) -> Result<String> {
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.extend(c.encode_utf16(&mut [0; 2]).iter().copied());
            continue;
        }
        let next = chars.next().ok_or("Incomplete properties escape")?;
        if next == 'u' {
            let hex = chars.by_ref().take(4).collect::<String>();
            if hex.len() != 4 {
                return Err("Invalid properties Unicode escape".into());
            }
            out.push(
                u16::from_str_radix(&hex, 16).map_err(|_| "Invalid properties Unicode escape")?,
            );
        } else {
            let c = match next {
                't' => '\t',
                'n' => '\n',
                'r' => '\r',
                'f' => '\u{c}',
                other => other,
            };
            out.extend(c.encode_utf16(&mut [0; 2]).iter().copied());
        }
    }
    String::from_utf16(&out).map_err(|_| "Invalid properties Unicode sequence".into())
}
fn encode(s: &str, key: bool) -> String {
    let mut out = String::new();
    for (index, c) in s.chars().enumerate() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{c}' => out.push_str("\\f"),
            ' ' if key || index == 0 => out.push_str("\\ "),
            '=' | ':' | '#' | '!' if key => {
                out.push('\\');
                out.push(c);
            }
            c if !c.is_ascii() => {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            other => out.push(other),
        }
    }
    out
}
fn entries(content: &str) -> Result<Vec<Entry>> {
    if content.len() > 65536 || content.contains('\0') {
        return Err("Properties must be UTF-8 text up to 64 KiB without NUL".into());
    }
    let mut lines = content.split_inclusive('\n');
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    while let Some(line) = lines.next() {
        let mut raw = line.to_string();
        let mut logical = line.trim_end_matches(['\r', '\n']).to_string();
        if logical.trim_start_matches(space).starts_with(['#', '!']) || logical.trim().is_empty() {
            result.push(Entry {
                raw,
                key: None,
                value: String::new(),
            });
            continue;
        }
        while !logical
            .chars()
            .rev()
            .take_while(|c| *c == '\\')
            .count()
            .is_multiple_of(2)
        {
            logical.pop();
            let next = lines.next().ok_or("Unfinished properties continuation")?;
            raw.push_str(next);
            logical.push_str(
                next.trim_end_matches(['\r', '\n'])
                    .trim_start_matches(space),
            );
        }
        let text = logical.trim_start_matches(space);
        let mut escaped = false;
        let mut split = None;
        for (i, c) in text.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if c == '\\' {
                escaped = true;
                continue;
            }
            if matches!(c, '=' | ':') || space(c) {
                split = Some((i, c));
                break;
            }
        }
        let (key, value) = if let Some((i, c)) = split {
            let mut rest = &text[i + c.len_utf8()..];
            if space(c) {
                rest = rest.trim_start_matches(space);
                if rest.starts_with(['=', ':']) {
                    rest = &rest[1..];
                }
            }
            (&text[..i], rest.trim_start_matches(space))
        } else {
            (text, "")
        };
        let key = decode(key)?;
        if key.is_empty() || key.chars().any(char::is_control) || !seen.insert(key.clone()) {
            return Err("Empty, duplicate or control-character properties key".into());
        }
        result.push(Entry {
            raw,
            key: Some(key),
            value: decode(value)?,
        });
    }
    Ok(result)
}
pub fn values(content: &str) -> Result<HashMap<String, String>> {
    Ok(entries(content)?
        .into_iter()
        .filter_map(|e| e.key.map(|k| (k, e.value)))
        .collect())
}
pub fn patch(content: &str, updates: &[(&str, &str)]) -> Result<String> {
    let mut used = HashSet::new();
    let mut out = String::new();
    for e in entries(content)? {
        if let Some((key, value)) = updates.iter().find(|(k, _)| e.key.as_deref() == Some(*k)) {
            out.push_str(&format!("{}={}\n", encode(key, true), encode(value, false)));
            used.insert(*key);
        } else {
            out.push_str(&e.raw);
            if !e.raw.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    for (key, value) in updates {
        if !used.contains(key) {
            out.push_str(&format!("{}={}\n", encode(key, true), encode(value, false)));
        }
    }
    Ok(out)
}
pub fn validate(content: &str, port: u16) -> Result<()> {
    let v = values(content)?;
    if v.get("server-port").and_then(|s| s.parse::<u16>().ok()) != Some(port)
        || v.get("online-mode").map(String::as_str) != Some("true")
    {
        return Err(
            "Keep the unique profile port and online-mode=true; change port in the settings form"
                .into(),
        );
    }
    if let Some(world) = v.get("level-name") {
        crate::worlds::world_name(world)?;
    }
    for key in [
        "enable-rcon",
        "enable-query",
        "enable-jmx-monitoring",
        "management-server-enabled",
    ] {
        if v.get(key).is_some_and(|s| s != "false") {
            return Err(format!(
                "Additional network listener {key} is not supported"
            ));
        }
    }
    Ok(())
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub port: u16,
    pub max_players: u32,
    pub difficulty: String,
    pub pvp: bool,
    pub whitelist: bool,
    pub view_distance: u32,
    pub simulation_distance: u32,
    pub motd: String,
    pub game_mode: String,
    pub spawn_protection: u32,
    pub allow_flight: bool,
    pub force_game_mode: bool,
}
impl Settings {
    fn read(content: &str, port: u16) -> Result<Self> {
        let v = values(content)?;
        let text = |k: &str, d: &str| v.get(k).cloned().unwrap_or_else(|| d.into());
        let number = |k: &str, d: u32| {
            v.get(k)
                .map(|v| {
                    v.parse::<u32>()
                        .map_err(|_| format!("Invalid numeric setting {k}"))
                })
                .unwrap_or(Ok(d))
        };
        let boolean = |k: &str, d: bool| match v.get(k).map(String::as_str) {
            None => Ok(d),
            Some("true") => Ok(true),
            Some("false") => Ok(false),
            _ => Err(format!("Invalid boolean setting {k}")),
        };
        Ok(Self {
            port,
            max_players: number("max-players", 20)?,
            difficulty: text("difficulty", "easy"),
            pvp: boolean("pvp", true)?,
            whitelist: boolean("white-list", false)?,
            view_distance: number("view-distance", 10)?,
            simulation_distance: number("simulation-distance", 10)?,
            motd: text("motd", "A Minecraft Server"),
            game_mode: text("gamemode", "survival"),
            spawn_protection: number("spawn-protection", 16)?,
            allow_flight: boolean("allow-flight", false)?,
            force_game_mode: boolean("force-gamemode", false)?,
        })
    }
    fn apply(&self, content: &str) -> Result<String> {
        if self.port < 1024
            || !(1..=100000).contains(&self.max_players)
            || !(2..=32).contains(&self.view_distance)
            || !(2..=32).contains(&self.simulation_distance)
            || self.spawn_protection > 29999984
            || self.motd.len() > 4096
            || self.motd.contains('\0')
            || !["peaceful", "easy", "normal", "hard"].contains(&self.difficulty.as_str())
            || !["survival", "creative", "adventure", "spectator"]
                .contains(&self.game_mode.as_str())
        {
            return Err("Invalid server setting range or enum".into());
        }
        let data = [
            ("server-port", self.port.to_string()),
            ("online-mode", "true".into()),
            ("max-players", self.max_players.to_string()),
            ("difficulty", self.difficulty.clone()),
            ("pvp", self.pvp.to_string()),
            ("white-list", self.whitelist.to_string()),
            ("view-distance", self.view_distance.to_string()),
            ("simulation-distance", self.simulation_distance.to_string()),
            ("motd", self.motd.clone()),
            ("gamemode", self.game_mode.clone()),
            ("spawn-protection", self.spawn_protection.to_string()),
            ("allow-flight", self.allow_flight.to_string()),
            ("force-gamemode", self.force_game_mode.to_string()),
        ];
        patch(
            content,
            &data
                .iter()
                .map(|(k, v)| (*k, v.as_str()))
                .collect::<Vec<_>>(),
        )
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    settings: Option<Settings>,
    revision: String,
    raw: String,
    profile_port: u16,
    warning: Option<String>,
}
fn revision(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}
fn read(root: &Path) -> Result<String> {
    let path = root.join("server.properties");
    safety::reject_links(&path)?;
    let mut s = String::new();
    std::fs::File::open(path)
        .map_err(error)?
        .take(65537)
        .read_to_string(&mut s)
        .map_err(error)?;
    if s.len() > 65536 {
        return Err("Properties exceed 64 KiB".into());
    }
    Ok(s)
}
#[tauri::command]
pub async fn properties_snapshot(state: State<'_, Shared>, id: String) -> Result<Snapshot> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (server, dir) = crate::profile_path(&shared, &id)?;
        let raw = read(&dir)?;
        let (settings, warning) = match Settings::read(&raw, server.port) {
            Ok(v) => (Some(v), None),
            Err(e) => (None, Some(e)),
        };
        Ok(Snapshot {
            settings,
            warning,
            profile_port: server.port,
            revision: revision(&raw),
            raw,
        })
    })
    .await
    .map_err(error)?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn java_escapes_comments_and_continuations_roundtrip_without_key_bypasses() {
        let content="# keep\nserver-port=25565\nonline-mode=true\nmotd=Hello\\n\\u041c\\u0438\\u0440 \\ud83c\\udf0d\ncustom\\ key : one\\\n  two\n";
        let v = values(content).unwrap();
        assert_eq!(v["custom key"], "onetwo");
        assert_eq!(v["motd"], "Hello\nМир 🌍");
        let new = patch(content, &[("motd", "Другой\nмир 🌍")]).unwrap();
        assert!(new.starts_with("# keep\n"));
        assert_eq!(values(&new).unwrap()["motd"], "Другой\nмир 🌍");
        validate(&new, 25565).unwrap();
        assert!(validate("server-port=25565\nonli\\u006ee-mode=false\n", 25565).is_err());
        assert!(values("server-port=1\nserver\\u002dport=2\n").is_err());
        assert!(values("motd=\\uXXXX").is_err());
    }
    #[test]
    fn port_changes_are_revision_checked_and_sql_collision_does_not_modify_file() {
        let stage = crate::archive::Stage::new(&std::env::temp_dir()).unwrap();
        let shared = crate::tests::memory_state();
        let (id, dir) = {
            let mut inner = crate::lock(&shared).unwrap();
            inner.root = stage.0.clone();
            let id: String = inner
                .db
                .query_row("SELECT id FROM servers", [], |r| r.get(0))
                .unwrap();
            let dir = inner.root.join("servers").join(&id);
            std::fs::create_dir_all(&dir).unwrap();
            let mut other = crate::load(&inner, &id).unwrap();
            other.id = uuid::Uuid::new_v4().to_string();
            other.port = 25566;
            crate::save(&inner, &other).unwrap();
            (id, dir)
        };
        let old = "server-port=25565\nonline-mode=true\nunknown=keep\n";
        std::fs::write(dir.join("server.properties"), old).unwrap();
        let expected = revision(old);
        let collision = patch(old, &[("server-port", "25566")]).unwrap();
        assert!(commit(&shared, &id, &expected, &collision, 25566, true).is_err());
        assert_eq!(read(&dir).unwrap(), old);
        let next = patch(old, &[("server-port", "25567")]).unwrap();
        assert!(commit(&shared, &id, "stale", &next, 25567, true).is_err());
        assert!(commit(&shared, &id, &expected, &next, 25567, false).is_err());
        commit(&shared, &id, &expected, &next, 25567, true).unwrap();
        assert_eq!(
            crate::load(&crate::lock(&shared).unwrap(), &id)
                .unwrap()
                .port,
            25567
        );
        assert_eq!(values(&read(&dir).unwrap()).unwrap()["unknown"], "keep");
    }
}
fn commit(
    shared: &Shared,
    id: &str,
    expected: &str,
    new: &str,
    port: u16,
    port_consent: bool,
) -> Result<()> {
    let inner = crate::lock(shared)?;
    let mut server = crate::load(&inner, id)?;
    let dir = crate::directory(&inner, id)?;
    let old = read(&dir)?;
    if revision(&old) != expected {
        return Err(
            "Settings changed since loading. Reload before saving; edits were not written.".into(),
        );
    }
    validate(new, port)?;
    if port < 1024 {
        return Err("Port must be 1024–65535".into());
    }
    if port != server.port && !port_consent {
        return Err(
            "Confirm port change and review existing firewall/router/tunnel configuration".into(),
        );
    }
    let transaction = inner.db.unchecked_transaction().map_err(error)?;
    server.port = port;
    transaction
        .execute(
            "UPDATE servers SET port=?1,data=?2 WHERE id=?3",
            rusqlite::params![port, serde_json::to_string(&server).map_err(error)?, id],
        )
        .map_err(|e| {
            format!("Port/settings could not be saved (check duplicate profile ports): {e}")
        })?;
    let path = dir.join("server.properties");
    files::backup(&dir, &path, "file-backups")?;
    safety::atomic_write(&path, new.as_bytes())?;
    if let Err(e) = transaction.commit() {
        if let Err(rollback) = safety::atomic_write(&path, old.as_bytes()) {
            return Err(format!(
                "{e}; file rollback failed: {rollback}. Previous file is in .minehost/file-backups"
            ));
        }
        return Err(error(e));
    }
    Ok(())
}
#[tauri::command]
pub async fn properties_save_form(
    state: State<'_, Shared>,
    id: String,
    revision: String,
    settings: Settings,
    port_consent: bool,
) -> Result<()> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = crate::begin(&shared, &id)?;
        let (_, dir) = crate::profile_path(&shared, &id)?;
        let new = settings.apply(&read(&dir)?)?;
        commit(&shared, &id, &revision, &new, settings.port, port_consent)
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub async fn properties_save_raw(
    state: State<'_, Shared>,
    id: String,
    revision: String,
    content: String,
    port_consent: bool,
) -> Result<()> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = crate::begin(&shared, &id)?;
        let port = values(&content)?
            .get("server-port")
            .ok_or("Missing server-port")?
            .parse::<u16>()
            .map_err(|_| "Invalid port")?;
        commit(&shared, &id, &revision, &content, port, port_consent)
    })
    .await
    .map_err(error)?
}
