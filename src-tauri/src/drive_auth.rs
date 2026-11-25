use crate::{error, safety, win, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const CLIENT: &str = "MineHost/google-desktop-client/v1";
const REFRESH: &str = "MineHost/google-refresh/v1";
pub const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
#[derive(Serialize, Deserialize)]
pub struct DesktopClient {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
}
pub fn client() -> Result<Client> {
    Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("MineHost/0.9.1")
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| "Cannot initialize Google HTTPS client".into())
}
pub fn json(response: Response) -> Result<Value> {
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "Google API HTTP {}. Check authorization, API enablement, quota and connectivity.",
            status.as_u16()
        ));
    }
    let mut bytes = Vec::new();
    response
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read Google response")?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("Google response exceeds limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid Google JSON response".into())
}
fn validate(c: &DesktopClient) -> Result<()> {
    let prefix = c
        .client_id
        .strip_suffix(".apps.googleusercontent.com")
        .ok_or("Expected Google Desktop app client ID")?;
    if prefix.is_empty()
        || prefix.len() > 160
        || !prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || c.client_secret.len() > 256
        || c.client_secret.chars().any(char::is_control)
    {
        return Err("Invalid Desktop OAuth client".into());
    }
    Ok(())
}
pub fn configuration() -> Result<Option<DesktopClient>> {
    win::credential_read(CLIENT)?
        .map(|v| serde_json::from_str(&v).map_err(|_| "Invalid saved Google client".into()))
        .transpose()
}
pub fn linked() -> Result<bool> {
    Ok(win::credential_read(REFRESH)?.is_some())
}
pub fn import(path: &Path) -> Result<()> {
    if linked()? {
        return Err("Disconnect Google before changing the OAuth client".into());
    }
    safety::reject_links(path)?;
    let mut text = Vec::new();
    std::fs::File::open(path)
        .map_err(error)?
        .take(16385)
        .read_to_end(&mut text)
        .map_err(error)?;
    if text.len() > 16384 {
        return Err("OAuth client JSON is too large".into());
    }
    let value: Value = serde_json::from_slice(&text).map_err(|_| "Invalid OAuth client JSON")?;
    let c: DesktopClient = serde_json::from_value(value.get("installed").ok_or("Choose a Desktop app client JSON (installed), not a web/service account credential")?.clone()).map_err(|_| "Missing Desktop OAuth client fields")?;
    validate(&c)?;
    win::credential_write(CLIENT, &serde_json::to_string(&c).map_err(error)?)
}
fn random() -> String {
    format!(
        "{}{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}
fn callback(request: &str, host: &str, state: &str) -> Result<Option<String>> {
    let mut lines = request.split("\r\n");
    let first = lines.next().ok_or("Invalid callback")?;
    let parts: Vec<_> = first.split_whitespace().collect();
    if parts.len() != 3
        || parts[0] != "GET"
        || !parts[1].starts_with("/callback?")
        || parts[2] != "HTTP/1.1"
    {
        return Ok(None);
    }
    let hosts: Vec<_> = lines
        .filter_map(|v| v.split_once(':'))
        .filter(|(k, _)| k.eq_ignore_ascii_case("host"))
        .collect();
    if hosts.len() != 1 || hosts[0].1.trim() != host {
        return Ok(None);
    }
    let url = reqwest::Url::parse(&format!("http://{host}{}", parts[1]))
        .map_err(|_| "Invalid callback URL")?;
    let values = |key: &str| {
        url.query_pairs()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
            .collect::<Vec<_>>()
    };
    if values("state") != [state.to_string()] {
        return Ok(None);
    }
    if !values("error").is_empty() {
        return Err("Google authorization was declined".into());
    }
    let codes = values("code");
    if codes.len() != 1 || codes[0].is_empty() || codes[0].len() > 4096 {
        return Err("Invalid OAuth authorization code".into());
    }
    Ok(Some(codes[0].clone()))
}
pub fn authorize(cancel: &AtomicBool) -> Result<()> {
    let c = configuration()?.ok_or("Import a Google Desktop OAuth client JSON first")?;
    validate(&c)?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(error)?;
    listener.set_nonblocking(true).map_err(error)?;
    let host = listener.local_addr().map_err(error)?.to_string();
    let redirect = format!("http://{host}/callback");
    let verifier = random();
    let state = random();
    let mut url =
        reqwest::Url::parse("https://accounts.google.com/o/oauth2/v2/auth").map_err(error)?;
    url.query_pairs_mut().extend_pairs([
        ("client_id", c.client_id.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("response_type", "code"),
        ("scope", SCOPE),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("code_challenge", challenge(&verifier).as_str()),
        ("code_challenge_method", "S256"),
        ("state", state.as_str()),
    ]);
    std::process::Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url.as_str()])
        .spawn()
        .map_err(|_| "Cannot open the system browser")?;
    let deadline = Instant::now() + Duration::from_secs(180);
    let code = loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Google sign-in cancelled".into());
        }
        if Instant::now() > deadline {
            return Err("Google sign-in timed out; try again".into());
        }
        match listener.accept() {
            Ok((mut socket, peer)) => {
                if !peer.ip().is_loopback() {
                    continue;
                }
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .map_err(error)?;
                socket
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .map_err(error)?;
                let mut request = Vec::new();
                let mut byte = [0];
                let read_deadline = Instant::now() + Duration::from_secs(2);
                while request.len() < 16384
                    && !request.ends_with(b"\r\n\r\n")
                    && Instant::now() < read_deadline
                    && !cancel.load(Ordering::Relaxed)
                {
                    if socket.read(&mut byte).unwrap_or(0) == 0 {
                        break;
                    }
                    request.push(byte[0]);
                }
                if !request.ends_with(b"\r\n\r\n") {
                    continue;
                }
                let result = callback(std::str::from_utf8(&request).unwrap_or(""), &host, &state);
                let valid = !matches!(result, Ok(None));
                let body = if valid {
                    "Return to MineHost to see the sign-in result. You may close this tab."
                } else {
                    "Invalid callback."
                };
                let status = if valid { "200 OK" } else { "400 Bad Request" };
                let _=write!(socket,"HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len());
                if let Some(code) = result? {
                    break code;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100))
            }
            Err(_) => return Err("Cannot receive Google callback".into()),
        }
    };
    let value = json(
        client()?
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("client_id", c.client_id),
                ("client_secret", c.client_secret),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect),
                ("grant_type", "authorization_code".into()),
            ])
            .send()
            .map_err(|_| "Google token exchange failed")?,
    )?;
    if let Some(scope) = value["scope"].as_str() {
        if !scope.split_whitespace().any(|s| s == SCOPE) {
            return Err("Google Drive file access was not granted".into());
        }
    }
    let token = value["refresh_token"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() < 2000)
        .ok_or("Google did not issue a refresh token; reconnect with consent")?;
    win::credential_write(REFRESH, token)
}
pub fn access_token() -> Result<String> {
    let c = configuration()?.ok_or("Configure Google OAuth first")?;
    let refresh = win::credential_read(REFRESH)?.ok_or("Connect Google Drive first")?;
    let value = json(
        client()?
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("client_id", c.client_id),
                ("client_secret", c.client_secret),
                ("refresh_token", refresh),
                ("grant_type", "refresh_token".into()),
            ])
            .send()
            .map_err(|_| "Cannot refresh Google authorization")?,
    )?;
    value["access_token"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() < 8192)
        .map(str::to_string)
        .ok_or("Google access token is missing".into())
}
pub fn disconnect(revoke: bool) -> Result<()> {
    if revoke {
        if let Some(token) = win::credential_read(REFRESH)? {
            let response = client()?
                .post("https://oauth2.googleapis.com/revoke")
                .form(&[("token", token)])
                .send()
                .map_err(|_| {
                    "Cannot revoke Google access; credentials retained so you can retry"
                })?;
            if !response.status().is_success() && response.status().as_u16() != 400 {
                return Err("Google revocation failed; retry or explicitly forget locally".into());
            }
        }
    }
    win::credential_delete(REFRESH)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pkce_and_callback_security() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let request = "GET /callback?state=s&code=ok HTTP/1.1\r\nHost: 127.0.0.1:99\r\n\r\n";
        assert_eq!(
            callback(request, "127.0.0.1:99", "s").unwrap(),
            Some("ok".into())
        );
        assert!(callback(request, "127.0.0.1:99", "wrong")
            .unwrap()
            .is_none());
        assert!(callback(
            &request.replace("state=s", "state=s&state=s"),
            "127.0.0.1:99",
            "s"
        )
        .unwrap()
        .is_none());
        assert!(callback(request, "evil.com", "s").unwrap().is_none());
    }
}
