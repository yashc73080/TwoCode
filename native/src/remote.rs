//! Follows Claude Code sessions that the desktop app runs on another machine over SSH.
//!
//! For an SSH environment the transcript is written on the remote machine. The app copies it to
//! ~/.claude/projects/ssh-<id>/ only when a turn ends or it reconnects, so the island would see the
//! result but none of the progress. The app's own session record names the host and the remote
//! transcript path, so this follows that file live over our own `ssh ... tail -F`.
use crate::agents::{self, Agent, Hub};
use crate::watcher;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

const SCAN: Duration = Duration::from_secs(10);
/// A session the app touched this recently gets followed.
const START_WITHIN_MS: u64 = 30 * 60 * 1000;
/// A followed session is let go once the app has not touched it for this long.
const KEEP_FOR_MS: u64 = 6 * 60 * 60 * 1000;
const FIRST_READ_BYTES: u64 = 512 * 1024;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The ids of the sessions followed live, so the watcher leaves their delayed local copies alone.
static FOLLOWED: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn is_followed(id: &str) -> bool {
    FOLLOWED.lock().is_ok_and(|f| f.iter().any(|x| x == id))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Remote {
    pub id: String,
    pub host: String,
    pub path: String,
    pub activity: u64,
    pub archived: bool,
}

/// One of the desktop app's session records (claude-code-sessions/**/local_*.json).
pub fn parse_record(record: &Value) -> Option<Remote> {
    let host = record["sshConfig"]["sshHost"].as_str()?.trim();
    let path = record["sshRemoteTranscriptPath"].as_str()?.trim();
    let id = record["cliSessionId"].as_str()?.trim();
    if host.is_empty() || path.is_empty() || id.is_empty() || host.starts_with('-') {
        return None;
    }
    Some(Remote {
        id: id.to_string(),
        host: host.to_string(),
        path: path.to_string(),
        activity: record["lastActivityAt"].as_u64().unwrap_or(0),
        archived: record["isArchived"].as_bool().unwrap_or(false),
    })
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The remote shell command: print the byte the tail starts at, then follow the file from there.
/// `start` is 1-based like `tail -c +N`; 0 means "the last FIRST_READ_BYTES of it".
pub fn tail_command(path: &str, start: u64) -> String {
    let start = if start > 0 {
        start.to_string()
    } else {
        format!("$(s=$(stat -c %s -- \"$f\" 2>/dev/null || echo 0); if [ \"$s\" -gt {FIRST_READ_BYTES} ]; then echo $((s - {FIRST_READ_BYTES} + 1)); else echo 1; fi)")
    };
    format!("f={}; n={start}; echo \"$n\"; exec tail -c +\"$n\" -F -- \"$f\" 2>/dev/null", quote(path))
}

fn record_roots() -> Vec<PathBuf> {
    let mut roots = vec![];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        if let Ok(entries) = std::fs::read_dir(PathBuf::from(local).join("Claude-Data")) {
            roots.extend(entries.flatten().map(|e| e.path().join("claude-code-sessions")));
        }
    }
    if let Some(roaming) = std::env::var_os("APPDATA") {
        roots.push(PathBuf::from(roaming).join("Claude").join("claude-code-sessions"));
    }
    roots.into_iter().filter(|r| r.is_dir()).collect()
}

fn collect(dir: &Path, out: &mut Vec<(PathBuf, SystemTime)>, depth: u32) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            if depth < 3 {
                collect(&path, out, depth + 1);
            }
        } else {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("local_") && name.ends_with(".json") {
                if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                    out.push((path, modified));
                }
            }
        }
    }
}

struct Follower {
    remote: Remote,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Follower {
    fn end(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(mut child) = self.child.lock() {
            if let Some(c) = child.as_mut() {
                let _ = c.kill();
            }
        }
    }
}

pub fn start(hub: Arc<Mutex<Hub>>, on_change: impl Fn() + Send + Sync + 'static) {
    let on_change: Arc<dyn Fn() + Send + Sync> = Arc::new(on_change);
    std::thread::spawn(move || {
        let mut records: HashMap<PathBuf, (SystemTime, Option<Remote>)> = HashMap::new();
        let mut followers: HashMap<String, Follower> = HashMap::new();
        loop {
            let mut files = vec![];
            for root in record_roots() {
                collect(&root, &mut files, 0);
            }
            records.retain(|path, _| files.iter().any(|(p, _)| p == path));
            for (path, modified) in files {
                if records.get(&path).is_some_and(|(at, _)| *at == modified) {
                    continue;
                }
                let remote = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|r| parse_record(&r));
                records.insert(path, (modified, remote));
            }

            let now = agents::now_ms();
            let mut wanted: HashMap<String, Remote> = HashMap::new();
            for remote in records.values().filter_map(|(_, r)| r.as_ref()) {
                let age = now.saturating_sub(remote.activity);
                let keep = if followers.contains_key(&remote.id) { KEEP_FOR_MS } else { START_WITHIN_MS };
                if !remote.archived && age < keep {
                    wanted.insert(remote.id.clone(), remote.clone());
                }
            }
            followers.retain(|id, f| {
                let same = wanted.get(id).is_some_and(|r| r.host == f.remote.host && r.path == f.remote.path);
                if !same {
                    f.end();
                }
                same
            });
            for (id, remote) in wanted {
                followers.entry(id).or_insert_with(|| follow(remote, hub.clone(), on_change.clone()));
            }
            if let Ok(mut list) = FOLLOWED.lock() {
                *list = followers.keys().cloned().collect();
            }
            std::thread::sleep(SCAN);
        }
    });
}

