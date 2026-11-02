//! Your upcoming Google Calendar events, read-only. Sign-in happens in your browser with PKCE; the app only ever
//! talks to Google, and the sign-in it keeps is encrypted for your Windows account with DPAPI.
use crate::lyrics::{encode, read_response};
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{LocalFree, HLOCAL, SYSTEMTIME};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::*;
use windows::Win32::System::Time::{SystemTimeToTzSpecificLocalTime, TzSpecificLocalTimeToSystemTime};

pub const CONSOLE_URL: &str = "https://console.cloud.google.com/apis/credentials";
const SCOPE: &str = "https://www.googleapis.com/auth/calendar.readonly";
const REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);
const SOONEST_REFRESH: Duration = Duration::from_secs(60);
const SIGN_IN_WAIT: Duration = Duration::from_secs(180);
const LOOK_AHEAD_MS: u64 = 7 * 24 * 3_600_000;
const MAX_EVENTS: usize = 20;
const DAY_MS: i64 = 86_400_000;

#[derive(Clone, PartialEq, Debug, Default)]
pub enum Phase {
    /// No Google client file yet.
    #[default]
    NeedsSetup,
    SignedOut(String),
    Connecting,
    Ready,
    Failed(String),
}

#[derive(Clone, PartialEq, Debug)]
pub struct Event {
    pub title: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub all_day: bool,
    pub join: Option<String>,
    pub link: String,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct State {
    pub phase: Phase,
    pub events: Vec<Event>,
}

pub enum Command {
    Refresh,
    Connect,
    Disconnect,
}

struct Client {
    id: String,
    secret: String,
}

fn client_file() -> PathBuf {
    crate::config::dir().join("google-client.json")
}

fn token_file() -> PathBuf {
    crate::config::dir().join("google-token.bin")
}

pub fn connected() -> bool {
    token_file().exists()
}

/// Google's "Desktop app" download.
fn parse_client(text: &str) -> Option<Client> {
    let data: Value = serde_json::from_str(text).ok()?;
    let installed = &data["installed"];
    Some(Client { id: installed["client_id"].as_str()?.to_string(), secret: installed["client_secret"].as_str().unwrap_or("").to_string() })
}

fn load_client() -> Option<Client> {
    parse_client(&fs::read_to_string(client_file()).ok()?)
}

pub fn start(state: Arc<Mutex<State>>, on_change: impl Fn() + Send + 'static) -> Sender<Command> {
    let (tx, rx) = channel::<Command>();
    std::thread::spawn(move || {
        let mut session = Session { state, on_change: Box::new(on_change), access: None, last_fetch: None };
        session.refresh(true);
        loop {
            match rx.recv_timeout(REFRESH_EVERY) {
                Ok(Command::Refresh) => session.refresh(false),
                Ok(Command::Connect) => session.connect(),
                Ok(Command::Disconnect) => session.disconnect(),
                Err(RecvTimeoutError::Timeout) => session.refresh(true),
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });
    tx
}

struct Session {
    state: Arc<Mutex<State>>,
    on_change: Box<dyn Fn() + Send>,
    access: Option<(String, Instant)>,
    last_fetch: Option<Instant>,
}

impl Session {
    fn set(&self, phase: Phase, events: Option<Vec<Event>>) {
        if let Ok(mut state) = self.state.lock() {
            state.phase = phase;
            if let Some(events) = events {
                state.events = events;
            }
        }
        (self.on_change)();
    }

    fn refresh(&mut self, force: bool) {
        let Some(client) = load_client() else {
            self.set(Phase::NeedsSetup, Some(vec![]));
            return;
        };
        let Some(refresh_token) = load_token() else {
            self.set(Phase::SignedOut(String::new()), Some(vec![]));
            return;
        };
        if !force && self.last_fetch.is_some_and(|at| at.elapsed() < SOONEST_REFRESH) {
            return;
        }
        self.last_fetch = Some(Instant::now());
        let access = match self.access_token(&client, &refresh_token) {
            Ok(access) => access,
            Err(Failure::Rejected) => {
                // Revoked, or the 7-day limit on Google projects still in testing.
                let _ = fs::remove_file(token_file());
                self.access = None;
                self.set(Phase::SignedOut("Google ended the sign-in. Connect again.".to_string()), Some(vec![]));
                return;
            }
            Err(Failure::Offline) => return self.offline(),
        };
        match fetch_events(&access) {
            Some(events) => self.set(Phase::Ready, Some(events)),
            None => self.offline(),
        }
    }

    /// Keep what is on screen if there is anything; old events simply pass.
    fn offline(&self) {
        let has_events = self.state.lock().is_ok_and(|s| !s.events.is_empty());
        if has_events {
            self.set(Phase::Ready, None);
        } else {
            self.set(Phase::Failed("Could not reach Google".to_string()), None);
        }
    }

    fn access_token(&mut self, client: &Client, refresh_token: &str) -> Result<String, Failure> {
        if let Some((token, until)) = &self.access {
            if Instant::now() < *until {
                return Ok(token.clone());
            }
        }
        let body = form(&[("client_id", &client.id), ("client_secret", &client.secret), ("refresh_token", refresh_token), ("grant_type", "refresh_token")]);
        let (status, text) = post_token(&body).ok_or(Failure::Offline)?;
        let data: Value = serde_json::from_str(&text).map_err(|_| Failure::Offline)?;
        if status == 400 || status == 401 {
            return Err(Failure::Rejected);
        }
        let token = data["access_token"].as_str().ok_or(Failure::Offline)?.to_string();
        let life = data["expires_in"].as_u64().unwrap_or(3600).saturating_sub(60);
        self.access = Some((token.clone(), Instant::now() + Duration::from_secs(life)));
        Ok(token)
    }

    fn connect(&mut self) {
        let Some(client) = load_client() else {
            self.set(Phase::NeedsSetup, None);
            return;
        };
        if load_token().is_some() {
            return self.refresh(true);
        }
        self.set(Phase::Connecting, None);
        match sign_in(&client) {
            Ok(refresh_token) => {
                if !save_token(&refresh_token) {
                    self.set(Phase::SignedOut("Could not save the sign-in".to_string()), None);
                    return;
                }
                self.access = None;
                self.refresh(true);
            }
            Err(why) => self.set(Phase::SignedOut(why), None),
        }
    }

    /// Forgets the sign-in here and asks Google to cancel it too.
    fn disconnect(&mut self) {
        if let Some(token) = load_token() {
            let body = form(&[("token", &token)]);
            let _ = request(w!("oauth2.googleapis.com"), w!("POST"), "/revoke", "Content-Type: application/x-www-form-urlencoded", Some(&body));
        }
        let _ = fs::remove_file(token_file());
        self.access = None;
        self.last_fetch = None;
        self.refresh(true);
    }
}

enum Failure {
    Offline,
    Rejected,
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect::<Vec<_>>().join("&")
}

fn post_token(body: &str) -> Option<(u32, String)> {
    request(w!("oauth2.googleapis.com"), w!("POST"), "/token", "Content-Type: application/x-www-form-urlencoded", Some(body))
}

fn request(host: PCWSTR, method: PCWSTR, path: &str, headers: &str, body: Option<&str>) -> Option<(u32, String)> {
    unsafe {
        let session = WinHttpOpen(w!("TwoCode"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() {
            return None;
        }
        let connection = WinHttpConnect(session, host, 443, 0);
        let wide = HSTRING::from(path);
        let handle = if connection.is_null() {
            std::ptr::null_mut()
        } else {
            WinHttpOpenRequest(connection, method, &wide, PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE)
        };
        let answer = if handle.is_null() { None } else { send(handle, headers, body) };
        for h in [handle, connection, session] {
            if !h.is_null() {
                let _ = WinHttpCloseHandle(h);
            }
        }
        answer
    }
}

unsafe fn send(handle: *mut core::ffi::c_void, headers: &str, body: Option<&str>) -> Option<(u32, String)> {
    let wide: Vec<u16> = headers.encode_utf16().collect();
    let bytes = body.map_or(&[][..], str::as_bytes);
    let optional = (!bytes.is_empty()).then_some(bytes.as_ptr() as *const core::ffi::c_void);
    let headers_arg = (!wide.is_empty()).then_some(wide.as_slice());
    WinHttpSendRequest(handle, headers_arg, optional, bytes.len() as u32, bytes.len() as u32, 0).ok()?;
    WinHttpReceiveResponse(handle, std::ptr::null_mut()).ok()?;
    let mut status = 0u32;
    let mut size = 4u32;
    WinHttpQueryHeaders(handle, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut status as *mut u32 as *mut _), &mut size, std::ptr::null_mut()).ok()?;
    Some((status, read_body(handle)?))
}

unsafe fn read_body(handle: *mut core::ffi::c_void) -> Option<String> {
    read_response(handle, false)
}

// ---- sign-in ----

fn base64_url(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..=chunk.len() {
            out.push(ABC[(n >> (18 - i * 6) & 63) as usize] as char);
        }
    }
    out
}

fn sha256(input: &[u8]) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, input, &mut out) }.is_ok().then_some(out)
}

