//! Synced lyrics from LRCLIB (lrclib.net). Only asked while the media page is open with lyrics on,
//! and it receives just the song's title, artist, album and length.
use serde_json::Value;
use std::collections::HashMap;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::*;

const MAX_BODY: usize = 4 * 1024 * 1024;
const NEAR_ENOUGH_S: f64 = 5.0;

#[derive(Clone, PartialEq, Debug)]
pub struct Line {
    pub at_ms: u64,
    pub text: String,
}

#[derive(Default)]
pub struct Store {
    pub key: String,
    pub lines: Arc<Vec<Line>>,
}

pub struct Request {
    pub key: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
}

pub fn start(store: Arc<Mutex<Store>>, on_change: impl Fn() + Send + 'static) -> Sender<Request> {
    let (tx, rx) = channel::<Request>();
    std::thread::spawn(move || {
        let mut found: HashMap<String, Arc<Vec<Line>>> = HashMap::new();
        while let Ok(first) = rx.recv() {
            // Skipping through songs: only the last one still matters.
            let request = rx.try_iter().last().unwrap_or(first);
            let lines = match found.get(&request.key) {
                Some(lines) => Some(lines.clone()),
                None => fetch(&request).map(Arc::new),
            };
            let Some(lines) = lines else { continue }; // offline: the next song tries again
            found.insert(request.key.clone(), lines.clone());
            if let Ok(mut store) = store.lock() {
                *store = Store { key: request.key, lines };
            }
            on_change();
        }
    });
    tx
}

/// `None` means the question could not be asked. An empty list means there are no synced lyrics.
fn fetch(r: &Request) -> Option<Vec<Line>> {
    let title = clean_title(&r.title);
    let artist = r.artist.trim_end_matches(" - Topic").trim().to_string();
    let duration_s = r.duration_ms as f64 / 1000.0;
    if !artist.is_empty() && !r.album.is_empty() && r.duration_ms > 0 {
        let path = format!(
            "/api/get?track_name={}&artist_name={}&album_name={}&duration={}",
            encode(&r.title),
            encode(&artist),
            encode(&r.album),
            duration_s.round() as u64
        );
        if let Some(text) = get(&path).and_then(|body| serde_json::from_str::<Value>(&body).ok()) {
            if let Some(synced) = text["syncedLyrics"].as_str() {
                return Some(parse_lrc(synced));
            }
        }
    }
    let path = if artist.is_empty() {
        format!("/api/search?q={}", encode(&title))
    } else {
        format!("/api/search?track_name={}&artist_name={}", encode(&title), encode(&artist))
    };
    let body = get(&path)?;
    let results: Value = serde_json::from_str(&body).ok()?;
    Some(best_match(&results, duration_s).map(parse_lrc).unwrap_or_default())
}

/// The synced result closest in length, so a live version does not get the album's timing.
fn best_match(results: &Value, duration_s: f64) -> Option<&str> {
    let list = results.as_array()?;
    let gap = |item: &Value| item["duration"].as_f64().map_or(0.0, |d| if duration_s > 0.0 { (d - duration_s).abs() } else { 0.0 });
    list.iter()
        .filter(|item| item["syncedLyrics"].is_string() && gap(item) <= NEAR_ENOUGH_S)
        .min_by(|a, b| gap(a).total_cmp(&gap(b)))
        .and_then(|item| item["syncedLyrics"].as_str())
}

/// "Song (Official Video) [4K]" to "Song", for players that report a video's title.
pub fn clean_title(title: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in title.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let clean = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.is_empty() {
        title.to_string()
    } else {
        clean
    }
}

/// "[01:23.45][02:00.00]words" lines into a timed list.
pub fn parse_lrc(text: &str) -> Vec<Line> {
    let mut lines = vec![];
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut stamps = vec![];
        while let Some(body) = rest.strip_prefix('[') {
            let Some((stamp, after)) = body.split_once(']') else { break };
            let Some(ms) = parse_stamp(stamp) else { break };
            stamps.push(ms);
            rest = after;
        }
        let words = rest.trim();
        if !words.is_empty() {
            lines.extend(stamps.into_iter().map(|at_ms| Line { at_ms, text: words.to_string() }));
        }
    }
    lines.sort_by_key(|l| l.at_ms);
    lines
}

