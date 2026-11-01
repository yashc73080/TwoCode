//! What Claude Code and Codex are doing. Transcript lines and hook events become the same small events,
//! so a session looks the same no matter which source reported it.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

const STALE_RUNNING_MS: u64 = 15 * 60 * 1000; // a closed window never says "stop", so silence ends a session
const KEEP_FINISHED_MS: u64 = 30 * 60 * 1000;
const FORGET_MS: u64 = 6 * 60 * 60 * 1000;
const KEEP_RUNS_MS: u64 = 3 * 60 * 60 * 1000;
const FRESH_MS: u64 = 15 * 1000; // older events are history being replayed at startup, not news
const HOLD_MS: u64 = 300 * 1000;
// Codex shows no prompt of its own until the hook answers, so it only waits a minute for you.
const CODEX_HOLD_MS: u64 = 60 * 1000;
const STUCK_S: u64 = 60;
const SAME_ERROR_TIMES: u32 = 3;
const MAX_RUNS: usize = 30;
const QUIET_S: u64 = 30; // no news for this long: it may be stuck or waiting in its own window
const MAX_STEPS: usize = 8;
const MAX_FILES: usize = 40;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
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
    /// Which app runs this session: "claude-desktop", "Codex Desktop", "cli" and so on.
    Source(String),
    ToolError(String),
    Turn,
    Tool { label: String },
    /// What the agent last wrote or reasoned, in its own words.
    Said(String),
    /// What you asked for this turn.
    Prompt(String),
    Plan(PlanChange),
    FileChanged(String),
    /// A test command started; its result arrives under this tool id.
    TestStarted(String),
    ToolResult { id: String, failed: bool },
    TestResult(bool),
    ToolDone,
    Done,
    Failed(String),
    Interrupted,
    ExternalWait(String),
    End,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PlanState {
    Pending,
    Active,
    Done,
}

#[derive(Clone, PartialEq, Debug)]
pub struct PlanItem {
    pub text: String,
    pub state: PlanState,
}

#[derive(Clone, PartialEq, Debug)]
pub enum PlanChange {
    Replace(Vec<PlanItem>),
    Add(String),
    /// Finish the active item and start the next waiting one.
    Advance,
    /// Item numbers count from 1.
    Mark { done: Vec<usize>, start: Option<usize> },
}

fn change_plan(plan: &mut Vec<PlanItem>, change: PlanChange) {
    match change {
        PlanChange::Replace(items) => *plan = items,
        PlanChange::Add(text) => plan.push(PlanItem { text, state: PlanState::Pending }),
        PlanChange::Advance => {
            for item in plan.iter_mut().filter(|i| i.state == PlanState::Active) {
                item.state = PlanState::Done;
            }
            if let Some(next) = plan.iter_mut().find(|i| i.state == PlanState::Pending) {
                next.state = PlanState::Active;
            }
        }
        PlanChange::Mark { done, start } => {
            for number in done {
                if let Some(item) = number.checked_sub(1).and_then(|at| plan.get_mut(at)) {
                    item.state = PlanState::Done;
                }
            }
            if let Some(item) = start.and_then(|n| n.checked_sub(1)).and_then(|at| plan.get_mut(at)) {
                item.state = PlanState::Active;
            }
        }
    }
}

/// What a held request wants to do, in enough detail to decide safely.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Approval {
    pub title: String,
    pub what: String,
    pub diff: Vec<DiffLine>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct DiffLine {
    pub added: bool,
    pub text: String,
}

/// A finished run, kept on this PC for a few hours so the agents page has something to show.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Run {
    pub agent: Agent,
    pub project: String,
    pub prompt: String,
    pub summary: String,
    pub files: Vec<String>,
    pub failed: bool,
    pub at_ms: u64,
    #[serde(default)]
    pub source: String,
}

impl Run {
    pub fn is_recent(&self, now: u64) -> bool {
        now.saturating_sub(self.at_ms) < KEEP_RUNS_MS
    }
}

/// Is any agent working or waiting? If not, the agents page shows what finished lately.
pub fn any_live(sessions: &[SessionView]) -> bool {
    sessions.iter().any(|s| matches!(s.status, Status::Running | Status::Waiting))
}

/// Answers a held PermissionRequest. `None` means "no opinion": the agent asks in its own window.
pub type Responder = Box<dyn FnOnce(Option<String>) + Send>;

