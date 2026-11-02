//! How loud each pitch is in what Windows is playing, for the waveform. Listens to the speakers' mix only while
//! the media page is open. Nothing is recorded or kept: each moment becomes a few bar heights and is thrown away.
use std::collections::VecDeque;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::core::Result;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

pub const BARS: usize = 9;
pub type Levels = [f32; BARS];
const WINDOW: usize = 1024;
const POLL: Duration = Duration::from_millis(16);
const RETRY: Duration = Duration::from_secs(1);
const LOWEST_HZ: f32 = 60.0;
const HIGHEST_HZ: f32 = 12_000.0;
const QUIETEST_DB: f32 = -60.0;
const FALL: f32 = 0.84; // bars jump up with the sound and sink back slowly
const BUFFER_100NS: i64 = 1_000_000;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// Send true to start listening and false to stop.
pub fn start(levels: Arc<Mutex<Levels>>) -> Sender<bool> {
    let (tx, rx) = channel::<bool>();
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED).ok();
        }
        let mut wanted = false;
        loop {
            if !wanted {
                match rx.recv() {
                    Ok(on) => wanted = on,
                    Err(_) => return,
                }
                continue;
            }
            let result = listen(&levels, &mut || match rx.recv_timeout(POLL) {
                Ok(on) => {
                    wanted = on;
                    wanted
                }
                Err(RecvTimeoutError::Timeout) => true,
                Err(RecvTimeoutError::Disconnected) => {
                    wanted = false;
                    false
                }
            });
            if let Ok(mut shared) = levels.lock() {
                *shared = [0.0; BARS];
            }
            if let Err(why) = &result {
                crate::config::log(&format!("could not listen for the waveform: {why}"));
            }
            // The output device changed or is busy: start over on the current one in a moment.
            if result.is_err() && wanted {
                match rx.recv_timeout(RETRY) {
                    Ok(on) => wanted = on,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        }
    });
    tx
}

fn listen(levels: &Mutex<Levels>, keep_going: &mut dyn FnMut() -> bool) -> Result<()> {
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = client.GetMixFormat()?;
        let info = std::ptr::read_unaligned(format);
        let opened = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, BUFFER_100NS, 0, format, None);
        CoTaskMemFree(Some(format as *const _));
        opened?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        let (channels, bits, tag, rate) = (info.nChannels.max(1) as usize, info.wBitsPerSample, info.wFormatTag, info.nSamplesPerSec as f32);
        let is_float = tag == WAVE_FORMAT_IEEE_FLOAT || (tag == WAVE_FORMAT_EXTENSIBLE && bits == 32);
        let mut recent: VecDeque<f32> = VecDeque::with_capacity(WINDOW);
        let mut shown = [0.0; BARS];
        let outcome = loop {
            if !keep_going() {
                break Ok(());
            }
            let mut heard = false;
            loop {
                let waiting = match capture.GetNextPacketSize() {
                    Ok(n) => n,
                    Err(why) => {
                        let _ = client.Stop();
                        return Err(why);
                    }
                };
                if waiting == 0 {
                    break;
                }
                let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                for frame in 0..frames as usize {
                    let sample = if silent { 0.0 } else { mono_sample(data, frame, channels, bits, is_float) };
                    if recent.len() == WINDOW {
                        recent.pop_front();
                    }
                    recent.push_back(sample);
                }
                capture.ReleaseBuffer(frames)?;
                heard = true;
            }
            let target = if heard && recent.len() == WINDOW {
                let samples: Vec<f32> = recent.iter().copied().collect();
                spectrum(&samples, rate)
            } else {
                [0.0; BARS]
            };
            settle_bars(&mut shown, &target);
            if let Ok(mut shared) = levels.lock() {
                *shared = shown;
            }
        };
        let _ = client.Stop();
        outcome
    }
}

/// One moment of all the channels mixed together.
unsafe fn mono_sample(data: *const u8, frame: usize, channels: usize, bits: u16, is_float: bool) -> f32 {
    let mut sum = 0.0;
    for channel in 0..channels {
        let index = frame * channels + channel;
        sum += match (bits, is_float) {
            (32, true) => (data as *const f32).add(index).read_unaligned(),
            (32, false) => (data as *const i32).add(index).read_unaligned() as f32 / i32::MAX as f32,
            (16, _) => (data as *const i16).add(index).read_unaligned() as f32 / i16::MAX as f32,
            _ => 0.0,
        };
    }
    sum / channels as f32
}

