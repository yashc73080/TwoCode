//! Brings the notch back when it crashes. Started plainly, twocode.exe becomes a tiny keeper that runs the
//! notch as a second copy of itself and starts it again if it dies. Quitting from the tray ends both.
use std::process::Command;
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Threading::CreateMutexW;

/// The argument the keeper starts the notch with.
pub const NOTCH_ARG: &str = "--notch";
/// More crashes than this inside the window means something is badly wrong: stop trying.
const MAX_CRASHES: usize = 5;
const CRASH_WINDOW: Duration = Duration::from_secs(10 * 60);
const RESTART_DELAY: Duration = Duration::from_secs(2);

/// True when this process is the notch itself rather than the keeper.
pub fn is_notch() -> bool {
    std::env::args().skip(1).any(|a| a == NOTCH_ARG)
}

/// Keeps the notch running. Returns false when it could not start one, so the caller runs the notch itself.
pub fn run() -> bool {
    unsafe {
        // One keeper is enough: a second start just leaves.
        let single = CreateMutexW(None, true, w!("Local\\TwoCodeKeeper"));
        if single.is_err() || GetLastError() == ERROR_ALREADY_EXISTS {
            return true;
        }
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    let mut crashes: Vec<Instant> = vec![];
    loop {
        let status = match Command::new(&exe).arg(NOTCH_ARG).status() {
            Ok(status) => status,
            Err(why) => {
                crate::config::log(&format!("the keeper could not start the notch: {why}"));
                return !crashes.is_empty();
            }
        };
        // Quit from the tray, or another notch was already running.
        if status.success() {
            return true;
        }
        let code = status.code().map(|c| c as u32).unwrap_or(0);
        crate::config::log(&format!("the notch stopped with code {code:#x}; starting it again"));
        let now = Instant::now();
        crashes.retain(|t| now.duration_since(*t) < CRASH_WINDOW);
        crashes.push(now);
        if crashes.len() > MAX_CRASHES {
            crate::config::log("the notch keeps crashing; the keeper gives up");
            return true;
        }
        std::thread::sleep(RESTART_DELAY);
    }
}
