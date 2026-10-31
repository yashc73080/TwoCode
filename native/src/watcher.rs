//! Follows the transcript files Claude Code and Codex write, whichever app started them.
use crate::agents::{self, Agent, Event, Hub};
use notify::{RecursiveMode, Watcher};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

const RECENT: Duration = Duration::from_secs(30 * 60);
const FIRST_READ_BYTES: u64 = 512 * 1024;
// Windows can delay change notices for a file that stays open, so busy sessions get a light check as well.
const BUSY_CHECK: Duration = Duration::from_millis(1500);
const QUIET_CHECK: Duration = Duration::from_secs(15);
const FULL_WALK: Duration = Duration::from_secs(30);
const ACTIVE_FOR: Duration = Duration::from_secs(5 * 60);

struct Tail {
    offset: u64,
    partial: Vec<u8>,
    grew: bool,
}

pub fn start(hub: Arc<Mutex<Hub>>, on_change: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        let home = PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default());
        let codex = std::env::var_os("CODEX_HOME").map_or_else(|| home.join(".codex"), PathBuf::from);
        let roots = [(Agent::Claude, home.join(".claude").join("projects")), (Agent::Codex, codex.join("sessions"))];
        let (tx, rx) = mpsc::channel::<Vec<PathBuf>>();
        let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            if let Ok(event) = result {
                let _ = tx.send(event.paths);
            }
        })
        .ok();
        for (_, root) in &roots {
            if let Some(w) = watcher.as_mut() {
                let _ = fs::create_dir_all(root);
                let _ = w.watch(root, RecursiveMode::Recursive);
            }
        }
        let mut tails = HashMap::new();
        let mut first = true;
        let mut last_walk: Option<Instant> = None;
        let mut active: HashMap<PathBuf, (Agent, Instant)> = HashMap::new();
        let mut noticed: Vec<PathBuf> = vec![];
        loop {
            let mut changed = false;
            let mut read = |agent: Agent, path: &Path, active: &mut HashMap<PathBuf, (Agent, Instant)>| {
                if read_new(agent, path, &mut tails, &hub, first) {
                    changed = true;
                }
                if tails.get(path).is_some_and(|t| t.grew) {
                    active.insert(path.to_path_buf(), (agent, Instant::now()));
                }
            };
            if last_walk.is_none_or(|at| at.elapsed() >= FULL_WALK) {
                for (agent, root) in &roots {
                    let mut files = vec![];
                    collect(root, &mut files, 0);
                    for path in files {
                        read(*agent, &path, &mut active);
                    }
                }
                last_walk = Some(Instant::now());
            } else {
                // Only what Windows just named, plus the files that recently grew.
                for path in noticed.drain(..) {
                    if let Some((agent, _)) = roots.iter().find(|(_, root)| path.starts_with(root)) {
                        let helper = path.components().any(|c| c.as_os_str() == "subagents");
                        if !helper && path.extension().is_some_and(|e| e == "jsonl") {
                            read(*agent, &path, &mut active);
                        }
                    }
                }
                let recent: Vec<(PathBuf, Agent)> = active.iter().map(|(p, (a, _))| (p.clone(), *a)).collect();
                for (path, agent) in recent {
                    read(agent, &path, &mut active);
                }
            }
            active.retain(|_, (_, at)| at.elapsed() < ACTIVE_FOR);
            first = false;
            changed |= hub.lock().map(|mut h| h.expire(agents::now_ms())).unwrap_or(false);
            if changed {
                on_change();
            }
            let wait = if active.is_empty() { QUIET_CHECK } else { BUSY_CHECK };
            noticed.clear();
            if let Ok(paths) = rx.recv_timeout(wait) {
                noticed.extend(paths);
                std::thread::sleep(Duration::from_millis(40)); // one burst of notices, one read
                noticed.extend(rx.try_iter().flatten());
                noticed.sort();
                noticed.dedup();
            }
        }
    });
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            // helpers have their own files; their parent's session already covers them
            if depth < 5 && entry.file_name() != "subagents" {
                collect(&path, out, depth + 1);
            }
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
}

fn session_id(agent: Agent, path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match agent {
        Agent::Claude => stem,
        // rollout-2026-10-30T22-20-05-<uuid>: the uuid is the last 36 characters
        Agent::Codex => stem.get(stem.len().saturating_sub(36)..).unwrap_or(&stem).to_string(),
    }
}