struct Hold {
    responder: Responder,
    approval: Approval,
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
    said: String,
    steps: Vec<Step>,
    prompt: String,
    plan: Vec<PlanItem>,
    files: Vec<String>,
    tests: Option<bool>,
    test_tool: Option<String>,
    /// Finished and the pill has not been acknowledged yet.
    unseen: bool,
    /// You have looked at the result, so the list can let it go.
    viewed: bool,
    source: String,
    last_error: String,
    error_count: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub label: String,
    pub done: bool,
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
    pub approval: Approval,
    pub source: String,
    /// Set once the same error has come back several times.
    pub repeated_error: Option<(String, u32)>,
    pub said: String,
    pub steps: Vec<Step>,
    pub quiet_s: u64,
    pub prompt: String,
    pub plan: Vec<PlanItem>,
    pub files: Vec<String>,
    pub tests: Option<bool>,
    pub unseen: bool,
    pub turn_over: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Pill {
    Quiet,
    Working,
    Finished,
    Failed,
}

/// The pill's colour: a failure until you look, then active work, then a finish you have not seen.
pub fn pill_state(list: &[SessionView]) -> Pill {
    if list.iter().any(|s| s.status == Status::Error && s.unseen) {
        Pill::Failed
    } else if list.iter().any(|s| s.status == Status::Running || (s.status == Status::Waiting && !s.turn_over)) {
        Pill::Working
    } else if list.iter().any(|s| s.unseen) {
        Pill::Finished
    } else {
        Pill::Quiet
    }
}

impl SessionView {
    /// "Step 3 of 7: Updating the parser", when the agent keeps a task list.
    pub fn progress_line(&self) -> Option<String> {
        let total = self.plan.len();
        if total == 0 {
            return None;
        }
        let at = self.plan.iter().position(|i| i.state == PlanState::Active).or_else(|| self.plan.iter().position(|i| i.state == PlanState::Pending));
        Some(match at {
            Some(at) => format!("Step {} of {total}: {}", at + 1, self.plan[at].text),
            None => format!("All {total} steps done"),
        })
    }

    pub fn progress(&self) -> Option<f32> {
        if self.plan.is_empty() {
            return None;
        }
        let done = self.plan.iter().filter(|i| i.state == PlanState::Done).count();
        Some(done as f32 / self.plan.len() as f32)
    }

    /// "Changed 4 files · tests passed · took 6m 02s".
    pub fn summary(&self) -> String {
        let mut parts = vec![match self.files.len() {
            0 => "No files changed".to_string(),
            1 => "Changed 1 file".to_string(),
            n => format!("Changed {n} files"),
        }];
        match self.tests {
            Some(true) => parts.push("tests passed".to_string()),
            Some(false) => parts.push("tests failed".to_string()),
            None => {}
        }
        if self.elapsed_s > 0 {
            parts.push(format!("took {}", crate::layout::elapsed(self.elapsed_s)));
        }
        parts.join(" · ")
    }

    /// Why this running session looks stuck, as specifically as the evidence allows.
    pub fn stuck_reason(&self) -> Option<String> {
        if self.status != Status::Running {
            return None;
        }
        if let Some((_, times)) = &self.repeated_error {
            return Some(format!("Same error {times} times"));
        }
        if self.quiet_s < QUIET_S {
            return None;
        }
        let long = crate::layout::elapsed(self.quiet_s);
        Some(match self.steps.last() {
            Some(step) if !step.done && self.quiet_s >= STUCK_S && step.label.starts_with("Running") => format!("Same command for {long}"),
            Some(step) if !step.done && self.quiet_s >= STUCK_S => format!("Same step for {long}"),
            _ => format!("Quiet for {long}"),
        })
    }

    pub fn is_quiet(&self) -> bool {
        self.stuck_reason().is_some()
    }