fn random_text() -> Option<String> {
    let mut bytes = [0u8; 32];
    unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }.is_ok().then(|| base64_url(&bytes))
}

fn auth_url(client_id: &str, redirect: &str, challenge: &str, state: &str) -> String {
    format!(
        "https://accounts.google.com/o/oauth2/v2/auth?{}",
        form(&[("client_id", client_id), ("redirect_uri", redirect), ("response_type", "code"), ("scope", SCOPE), ("code_challenge", challenge), ("code_challenge_method", "S256"), ("state", state), ("access_type", "offline"), ("prompt", "consent")])
    )
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%').then(|| bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok())).flatten();
        if let Some(value) = hex {
            out.push(value);
            i += 3;
        } else {
            out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Reads the browser's "GET /?code=...&state=... HTTP/1.1" line. Only an answer carrying our state counts.
fn answer_from(request_line: &str, state: &str) -> Option<Result<String, String>> {
    let target = request_line.strip_prefix("GET /")?.split(' ').next()?;
    let query = target.strip_prefix('?')?;
    let value = |name: &str| query.split('&').find_map(|pair| pair.strip_prefix(name)?.strip_prefix('=')).map(decode);
    if value("state")? != state {
        return None;
    }
    match value("code") {
        Some(code) => Some(Ok(code)),
        None => Some(Err(if value("error").as_deref() == Some("access_denied") { "Sign-in was cancelled".to_string() } else { "Google refused the sign-in".to_string() })),
    }
}

fn wait_for_browser(listener: &TcpListener, state: &str) -> Result<String, String> {
    listener.set_nonblocking(true).map_err(|_| "Could not start sign-in".to_string())?;
    let until = Instant::now() + SIGN_IN_WAIT;
    while Instant::now() < until {
        let Ok((mut stream, _)) = listener.accept() else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buffer = [0u8; 8192];
        let n = stream.read(&mut buffer).unwrap_or(0);
        let text = String::from_utf8_lossy(&buffer[..n]).into_owned();
        let answer = answer_from(text.lines().next().unwrap_or(""), state);
        let page = if answer.is_some() { "You can close this tab and go back to TwoCode." } else { "Not found" };
        let status = if answer.is_some() { "200 OK" } else { "404 Not Found" };
        let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len());
        if let Some(answer) = answer {
            return answer;
        }
    }
    Err("Sign-in timed out".to_string())
}

fn sign_in(client: &Client) -> Result<String, String> {
    let failed = || "Could not start sign-in".to_string();
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|_| failed())?;
    let port = listener.local_addr().map_err(|_| failed())?.port();
    let redirect = format!("http://127.0.0.1:{port}");
    let verifier = random_text().ok_or_else(failed)?;
    let state = random_text().ok_or_else(failed)?;
    let challenge = base64_url(&sha256(verifier.as_bytes()).ok_or_else(failed)?);
    crate::shell::open(&auth_url(&client.id, &redirect, &challenge, &state));
    let code = wait_for_browser(&listener, &state)?;
    let body = form(&[("client_id", &client.id), ("client_secret", &client.secret), ("code", &code), ("code_verifier", &verifier), ("redirect_uri", &redirect), ("grant_type", "authorization_code")]);
    let (_, text) = post_token(&body).ok_or("Could not reach Google")?;
    let data: Value = serde_json::from_str(&text).map_err(|_| "Google sent an unexpected answer".to_string())?;
    data["refresh_token"].as_str().map(str::to_string).ok_or_else(|| "Google did not allow the sign-in".to_string())
}