fn read_new(agent: Agent, path: &Path, tails: &mut HashMap<PathBuf, Tail>, hub: &Mutex<Hub>, first: bool) -> bool {
    let Ok(meta) = fs::metadata(path) else { return false };
    let len = meta.len();
    let tail = match tails.get_mut(path) {
        Some(tail) => tail,
        None => {
            let recent = meta.modified().ok().and_then(|m| SystemTime::now().duration_since(m).ok()).is_some_and(|age| age < RECENT);
            // At startup old files are not read. A file that appears later is a new session: read it whole.
            let offset = if !first {
                0
            } else if recent {
                len.saturating_sub(FIRST_READ_BYTES)
            } else {
                len
            };
            tails.entry(path.to_path_buf()).or_insert(Tail { offset, partial: vec![], grew: false })
        }
    };
    if len < tail.offset {
        *tail = Tail { offset: 0, partial: vec![], grew: false };
    }
    tail.grew = len != tail.offset;
    if !tail.grew {
        return false;
    }
    let cut_in_half = tail.offset > 0 && first;
    let Ok(mut file) = File::open(path) else { return false };
    if file.seek(SeekFrom::Start(tail.offset)).is_err() {
        return false;
    }
    let mut bytes = vec![];
    if file.take(len - tail.offset).read_to_end(&mut bytes).is_err() {
        return false;
    }
    tail.offset = len;
    tail.partial.extend_from_slice(&bytes);
    let Some(end) = tail.partial.iter().rposition(|b| *b == b'\n') else { return false };
    let rest = tail.partial.split_off(end + 1);
    let whole = std::mem::replace(&mut tail.partial, rest);
    let text = String::from_utf8_lossy(&whole);
    let mut lines = text.lines();
    if cut_in_half {
        lines.next();
    }
    let id = session_id(agent, path);
    let Ok(mut hub) = hub.lock() else { return false };
    let now = agents::now_ms();
    let mut changed = false;
    for line in lines {
        changed |= apply_line(agent, &id, line, now, &mut hub);
    }
    changed
}

/// Exposed for the tests: one transcript line into the hub.
fn apply_line(agent: Agent, id: &str, line: &str, now: u64, hub: &mut Hub) -> bool {
    let events = match agent {
        Agent::Claude => agents::parse_claude(line),
        Agent::Codex => agents::parse_codex(line),
    };
    if events.is_empty() {
        return false;
    }
    let ts = serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|o| o["timestamp"].as_str().and_then(agents::parse_time))
        .unwrap_or(now);
    let mut real = false;
    for event in events {
        real |= !matches!(event, Event::Meta { .. });
        hub.apply(agent, id, ts, event, now);
    }
    real
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_a_growing_file_and_waits_for_whole_lines() {
        let dir = std::env::temp_dir().join(format!("twocode-test-{}", agents::now_ms()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("abc.jsonl");
        fs::write(&path, "{\"type\":\"user\",\"cwd\":\"C:\\\\dev\\\\shop\",\"message\":{\"content\":\"go\"}}\n{\"type\":\"assis").unwrap();
        let hub = Mutex::new(Hub::default());
        let mut tails = HashMap::new();
        assert!(read_new(Agent::Claude, &path, &mut tails, &hub, false));
        let view = hub.lock().unwrap().view(agents::now_ms());
        assert_eq!((view[0].id.as_str(), view[0].project.as_str()), ("abc", "shop"));
        assert_eq!(view[0].status, agents::Status::Running);
        let mut more = fs::read(&path).unwrap();
        more.extend_from_slice(b"tant\",\"message\":{\"stop_reason\":\"end_turn\",\"content\":[]}}\n");
        fs::write(&path, more).unwrap();
        assert!(read_new(Agent::Claude, &path, &mut tails, &hub, false));
        assert_eq!(hub.lock().unwrap().view(agents::now_ms())[0].status, agents::Status::Done);
        assert!(!read_new(Agent::Claude, &path, &mut tails, &hub, false));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_files_are_skipped_at_startup_but_followed_after() {
        let dir = std::env::temp_dir().join(format!("twocode-test2-{}", agents::now_ms()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-2026-01-01T00-00-00-01a19066-95db-7f70-9059-15afcb2686f5.jsonl");
        let file = File::create(&path).unwrap();
        use std::io::Write;
        (&file).write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n").unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(7200)).unwrap();
        let hub = Mutex::new(Hub::default());
        let mut tails = HashMap::new();
        assert!(!read_new(Agent::Codex, &path, &mut tails, &hub, true));
        (&file).write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n").unwrap();
        drop(file);
        assert!(read_new(Agent::Codex, &path, &mut tails, &hub, false));
        let view = hub.lock().unwrap().view(agents::now_ms());
        assert_eq!(view[0].id, "01a19066-95db-7f70-9059-15afcb2686f5");
        fs::remove_dir_all(dir).unwrap();
    }
}