    /// One plain phrase for what this session needs from you.
    pub fn status_text(&self) -> String {
        match self.status {
            Status::Waiting if self.can_decide => "Needs approval".to_string(),
            Status::Waiting => "Needs you".to_string(),
            Status::Error => "Error".to_string(),
            Status::Running => self.stuck_reason().unwrap_or_else(|| "Working".to_string()),
            Status::Done => "Done".to_string(),
            Status::Idle => "Stopped".to_string(),
        }
    }
}

#[derive(Default)]
pub struct Hub {
    sessions: HashMap<(Agent, String), Session>,
    /// Goes up when a session really finishes now, so the notch can show a short sign.
    pub finished_count: u64,
    pub recent: Vec<Run>,
    pub recent_changed: bool,
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
            said: String::new(),
            steps: vec![],
            prompt: String::new(),
            plan: vec![],
            files: vec![],
            tests: None,
            test_tool: None,
            unseen: false,
            viewed: false,
            source: String::new(),
            last_error: String::new(),
            error_count: 0,
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
        if let Event::Source(source) = &event {
            s.source = source.clone();
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
                s.said.clear();
                s.steps.clear();
                s.prompt.clear();
                s.plan.clear();
                s.files.clear();
                s.tests = None;
                s.test_tool = None;
                s.unseen = false;
                s.viewed = false;
                s.last_error.clear();
                s.error_count = 0;
                s.started = Some(ts);
                s.ended = None;
            }
            Event::Tool { label } => {
                if s.started.is_none() || s.ended.is_some() {
                    s.started = Some(ts);
                }
                s.ended = None;
                s.steps.push(Step { label: label.clone(), done: false });
                if s.steps.len() > MAX_STEPS {
                    s.steps.remove(0);
                }
                s.detail = label;
                if s.hold.is_none() {
                    s.status = Status::Running;
                }
            }
            Event::Said(text) => s.said = text,
            Event::Prompt(text) => s.prompt = text,
            Event::Plan(change) => change_plan(&mut s.plan, change),
            Event::FileChanged(name) => {
                if !s.files.contains(&name) && s.files.len() < MAX_FILES {
                    s.files.push(name);
                }
            }
            Event::TestStarted(id) => s.test_tool = Some(id),
            Event::ToolResult { id, failed } => {
                if s.test_tool.as_deref() == Some(id.as_str()) {
                    s.tests = Some(!failed);
                    s.test_tool = None;
                }
            }
            Event::TestResult(passed) => s.tests = Some(passed),
            Event::ToolError(text) => {
                if s.last_error == text {
                    s.error_count += 1;
                } else {
                    s.last_error = text;
                    s.error_count = 1;
                }
            }
            Event::ToolDone => {
                release(s, None);
                if let Some(step) = s.steps.iter_mut().find(|step| !step.done) {
                    step.done = true;
                }
                if s.started.is_none() || s.ended.is_some() {
                    s.started = Some(ts);
                }
                s.ended = None;
                s.status = Status::Running;
            }
            Event::Done => {
                release(s, None);
                // A turn that ends on a question is waiting for you, not finished.
                let next = if s.said.trim_end().ends_with('?') { Status::Waiting } else { Status::Done };
                finished = (s.status != next).then_some(next);
                s.status = next;
                s.detail = if next == Status::Waiting { "Asked you a question" } else { "Finished" }.to_string();
                s.ended = Some(ts);
                for step in &mut s.steps {
                    step.done = true;
                }
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
                finished = (s.status != Status::Waiting).then_some(Status::Waiting);
                s.status = Status::Waiting;
                s.detail = what;
            }
            Event::Meta { .. } | Event::Source(_) | Event::End => {}
        }
        if let Some(status) = finished {
            let fresh = now.saturating_sub(ts) < FRESH_MS;
            s.unseen = fresh;
            s.viewed = !fresh; // history replayed at startup is not news
            if fresh {
                let project = s.project.clone();
                if matches!(status, Status::Done | Status::Error) {
                    let v = snapshot(s, now);
                    let summary = if status == Status::Error { v.detail.clone() } else { v.summary() };
                    self.recent.retain(|run| run.is_recent(now));
                    self.recent.insert(0, Run { agent, project: v.project, prompt: v.prompt, summary, files: v.files, failed: status == Status::Error, at_ms: ts, source: v.source });
                    self.recent.truncate(MAX_RUNS);
                    self.recent_changed = true;
                }
                self.finished_count += 1;
                self.last_finished = Some((agent, project, status));
            }
        }
    }

    /// You opened the notch, so the pill goes back to black.
    pub fn acknowledge(&mut self) {
        for s in self.sessions.values_mut() {
            s.unseen = false;
        }
    }

    /// You looked at the agents page: finished work and errors can leave the list.
    pub fn mark_viewed(&mut self) {
        for s in self.sessions.values_mut() {
            if matches!(s.status, Status::Done | Status::Error) {
                s.viewed = true;
            }
        }
    }

    /// Keep a PermissionRequest open until someone answers in the notch or the agent moves on.
    pub fn hold(&mut self, agent: Agent, id: &str, label: String, approval: Approval, input: Value, responder: Responder, now: u64) {
        let s = self.entry(agent, id, now);
        release(s, None);
        s.hold = Some(Hold { responder, approval, input, since: now });
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
        // Codex fails closed on updatedInput, so it gets a plain allow.
        let decision = if allow && agent == Agent::Codex {
            serde_json::json!({ "behavior": "allow" })
        } else if allow {
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
            let limit = if s.agent == Agent::Codex { CODEX_HOLD_MS } else { HOLD_MS };
            if s.hold.as_ref().is_some_and(|h| now.saturating_sub(h.since) > limit) {
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
                Status::Waiting => s.hold.is_some() || now.saturating_sub(s.updated) < KEEP_FINISHED_MS,
                Status::Running => now.saturating_sub(s.updated) <= STALE_RUNNING_MS,
                Status::Done | Status::Error => !s.viewed && now.saturating_sub(s.updated) < KEEP_FINISHED_MS,
                Status::Idle => false,
            })
            .collect();
        // What needs you comes first, then whatever was active most recently.
        list.sort_by_key(|s| (s.status != Status::Waiting, std::cmp::Reverse(s.updated)));
        list.into_iter()
            .map(|s| snapshot(s, now))
            .collect()
    }
}