fn save_token(token: &str) -> bool {
    let _ = fs::create_dir_all(crate::config::dir());
    let mut bytes = token.as_bytes().to_vec();
    let input = CRYPT_INTEGER_BLOB { cbData: bytes.len() as u32, pbData: bytes.as_mut_ptr() };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        if CryptProtectData(&input, PCWSTR::null(), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output).is_err() {
            return false;
        }
        let saved = fs::write(token_file(), std::slice::from_raw_parts(output.pbData, output.cbData as usize)).is_ok();
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
        saved
    }
}

fn load_token() -> Option<String> {
    let mut bytes = fs::read(token_file()).ok()?;
    let input = CRYPT_INTEGER_BLOB { cbData: bytes.len() as u32, pbData: bytes.as_mut_ptr() };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output).ok()?;
        let text = String::from_utf8(std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec()).ok();
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
        text
    }
}

// ---- events ----

fn fetch_events(access: &str) -> Option<Vec<Event>> {
    let headers = format!("Authorization: Bearer {access}");
    let get = |path: &str| -> Option<Value> {
        let (status, text) = request(w!("www.googleapis.com"), w!("GET"), path, &headers, None)?;
        if status != 200 {
            return None;
        }
        serde_json::from_str(&text).ok()
    };
    let calendars = get("/calendar/v3/users/me/calendarList?minAccessRole=reader&fields=items(id,selected,primary)")?;
    let now = crate::agents::now_ms();
    let (from, to) = (rfc3339(now), rfc3339(now + LOOK_AHEAD_MS));
    let mut events = vec![];
    for calendar in calendars["items"].as_array()? {
        if calendar["selected"] != Value::Bool(true) && calendar["primary"] != Value::Bool(true) {
            continue;
        }
        let Some(id) = calendar["id"].as_str() else { continue };
        let path = format!(
            "/calendar/v3/calendars/{}/events?singleEvents=true&orderBy=startTime&maxResults=20&timeMin={}&timeMax={}&fields=items(summary,status,htmlLink,hangoutLink,start,end,attendees(self,responseStatus),conferenceData(entryPoints(entryPointType,uri)))",
            encode(id),
            encode(&from),
            encode(&to)
        );
        // One calendar that cannot be read should not hide the others.
        if let Some(found) = get(&path) {
            events.extend(found["items"].as_array().into_iter().flatten().filter_map(parse_event));
        }
    }
    events.sort_by_key(|e| (e.start_ms, e.end_ms));
    events.dedup_by(|a, b| a.title == b.title && a.start_ms == b.start_ms);
    events.truncate(MAX_EVENTS);
    Some(events)
}

