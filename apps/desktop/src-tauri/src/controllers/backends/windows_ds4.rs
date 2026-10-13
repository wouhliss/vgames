//! DS4 uses the public ViGEm bus ABI, not private Rust SDK layout.
//! ABI: ViGEmClient b66d02d57e32cc8595369c53418b843e958649b4,
//! include/ViGEm/km/BusShared.h. Fixed byte buffers initialize all padding.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use vigem_client::Client;
use windows_sys::Win32::Foundation::ERROR_IO_PENDING;
use windows_sys::Win32::System::IO::{
    CancelIoEx, DeviceIoControl, GetOverlappedResult, OVERLAPPED,
};
use windows_sys::Win32::System::Threading::CreateEventW;

use super::windows_ds4_report::{header, plugin, report};
use super::{BackendError, Rumble, RumbleSink, VirtualPad};
use crate::controllers::mapping::XReport;

const PLUG: u32 = 0x2aa004;
const UNPLUG: u32 = 0x2aa008;
const READY: u32 = 0x2aa010;
const REPORT: u32 = 0x2aa80c;
const FEEDBACK: u32 = 0x2aa810;

// Own the event and stable buffers until the overlapped operation completes.
// Requests never resize their bytes or move their boxed OVERLAPPED after submission.
struct Request {
    event: OwnedHandle,
    overlapped: Box<OVERLAPPED>,
    bytes: Vec<u8>,
}
#[allow(unsafe_code)]
// SAFETY: owned event, Vec storage and boxed OVERLAPPED retain their addresses
// when the owner moves between threads. No request moves while it is submitted.
unsafe impl Send for Request {}
impl Request {
    fn new(bytes: Vec<u8>) -> Result<Self, BackendError> {
        #[allow(unsafe_code)]
        // SAFETY: null optional arguments create a private non-inherited event.
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        #[allow(unsafe_code)]
        // SAFETY: the successful create transfers ownership of exactly this handle.
        let event = unsafe { OwnedHandle::from_raw_handle(event) };
        #[allow(unsafe_code)]
        // SAFETY: the Win32 OVERLAPPED ABI requires zero initialization before first use.
        let mut overlapped: Box<OVERLAPPED> = Box::new(unsafe { std::mem::zeroed() });
        overlapped.hEvent = event.as_raw_handle();
        Ok(Self {
            event,
            overlapped,
            bytes,
        })
    }
    fn begin(&mut self, client: &Client, code: u32) -> Result<(), BackendError> {
        let size = u32::try_from(self.bytes.len()).map_err(|_| BackendError::State)?;
        let mut transferred = 0;
        #[allow(unsafe_code)]
        // SAFETY: this live client and event outlive completion. The stable buffer is
        // both input and output, as specified by the notification ABI; its bounds are size.
        let result = unsafe {
            DeviceIoControl(
                client.as_raw_handle(),
                code,
                self.bytes.as_mut_ptr().cast(),
                size,
                self.bytes.as_mut_ptr().cast(),
                size,
                &mut transferred,
                &mut *self.overlapped,
            )
        };
        if result == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(error.into());
            }
        }
        Ok(())
    }
    fn complete(&mut self, client: &Client) -> Result<(), BackendError> {
        let _ = &self.event;
        let mut transferred = 0;
        #[allow(unsafe_code)]
        // SAFETY: begin submitted this OVERLAPPED on this file object. Wait drains
        // it before any request buffer can be accessed, reused or freed.
        let result = unsafe {
            GetOverlappedResult(
                client.as_raw_handle(),
                &*self.overlapped,
                &mut transferred,
                1,
            )
        };
        if result == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}

fn command(client: &Client, code: u32, bytes: Vec<u8>) -> Result<(), BackendError> {
    let mut request = Request::new(bytes)?;
    request.begin(client, code)?;
    request.complete(client)
}

