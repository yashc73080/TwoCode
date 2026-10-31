//! What Claude Code and Codex are doing. Transcript lines and hook events become the same small events,
//! so a session looks the same no matter which source reported it.
use serde_json::Value;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

const STALE_RUNNING_MS: u64 = 15 * 60 * 1000; // a closed window never says "stop", so silence ends a session
const KEEP_FINISHED_MS: u64 = 30 * 60 * 1000;
const FORGET_MS: u64 = 6 * 60 * 60 * 1000;
const FRESH_MS: u64 = 15 * 1000; // older events are history being replayed at startup, not news
const HOLD_MS: u64 = 300 * 1000;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Waiting,
    Error,
    Running,
    Done,
    Idle,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Event {
    Meta { cwd: String },
    Turn,
    Tool { label: String },
    ToolDone,
    Done,
    Failed(String),
    Interrupted,
    ExternalWait(String),
    End,
}

/// Answers a held PermissionRequest. `None` means "no opinion": the agent asks in its own window.
pub type Responder = Box<dyn FnOnce(Option<String>) + Send>;

struct Hold {
    responder: Responder,
    input: Value,
    since: u64,
}

struct Session {
    agent: Agent,
    id: String,
    project: String,
    status: Status,
    detail: String,
    started: Option<u64>,
    ended: Option<u64>,
    updated: u64,
    hold: Option<Hold>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionView {
    pub agent: Agent,
    pub id: String,
    pub project: String,
    pub status: Status,
    pub detail: String,
    pub elapsed_s: u64,
    pub can_decide: bool,
}

#[derive(Default)]
pub struct Hub {
    sessions: HashMap<(Agent, String), Session>,
    /// Goes up when a session really finishes now, so the notch can show a short sign.
    pub finished_count: u64,
    pub last_finished: Option<(Agent, String, Status)>,
}

impl Hub {
    fn entry(&mut self, agent: Agent, id: &str, ts: u64) -> &mut Session {
        self.sessions.entry((agent, id.to_string())).or_insert_with(|| Session {
            agent,
            id: id.to_string(),
            project: agent.name().to_string(),
            status: Status::Idle,
            detail: String::new(),
            started: None,
            ended: None,
            updated: ts,
            hold: None,
        })
    }

    pub fn apply(&mut self, agent: Agent, id: &str, ts: u64, event: Event, now: u64) {
        if event == Event::End {
            if let Some(mut s) = self.sessions.remove(&(agent, id.to_string())) {
                release(&mut s, None);
            }
            return;
        }
        let s = self.entry(agent, id, ts);
        if let Event::Meta { cwd } = &event {
            let name = cwd.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("");
            if !name.is_empty() {
                s.project = name.to_string();
            }
            return;
        }
        // A hook can report the end before the transcript's last lines are read. Those lines are old news.
        if s.ended.is_some_and(|ended| ts < ended) {
            return;
        }
        s.updated = s.updated.max(ts);
        let mut finished = None;
        match event {
            Event::Turn => {
                release(s, None);
                s.status = Status::Running;
                s.detail = "Thinking".to_string();
                s.started = Some(ts);
                s.ended = None;
            }
            Event::Tool { label } => {
                if s.started.is_none() || s.ended.is_some() {
                    s.started = Some(ts);
                }
                s.ended = None;
                s.detail = label;
                if s.hold.is_none() {
                    s.status = Status::Running;
                }
            }
            Event::ToolDone => {
                release(s, None);
                if s.started.is_none() || s.ended.is_some() {
                    s.started = Some(ts);
                }
                s.ended = None;
                s.status = Status::Running;
            }
            Event::Done => {
                release(s, None);
                finished = (s.status != Status::Done).then_some(Status::Done);
                s.status = Status::Done;
                s.detail = "Finished".to_string();
                s.ended = Some(ts);
            }
            Event::Failed(why) => {
                release(s, None);
                finished = (s.status != Status::Error).then_some(Status::Error);
                s.status = Status::Error;
                s.detail = why;
                s.ended = Some(ts);
            }
            Event::Interrupted => {
                release(s, None);
                s.status = Status::Idle;
                s.detail = "Stopped".to_string();
                s.ended = Some(ts);
            }
            Event::ExternalWait(what) => {
                s.status = Status::Waiting;
                s.detail = what;
            }
            Event::Meta { .. } | Event::End => {}
        }
        if let Some(status) = finished {
            if now.saturating_sub(ts) < FRESH_MS {
                let project = s.project.clone();
                self.finished_count += 1;
                self.last_finished = Some((agent, project, status));
            }
        }
    }