fn https(url: Option<&str>) -> Option<String> {
    url.filter(|u| u.starts_with("https://")).map(str::to_string)
}

fn parse_event(item: &Value) -> Option<Event> {
    if item["status"] == "cancelled" {
        return None;
    }
    let declined = item["attendees"].as_array().is_some_and(|list| list.iter().any(|a| a["self"] == Value::Bool(true) && a["responseStatus"] == "declined"));
    if declined {
        return None;
    }
    let all_day = item["start"]["dateTime"].is_null();
    let time = |side: &Value| -> Option<u64> {
        match side["dateTime"].as_str() {
            Some(text) => parse_rfc3339(text),
            None => local_midnight(side["date"].as_str()?),
        }
    };
    let video = item["conferenceData"]["entryPoints"].as_array().and_then(|list| list.iter().find(|p| p["entryPointType"] == "video")).and_then(|p| p["uri"].as_str());
    Some(Event {
        title: item["summary"].as_str().unwrap_or("(No title)").to_string(),
        start_ms: time(&item["start"])?,
        end_ms: time(&item["end"])?,
        all_day,
        join: https(item["hangoutLink"].as_str()).or_else(|| https(video)),
        link: https(item["htmlLink"].as_str()).unwrap_or_default(),
    })
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

fn rfc3339(ms: u64) -> String {
    let (days, rest) = ((ms as i64).div_euclid(DAY_MS), (ms as i64).rem_euclid(DAY_MS) / 1000);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest / 60 % 60, rest % 60)
}

