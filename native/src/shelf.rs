//! Files parked in the notch. Only their paths are kept, in %APPDATA%\TwoCode\shelf.json: nothing is copied or moved.
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use windows::core::{implement, Ref, Result, BOOL, HRESULT, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

pub const WM_DRAG: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 4;
pub const WM_DROPPED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 5;
const CF_HDROP_FORMAT: u16 = 15;

pub static ACCEPTS: AtomicBool = AtomicBool::new(true);
static DRAGGING_OUT: AtomicBool = AtomicBool::new(false);

#[derive(Clone, PartialEq, Debug)]
pub struct Item {
    pub path: String,
    pub name: String,
    pub badge: String,
}

impl Item {
    pub fn new(path: &str) -> Item {
        let p = Path::new(path);
        let name = p.file_name().map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned());
        let badge = if p.is_dir() {
            "DIR".to_string()
        } else {
            badge_for(p.extension().map(|e| e.to_string_lossy().into_owned()).as_deref())
        };
        Item { path: path.to_string(), name, badge }
    }
}

fn badge_for(extension: Option<&str>) -> String {
    match extension {
        Some(e) if !e.is_empty() => e.chars().take(4).collect::<String>().to_uppercase(),
        _ => "FILE".to_string(),
    }
}

/// Adds what is not already on the shelf. Returns true if anything was added.
pub fn add(items: &mut Vec<Item>, paths: &[String]) -> bool {
    let before = items.len();
    for path in paths {
        if !items.iter().any(|i| i.path.eq_ignore_ascii_case(path)) {
            items.push(Item::new(path));
        }
    }
    items.len() != before
}

pub fn load() -> Vec<Item> {
    let paths: Vec<String> = fs::read_to_string(crate::config::dir().join("shelf.json")).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
    paths.iter().filter(|p| Path::new(p).exists()).map(|p| Item::new(p)).collect()
}

pub fn save(items: &[Item]) {
    let dir = crate::config::dir();
    let _ = fs::create_dir_all(&dir);
    let paths: Vec<&str> = items.iter().map(|i| i.path.as_str()).collect();
    if let Ok(text) = serde_json::to_string_pretty(&paths) {
        let _ = fs::write(dir.join("shelf.json"), text);
    }
}

fn paths_from(data: &IDataObject) -> Vec<String> {
    let format = FORMATETC { cfFormat: CF_HDROP_FORMAT, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
    let mut paths = vec![];
    unsafe {
        let Ok(mut medium) = data.GetData(&format) else { return paths };
        let drop = HDROP(medium.u.hGlobal.0);
        for i in 0..DragQueryFileW(drop, u32::MAX, None) {
            let mut wide = vec![0u16; DragQueryFileW(drop, i, None) as usize + 1];
            let n = DragQueryFileW(drop, i, Some(&mut wide)) as usize;
            paths.push(String::from_utf16_lossy(&wide[..n]));
        }
        ReleaseStgMedium(&mut medium);
    }
    paths
}

/// Tells the window when a file drag arrives, so the shelf can open under it.
#[implement(IDropTarget)]
struct Target {
    hwnd: isize,
    dropped: Arc<Mutex<Vec<String>>>,
    has_files: AtomicBool,
}

impl Target {
    fn post(&self, message: u32, value: usize) {
        unsafe {
            let _ = PostMessageW(Some(HWND(self.hwnd as *mut _)), message, WPARAM(value), LPARAM(0));
        }
    }

    fn effect(&self) -> DROPEFFECT {
        if self.has_files.load(Ordering::SeqCst) {
            DROPEFFECT_COPY
        } else {
            DROPEFFECT_NONE
        }
    }
}

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(&self, data: Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let wanted = ACCEPTS.load(Ordering::SeqCst) && !DRAGGING_OUT.load(Ordering::SeqCst);
        let has = wanted && data.as_ref().is_some_and(|d| !paths_from(d).is_empty());
        self.has_files.store(has, Ordering::SeqCst);
        if has {
            self.post(WM_DRAG, 1);
        }
        unsafe { *effect = self.effect() };
        Ok(())
    }

    fn DragOver(&self, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        unsafe { *effect = self.effect() };
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        if self.has_files.swap(false, Ordering::SeqCst) {
            self.post(WM_DRAG, 0);
        }
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        unsafe { *effect = self.effect() };
        if self.has_files.swap(false, Ordering::SeqCst) {
            if let (Some(d), Ok(mut dropped)) = (data.as_ref(), self.dropped.lock()) {
                dropped.extend(paths_from(d));
            }
            self.post(WM_DROPPED, 0);
        }
        Ok(())
    }
}

pub fn accept_drops(hwnd: HWND, dropped: Arc<Mutex<Vec<String>>>) {
    let target: IDropTarget = Target { hwnd: hwnd.0 as isize, dropped, has_files: AtomicBool::new(false) }.into();
    unsafe {
        if let Err(why) = RegisterDragDrop(hwnd, &target) {
            crate::config::log(&format!("the shelf cannot accept files: {why}"));
        }
    }
}

#[implement(IDropSource)]
struct Source;

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Drags a file out of the shelf as a copy, so the original stays where it is. Waits until the drop.
pub fn drag_out(path: &str) {
    let start = || -> Result<()> {
        unsafe {
            let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path), None)?;
            let data: IDataObject = item.BindToHandler(None, &BHID_DataObject)?;
            let source: IDropSource = Source.into();
            let mut effect = DROPEFFECT_NONE;
            let _ = DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect);
            Ok(())
        }
    };
    DRAGGING_OUT.store(true, Ordering::SeqCst);
    if let Err(why) = start() {
        crate::config::log(&format!("could not drag a file from the shelf: {why}"));
    }
    DRAGGING_OUT.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_badges_come_from_the_path() {
        let item = Item::new("C:\\no-such-folder\\Report.final.pdf");
        assert_eq!((item.name.as_str(), item.badge.as_str()), ("Report.final.pdf", "PDF"));
        assert_eq!(Item::new("C:\\no-such-folder\\Makefile").badge, "FILE");
        assert_eq!(Item::new("C:\\no-such-folder\\a.markdown").badge, "MARK");
        assert_eq!(Item::new("C:\\Windows").badge, "DIR");
    }

    #[test]
    fn the_same_file_is_only_added_once() {
        let mut items = vec![];
        assert!(add(&mut items, &["C:\\x\\a.txt".to_string(), "c:\\X\\A.TXT".to_string()]));
        assert!(!add(&mut items, &["C:\\x\\a.txt".to_string()]));
        assert_eq!(items.len(), 1);
    }
}
