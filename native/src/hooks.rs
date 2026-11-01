//! Allow and Deny from the notch: a small local server for Claude Code hooks, and adding those hooks to settings.json.
//! Tracking works without any of this; it reads the transcripts.
use crate::agents::{self, Agent, Approval, DiffLine, Hub};
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PORT: u16 = 47823;
const ROUTE: &str = "/twocode-hook";
const EVENTS: [&str; 6] = ["UserPromptSubmit", "PostToolUse", "PostToolUseFailure", "PermissionRequest", "Stop", "SessionEnd"];
const PERMISSION_WAIT_S: u64 = 300;
const MAX_BODY: usize = 1_000_000;

fn hook_url() -> String {
    format!("http://127.0.0.1:{PORT}{ROUTE}?v=5")
}

/// The app's secret for its hook server, created on first use and kept in the app's own folder.
pub fn token(dir: &Path) -> String {
    let file = dir.join("hook-token");
    if let Ok(saved) = fs::read_to_string(&file) {
        if saved.trim().len() == 48 {
            return saved.trim().to_string();
        }
    }
    let mut bytes = [0u8; 24];
    let status = unsafe {
        windows::Win32::Security::Cryptography::BCryptGenRandom(
            None,
            &mut bytes,
            windows::Win32::Security::Cryptography::BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    assert!(status.is_ok(), "the system random generator failed");
    let fresh: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let _ = fs::create_dir_all(dir);
    let _ = fs::write(&file, &fresh);
    fresh
}

// ---- server ----

pub fn serve(hub: Arc<Mutex<Hub>>, secret: String, on_change: impl Fn() + Send + Sync + 'static) {
    let on_change = Arc::new(on_change);
    std::thread::spawn(move || {
        // Taken: another copy (or the old TwoCode) has the port. Tracking still works from the transcripts.
        let Ok(listener) = TcpListener::bind(("127.0.0.1", PORT)) else { return };
        for stream in listener.incoming().flatten() {
            let (hub, secret, on_change) = (hub.clone(), secret.clone(), on_change.clone());
            std::thread::spawn(move || handle(stream, &hub, &secret, &*on_change));
        }
    });
}

fn handle(mut stream: TcpStream, hub: &Mutex<Hub>, secret: &str, on_change: &(dyn Fn() + Send + Sync)) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let Some((body, agent)) = read_request(&mut stream, secret) else {
        respond(&mut stream, "403 Forbidden", "");
        return;
    };
    let Ok(data) = serde_json::from_slice::<Value>(&body) else {
        respond(&mut stream, "200 OK", "");
        return;
    };
    let id = data["session_id"].as_str().unwrap_or("").to_string();
    if id.is_empty() {
        respond(&mut stream, "200 OK", "");
        return;
    }
    let now = agents::now_ms();
    let tool = data["tool_name"].as_str().unwrap_or("");
    // Questions and plans need a real answer, so those stay in Claude's own window.
    let holds = data["hook_event_name"] == "PermissionRequest" && tool != "AskUserQuestion" && tool != "ExitPlanMode";
    let hub_for_hangup = hub;
    if let Ok(mut hub) = hub.lock() {
        for event in agents::parse_hook(&data) {
            hub.apply(agent, &id, now, event, now);
        }
        if holds {
            let label = agents::tool_label(tool, &data["tool_input"]);
            let approval = approval_for(tool, &data["tool_input"]);
            let mut held = stream.try_clone().ok();
            if let Some(conn) = held.take() {
                let mut conn = conn;
                let responder = Box::new(move |answer: Option<String>| {
                    respond(&mut conn, "200 OK", answer.as_deref().unwrap_or(""));
                    let _ = conn.shutdown(std::net::Shutdown::Write);
                });
                hub.hold(agent, &id, label, approval, data["tool_input"].clone(), responder, now);
                drop(hub);
                on_change();
                // Stay on this thread until the agent closes the connection, so a request
                // answered elsewhere closes the notch right away instead of after the timeout.
                let _ = stream.set_read_timeout(None);
                let mut byte = [0u8; 1];
                while matches!(stream.read(&mut byte), Ok(n) if n > 0) {}
                if let Ok(mut hub) = hub_for_hangup.lock() {
                    if hub.hung_up(agent, &id, now) {
                        drop(hub);
                        on_change();
                    }
                }
                return;
            }
        }
    }
    respond(&mut stream, "200 OK", "");
    on_change();
}

/// What to show for a request: the exact command, or the first changed lines of an edit.
pub fn approval_for(tool: &str, input: &Value) -> Approval {
    let file = input["file_path"].as_str().unwrap_or("");
    let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
    let first = |key: &str| input[key].as_str().map(str::to_string);
    if let (Some(old), Some(new)) = (first("old_string"), first("new_string")) {
        return Approval { title: format!("Wants to edit {name}"), what: file.to_string(), diff: changed_lines(&old, &new) };
    }
    if let Some(content) = first("content") {
        return Approval { title: format!("Wants to write {name}"), what: file.to_string(), diff: changed_lines("", &content) };
    }
    if let Some(command) = first("command") {
        return Approval { title: "Wants to run".to_string(), what: command.chars().take(400).collect(), diff: vec![] };
    }
    let what = ["file_path", "url", "query"].iter().find_map(|k| input[*k].as_str()).unwrap_or(tool);
    Approval { title: format!("Wants to use {tool}"), what: what.chars().take(400).collect(), diff: vec![] }
}

/// The first removed and added lines, skipping what both sides share.
fn changed_lines(old: &str, new: &str) -> Vec<DiffLine> {
    let shared = |line: &&str| !line.trim().is_empty();
    let old_lines: Vec<&str> = old.lines().filter(shared).collect();
    let new_lines: Vec<&str> = new.lines().filter(shared).collect();
    let removed = old_lines.iter().find(|l| !new_lines.contains(l));
    let added: Vec<&&str> = new_lines.iter().filter(|l| !old_lines.contains(l)).collect();
    let line = |added: bool, text: &str| DiffLine { added, text: text.trim().chars().take(120).collect() };
    let mut out: Vec<DiffLine> = removed.map(|r| line(false, r)).into_iter().collect();
    out.extend(added.iter().take(2 - out.len()).map(|a| line(true, a)));
    out
}

fn read_request(stream: &mut TcpStream, secret: &str) -> Option<(Vec<u8>, Agent)> {
    let mut data = vec![];
    let mut buf = [0u8; 8192];
    let header_end = loop {
        if let Some(at) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        let n = stream.read(&mut buf).ok()?;
        if n == 0 || data.len() > 64 * 1024 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&data[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let first = lines.next()?;
    let mut parts = first.split(' ');
    let method = parts.next()?;
    let target = parts.next()?;
    if method != "POST" || target.split('?').next()? != ROUTE {
        return None;
    }
    let agent = if target.contains("agent=codex") { Agent::Codex } else { Agent::Claude };
    let (mut given, mut length) = ("", 0usize);
    for line in lines {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("x-twocode-token") {
            given = value.trim();
        } else if name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().ok()?;
        }
    }
    if length > MAX_BODY || !same(given.as_bytes(), secret.as_bytes()) {
        return None;
    }
    let mut body = data[header_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&buf[..n]);
    }
    Some((body, agent))
}

/// Comparison that takes the same time wherever the first difference is.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let kind = if body.is_empty() { "" } else { "Content-Type: application/json\r\n" };
    let _ = write!(stream, "HTTP/1.1 {status}\r\n{kind}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = stream.flush();
}

// ---- ~/.claude/settings.json ----

/// Ours, including what the old Electron TwoCode installed.
fn is_ours(hook: &Value) -> bool {
    let url = hook["url"].as_str().unwrap_or("");
    let command = hook["command"].as_str().unwrap_or("");
    url.starts_with(&format!("http://127.0.0.1:{PORT}{ROUTE}")) || command.contains("twocode-reply")
}

pub fn strip(settings: &mut Value) {
    if let Some(hooks) = settings["hooks"].as_object_mut() {
        hooks.retain(|_, groups| {
            if let Some(list) = groups.as_array_mut() {
                for group in list.iter_mut() {
                    if let Some(inner) = group["hooks"].as_array_mut() {
                        inner.retain(|h| !is_ours(h));
                    }
                }
                list.retain(|g| g["hooks"].as_array().is_none_or(|h| !h.is_empty()));
                !list.is_empty()
            } else {
                true
            }
        });
        if hooks.is_empty() {
            settings.as_object_mut().map(|o| o.remove("hooks"));
        }
    }
    if settings["statusLine"]["command"].as_str().is_some_and(|c| c.contains("twocode-statusline")) {
        settings.as_object_mut().map(|o| o.remove("statusLine"));
    }
}

pub fn add(settings: &mut Value, secret: &str) {
    strip(settings);
    if !settings["hooks"].is_object() {
        settings["hooks"] = json!({});
    }
    for event in EVENTS {
        let timeout = if event == "PermissionRequest" { PERMISSION_WAIT_S + 5 } else { 2 };
        let hook = json!({ "type": "http", "url": hook_url(), "timeout": timeout, "headers": { "X-TwoCode-Token": secret } });
        let groups = &mut settings["hooks"][event];
        if !groups.is_array() {
            *groups = json!([]);
        }
        groups.as_array_mut().map(|g| g.push(json!({ "hooks": [hook] })));
    }
}

pub fn has_current(settings: &Value, secret: &str) -> bool {
    EVENTS.iter().all(|event| {
        settings["hooks"][event].as_array().is_some_and(|groups| {
            groups.iter().any(|g| {
                g["hooks"].as_array().is_some_and(|hooks| {
                    hooks.iter().any(|h| h["url"].as_str() == Some(hook_url().as_str()) && h["headers"]["X-TwoCode-Token"] == secret)
                })
            })
        })
    })
}

// ---- ~/.codex/hooks.json ----

fn codex_command(secret: &str) -> String {
    format!(r#"curl.exe -s -m 65 -H "X-TwoCode-Token: {secret}" -H "Content-Type: application/json" --data-binary @- "{}&agent=codex""#, hook_url())
}

fn is_ours_codex(hook: &Value) -> bool {
    hook["command"].as_str().is_some_and(|c| c.contains(&format!("127.0.0.1:{PORT}{ROUTE}")))
}

pub fn strip_codex(config: &mut Value) {
    if let Some(hooks) = config["hooks"].as_object_mut() {
        hooks.retain(|_, groups| {
            if let Some(list) = groups.as_array_mut() {
                for group in list.iter_mut() {
                    if let Some(inner) = group["hooks"].as_array_mut() {
                        inner.retain(|h| !is_ours_codex(h));
                    }
                }
                list.retain(|g| g["hooks"].as_array().is_none_or(|h| !h.is_empty()));
                !list.is_empty()
            } else {
                true
            }
        });
        if hooks.is_empty() {
            config.as_object_mut().map(|o| o.remove("hooks"));
        }
    }
}

pub fn add_codex(config: &mut Value, secret: &str) {
    strip_codex(config);
    if !config["hooks"].is_object() {
        config["hooks"] = json!({});
    }
    let groups = &mut config["hooks"]["PermissionRequest"];
    if !groups.is_array() {
        *groups = json!([]);
    }
    let hook = json!({ "type": "command", "command": codex_command(secret), "timeout": 70, "statusMessage": "Waiting for TwoCode" });
    groups.as_array_mut().map(|g| g.push(json!({ "hooks": [hook] })));
}

pub fn has_codex(config: &Value, secret: &str) -> bool {
    config["hooks"]["PermissionRequest"].as_array().is_some_and(|groups| {
        groups.iter().any(|g| g["hooks"].as_array().is_some_and(|hooks| hooks.iter().any(|h| h["command"].as_str() == Some(codex_command(secret).as_str()))))
    })
}

fn codex_file() -> std::path::PathBuf {
    let home = std::env::var_os("CODEX_HOME").map_or_else(|| Path::new(&std::env::var_os("USERPROFILE").unwrap_or_default()).join(".codex"), std::path::PathBuf::from);
    home.join("hooks.json")
}

pub fn codex_installed(secret: &str) -> bool {
    read_settings(&codex_file()).is_ok_and(|c| has_codex(&c, secret))
}

pub fn set_codex_installed(on: bool, secret: &str) -> Result<(), String> {
    let file = codex_file();
    let mut config = read_settings(&file)?;
    if on {
        add_codex(&mut config, secret);
    } else {
        strip_codex(&mut config);
    }
    write_settings(&file, &config)
}

fn settings_file() -> std::path::PathBuf {
    Path::new(&std::env::var_os("USERPROFILE").unwrap_or_default()).join(".claude").join("settings.json")
}

fn read_settings(file: &Path) -> Result<Value, String> {
    match fs::read_to_string(file) {
        Ok(text) => serde_json::from_str(&text).map_err(|_| "settings.json is not valid JSON, so nothing was changed.".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e.to_string()),
    }
}

fn write_settings(file: &Path, settings: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())? + "\n";
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    if file.exists() {
        fs::copy(file, file.with_extension("json.twocode-backup")).map_err(|e| e.to_string())?;
    }
    let temp = file.with_extension("json.tmp");
    fs::write(&temp, text).map_err(|e| e.to_string())?;
    fs::rename(&temp, file).map_err(|e| e.to_string())
}

pub fn installed(secret: &str) -> bool {
    read_settings(&settings_file()).is_ok_and(|s| has_current(&s, secret))
}

pub fn set_installed(on: bool, secret: &str) -> Result<(), String> {
    let file = settings_file();
    let mut settings = read_settings(&file)?;
    if on {
        add(&mut settings, secret);
    } else {
        strip(&mut settings);
    }
    write_settings(&file, &settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old_install() -> Value {
        json!({
            "permissions": { "allow": ["Read"] },
            "statusLine": { "type": "command", "command": "~/.claude/mine.sh" },
            "hooks": {
                "Stop": [
                    { "hooks": [{ "type": "http", "url": "http://127.0.0.1:47823/twocode-hook?v=4", "headers": { "X-TwoCode-Token": "old" } }] },
                    { "hooks": [{ "type": "command", "command": "node \"C:\\Downloads\\twocode-reply.cjs\" old", "asyncRewake": true }] },
                    { "hooks": [{ "type": "command", "command": "my-own-hook" }] }
                ],
                "PreToolUse": [{ "hooks": [{ "type": "http", "url": "http://127.0.0.1:47823/twocode-hook?v=4" }] }]
            }
        })
    }

    #[test]
    fn removing_keeps_everything_of_yours() {
        let mut s = old_install();
        strip(&mut s);
        assert_eq!(s["permissions"]["allow"][0], "Read");
        assert_eq!(s["statusLine"]["command"], "~/.claude/mine.sh");
        assert_eq!(s["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(s["hooks"]["Stop"][0]["hooks"][0]["command"], "my-own-hook");
        assert!(s["hooks"].get("PreToolUse").is_none());
    }

    #[test]
    fn adding_replaces_the_old_install() {
        let mut s = old_install();
        assert!(!has_current(&s, "secret"));
        add(&mut s, "secret");
        assert!(has_current(&s, "secret"));
        assert_eq!(s["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert!(s["hooks"].get("PreToolUse").is_none());
        assert_eq!(s["hooks"]["PermissionRequest"][0]["hooks"][0]["timeout"], 305);
        strip(&mut s);
        assert!(!has_current(&s, "secret"));
        assert_eq!(s["hooks"]["Stop"][0]["hooks"][0]["command"], "my-own-hook");
    }

    #[test]
    fn an_old_twocode_statusline_goes_but_yours_stays() {
        let mut s = json!({ "statusLine": { "command": "node twocode-statusline.cjs x" } });
        strip(&mut s);
        assert!(s.get("statusLine").is_none());
    }

    #[test]
    fn codex_hooks_come_and_go_without_touching_yours() {
        let mut c = json!({ "hooks": { "PermissionRequest": [{ "hooks": [{ "type": "command", "command": "mine" }] }], "Stop": [{ "hooks": [{ "type": "command", "command": "other" }] }] } });
        add_codex(&mut c, "secret");
        assert!(has_codex(&c, "secret"));
        assert_eq!(c["hooks"]["PermissionRequest"].as_array().unwrap().len(), 2);
        assert!(c["hooks"]["PermissionRequest"][1]["hooks"][0]["command"].as_str().unwrap().contains("agent=codex"));
        strip_codex(&mut c);
        assert!(!has_codex(&c, "secret"));
        assert_eq!(c["hooks"]["PermissionRequest"][0]["hooks"][0]["command"], "mine");
        assert_eq!(c["hooks"]["Stop"][0]["hooks"][0]["command"], "other");
    }

    #[test]
    fn approvals_show_what_will_change() {
        let edit = approval_for("Edit", &json!({ "file_path": "C:\\x\\a.rs", "old_string": "keep\nconst A = 1;", "new_string": "keep\nconst A = 2;" }));
        assert_eq!(edit.title, "Wants to edit a.rs");
        assert_eq!(edit.diff[0], DiffLine { added: false, text: "const A = 1;".into() });
        assert_eq!(edit.diff[1], DiffLine { added: true, text: "const A = 2;".into() });
        let write = approval_for("Write", &json!({ "file_path": "b.txt", "content": "one\ntwo\nthree" }));
        assert_eq!((write.title.as_str(), write.diff.len()), ("Wants to write b.txt", 2));
        let run = approval_for("Bash", &json!({ "command": "npm i" }));
        assert_eq!((run.title.as_str(), run.what.as_str()), ("Wants to run", "npm i"));
    }

    #[test]
    fn invalid_settings_are_left_alone() {
        let dir = std::env::temp_dir().join(format!("twocode-hooks-{}", agents::now_ms()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("settings.json");
        fs::write(&file, "{ broken").unwrap();
        assert!(read_settings(&file).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "{ broken");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn writing_keeps_a_backup() {
        let dir = std::env::temp_dir().join(format!("twocode-hooks2-{}", agents::now_ms()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("settings.json");
        fs::write(&file, "{\"a\":1}").unwrap();
        write_settings(&file, &json!({ "b": 2 })).unwrap();
        assert_eq!(fs::read_to_string(file.with_extension("json.twocode-backup")).unwrap(), "{\"a\":1}");
        assert_eq!(read_settings(&file).unwrap()["b"], 2);
        fs::remove_dir_all(dir).unwrap();
    }

    fn roundtrip(request: Vec<u8>, secret: &'static str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let hub = Arc::new(Mutex::new(Hub::default()));
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handle(stream, &hub, secret, &|| {});
        });
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client.write_all(&request).unwrap();
        let mut out = String::new();
        client.read_to_string(&mut out).unwrap();
        server.join().unwrap();
        out
    }

    fn post(token: &str, body: &str) -> Vec<u8> {
        format!("POST {ROUTE}?v=5 HTTP/1.1\r\nx-twocode-token: {token}\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    #[test]
    fn the_server_checks_the_token() {
        assert!(roundtrip(post("wrong", "{}"), "right").starts_with("HTTP/1.1 403"));
        assert!(roundtrip(post("right", "{}"), "right").starts_with("HTTP/1.1 200"));
    }

    #[test]
    fn questions_and_plans_are_not_held() {
        let body = r#"{"session_id":"s","hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion","tool_input":{}}"#;
        assert!(roundtrip(post("right", body), "right").starts_with("HTTP/1.1 200"));
    }

    #[test]
    fn allow_reaches_the_waiting_connection() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let hub = Arc::new(Mutex::new(Hub::default()));
        let copy = hub.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handle(stream, &copy, "right", &|| {});
        });
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let body = r#"{"session_id":"s","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"npm i"}}"#;
        client.write_all(&post("right", body)).unwrap();
        let mut view = hub.lock().unwrap().view(agents::now_ms());
        for _ in 0..200 {
            if !view.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
            view = hub.lock().unwrap().view(agents::now_ms());
        }
        assert!(view[0].can_decide);
        assert_eq!(view[0].detail, "Running npm i");
        assert!(hub.lock().unwrap().decide(Agent::Claude, "s", true));
        let mut out = String::new();
        client.read_to_string(&mut out).unwrap();
        assert!(out.contains("\"behavior\":\"allow\""), "{out}");
        assert!(out.contains("npm i"));
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn hanging_up_lets_go_of_the_notch() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let hub = Arc::new(Mutex::new(Hub::default()));
        let copy = hub.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handle(stream, &copy, "right", &|| {});
        });
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let body = r#"{"session_id":"s","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{}}"#;
        client.write_all(&post("right", body)).unwrap();
        for _ in 0..200 {
            if hub.lock().unwrap().view(agents::now_ms()).first().is_some_and(|v| v.can_decide) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(client);
        server.join().unwrap();
        assert!(!hub.lock().unwrap().view(agents::now_ms())[0].can_decide);
    }
}
