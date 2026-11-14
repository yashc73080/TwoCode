//! Whatever is playing on Windows, through the system media controls. Windows announces every change.
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::Result;
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackInfo as PlaybackInfo, GlobalSystemMediaTransportControlsSessionPlaybackStatus as Playback,
};
use windows::Storage::Streams::DataReader;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

#[derive(Clone, Default)]
pub struct Media {
    pub present: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub app: String,
    pub playing: bool,
    pub can_seek: bool,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub read_at: Option<Instant>,
    pub art: Option<Arc<Vec<u8>>>,
    pub art_rev: u64,
}

impl Media {
    pub fn position_now(&self) -> u64 {
        let moved = if self.playing { self.read_at.map_or(0, |at| at.elapsed().as_millis() as u64) } else { 0 };
        (self.position_ms + moved).min(self.duration_ms.max(self.position_ms))
    }
}

pub enum Command {
    Refresh,
    Resubscribe,
    Toggle,
    Next,
    Previous,
    Seek(u64),
}

/// "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify" and "chrome.exe" to a readable name.
pub fn app_name(id: &str) -> String {
    let last = id.rsplit('!').next().unwrap_or(id);
    let last = last.rsplit(['\\', '/']).next().unwrap_or(last);
    let last = last.strip_suffix(".exe").or_else(|| last.strip_suffix(".EXE")).unwrap_or(last);
    let mut letters = last.chars();
    match letters.next() {
        Some(first) => first.to_uppercase().chain(letters).collect(),
        None => String::new(),
    }
}

/// Where a track is now: Windows reports a position and the moment it was true.
fn position(position_ticks: i64, updated_ticks: i64, now_ticks: i64, playing: bool) -> u64 {
    let moved = if playing && updated_ticks > 0 { (now_ticks - updated_ticks).max(0) } else { 0 };
    ((position_ticks + moved).max(0) / 10_000) as u64
}

/// Windows announces a change in bursts while it is still updating the session on its own threads. Reading in the
/// middle of that has crashed inside Windows' media library (a double free when dropping playback info), so a
/// refresh waits for the burst to settle. Play, skip and seek still happen at once.
const SETTLE: Duration = Duration::from_millis(150);

pub fn start(state: Arc<Mutex<Media>>, on_change: impl Fn() + Send + Sync + 'static) -> Sender<Command> {
    let (tx, rx) = channel();
    let events = tx.clone();
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED).ok();
        }
        let Ok(manager) = Manager::RequestAsync().and_then(|r| r.join()) else { return };
        let on_change = Arc::new(on_change);
        let notify = |cmd: fn() -> Command| {
            let events = events.clone();
            move || {
                let _ = events.send(cmd());
            }
        };
        let (a, b) = (notify(|| Command::Resubscribe), notify(|| Command::Resubscribe));
        let _ = manager.CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
            a();
            Ok(())
        }));
        let _ = manager.SessionsChanged(&TypedEventHandler::new(move |_, _| {
            b();
            Ok(())
        }));
        let mut subscribed = subscribe(&manager, &events);
        let mut art_key = String::new();
        let mut art_found = false;
        let _ = events.send(Command::Refresh);
        while let Ok(first) = rx.recv() {
            let mut refresh = false;
            let mut resubscribe = false;
            let mut commands: Vec<Command> = std::iter::once(first).chain(rx.try_iter()).collect();
            if commands.iter().all(|c| matches!(c, Command::Refresh | Command::Resubscribe)) {
                std::thread::sleep(SETTLE);
                commands.extend(rx.try_iter());
            }
            for command in commands {
                match command {
                    Command::Refresh => refresh = true,
                    Command::Resubscribe => resubscribe = true,
                    other => {
                        if let Some((session, _)) = choose(&manager) {
                            let _ = control(&session, other);
                            refresh = true;
                        }
                    }
                }
            }
            if resubscribe {
                for (session, tokens) in subscribed.drain(..) {
                    unsubscribe(&session, tokens);
                }
                subscribed = subscribe(&manager, &events);
                refresh = true;
            }
            if refresh && read(&manager, &state, &mut art_key, &mut art_found).is_some() {
                on_change();
            }
        }
    });
    tx
}

type Tokens = [i64; 3];

fn subscribe(manager: &Manager, events: &Sender<Command>) -> Vec<(Session, Tokens)> {
    let mut out = vec![];
    let Ok(sessions) = manager.GetSessions() else { return out };
    for session in sessions {
        let handler = || {
            let events = events.clone();
            move || {
                let _ = events.send(Command::Refresh);
            }
        };
        let (a, b, c) = (handler(), handler(), handler());
        let tokens = (|| -> Result<Tokens> {
            Ok([
                session.MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
                    a();
                    Ok(())
                }))?,
                session.PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
                    b();
                    Ok(())
                }))?,
                session.TimelinePropertiesChanged(&TypedEventHandler::new(move |_, _| {
                    c();
                    Ok(())
                }))?,
            ])
        })();
        if let Ok(tokens) = tokens {
            out.push((session, tokens));
        }
    }
    out
}

fn unsubscribe(session: &Session, tokens: Tokens) {
    let _ = session.RemoveMediaPropertiesChanged(tokens[0]);
    let _ = session.RemovePlaybackInfoChanged(tokens[1]);
    let _ = session.RemoveTimelinePropertiesChanged(tokens[2]);
}