pub fn settle_bars(shown: &mut Levels, target: &Levels) {
    for (bar, goal) in shown.iter_mut().zip(target) {
        *bar = if goal > bar { *goal } else { (*bar * FALL).max(*goal) };
        if *bar < 0.005 {
            *bar = 0.0;
        }
    }
}

/// Bar heights from 0 to 1, low notes on the left, spaced the way hearing is.
pub fn spectrum(samples: &[f32], sample_rate: f32) -> Levels {
    let n = samples.len().next_power_of_two() / if samples.len().is_power_of_two() { 1 } else { 2 };
    let mut bars = [0.0; BARS];
    if n < 2 || sample_rate <= 0.0 {
        return bars;
    }
    let mut real: Vec<f32> = samples[..n].iter().enumerate().map(|(i, s)| s * (0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())).collect();
    let mut imaginary = vec![0.0; n];
    fft(&mut real, &mut imaginary);
    let hz_per_bin = sample_rate / n as f32;
    let ratio = (HIGHEST_HZ / LOWEST_HZ).powf(1.0 / BARS as f32);
    for (i, bar) in bars.iter_mut().enumerate() {
        let from = ((LOWEST_HZ * ratio.powi(i as i32) / hz_per_bin) as usize).clamp(1, n / 2 - 1);
        let to = ((LOWEST_HZ * ratio.powi(i as i32 + 1) / hz_per_bin).ceil() as usize).clamp(from + 1, n / 2);
        // A Hann window halves the height, so 4/n reads a full-scale wave as 1.
        let peak = (from..to).map(|k| (real[k] * real[k] + imaginary[k] * imaginary[k]).sqrt()).fold(0.0, f32::max) * 4.0 / n as f32;
        let db = 20.0 * peak.max(1e-6).log10();
        *bar = ((db - QUIETEST_DB) / -QUIETEST_DB).clamp(0.0, 1.0);
    }
    bars
}

fn fft(real: &mut [f32], imaginary: &mut [f32]) {
    let n = real.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            real.swap(i, j);
            imaginary.swap(i, j);
        }
    }
    let mut size = 2;
    while size <= n {
        let angle = -std::f32::consts::TAU / size as f32;
        let (step_re, step_im) = (angle.cos(), angle.sin());
        for start in (0..n).step_by(size) {
            let (mut w_re, mut w_im) = (1.0f32, 0.0f32);
            for k in 0..size / 2 {
                let (a, b) = (start + k, start + k + size / 2);
                let (t_re, t_im) = (real[b] * w_re - imaginary[b] * w_im, real[b] * w_im + imaginary[b] * w_re);
                real[b] = real[a] - t_re;
                imaginary[b] = imaginary[a] - t_im;
                real[a] += t_re;
                imaginary[a] += t_im;
                (w_re, w_im) = (w_re * step_re - w_im * step_im, w_re * step_im + w_im * step_re);
            }
        }
        size *= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, amplitude: f32) -> Vec<f32> {
        (0..WINDOW).map(|i| amplitude * (std::f32::consts::TAU * hz * i as f32 / 48_000.0).sin()).collect()
    }

    fn loudest(bars: &Levels) -> usize {
        (0..BARS).max_by(|a, b| bars[*a].total_cmp(&bars[*b])).unwrap()
    }

    #[test]
    fn silence_makes_no_bars() {
        assert_eq!(spectrum(&vec![0.0; WINDOW], 48_000.0), [0.0; BARS]);
    }

    #[test]
    fn low_and_high_notes_land_on_opposite_sides() {
        let low = spectrum(&tone(100.0, 0.5), 48_000.0);
        let high = spectrum(&tone(8_000.0, 0.5), 48_000.0);
        assert!(loudest(&low) <= 1, "{low:?}");
        assert!(loudest(&high) >= BARS - 2, "{high:?}");
    }

    #[test]
    fn louder_sound_makes_taller_bars() {
        let quiet = spectrum(&tone(1_000.0, 0.01), 48_000.0);
        let loud = spectrum(&tone(1_000.0, 0.5), 48_000.0);
        let at = loudest(&loud);
        assert!(loud[at] > quiet[at] + 0.3 && loud[at] <= 1.0);
    }

    #[test]
    fn bars_jump_up_and_sink_slowly() {
        let mut shown = [0.0; BARS];
        let mut target = [0.0; BARS];
        target[0] = 0.8;
        settle_bars(&mut shown, &target);
        assert_eq!(shown[0], 0.8);
        settle_bars(&mut shown, &[0.0; BARS]);
        assert!(shown[0] < 0.8 && shown[0] > 0.5);
        for _ in 0..100 {
            settle_bars(&mut shown, &[0.0; BARS]);
        }
        assert_eq!(shown[0], 0.0);
    }
}