fn follow(remote: Remote, hub: Arc<Mutex<Hub>>, on_change: Arc<dyn Fn() + Send + Sync>) -> Follower {
    let stop = Arc::new(AtomicBool::new(false));
    let child: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let follower = Follower { remote: remote.clone(), stop: stop.clone(), child: child.clone() };
    std::thread::spawn(move || {
        // 1-based byte the next tail starts at; 0 until the remote side has told us.
        let mut next: u64 = 0;
        let mut backoff = Duration::from_secs(5);
        while !stop.load(Ordering::SeqCst) {
            let began = Instant::now();
            let spawned = Command::new("ssh")
                .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=30", "-o", "ServerAliveCountMax=3", "-T"])
                .arg(&remote.host)
                .arg(tail_command(&remote.path, next))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
            if let Ok(mut c) = spawned {
                let out = c.stdout.take();
                if let Ok(mut slot) = child.lock() {
                    *slot = Some(c);
                }
                if let Some(out) = out {
                    read_tail(out, &remote.id, &mut next, &hub, &*on_change, &stop);
                }
                if let Ok(mut slot) = child.lock() {
                    if let Some(mut c) = slot.take() {
                        let _ = c.kill();
                        let _ = c.wait();
                    }
                }
            }
            if began.elapsed() > Duration::from_secs(60) {
                backoff = Duration::from_secs(5);
            }
            let until = Instant::now() + backoff;
            while Instant::now() < until && !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(500));
            }
            backoff = (backoff * 2).min(Duration::from_secs(300));
        }
    });
    follower
}

fn read_tail(out: impl std::io::Read, id: &str, next: &mut u64, hub: &Mutex<Hub>, on_change: &(dyn Fn() + Send + Sync), stop: &AtomicBool) {
    let mut reader = BufReader::new(out);
    let mut header = String::new();
    if reader.read_line(&mut header).unwrap_or(0) == 0 {
        return;
    }
    let Ok(start) = header.trim().parse::<u64>() else { return };
    // Starting partway into the file lands inside a line unless we already know where lines end.
    let mut skip_first = *next == 0 && start > 1;
    *next = start.max(1);
    let mut line = vec![];
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if stop.load(Ordering::SeqCst) || line.last() != Some(&b'\n') {
            return;
        }
        *next += line.len() as u64;
        if std::mem::take(&mut skip_first) {
            continue;
        }
        let text = String::from_utf8_lossy(&line);
        let changed = hub.lock().map(|mut h| watcher::apply_line(Agent::Claude, id, text.trim_end(), agents::now_ms(), &mut h)).unwrap_or(false);
        if changed {
            on_change();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_apps_ssh_session_record() {
        let record: Value = serde_json::from_str(
            r#"{"cliSessionId":"9de7","sshConfig":{"sshHost":"ec2-box"},"sshRemoteTranscriptPath":"/home/u/.claude/projects/p/9de7.jsonl","lastActivityAt":5,"isArchived":false}"#,
        )
        .unwrap();
        let remote = parse_record(&record).unwrap();
        assert_eq!(remote.host, "ec2-box");
        assert_eq!(remote.path, "/home/u/.claude/projects/p/9de7.jsonl");
        assert_eq!(remote.activity, 5);
        let local: Value = serde_json::from_str(r#"{"cliSessionId":"a","cwd":"C:\\dev"}"#).unwrap();
        assert!(parse_record(&local).is_none());
        let sneaky: Value = serde_json::from_str(r#"{"cliSessionId":"a","sshConfig":{"sshHost":"-oProxyCommand=x"},"sshRemoteTranscriptPath":"/p"}"#).unwrap();
        assert!(parse_record(&sneaky).is_none());
    }

    #[test]
    fn the_remote_path_is_quoted() {
        let cmd = tail_command("/home/u/it's here.jsonl", 42);
        assert!(cmd.starts_with(r"f='/home/u/it'\''s here.jsonl'; n=42;"));
        assert!(tail_command("/p", 0).contains("stat -c %s"));
    }

    #[test]
    fn a_tail_resumes_where_it_stopped_and_skips_a_cut_line() {
        let hub = Mutex::new(Hub::default());
        let stop = AtomicBool::new(false);
        let mut next = 0;
        let stream = b"101\nhalf a line\n{\"type\":\"x\"}\npartial".to_vec();
        read_tail(&stream[..], "s", &mut next, &hub, &|| {}, &stop);
        assert_eq!(next, 101 + "half a line\n".len() as u64 + "{\"type\":\"x\"}\n".len() as u64);
    }
}