    /// Keep a PermissionRequest open until someone answers in the notch or the agent moves on.
    pub fn hold(&mut self, agent: Agent, id: &str, label: String, input: Value, responder: Responder, now: u64) {
        let s = self.entry(agent, id, now);
        release(s, None);
        s.hold = Some(Hold { responder, input, since: now });
        s.status = Status::Waiting;
        s.detail = label;
        s.updated = now;
        s.ended = None;
    }

    /// The agent hung up on a held request, because it was answered in its own window or it stopped.
    /// `since` tells this request apart from a newer one for the same session.
    pub fn hung_up(&mut self, agent: Agent, id: &str, since: u64) -> bool {
        let Some(s) = self.sessions.get_mut(&(agent, id.to_string())) else { return false };
        if s.hold.as_ref().is_none_or(|h| h.since != since) {
            return false;
        }
        release(s, None);
        true
    }

    pub fn decide(&mut self, agent: Agent, id: &str, allow: bool) -> bool {
        let Some(s) = self.sessions.get_mut(&(agent, id.to_string())) else { return false };
        let Some(hold) = s.hold.take() else { return false };
        let decision = if allow {
            serde_json::json!({ "behavior": "allow", "updatedInput": hold.input })
        } else {
            serde_json::json!({ "behavior": "deny", "message": "Denied from TwoCode." })
        };
        let body = serde_json::json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } });
        (hold.responder)(Some(body.to_string()));
        s.status = Status::Running;
        s.detail = if allow { "Allowed".to_string() } else { "Denied".to_string() };
        true
    }

    /// Lets go of old permission requests and forgets old sessions. Returns true if something changed.
    pub fn expire(&mut self, now: u64) -> bool {
        let mut changed = false;
        for s in self.sessions.values_mut() {
            if s.hold.as_ref().is_some_and(|h| now.saturating_sub(h.since) > HOLD_MS) {
                release(s, None);
                s.status = Status::Running;
                changed = true;
            }
            if s.status == Status::Running && now.saturating_sub(s.updated) > STALE_RUNNING_MS {
                s.status = Status::Idle;
                s.ended = Some(s.updated);
                changed = true;
            }
        }
        let before = self.sessions.len();
        self.sessions.retain(|_, s| s.hold.is_some() || now.saturating_sub(s.updated) < FORGET_MS);
        changed || before != self.sessions.len()
    }

    /// The sessions worth showing, the ones that need you first.
    pub fn view(&self, now: u64) -> Vec<SessionView> {
        let mut list: Vec<&Session> = self
            .sessions
            .values()
            .filter(|s| match s.status {
                Status::Waiting => true,
                Status::Running => now.saturating_sub(s.updated) <= STALE_RUNNING_MS,
                Status::Done | Status::Error => now.saturating_sub(s.updated) < KEEP_FINISHED_MS,
                Status::Idle => false,
            })
            .collect();
        list.sort_by_key(|s| (s.status as u8, std::cmp::Reverse(s.updated)));
        list.into_iter()
            .map(|s| SessionView {
                agent: s.agent,
                id: s.id.clone(),
                project: s.project.clone(),
                status: s.status,
                detail: s.detail.clone(),
                elapsed_s: s.started.map_or(0, |at| s.ended.unwrap_or(now).saturating_sub(at) / 1000),
                can_decide: s.hold.is_some(),
            })
            .collect()
    }
}