/// "2026-11-01T15:00:00-05:00" to milliseconds since 1970.
fn parse_rfc3339(text: &str) -> Option<u64> {
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let days = days_from_civil(number(0..4)?, number(5..7)?, number(8..10)?);
    let seconds = number(11..13)? * 3600 + number(14..16)? * 60 + number(17..19)?;
    let zone = text.get(19..)?;
    let at = zone.find(['Z', 'z', '+', '-'])?;
    let offset = match &zone[at..] {
        "Z" | "z" => 0,
        tail => {
            let sign = if tail.starts_with('-') { -1 } else { 1 };
            sign * (tail.get(1..3)?.parse::<i64>().ok()? * 3600 + tail.get(4..6)?.parse::<i64>().ok()? * 60)
        }
    };
    u64::try_from(days * DAY_MS + (seconds - offset) * 1000).ok()
}

fn to_system_time(ms: u64) -> SYSTEMTIME {
    let (days, rest) = ((ms as i64).div_euclid(DAY_MS), (ms as i64).rem_euclid(DAY_MS) / 1000);
    let (y, m, d) = civil_from_days(days);
    SYSTEMTIME { wYear: y as u16, wMonth: m as u16, wDay: d as u16, wHour: (rest / 3600) as u16, wMinute: (rest / 60 % 60) as u16, wSecond: (rest % 60) as u16, ..Default::default() }
}

/// An all-day event starts at midnight wherever you are.
fn local_midnight(date: &str) -> Option<u64> {
    let number = |range: std::ops::Range<usize>| date.get(range)?.parse::<u16>().ok();
    let local = SYSTEMTIME { wYear: number(0..4)?, wMonth: number(5..7)?, wDay: number(8..10)?, ..Default::default() };
    let mut utc = SYSTEMTIME::default();
    unsafe { TzSpecificLocalTimeToSystemTime(None, &local, &mut utc) }.ok()?;
    let days = days_from_civil(utc.wYear as i64, utc.wMonth as i64, utc.wDay as i64);
    u64::try_from(days * DAY_MS + (utc.wHour as i64 * 3600 + utc.wMinute as i64 * 60) * 1000).ok()
}

/// (day number, hour, minute) on this PC's clock.
fn local_parts(ms: u64) -> Option<(i64, u16, u16)> {
    let mut local = SYSTEMTIME::default();
    unsafe { SystemTimeToTzSpecificLocalTime(None, &to_system_time(ms), &mut local) }.ok()?;
    Some((days_from_civil(local.wYear as i64, local.wMonth as i64, local.wDay as i64), local.wHour, local.wMinute))
}

fn clock(hour: u16, minute: u16) -> String {
    let shown = if hour % 12 == 0 { 12 } else { hour % 12 };
    format!("{shown}:{minute:02} {}", if hour < 12 { "AM" } else { "PM" })
}

fn day_name(day: i64, today: i64) -> String {
    match day - today {
        0 => "Today".to_string(),
        1 => "Tomorrow".to_string(),
        _ => ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][day.rem_euclid(7) as usize].to_string(),
    }
}

fn countdown(ms: u64) -> String {
    let minutes = ms.div_ceil(60_000);
    if minutes < 60 {
        format!("in {minutes}m")
    } else {
        format!("in {}h", minutes.div_ceil(60))
    }
}