fn snapshot(s: &Session, now: u64) -> SessionView {
    SessionView {
        agent: s.agent,
        id: s.id.clone(),
        project: s.project.clone(),
        status: s.status,
        detail: s.detail.clone(),
        elapsed_s: s.started.map_or(0, |at| s.ended.unwrap_or(now).saturating_sub(at) / 1000),
        can_decide: s.hold.is_some(),
        approval: s.hold.as_ref().map_or(Approval::default(), |h| h.approval.clone()),
        source: s.source.clone(),
        repeated_error: (s.error_count >= SAME_ERROR_TIMES).then(|| (s.last_error.clone(), s.error_count)),
        said: s.said.clone(),
        steps: s.steps.clone(),
        quiet_s: if s.status == Status::Running { now.saturating_sub(s.updated) / 1000 } else { 0 },
        prompt: s.prompt.clone(),
        plan: s.plan.clone(),
        files: s.files.clone(),
        tests: s.tests,
        unseen: s.unseen,
        turn_over: s.ended.is_some(),
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

/// The last thing said, short enough for the notch and without markdown marks.
pub fn latest_words(text: &str) -> String {
    const MAX: usize = 220;
    let clean = text.replace("**", "").replace('`', "").replace('#', "");
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = clean.chars().count();
    if count <= MAX {
        return clean;
    }
    let tail: String = clean.chars().skip(count - MAX).collect();
    match tail.find(". ") {
        Some(at) if at < MAX / 2 => tail[at + 2..].to_string(),
        _ => format!("…{tail}"),
    }
}

/// What you asked, without comment markers or quote marks from a pasted reply.
fn prompt_line(text: &str) -> String {
    let mut rest = text;
    let mut clean = String::new();
    while let Some(start) = rest.find("<!--") {
        clean.push_str(&rest[..start]);
        rest = rest[start..].find("-->").map_or("", |end| &rest[start + end + 3..]);
    }
    clean.push_str(rest);
    let lines: Vec<&str> = clean.lines().map(|l| l.trim_start().trim_start_matches('>').trim()).filter(|l| !l.is_empty()).collect();
    one_line(&lines.join(" "), 120)
}

fn is_test_command(command: &str) -> bool {
    let c = command.to_lowercase();
    ["cargo test", "npm test", "npm run test", "yarn test", "pnpm test", "pytest", "jest", "vitest", "go test", "dotnet test", "node --test"]
        .iter()
        .any(|t| c.contains(t))
}

fn plan_state(status: &str) -> PlanState {
    match status {
        "in_progress" => PlanState::Active,
        "completed" | "done" => PlanState::Done,
        _ => PlanState::Pending,
    }
}

fn plan_items(list: &Value, text_keys: &[&str]) -> Vec<PlanItem> {
    list.as_array()
        .map(|items| {
            items
                .iter()
                .map(|i| PlanItem {
                    text: one_line(text_keys.iter().find_map(|k| i[*k].as_str()).unwrap_or(""), 80),
                    state: plan_state(i["status"].as_str().unwrap_or("")),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The task-list tools Claude uses, as plan changes. `None` means this is not one of them.
fn claude_plan(name: &str, input: &Value) -> Option<Vec<Event>> {
    let change = match name {
        "TodoWrite" => Some(PlanChange::Replace(plan_items(&input["todos"], &["content", "activeForm"]))),
        "TaskCreate" => input["subject"].as_str().map(|s| PlanChange::Add(one_line(s, 80))),
        "TaskUpdate" => {
            let number = input["taskId"].as_str().and_then(|id| id.parse::<usize>().ok());
            match input["status"].as_str() {
                Some("completed") => Some(PlanChange::Mark { done: number.into_iter().collect(), start: None }),
                Some("in_progress") => Some(PlanChange::Mark { done: vec![], start: number }),
                _ => None,
            }
        }
        "mcp__ccd_turn__update_plan" if input["items"].is_array() => Some(PlanChange::Replace(plan_items(&input["items"], &["content"]))),
        "mcp__ccd_turn__update_plan" if input["advance"] == true => Some(PlanChange::Advance),
        "mcp__ccd_turn__update_plan" => Some(PlanChange::Mark {
            done: input["done"].as_array().map(|d| d.iter().filter_map(|n| n.as_u64().map(|n| n as usize)).collect()).unwrap_or_default(),
            start: input["start"].as_u64().map(|n| n as usize),
        }),
        _ => return None,
    };
    Some(change.map(Event::Plan).into_iter().collect())
}

/// File names out of a Codex patch ("*** Update File: src/a.rs").
fn patched_files(patch: &str) -> Vec<Event> {
    patch
        .lines()
        .filter_map(|l| ["*** Update File: ", "*** Add File: ", "*** Delete File: "].iter().find_map(|p| l.strip_prefix(p)))
        .map(|path| Event::FileChanged(base(path.trim()).to_string()))
        .collect()
}

fn is_interrupt(text: &str) -> bool {
    text.starts_with("[Request interrupted")
}

/// True for user lines the app writes itself (slash command echoes and the like), which start no turn.
fn is_synthetic(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<command-") || t.starts_with("<local-command") || t.starts_with("<task-notification") || t.starts_with("<system-reminder")
}

fn words(text: Option<&str>) -> Option<Event> {
    let clean = latest_words(text?);
    (!clean.is_empty()).then_some(Event::Said(clean))
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
    if let Some(source) = o["entrypoint"].as_str() {
        out.push(Event::Source(source.to_string()));
    }
    let content = &o["message"]["content"];
    if kind == "user" {
        match content {
            Value::String(text) if is_interrupt(text) => out.push(Event::Interrupted),
            Value::String(text) if !is_synthetic(text) => {
                out.push(Event::Turn);
                out.push(Event::Prompt(prompt_line(text)));
            }
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
                    for p in parts.iter().filter(|p| p["type"] == "tool_result") {
                        if let Some(id) = p["tool_use_id"].as_str() {
                            out.push(Event::ToolResult { id: id.to_string(), failed: p["is_error"] == true });
                        }
                        if p["is_error"] == true {
                            out.extend(p["content"].as_str().map(|text| Event::ToolError(one_line(text, 120))));
                        }
                    }
                } else if typed || parts.iter().any(|p| p["type"] == "image") {
                    out.push(Event::Turn);
                    let first = parts.iter().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()).find(|t| !is_synthetic(t));
                    out.push(Event::Prompt(prompt_line(first.unwrap_or("Image"))));
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
        for p in parts {
            match p["type"].as_str().unwrap_or("") {
                "text" => out.extend(words(p["text"].as_str())),
                "thinking" => out.extend(words(p["thinking"].as_str())),
                "tool_use" => match p["name"].as_str().unwrap_or("") {
                    "AskUserQuestion" => {
                        out.extend(words(p["input"]["questions"][0]["question"].as_str()));
                        out.push(Event::ExternalWait("Waiting for your answer".to_string()));
                    }
                    "ExitPlanMode" => out.push(Event::ExternalWait("Plan ready for your review".to_string())),
                    name => {
                        if let Some(plan) = claude_plan(name, &p["input"]) {
                            out.extend(plan);
                            continue;
                        }
                        out.push(Event::Tool { label: tool_label(name, &p["input"]) });
                        if matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
                            let file = p["input"]["file_path"].as_str().or(p["input"]["notebook_path"].as_str());
                            out.extend(file.map(|f| Event::FileChanged(base(f).to_string())));
                        }
                        if p["input"]["command"].as_str().is_some_and(is_test_command) {
                            out.extend(p["id"].as_str().map(|id| Event::TestStarted(id.to_string())));
                        }
                    }
                },
                _ => {}
            }
        }
    }
    // Thinking arrives as its own line with the turn's stop reason; the answer itself is still to come.
    let only_thinking = content.as_array().is_some_and(|parts| !parts.is_empty() && parts.iter().all(|p| p["type"] == "thinking"));
    if !only_thinking && matches!(o["message"]["stop_reason"].as_str(), Some("end_turn" | "stop_sequence" | "max_tokens")) {
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
            let mut out: Vec<Event> = p["cwd"].as_str().map(|cwd| Event::Meta { cwd: cwd.to_string() }).into_iter().collect();
            out.extend(p["originator"].as_str().map(|source| Event::Source(source.to_string())));
            out
        }
        "event_msg" => match kind {
            "task_started" | "turn_started" => vec![Event::Turn],
            "task_complete" | "turn_complete" => vec![Event::Done],
            "turn_aborted" => vec![Event::Interrupted],
            "error" => vec![Event::Failed(one_line(p["message"].as_str().unwrap_or("Codex error"), 80))],
            "item_completed" => match p["item"]["type"].as_str().unwrap_or("") {
                "UserMessage" => {
                    let text = p["item"]["content"][0]["text"].as_str().unwrap_or("");
                    if text.is_empty() { vec![] } else { vec![Event::Prompt(prompt_line(text))] }
                }
                "CommandExecution" => {
                    let command = p["item"]["command"].as_array().map(|c| c.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default();
                    if is_test_command(&command) { p["item"]["exit_code"].as_i64().map(|code| Event::TestResult(code == 0)).into_iter().collect() } else { vec![] }
                }
                _ => vec![],
            },
            "exec_approval_request" | "apply_patch_approval_request" => {
                vec![Event::ExternalWait("Waiting for approval in Codex".to_string())]
            }
            _ => vec![],
        },
        "response_item" => match kind {
            "message" if p["role"] == "assistant" => {
                p["content"].as_array().map_or(vec![], |parts| parts.iter().filter_map(|c| words(c["text"].as_str())).collect())
            }
            "reasoning" => p["summary"].as_array().map_or(vec![], |parts| parts.iter().filter_map(|c| words(c["text"].as_str())).collect()),
            "function_call" if p["name"] == "update_plan" => {
                let args: Value = p["arguments"].as_str().and_then(|a| serde_json::from_str(a).ok()).unwrap_or(Value::Null);
                vec![Event::Plan(PlanChange::Replace(plan_items(&args["plan"], &["step"])))]
            }
            "custom_tool_call" | "function_call" if p["name"] == "apply_patch" => {
                let args: Value = p["arguments"].as_str().and_then(|a| serde_json::from_str(a).ok()).unwrap_or(Value::Null);
                let patch = p["input"].as_str().or(args["input"].as_str()).unwrap_or("");
                let mut out = vec![Event::Tool { label: "Editing files".to_string() }];
                out.extend(patched_files(patch));
                out
            }
            "custom_tool_call" if p["name"] == "exec" && p["input"].is_string() => {
                vec![Event::Tool { label: format!("Running {}", one_line(p["input"].as_str().unwrap_or(""), 60)) }]
            }
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
        "UserPromptSubmit" => {
            out.push(Event::Turn);
            out.extend(data["prompt"].as_str().map(|p| Event::Prompt(prompt_line(p))));
        }
        "PostToolUse" | "PostToolUseFailure" => out.push(Event::ToolDone),
        "Stop" => {
            out.extend(words(data["last_assistant_message"].as_str()));
            out.push(Event::Done);
        }
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
        assert_eq!(parse_claude(end), vec![Event::Said("ok".into()), Event::Done]);
        assert_eq!(parse_claude(r#"{"type":"user","message":{"content":"fix it"}}"#), vec![Event::Turn, Event::Prompt("fix it".into())]);
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
        assert_eq!(parse_codex(r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"npm test"}}"#), vec![Event::Tool { label: "Running npm test".into() }]);
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
    fn thinking_alone_does_not_end_a_turn() {
        let line = r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"thinking","thinking":"hmm"}]}}"#;
        assert_eq!(parse_claude(line), vec![Event::Said("hmm".into())]);
        let redacted = r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"thinking","thinking":""}]}}"#;
        assert_eq!(parse_claude(redacted), vec![]);
    }

    #[test]
    fn questions_and_plans_wait_for_you() {
        let ask = r#"{"type":"assistant","message":{"stop_reason":"tool_use","content":[{"type":"tool_use","name":"AskUserQuestion","input":{"questions":[{"question":"Blue or green?"}]}}]}}"#;
        assert_eq!(parse_claude(ask), vec![Event::Said("Blue or green?".into()), Event::ExternalWait("Waiting for your answer".into())]);
        let mut hub = Hub::default();
        hub.apply(Agent::Claude, "s", NOW, Event::Turn, NOW);
        hub.apply(Agent::Claude, "s", NOW, Event::Said("Which colour do you want?".into()), NOW);
        hub.apply(Agent::Claude, "s", NOW, Event::Done, NOW);
        assert_eq!(seen(&hub).status, Status::Waiting);
        assert_eq!(seen(&hub).status_text(), "Needs you");
        assert_eq!(hub.last_finished.as_ref().map(|f| f.2), Some(Status::Waiting));
    }

    #[test]
    fn steps_follow_the_work() {
        let mut hub = Hub::default();
        hub.apply(Agent::Codex, "s", NOW, Event::Turn, NOW);
        hub.apply(Agent::Codex, "s", NOW, Event::Tool { label: "a".into() }, NOW);
        hub.apply(Agent::Codex, "s", NOW, Event::ToolDone, NOW);
        hub.apply(Agent::Codex, "s", NOW, Event::Tool { label: "b".into() }, NOW);
        let steps = seen(&hub).steps;
        assert_eq!((steps[0].done, steps[1].done, steps[1].label.as_str()), (true, false, "b"));
        hub.apply(Agent::Codex, "s", NOW, Event::Turn, NOW);
        assert!(seen(&hub).steps.is_empty());
    }

    #[test]
    fn a_silent_session_says_so() {
        let mut hub = Hub::default();
        hub.apply(Agent::Claude, "s", NOW - 45_000, Event::Turn, NOW);
        assert!(seen(&hub).is_quiet());
        assert_eq!(seen(&hub).status_text(), "Quiet for 45s");
    }

    #[test]
    fn long_text_keeps_its_ending() {
        assert_eq!(latest_words("**Fixed** the `bug`"), "Fixed the bug");
        let long = format!("{}. The last sentence.", "word ".repeat(100));
        assert!(latest_words(&long).ends_with("The last sentence."));
        assert!(latest_words(&long).chars().count() <= 221);
    }

    fn run(hub: &mut Hub, events: Vec<Event>) {
        for e in events {
            hub.apply(Agent::Claude, "s", NOW, e, NOW);
        }
    }

    #[test]
    fn stuck_reasons_are_specific() {
        let mut hub = Hub::default();
        hub.apply(Agent::Claude, "s", NOW - 400_000, Event::Turn, NOW);
        hub.apply(Agent::Claude, "s", NOW - 360_000, Event::Tool { label: "Running npm test".into() }, NOW);
        assert_eq!(seen(&hub).status_text(), "Same command for 6m 00s");
        hub.apply(Agent::Claude, "s", NOW, Event::Tool { label: "Editing a.rs".into() }, NOW);
        assert_eq!(seen(&hub).status_text(), "Working");
        for _ in 0..3 {
            hub.apply(Agent::Claude, "s", NOW, Event::ToolError("exit 1".into()), NOW);
        }
        assert_eq!(seen(&hub).status_text(), "Same error 3 times");
        hub.apply(Agent::Claude, "s", NOW, Event::ToolError("different".into()), NOW);
        assert_eq!(seen(&hub).status_text(), "Working");
    }

    #[test]
    fn claude_errors_and_sources_are_read() {
        let line = r#"{"type":"user","entrypoint":"claude-desktop","message":{"content":[{"type":"tool_result","tool_use_id":"t","is_error":true,"content":"boom"}]}}"#;
        let events = parse_claude(line);
        assert!(events.contains(&Event::Source("claude-desktop".into())));
        assert!(events.contains(&Event::ToolError("boom".into())));
        let meta = parse_codex(r#"{"type":"session_meta","payload":{"cwd":"C:\\x","originator":"Codex Desktop"}}"#);
        assert!(meta.contains(&Event::Source("Codex Desktop".into())));
    }

    #[test]
    fn finished_runs_are_remembered_but_history_is_not() {
        let mut hub = Hub::default();
        hub.apply(Agent::Claude, "old", NOW - 60_000, Event::Done, NOW);
        assert!(hub.recent.is_empty());
        hub.apply(Agent::Codex, "s", NOW - 1000, Event::Turn, NOW);
        hub.apply(Agent::Codex, "s", NOW - 1000, Event::Prompt("Fix it".into()), NOW);
        hub.apply(Agent::Codex, "s", NOW, Event::FileChanged("a.rs".into()), NOW);
        hub.apply(Agent::Codex, "s", NOW, Event::Done, NOW);
        let run = &hub.recent[0];
        assert_eq!((run.prompt.as_str(), run.summary.as_str(), run.failed), ("Fix it", "Changed 1 file · took 1s", false));
        assert!(hub.recent_changed);
    }

    #[test]
    fn runs_last_a_few_hours() {
        let now = NOW + 10 * 3_600_000;
        let run = |age: u64| Run { agent: Agent::Claude, project: String::new(), prompt: String::new(), summary: String::new(), files: vec![], failed: false, at_ms: now - age, source: String::new() };
        assert!(run(2 * 3_600_000).is_recent(now));
        assert!(!run(4 * 3_600_000).is_recent(now));
        let mut hub = Hub::default();
        hub.recent = vec![run(4 * 3_600_000)];
        hub.apply(Agent::Codex, "s", now, Event::Turn, now);
        hub.apply(Agent::Codex, "s", now, Event::Done, now);
        assert_eq!(hub.recent.len(), 1);
        assert_eq!(hub.recent[0].at_ms, now);
    }

    #[test]
    fn only_working_or_waiting_counts_as_live() {
        let mut hub = Hub::default();
        hub.apply(Agent::Codex, "s", NOW, Event::Turn, NOW);
        assert!(any_live(&hub.view(NOW)));
        hub.apply(Agent::Codex, "s", NOW, Event::Done, NOW);
        assert!(!any_live(&hub.view(NOW)));
    }

    #[test]
    fn codex_gets_a_plain_allow() {
        let mut hub = Hub::default();
        let (log, responder) = answers();
        hub.hold(Agent::Codex, "s", "x".into(), Approval::default(), serde_json::json!({ "command": "ls" }), responder, NOW);
        assert!(hub.decide(Agent::Codex, "s", true));
        let body: Value = serde_json::from_str(log.lock().unwrap()[0].as_ref().unwrap()).unwrap();
        assert_eq!(body["hookSpecificOutput"]["decision"], serde_json::json!({ "behavior": "allow" }));
    }

    #[test]
    fn codex_lets_go_sooner() {
        let mut hub = Hub::default();
        let (log, responder) = answers();
        hub.hold(Agent::Codex, "s", "x".into(), Approval::default(), Value::Null, responder, NOW);
        assert!(hub.expire(NOW + CODEX_HOLD_MS + 1));
        assert_eq!(*log.lock().unwrap(), vec![None]);
    }

    #[test]
    fn prompts_lose_reply_markers() {
        assert_eq!(prompt_line("<!-- reply 5 -->\n> quoted\n\nthis is good"), "quoted this is good");
        assert_eq!(prompt_line("plain"), "plain");
        assert_eq!(prompt_line("<!-- unfinished"), "");
    }

    #[test]
    fn the_desktop_plan_becomes_progress() {
        let mut hub = Hub::default();
        run(&mut hub, vec![Event::Turn]);
        let replace = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"mcp__ccd_turn__update_plan","input":{"items":[{"content":"One","status":"in_progress"},{"content":"Two","status":"pending"},{"content":"Three","status":"pending"}]}}]}}"#;
        run(&mut hub, parse_claude(replace));
        assert_eq!(seen(&hub).progress_line().as_deref(), Some("Step 1 of 3: One"));
        let advance = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"mcp__ccd_turn__update_plan","input":{"advance":true}}]}}"#;
        run(&mut hub, parse_claude(advance));
        assert_eq!(seen(&hub).progress_line().as_deref(), Some("Step 2 of 3: Two"));
        let mark = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"mcp__ccd_turn__update_plan","input":{"done":[2],"start":3}}]}}"#;
        run(&mut hub, parse_claude(mark));
        assert_eq!(seen(&hub).progress_line().as_deref(), Some("Step 3 of 3: Three"));
        assert!((seen(&hub).progress().unwrap() - 2.0 / 3.0).abs() < 0.01);
        assert!(seen(&hub).steps.is_empty()); // plan updates are not work steps
    }

    #[test]
    fn claude_tasks_and_todos_become_progress() {
        let mut hub = Hub::default();
        run(&mut hub, vec![Event::Turn]);
        for name in ["A", "B"] {
            let line = format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"TaskCreate","input":{{"subject":"{name}"}}}}]}}}}"#);
            run(&mut hub, parse_claude(&line));
        }
        run(&mut hub, parse_claude(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TaskUpdate","input":{"taskId":"1","status":"completed"}}]}}"#));
        run(&mut hub, parse_claude(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TaskUpdate","input":{"taskId":"2","status":"in_progress"}}]}}"#));
        assert_eq!(seen(&hub).progress_line().as_deref(), Some("Step 2 of 2: B"));
        run(&mut hub, parse_claude(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"X","status":"completed"}]}}]}}"#));
        assert_eq!(seen(&hub).progress_line().as_deref(), Some("All 1 steps done"));
    }

    #[test]
    fn codex_plans_patches_and_tests() {
        let plan = parse_codex(r#"{"type":"response_item","payload":{"type":"function_call","name":"update_plan","arguments":"{\"plan\":[{\"step\":\"Fix\",\"status\":\"in_progress\"}]}"}}"#);
        assert_eq!(plan, vec![Event::Plan(PlanChange::Replace(vec![PlanItem { text: "Fix".into(), state: PlanState::Active }]))]);
        let patch = parse_codex(r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch\n*** Update File: src/a.rs\n*** End Patch"}}"#);
        assert_eq!(patch[1], Event::FileChanged("a.rs".into()));
        let test = parse_codex(r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","command":["cargo","test"],"exit_code":1}}}"#);
        assert_eq!(test, vec![Event::TestResult(false)]);
    }

    #[test]
    fn a_finished_run_sums_itself_up() {
        let mut hub = Hub::default();
        hub.apply(Agent::Claude, "s", NOW - 362_000, Event::Turn, NOW);
        let edit = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"C:\\dev\\a.rs"}},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"npm test"}}]}}"#;
        run(&mut hub, parse_claude(edit));
        run(&mut hub, parse_claude(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":false,"content":"ok"}]}}"#));
        run(&mut hub, vec![Event::Done]);
        assert_eq!(seen(&hub).summary(), "Changed 1 file · tests passed · took 6m 02s");
    }

    #[test]
    fn the_pill_follows_the_agents() {
        let mut hub = Hub::default();
        assert_eq!(pill_state(&hub.view(NOW)), Pill::Quiet);
        hub.apply(Agent::Claude, "a", NOW, Event::Turn, NOW);
        assert_eq!(pill_state(&hub.view(NOW)), Pill::Working);
        hub.apply(Agent::Claude, "a", NOW, Event::Done, NOW);
        assert_eq!(pill_state(&hub.view(NOW)), Pill::Finished);
        hub.acknowledge();
        assert_eq!(pill_state(&hub.view(NOW)), Pill::Quiet);
        hub.apply(Agent::Codex, "b", NOW, Event::Failed("x".into()), NOW);
        hub.apply(Agent::Claude, "c", NOW, Event::Turn, NOW);
        assert_eq!(pill_state(&hub.view(NOW)), Pill::Failed); // a failure outranks other work
        hub.acknowledge();
        assert_eq!(pill_state(&hub.view(NOW)), Pill::Working);
    }

    #[test]
    fn viewed_results_leave_the_list_and_history_stays_quiet() {
        let mut hub = Hub::default();
        hub.apply(Agent::Claude, "old", NOW - 60_000, Event::Failed("x".into()), NOW);
        assert!(hub.view(NOW).is_empty());
        hub.apply(Agent::Claude, "a", NOW, Event::Done, NOW);
        assert_eq!(hub.view(NOW).len(), 1);
        hub.mark_viewed();
        assert!(hub.view(NOW).is_empty());
        hub.apply(Agent::Claude, "a", NOW + 1, Event::Turn, NOW + 1);
        assert_eq!(hub.view(NOW + 1).len(), 1);
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
        hub.hold(Agent::Claude, "s", "Running npm i".into(), Approval::default(), serde_json::json!({ "command": "npm i" }), responder, NOW);
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
        hub.hold(Agent::Claude, "s", "x".into(), Approval::default(), Value::Null, responder, NOW);
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
        hub.hold(Agent::Claude, "s", "x".into(), Approval::default(), Value::Null, first, NOW);
        let (_, second) = answers();
        hub.hold(Agent::Claude, "s", "y".into(), Approval::default(), Value::Null, second, NOW + 5);
        assert!(!hub.hung_up(Agent::Claude, "s", NOW)); // the old request's connection closing changes nothing
        assert!(seen(&hub).can_decide);
        assert!(hub.hung_up(Agent::Claude, "s", NOW + 5));
        assert!(!seen(&hub).can_decide);
    }

    #[test]
    fn a_forgotten_request_times_out() {
        let mut hub = Hub::default();
        let (log, responder) = answers();
        hub.hold(Agent::Claude, "s", "x".into(), Approval::default(), Value::Null, responder, NOW);
        assert!(hub.expire(NOW + HOLD_MS + 1));
        assert_eq!(*log.lock().unwrap(), vec![None]);
    }
}