struct Pad {
    client: Arc<Client>,
    serial: u32,
    index: u16,
    stopped: Arc<Mutex<bool>>,
    worker: Option<JoinHandle<()>>,
    report: Request,
}
pub(super) fn create(index: u16, sink: RumbleSink) -> Result<Box<dyn VirtualPad>, BackendError> {
    let client = Arc::new(Client::connect().map_err(super::windows::driver_error)?);
    let mut pad = Pad {
        client,
        serial: 0,
        index,
        stopped: Arc::new(Mutex::new(false)),
        worker: None,
        report: Request::new(report(0, &XReport::default()).to_vec())?,
    };
    // The public bus ABI allocates targets by serial number. Only a collision
    // (ERROR_INVALID_PARAMETER) warrants trying another slot; driver failures stop.
    for serial in 1..=u32::from(u16::MAX) {
        match command(&pad.client, PLUG, plugin(serial).to_vec()) {
            Ok(()) => {
                pad.serial = serial;
                break;
            }
            Err(BackendError::Io(error)) if error.raw_os_error() == Some(87) => continue,
            Err(error) => return Err(error),
        }
    }
    if pad.serial == 0 {
        return Err(BackendError::Driver(
            "No free virtual controller slot.".into(),
        ));
    }
    command(&pad.client, READY, header::<8>(pad.serial).to_vec())?;
    let client = pad.client.clone();
    let stopped = pad.stopped.clone();
    let serial = pad.serial;
    let (ready, started) = std::sync::mpsc::sync_channel(1);
    pad.worker = Some(
        thread::Builder::new()
            .name("vgames-rumble-ds4".into())
            .spawn(move || {
                let mut request = match Request::new(header::<16>(serial).to_vec()) {
                    Ok(request) => request,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                let mut first = Some(ready);
                loop {
                    let Ok(stop) = stopped.lock() else {
                        break;
                    };
                    if *stop {
                        break;
                    }
                    let submitted = request.begin(&client, FEEDBACK);
                    drop(stop);
                    if let Some(ready) = first.take() {
                        match submitted {
                            Ok(()) => {
                                let _ = ready.send(Ok(()));
                            }
                            Err(error) => {
                                let _ = ready.send(Err(error));
                                break;
                            }
                        }
                    } else if submitted.is_err() {
                        break;
                    }
                    if request.complete(&client).is_err() {
                        break;
                    }
                    let small = request.bytes.get(8).copied().unwrap_or_default();
                    let large = request.bytes.get(9).copied().unwrap_or_default();
                    sink(Rumble {
                        large: u16::from(large) * 257,
                        small: u16::from(small) * 257,
                        duration_ms: if small == 0 && large == 0 {
                            0
                        } else {
                            u32::MAX
                        },
                        delay_ms: 0,
                    });
                }
            })?,
    );
    started.recv().map_err(|_| BackendError::Stopped)??;
    Ok(Box::new(pad))
}
impl VirtualPad for Pad {
    fn update(&mut self, state: &XReport) -> Result<(), BackendError> {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            return Err(BackendError::Stopped);
        }
        self.report
            .bytes
            .copy_from_slice(&report(self.serial, state));
        self.report.begin(&self.client, REPORT)?;
        self.report.complete(&self.client)
    }
    fn player_index(&self) -> u16 {
        self.index
    }
}
impl Drop for Pad {
    fn drop(&mut self) {
        {
            let mut stop = self
                .stopped
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *stop = true;
            #[allow(unsafe_code)]
            // SAFETY: this live client owns all submitted notification requests.
            // The stop lock prevents any enqueue after cancellation. The worker
            // drains completion before its buffers are freed and is joined below.
            unsafe {
                CancelIoEx(self.client.as_raw_handle(), std::ptr::null());
            }
        }
        if self.serial != 0
            && let Err(error) = command(&self.client, UNPLUG, header::<8>(self.serial).to_vec())
        {
            tracing::warn!(%error, "cannot unplug virtual DS4");
        }
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::warn!("virtual DS4 notification thread panicked");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn native_detection_fails_closed_or_a_real_target_updates_and_cancels_cleanly() {
        match Client::connect() {
            Err(_) => assert!(matches!(
                create(1, Arc::new(|_| {})),
                Err(BackendError::Driver(_))
            )),
            Ok(client) => {
                drop(client);
                let mut pad = create(1, Arc::new(|_| {})).unwrap();
                assert_eq!(pad.player_index(), 1);
                pad.update(&XReport {
                    buttons: 0x1000,
                    ..XReport::default()
                })
                .unwrap();
                pad.update(&XReport::default()).unwrap();
                drop(pad);
            }
        }
    }
    #[test]
    fn owned_request_storage_keeps_its_address_when_the_owner_moves() {
        let request = Request::new(header::<16>(7).to_vec()).unwrap();
        let buffer = request.bytes.as_ptr() as usize;
        let overlapped = (&*request.overlapped as *const OVERLAPPED) as usize;
        thread::spawn(move || {
            assert_eq!(request.bytes.as_ptr() as usize, buffer);
            assert_eq!(
                (&*request.overlapped as *const OVERLAPPED) as usize,
                overlapped
            );
            drop(request);
        })
        .join()
        .unwrap();
    }
}