/// "Now · until 4:00 PM", "Today 3:00 PM · in 20m", "Tomorrow · all day".
pub fn when(event: &Event, now: u64) -> String {
    let (Some((start_day, h, m)), Some((today, _, _)), Some((_, end_h, end_m))) = (local_parts(event.start_ms), local_parts(now), local_parts(event.end_ms)) else { return String::new() };
    if event.all_day {
        let day = if event.start_ms <= now { today } else { start_day };
        return format!("{} · all day", day_name(day, today));
    }
    if event.start_ms <= now {
        return format!("Now · until {}", clock(end_h, end_m));
    }
    let start = format!("{} {}", day_name(start_day, today), clock(h, m));
    if start_day == today {
        format!("{start} · {}", countdown(event.start_ms - now))
    } else {
        start
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_google_times() {
        assert_eq!(parse_rfc3339("1970-01-02T00:00:00Z"), Some(86_400_000));
        assert_eq!(parse_rfc3339("2026-11-01T15:00:00-05:00"), parse_rfc3339("2026-11-01T20:00:00Z"));
        assert_eq!(parse_rfc3339("2026-11-01T15:00:00.000+05:30"), parse_rfc3339("2026-11-01T09:30:00Z"));
        assert_eq!(parse_rfc3339("nonsense"), None);
    }

    #[test]
    fn writes_google_times() {
        let ms = parse_rfc3339("2028-02-29T23:59:58Z").unwrap();
        assert_eq!(rfc3339(ms), "2028-02-29T23:59:58Z");
    }

    #[test]
    fn dates_round_trip() {
        for days in [-1, 0, 59, 20_000, 22_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn pkce_matches_the_published_example() {
        // RFC 7636, appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(base64_url(&sha256(verifier.as_bytes()).unwrap()), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn the_sign_in_link_asks_only_to_read() {
        let url = auth_url("id", "http://127.0.0.1:5000", "c", "s");
        assert!(url.starts_with("https://accounts.google.com/"));
        assert!(url.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcalendar.readonly"));
        assert!(url.contains("code_challenge_method=S256"));
    }

    #[test]
    fn only_our_own_answer_is_accepted() {
        assert_eq!(answer_from("GET /?state=abc&code=4%2F0x HTTP/1.1", "abc"), Some(Ok("4/0x".to_string())));
        assert_eq!(answer_from("GET /?state=evil&code=x HTTP/1.1", "abc"), None);
        assert_eq!(answer_from("GET /favicon.ico HTTP/1.1", "abc"), None);
        assert_eq!(answer_from("GET /?state=abc&error=access_denied HTTP/1.1", "abc"), Some(Err("Sign-in was cancelled".to_string())));
    }

    #[test]
    fn reads_the_desktop_client_file_only() {
        let client = parse_client(r#"{"installed":{"client_id":"a","client_secret":"b"}}"#).unwrap();
        assert_eq!((client.id.as_str(), client.secret.as_str()), ("a", "b"));
        assert!(parse_client(r#"{"web":{"client_id":"a"}}"#).is_none());
    }

    #[test]
    fn events_skip_cancelled_and_declined_and_only_link_to_https() {
        let timed = json!({ "dateTime": "2026-11-01T15:00:00Z" });
        let event = parse_event(&json!({ "summary": "Sync", "start": timed, "end": timed, "hangoutLink": "https://meet.google.com/x", "htmlLink": "javascript:alert(1)" })).unwrap();
        assert_eq!(event.join.as_deref(), Some("https://meet.google.com/x"));
        assert_eq!(event.link, "");
        assert!(!event.all_day);
        assert!(parse_event(&json!({ "status": "cancelled", "start": timed, "end": timed })).is_none());
        assert!(parse_event(&json!({ "start": timed, "end": timed, "attendees": [{ "self": true, "responseStatus": "declined" }] })).is_none());
        let video = parse_event(&json!({ "start": timed, "end": timed, "conferenceData": { "entryPoints": [{ "entryPointType": "phone", "uri": "tel:1" }, { "entryPointType": "video", "uri": "https://zoom.us/j/1" }] } })).unwrap();
        assert_eq!((video.title.as_str(), video.join.as_deref()), ("(No title)", Some("https://zoom.us/j/1")));
        let day = parse_event(&json!({ "start": { "date": "2026-11-01" }, "end": { "date": "2026-11-02" } })).unwrap();
        assert!(day.all_day && day.end_ms > day.start_ms);
    }

    #[test]
    fn readable_labels() {
        assert_eq!((clock(0, 5), clock(12, 0), clock(15, 30)), ("12:05 AM".to_string(), "12:00 PM".to_string(), "3:30 PM".to_string()));
        assert_eq!((countdown(1), countdown(20 * 60_000), countdown(61 * 60_000)), ("in 1m".to_string(), "in 20m".to_string(), "in 2h".to_string()));
        let today = days_from_civil(2026, 11, 1); // a Sunday
        assert_eq!((day_name(today, today).as_str(), day_name(today + 1, today).as_str(), day_name(today + 2, today).as_str()), ("Today", "Tomorrow", "Tue"));
    }
}