fn release(s: &mut Session, answer: Option<String>) {
    if let Some(hold) = s.hold.take() {
        (hold.responder)(answer);
        if s.status == Status::Waiting {
            s.status = Status::Running;
        }
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// "2026-10-30T22:20:05.123Z" to milliseconds since 1970.
pub fn parse_time(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[10] != b'T' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| text.get(r)?.parse::<i64>().ok();
    let (year, month, day) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hour, minute, second) = (n(11..13)?, n(14..16)?, n(17..19)?);
    let millis = if b.get(19) == Some(&b'.') { n(20..23).unwrap_or(0) } else { 0 };
    // days from civil date (Howard Hinnant's algorithm)
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from((days * 86400 + hour * 3600 + minute * 60 + second) * 1000 + millis).ok()
}

fn base(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

pub fn tool_label(tool: &str, input: &Value) -> String {
    let file = input.get("file_path").and_then(Value::as_str).map(base).filter(|f| !f.is_empty());
    let with = |verb: &str| file.map_or_else(|| format!("{verb} a file"), |f| format!("{verb} {f}"));
    let command = || input.get("command").and_then(Value::as_str).map(|c| one_line(c, 60));
    match tool {
        "Read" => with("Reading"),
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => with("Editing"),
        "apply_patch" => "Editing files".to_string(),
        "Bash" | "PowerShell" | "shell" | "shell_command" | "local_shell" | "exec_command" | "unified_exec" => {
            command().map_or_else(|| "Running a command".to_string(), |c| format!("Running {c}"))
        }
        "exec" => "Running a command".to_string(),
        "Grep" | "Glob" => "Searching the code".to_string(),
        "WebSearch" | "WebFetch" | "web_search" => "Searching the web".to_string(),
        "Agent" | "Task" => "Starting a helper".to_string(),
        "" => "Working".to_string(),
        other => format!("Using {}", one_line(other.rsplit("__").next().unwrap_or(other), 40)),
    }
}

pub fn one_line(text: &str, max: usize) -> String {
    let clean = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.chars().count() <= max {
        clean
    } else {
        clean.chars().take(max).collect::<String>() + "…"
    }
}

fn is_interrupt(text: &str) -> bool {
    text.starts_with("[Request interrupted")
}

/// True for user lines the app writes itself (slash command echoes and the like), which start no turn.
fn is_synthetic(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<command-") || t.starts_with("<local-command") || t.starts_with("<task-notification") || t.starts_with("<system-reminder")
}

/// One line of ~/.claude/projects/*/<session>.jsonl.
pub fn parse_claude(line: &str) -> Vec<Event> {
    let Ok(o) = serde_json::from_str::<Value>(line) else { return vec![] };
    if o["isSidechain"].as_bool() == Some(true) || o["isMeta"].as_bool() == Some(true) {
        return vec![];
    }
    let kind = o["type"].as_str().unwrap_or("");
    if kind != "user" && kind != "assistant" {
        return vec![];
    }
    let mut out = vec![];
    if let Some(cwd) = o["cwd"].as_str() {
        out.push(Event::Meta { cwd: cwd.to_string() });
    }
    let content = &o["message"]["content"];
    if kind == "user" {
        match content {
            Value::String(text) if is_interrupt(text) => out.push(Event::Interrupted),
            Value::String(text) if !is_synthetic(text) => out.push(Event::Turn),
            Value::Array(parts) => {
                let text = parts.iter().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str());
                let (mut typed, mut interrupted) = (false, false);
                for t in text {
                    interrupted |= is_interrupt(t);
                    typed |= !is_synthetic(t);
                }
                if interrupted {
                    out.push(Event::Interrupted);
                } else if parts.iter().any(|p| p["type"] == "tool_result") {
                    out.push(Event::ToolDone);
                } else if typed || parts.iter().any(|p| p["type"] == "image") {
                    out.push(Event::Turn);
                }
            }
            _ => {}
        }
        return out;
    }
    if o["isApiErrorMessage"].as_bool() == Some(true) {
        out.push(Event::Failed("API error".to_string()));
        return out;
    }
    if let Some(parts) = content.as_array() {
        for p in parts.iter().filter(|p| p["type"] == "tool_use") {
            out.push(Event::Tool { label: tool_label(p["name"].as_str().unwrap_or(""), &p["input"]) });
        }
    }
    if matches!(o["message"]["stop_reason"].as_str(), Some("end_turn" | "stop_sequence" | "max_tokens")) {
        out.push(Event::Done);
    }
    out
}

/// One line of ~/.codex/sessions/**/rollout-*.jsonl.
pub fn parse_codex(line: &str) -> Vec<Event> {
    let Ok(o) = serde_json::from_str::<Value>(line) else { return vec![] };
    let p = &o["payload"];
    let kind = p["type"].as_str().unwrap_or("");
    match o["type"].as_str().unwrap_or("") {
        "session_meta" | "turn_context" => {
            p["cwd"].as_str().map(|cwd| vec![Event::Meta { cwd: cwd.to_string() }]).unwrap_or_default()
        }
        "event_msg" => match kind {
            "task_started" | "turn_started" => vec![Event::Turn],
            "task_complete" | "turn_complete" => vec![Event::Done],
            "turn_aborted" => vec![Event::Interrupted],
            "error" => vec![Event::Failed(one_line(p["message"].as_str().unwrap_or("Codex error"), 80))],
            "exec_approval_request" | "apply_patch_approval_request" => {
                vec![Event::ExternalWait("Waiting for approval in Codex".to_string())]
            }
            _ => vec![],
        },
        "response_item" => match kind {
            "function_call" | "custom_tool_call" => {
                let args = p["arguments"].as_str().and_then(|a| serde_json::from_str(a).ok()).unwrap_or(Value::Null);
                vec![Event::Tool { label: tool_label(p["name"].as_str().unwrap_or(""), &args) }]
            }
            "local_shell_call" => vec![Event::Tool { label: "Running a command".to_string() }],
            "web_search_call" => vec![Event::Tool { label: "Searching the web".to_string() }],
            "function_call_output" | "custom_tool_call_output" | "local_shell_call_output" => vec![Event::ToolDone],
            _ => vec![],
        },
        _ => vec![],
    }
}

/// A Claude Code hook event. PermissionRequest is handled by the server, which has the connection to hold.
pub fn parse_hook(data: &Value) -> Vec<Event> {
    let mut out = vec![];
    if let Some(cwd) = data["cwd"].as_str() {
        out.push(Event::Meta { cwd: cwd.to_string() });
    }
    match data["hook_event_name"].as_str().unwrap_or("") {
        "UserPromptSubmit" => out.push(Event::Turn),
        "PostToolUse" | "PostToolUseFailure" => out.push(Event::ToolDone),
        "Stop" => out.push(Event::Done),
        "StopFailure" => out.push(Event::Failed(one_line(data["error_type"].as_str().unwrap_or("Claude stopped with an error"), 80))),
        "SessionEnd" => out.push(Event::End),
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const NOW: u64 = 10_000_000;

    fn seen(hub: &Hub) -> SessionView {
        hub.view(NOW).into_iter().next().expect("a session")
    }

    #[test]
    fn reads_time() {
        assert_eq!(parse_time("1970-01-01T00:00:01.500Z"), Some(1500));
        assert_eq!(parse_time("2026-10-31T00:00:00Z"), Some(1_793_404_800_000));
        assert_eq!(parse_time("nonsense"), None);
    }

    #[test]
    fn claude_transcript_lines() {
        let tool = r#"{"type":"assistant","cwd":"C:\\dev\\shop","message":{"stop_reason":"tool_use","content":[{"type":"tool_use","name":"Read","input":{"file_path":"C:\\dev\\shop\\a.rs"}}]}}"#;
        assert_eq!(parse_claude(tool), vec![Event::Meta { cwd: "C:\\dev\\shop".into() }, Event::Tool { label: "Reading a.rs".into() }]);
        let end = r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"ok"}]}}"#;
        assert_eq!(parse_claude(end), vec![Event::Done]);
        assert_eq!(parse_claude(r#"{"type":"user","message":{"content":"fix it"}}"#), vec![Event::Turn]);
        assert_eq!(parse_claude(r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"x"}]}}"#), vec![Event::ToolDone]);
        assert_eq!(parse_claude(r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#), vec![Event::Interrupted]);
        assert_eq!(parse_claude(r#"{"type":"user","message":{"content":"<command-name>/clear</command-name>"}}"#), vec![]);
    }

    #[test]
    fn claude_skips_helpers_and_noise() {
        assert_eq!(parse_claude(r#"{"type":"assistant","isSidechain":true,"message":{"stop_reason":"end_turn"}}"#), vec![]);
        assert_eq!(parse_claude(r#"{"type":"user","isMeta":true,"message":{"content":"hidden"}}"#), vec![]);
        assert_eq!(parse_claude(r#"{"type":"last-prompt","lastPrompt":"x"}"#), vec![]);
        assert_eq!(parse_claude("not json"), vec![]);
    }

    #[test]
    fn codex_rollout_lines() {
        assert_eq!(parse_codex(r#"{"type":"session_meta","payload":{"cwd":"C:\\dev\\x"}}"#), vec![Event::Meta { cwd: "C:\\dev\\x".into() }]);
        assert_eq!(parse_codex(r#"{"type":"event_msg","payload":{"type":"task_started"}}"#), vec![Event::Turn]);
        assert_eq!(parse_codex(r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"x"}}"#), vec![Event::Tool { label: "Running a command".into() }]);
        assert_eq!(
            parse_codex(r#"{"type":"response_item","payload":{"type":"function_call","name":"shell_command","arguments":"{\"command\":\"cargo  test\"}"}}"#),
            vec![Event::Tool { label: "Running cargo test".into() }]
        );
        assert_eq!(parse_codex(r#"{"type":"response_item","payload":{"type":"custom_tool_call_output"}}"#), vec![Event::ToolDone]);
        assert_eq!(parse_codex(r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#), vec![Event::Done]);
        assert_eq!(parse_codex(r#"{"type":"response_item","payload":{"type":"message","role":"user"}}"#), vec![]);
    }

    #[test]
    fn a_turn_from_start_to_finish() {
        let mut hub = Hub::default();
        let id = "s1";
        hub.apply(Agent::Codex, id, NOW - 9000, Event::Meta { cwd: "C:\\dev\\shop\\".into() }, NOW);
        hub.apply(Agent::Codex, id, NOW - 9000, Event::Turn, NOW);
        hub.apply(Agent::Codex, id, NOW - 5000, Event::Tool { label: "Editing a.rs".into() }, NOW);
        let v = seen(&hub);
        assert_eq!((v.project.as_str(), v.status, v.detail.as_str(), v.elapsed_s), ("shop", Status::Running, "Editing a.rs", 9));
        hub.apply(Agent::Codex, id, NOW - 1000, Event::Done, NOW);
        assert_eq!(seen(&hub).status, Status::Done);
        assert_eq!(seen(&hub).elapsed_s, 8);
        assert_eq!(hub.finished_count, 1);
    }

    #[test]
    fn replayed_history_is_not_news() {
        let mut hub = Hub::default();
        hub.apply(Agent::Claude, "old", NOW - 60_000, Event::Done, NOW);
        assert_eq!(hub.finished_count, 0);
    }

    #[test]
    fn late_transcript_lines_do_not_reopen_a_finished_turn() {
        let mut hub = Hub::default();
        hub.apply(Agent::Claude, "s", NOW - 100, Event::Done, NOW); // from the hook
        hub.apply(Agent::Claude, "s", NOW - 500, Event::ToolDone, NOW); // from the file, written earlier
        hub.apply(Agent::Claude, "s", NOW - 400, Event::Done, NOW);
        assert_eq!(seen(&hub).status, Status::Done);
        assert_eq!(hub.finished_count, 1);
    }

    #[test]
    fn silence_ends_a_running_session() {
        let mut hub = Hub::default();
        hub.apply(Agent::Claude, "s", NOW, Event::Turn, NOW);
        assert_eq!(hub.view(NOW + STALE_RUNNING_MS).len(), 1);
        assert!(hub.view(NOW + STALE_RUNNING_MS + 1).is_empty());
        assert!(hub.expire(NOW + STALE_RUNNING_MS + 1));
        assert!(hub.view(NOW + STALE_RUNNING_MS + 2).is_empty());
    }

    #[test]
    fn needs_you_sorts_first() {
        let mut hub = Hub::default();
        hub.apply(Agent::Claude, "a", NOW, Event::Turn, NOW);
        hub.apply(Agent::Codex, "b", NOW - 1000, Event::ExternalWait("Waiting".into()), NOW);
        assert_eq!(seen(&hub).id, "b");
    }

    fn answers() -> (Arc<Mutex<Vec<Option<String>>>>, Responder) {
        let log = Arc::new(Mutex::new(vec![]));
        let copy = log.clone();
        (log, Box::new(move |a| copy.lock().unwrap().push(a)))
    }

    #[test]
    fn allow_answers_the_held_request_once() {
        let mut hub = Hub::default();
        let (log, responder) = answers();
        hub.hold(Agent::Claude, "s", "Running npm i".into(), serde_json::json!({ "command": "npm i" }), responder, NOW);
        assert!(seen(&hub).can_decide);
        assert_eq!(seen(&hub).status, Status::Waiting);
        assert!(hub.decide(Agent::Claude, "s", true));
        assert!(!hub.decide(Agent::Claude, "s", true));
        let got = log.lock().unwrap();
        let body: Value = serde_json::from_str(got[0].as_ref().unwrap()).unwrap();
        assert_eq!(body["hookSpecificOutput"]["decision"]["behavior"], "allow");
        assert_eq!(body["hookSpecificOutput"]["decision"]["updatedInput"]["command"], "npm i");
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn answering_in_the_agent_window_releases_the_hold() {
        let mut hub = Hub::default();
        let (log, responder) = answers();
        hub.hold(Agent::Claude, "s", "x".into(), Value::Null, responder, NOW);
        hub.apply(Agent::Claude, "s", NOW + 10, Event::Tool { label: "parallel".into() }, NOW + 10);
        assert_eq!(seen(&hub).status, Status::Waiting); // a parallel tool does not answer the question
        hub.apply(Agent::Claude, "s", NOW + 20, Event::ToolDone, NOW + 20);
        assert_eq!(*log.lock().unwrap(), vec![None]);
        assert_eq!(seen(&hub).status, Status::Running);
    }

    #[test]
    fn hanging_up_ends_only_that_request() {
        let mut hub = Hub::default();
        let (_, first) = answers();
        hub.hold(Agent::Claude, "s", "x".into(), Value::Null, first, NOW);
        let (_, second) = answers();
        hub.hold(Agent::Claude, "s", "y".into(), Value::Null, second, NOW + 5);
        assert!(!hub.hung_up(Agent::Claude, "s", NOW)); // the old request's connection closing changes nothing
        assert!(seen(&hub).can_decide);
        assert!(hub.hung_up(Agent::Claude, "s", NOW + 5));
        assert!(!seen(&hub).can_decide);
    }

    #[test]
    fn a_forgotten_request_times_out() {
        let mut hub = Hub::default();
        let (log, responder) = answers();
        hub.hold(Agent::Claude, "s", "x".into(), Value::Null, responder, NOW);
        assert!(hub.expire(NOW + HOLD_MS + 1));
        assert_eq!(*log.lock().unwrap(), vec![None]);
    }
}