fn parse_stamp(stamp: &str) -> Option<u64> {
    let (minutes, seconds) = stamp.split_once(':')?;
    let minutes: u64 = minutes.parse().ok()?;
    let seconds: f64 = seconds.parse().ok()?;
    Some(minutes * 60_000 + (seconds * 1000.0).round() as u64)
}

/// The line being sung at this moment. `None` before the first one.
pub fn current_line(lines: &[Line], position_ms: u64) -> Option<usize> {
    lines.partition_point(|l| l.at_ms <= position_ms).checked_sub(1)
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn get(path: &str) -> Option<String> {
    unsafe {
        let session = WinHttpOpen(w!("TwoCode (https://github.com/levon-z/TwoCode)"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() {
            return None;
        }
        let connection = WinHttpConnect(session, w!("lrclib.net"), 443, 0);
        let wide = HSTRING::from(path);
        let request = if connection.is_null() {
            std::ptr::null_mut()
        } else {
            WinHttpOpenRequest(connection, w!("GET"), &wide, PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE)
        };
        let body = if request.is_null() { None } else { read_response(request) };
        for handle in [request, connection, session] {
            if !handle.is_null() {
                let _ = WinHttpCloseHandle(handle);
            }
        }
        body
    }
}

unsafe fn read_response(request: *mut core::ffi::c_void) -> Option<String> {
    let _ = WinHttpSetTimeouts(request, 5000, 5000, 8000, 8000);
    WinHttpSendRequest(request, None, None, 0, 0, 0).ok()?;
    WinHttpReceiveResponse(request, std::ptr::null_mut()).ok()?;
    let mut body = vec![];
    loop {
        let mut chunk = [0u8; 16 * 1024];
        let mut read = 0u32;
        WinHttpReadData(request, chunk.as_mut_ptr() as *mut _, chunk.len() as u32, &mut read).ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read as usize]);
        if body.len() > MAX_BODY {
            return None;
        }
    }
    String::from_utf8(body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Asks the real LRCLIB. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn asks_the_real_service() {
        let request = Request {
            key: String::new(),
            title: "Cold (feat. Future)".to_string(),
            artist: "Maroon 5".to_string(),
            album: String::new(),
            duration_ms: 234_000,
        };
        let lines = fetch(&request).expect("an answer from LRCLIB");
        assert!(lines.len() > 10, "found {} lines", lines.len());
    }

    #[test]
    fn reads_timed_lines() {
        let lines = parse_lrc("[ar:Someone]\n[00:12.50]first\n[00:05.00][01:00.00]again\n[00:20.00]\nno stamp\n");
        let got: Vec<(u64, &str)> = lines.iter().map(|l| (l.at_ms, l.text.as_str())).collect();
        assert_eq!(got, vec![(5000, "again"), (12500, "first"), (60000, "again")]);
    }

    #[test]
    fn finds_the_current_line() {
        let lines = parse_lrc("[00:10.00]a\n[00:20.00]b");
        assert_eq!(current_line(&lines, 9_999), None);
        assert_eq!(current_line(&lines, 10_000), Some(0));
        assert_eq!(current_line(&lines, 99_000), Some(1));
        assert_eq!(current_line(&[], 5), None);
    }

    #[test]
    fn cleans_video_titles() {
        assert_eq!(clean_title("Song (Official Video) [4K]"), "Song");
        assert_eq!(clean_title("(Intro)"), "(Intro)");
    }

    #[test]
    fn encodes_for_a_url() {
        assert_eq!(encode("Cold (feat. Future) é"), "Cold%20%28feat.%20Future%29%20%C3%A9");
    }

    #[test]
    fn picks_the_closest_synced_result() {
        let results = json!([
            { "duration": 300.0, "syncedLyrics": "far" },
            { "duration": 234.0, "syncedLyrics": null },
            { "duration": 236.0, "syncedLyrics": "near" }
        ]);
        assert_eq!(best_match(&results, 234.0), Some("near"));
        assert_eq!(best_match(&results, 100.0), None);
    }
}