fn is_playing(info: &Option<PlaybackInfo>) -> bool {
    info.as_ref().is_some_and(|i| i.PlaybackStatus().is_ok_and(|s| s == Playback::Playing))
}

/// Windows' current session, unless it is paused while another player is really playing. Each session's playback
/// info is asked for once and handed back, so a refresh touches Windows' media objects as little as it can.
fn choose(manager: &Manager) -> Option<(Session, Option<PlaybackInfo>)> {
    let current = manager.GetCurrentSession().ok().map(|s| {
        let info = s.GetPlaybackInfo().ok();
        (s, info)
    });
    if current.as_ref().is_some_and(|(_, info)| is_playing(info)) {
        return current;
    }
    let sessions = manager.GetSessions().ok()?;
    let playing = sessions.into_iter().filter(|s| current.as_ref().is_none_or(|(c, _)| c != s)).find_map(|s| {
        let info = s.GetPlaybackInfo().ok();
        is_playing(&info).then_some((s, info))
    });
    playing.or(current)
}

fn control(session: &Session, command: Command) -> Result<()> {
    let done = match command {
        Command::Toggle => session.TryTogglePlayPauseAsync()?,
        Command::Next => session.TrySkipNextAsync()?,
        Command::Previous => session.TrySkipPreviousAsync()?,
        Command::Seek(ms) => session.TryChangePlaybackPositionAsync(ms as i64 * 10_000)?,
        Command::Refresh | Command::Resubscribe => return Ok(()),
    };
    done.join().map(|_| ())
}

fn read(manager: &Manager, state: &Mutex<Media>, art_key: &mut String, art_found: &mut bool) -> Option<()> {
    let Some((session, info)) = choose(manager) else {
        let mut media = state.lock().ok()?;
        let rev = media.art_rev;
        *media = Media { art_rev: rev + 1, ..Media::default() };
        art_key.clear();
        return Some(());
    };
    let props = session.TryGetMediaPropertiesAsync().ok()?.join().ok()?;
    let info = info?;
    let timeline = session.GetTimelineProperties().ok()?;
    let title = props.Title().map(|t| t.to_string()).unwrap_or_default();
    let artist = props.Artist().map(|t| t.to_string()).unwrap_or_default();
    let playing = info.PlaybackStatus().is_ok_and(|s| s == Playback::Playing);
    let ticks = |t: Result<TimeSpan>| t.map_or(0, |t| t.Duration);
    let start = ticks(timeline.StartTime());
    let end = ticks(timeline.EndTime());
    let updated = timeline.LastUpdatedTime().map_or(0, |t| t.UniversalTime);
    let now = crate::agents::now_ms() as i64 * 10_000 + 116_444_736_000_000_000;
    let position_ms = position(ticks(timeline.Position()) - start, updated, now, playing);
    let key = format!("{title}\u{1}{artist}");
    let mut new_art = None;
    if key != *art_key || !*art_found {
        new_art = Some(load_art(&props));
        *art_found = matches!(new_art, Some(Some(_)));
        *art_key = key;
    }
    let mut media = state.lock().ok()?;
    media.present = true;
    media.title = title;
    media.artist = artist;
    media.album = props.AlbumTitle().map(|t| t.to_string()).unwrap_or_default();
    media.app = session.SourceAppUserModelId().map(|s| app_name(&s.to_string())).unwrap_or_default();
    media.playing = playing;
    media.can_seek = info.Controls().and_then(|c| c.IsPlaybackPositionEnabled()).unwrap_or(false);
    media.position_ms = position_ms;
    media.duration_ms = ((end - start).max(0) / 10_000) as u64;
    media.read_at = Some(Instant::now());
    if let Some(art) = new_art {
        media.art = art;
        media.art_rev += 1;
    }
    Some(())
}

fn load_art(props: &windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties) -> Option<Arc<Vec<u8>>> {
    let stream = props.Thumbnail().ok()?.OpenReadAsync().ok()?.join().ok()?;
    let size = stream.Size().ok()? as u32;
    if size == 0 || size > 16 * 1024 * 1024 {
        return None;
    }
    let reader = DataReader::CreateDataReader(&stream).ok()?;
    reader.LoadAsync(size).ok()?.join().ok()?;
    let mut bytes = vec![0u8; size as usize];
    reader.ReadBytes(&mut bytes).ok()?;
    Some(Arc::new(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_app_names() {
        assert_eq!(app_name("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), "Spotify");
        assert_eq!(app_name("chrome.exe"), "Chrome");
        assert_eq!(app_name(r"C:\Program Files\foobar2000\foobar2000.exe"), "Foobar2000");
        assert_eq!(app_name(""), "");
    }

    #[test]
    fn position_moves_only_while_playing() {
        assert_eq!(position(10_000_000, 50, 10_050, true), 1001);
        assert_eq!(position(10_000_000, 50, 10_050, false), 1000);
        assert_eq!(position(10_000_000, 0, 99_999_999, true), 1000); // no update time reported
    }

    #[test]
    fn position_now_stops_at_the_end() {
        let media = Media { playing: false, position_ms: 900, duration_ms: 1000, ..Media::default() };
        assert_eq!(media.position_now(), 900);
        let over = Media { playing: true, position_ms: 900, duration_ms: 1000, read_at: Instant::now().checked_sub(std::time::Duration::from_secs(5)), ..Media::default() };
        assert_eq!(over.position_now(), 1000);
    }
}
