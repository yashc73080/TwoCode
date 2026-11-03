//! Headphones and speakers connecting over Bluetooth. Only reads what Windows already knows: nothing is paired,
//! changed or sent to the devices.
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::{HSTRING, PCWSTR};
use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothDevice, BluetoothMajorClass};
use windows::Devices::Enumeration::{DeviceInformation, DeviceWatcher};
use windows::Foundation::TypedEventHandler;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::Properties::DEVPROPTYPE;
use windows::Win32::Foundation::DEVPROPKEY;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// The battery level Windows' own Bluetooth settings show, when the device reports one.
const BATTERY_KEY: DEVPROPKEY = DEVPROPKEY { fmtid: windows::core::GUID::from_u128(0x104ea319_6ee2_4701_bd47_8ddbf425bbe5), pid: 2 };
/// Headsets report their battery a few seconds after connecting, if at all.
const BATTERY_TRIES: u32 = 6;
const BATTERY_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, PartialEq, Debug)]
pub struct Notice {
    pub name: String,
    pub connected: bool,
    pub battery: Option<u8>,
    /// Changes with every new event, so the window knows to pop open again.
    pub id: u64,
}

pub fn start(state: Arc<Mutex<Option<Notice>>>, on_change: impl Fn() + Send + Sync + 'static) {
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED).ok();
        }
        if let Err(why) = watch(state, Arc::new(on_change)) {
            crate::config::log(&format!("not watching Bluetooth: {why}"));
        }
    });
}

fn watch(state: Arc<Mutex<Option<Notice>>>, on_change: Arc<dyn Fn() + Send + Sync>) -> windows::core::Result<()> {
    let selector = BluetoothDevice::GetDeviceSelectorFromPairingState(true)?;
    let watcher = DeviceInformation::CreateWatcherAqsFilter(&selector)?;
    // Devices are kept here so their connection events keep arriving.
    let devices: Arc<Mutex<Vec<BluetoothDevice>>> = Arc::default();
    let (found, arrivals) = channel::<HSTRING>();
    watcher.Added(&TypedEventHandler::<DeviceWatcher, DeviceInformation>::new(move |_, info| {
        if let Some(info) = info.as_ref() {
            let _ = found.send(info.Id()?);
        }
        Ok(())
    }))?;
    watcher.Start()?;
    let started = Instant::now();
    while let Ok(id) = arrivals.recv() {
        let Ok(device) = BluetoothDevice::FromIdAsync(&id).and_then(|op| op.join()) else { continue };
        if !is_audio(&device) {
            continue;
        }
        let (state, on_change) = (state.clone(), on_change.clone());
        let counter = Arc::new(Mutex::new(0u64));
        device.ConnectionStatusChanged(&TypedEventHandler::new(move |sender: windows::core::Ref<BluetoothDevice>, _| {
            let Some(device) = sender.as_ref() else { return Ok(()) };
            let connected = device.ConnectionStatus()? == BluetoothConnectionStatus::Connected;
            let name = device.Name()?.to_string();
            let address = device.BluetoothAddress()?;
            let id = {
                let mut n = counter.lock().map_err(|_| windows::core::Error::empty())?;
                *n += 1;
                address.wrapping_mul(1_000_003).wrapping_add(*n)
            };
            show(&state, Notice { name: name.clone(), connected, battery: None, id });
            on_change();
            if connected {
                let (state, on_change) = (state.clone(), on_change.clone());
                std::thread::spawn(move || {
                    for _ in 0..BATTERY_TRIES {
                        std::thread::sleep(BATTERY_WAIT);
                        if let Some(level) = battery(address) {
                            let mut updated = false;
                            if let Ok(mut current) = state.lock() {
                                if let Some(notice) = current.as_mut().filter(|n| n.id == id) {
                                    notice.battery = Some(level);
                                    updated = true;
                                }
                            }
                            if updated {
                                on_change();
                            }
                            return;
                        }
                    }
                });
            }
            Ok(())
        }))?;
        if let Ok(mut list) = devices.lock() {
            list.push(device);
        }
        let _ = started;
    }
    let _ = watcher.Stop();
    Ok(())
}

fn show(state: &Mutex<Option<Notice>>, notice: Notice) {
    if let Ok(mut current) = state.lock() {
        *current = Some(notice);
    }
}

/// Headphones, earbuds and speakers. A mouse or keyboard reconnecting doesn't pop anything up.
fn is_audio(device: &BluetoothDevice) -> bool {
    device.ClassOfDevice().and_then(|c| c.MajorClass()).is_ok_and(|major| major == BluetoothMajorClass::AudioVideo)
}

/// "70AE2ACA1B9C", the way device IDs spell an address.
pub fn address_hex(address: u64) -> String {
    format!("{:012X}", address & 0xFFFF_FFFF_FFFF)
}

/// The battery level Windows has for any part of this device, from 0 to 100.
fn battery(address: u64) -> Option<u8> {
    let needle = address_hex(address);
    device_ids().into_iter().filter(|id| id.to_uppercase().contains(&needle)).find_map(|id| battery_of(&id))
}

fn device_ids() -> Vec<String> {
    let filter: Vec<u16> = "BTHENUM".encode_utf16().chain(Some(0)).collect();
    let flags = CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT;
    unsafe {
        let mut size = 0u32;
        if CM_Get_Device_ID_List_SizeW(&mut size, PCWSTR(filter.as_ptr()), flags) != CR_SUCCESS || size == 0 {
            return vec![];
        }
        let mut buffer = vec![0u16; size as usize];
        if CM_Get_Device_ID_ListW(PCWSTR(filter.as_ptr()), &mut buffer, flags) != CR_SUCCESS {
            return vec![];
        }
        split_ids(&buffer)
    }
}

/// The list comes as names separated by nulls and ended by an empty one.
pub fn split_ids(buffer: &[u16]) -> Vec<String> {
    buffer.split(|c| *c == 0).take_while(|part| !part.is_empty()).map(String::from_utf16_lossy).collect()
}

fn battery_of(id: &str) -> Option<u8> {
    let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let mut node = 0u32;
        if CM_Locate_DevNodeW(&mut node, PCWSTR(wide.as_ptr()), CM_LOCATE_DEVNODE_NORMAL) != CR_SUCCESS {
            return None;
        }
        let mut kind = DEVPROPTYPE::default();
        let mut value = [0u8; 4];
        let mut size = value.len() as u32;
        if CM_Get_DevNode_PropertyW(node, &BATTERY_KEY, &mut kind, Some(value.as_mut_ptr()), &mut size, 0) != CR_SUCCESS || size == 0 {
            return None;
        }
        Some(value[0]).filter(|level| *level <= 100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_read_like_device_ids() {
        assert_eq!(address_hex(0x70AE2ACA1B9C), "70AE2ACA1B9C");
        assert_eq!(address_hex(0x1), "000000000001");
    }

    #[test]
    fn device_id_lists_split_on_nulls() {
        let raw: Vec<u16> = "BTHENUM\\A\0BTHENUM\\B\0\0junk".encode_utf16().collect();
        assert_eq!(split_ids(&raw), ["BTHENUM\\A", "BTHENUM\\B"]);
        assert!(split_ids(&[0, 0]).is_empty());
    }
}
