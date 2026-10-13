//! ViGEm output. Notifications block in the driver and unplugging the target
//! cancels their wait before the owned reader is joined.
use std::os::windows::io::AsRawHandle;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use vigem_client::{Client, XGamepad, Xbox360Wired};

use super::{BackendError, EmulatedController, Rumble, RumbleSink, VirtualPad, VirtualPadBackend};
use crate::controllers::mapping::XReport;

pub struct Windows;
trait Driver: Send {
    fn plug(&mut self) -> Result<(), BackendError>;
    fn ready(&mut self) -> Result<(), BackendError>;
    fn index(&mut self) -> Result<u16, BackendError>;
    fn update(&mut self, report: &XGamepad) -> Result<(), BackendError>;
    fn feedback(&mut self, sink: RumbleSink) -> Result<Option<JoinHandle<()>>, BackendError>;
    fn unplug(&mut self) -> Result<(), BackendError>;
}
struct Native {
    target: Xbox360Wired<Client>,
    stopped: Arc<Mutex<bool>>,
}
impl Driver for Native {
    fn plug(&mut self) -> Result<(), BackendError> {
        self.target.plugin().map_err(driver_error)
    }
    fn ready(&mut self) -> Result<(), BackendError> {
        self.target.wait_ready().map_err(driver_error)
    }
    fn index(&mut self) -> Result<u16, BackendError> {
        u16::try_from(self.target.get_user_index().map_err(driver_error)?)
            .map_err(|error| BackendError::Driver(error.to_string()))
    }
    fn update(&mut self, report: &XGamepad) -> Result<(), BackendError> {
        self.target.update(report).map_err(driver_error)
    }
    fn feedback(&mut self, sink: RumbleSink) -> Result<Option<JoinHandle<()>>, BackendError> {
        let notification = self.target.request_notification().map_err(driver_error)?;
        let stopped = self.stopped.clone();
        Ok(Some(
            thread::Builder::new()
                .name("vgames-rumble".into())
                .spawn(move || {
                    let mut request = Box::pin(notification);
                    loop {
                        let Ok(stop) = stopped.lock() else {
                            break;
                        };
                        if *stop {
                            break;
                        }
                        // Enqueue while holding the stop lock. Cancellation cannot run
                        // before a request that already passed this predicate is issued.
                        request.as_mut().request();
                        drop(stop);
                        match request.as_mut().poll(true) {
                            Ok(Some(rumble)) => sink(Rumble {
                                large: u16::from(rumble.large_motor) * 257,
                                small: u16::from(rumble.small_motor) * 257,
                                duration_ms: if rumble.large_motor == 0 && rumble.small_motor == 0 {
                                    0
                                } else {
                                    u32::MAX
                                },
                                delay_ms: 0,
                            }),
                            Ok(None) => {}
                            Err(_) => break,
                        }
                    }
                })?,
        ))
    }

    fn unplug(&mut self) -> Result<(), BackendError> {
        let mut stopped = self.stopped.lock().map_err(|_| BackendError::State)?;
        *stopped = true;
        let result = if self.target.is_attached() {
            self.target.unplug().map_err(driver_error)
        } else {
            Ok(())
        };
        #[allow(unsafe_code)]
        // SAFETY: this live client owns the file object duplicated by the SDK's
        // notification client. Cancel all its IRPs while holding the request/stop
        // lock; no notification can be submitted after this cancellation.
        unsafe {
            CancelIoEx(self.target.client().as_raw_handle(), std::ptr::null_mut());
        }
        result
    }
}
struct Pad<D: Driver> {
    driver: D,
    index: u16,
    feedback: Option<JoinHandle<()>>,
}
impl<D: Driver> Pad<D> {
    fn create(driver: D, sink: RumbleSink) -> Result<Self, BackendError> {
        let mut pad = Self {
            driver,
            index: 0,
            feedback: None,
        };
        pad.driver.plug()?;
        pad.driver.ready()?;
        pad.index = pad.driver.index()?;
        pad.feedback = pad.driver.feedback(sink)?;
        Ok(pad)
    }
}
impl VirtualPadBackend for Windows {
    fn create(
        &self,
        target: EmulatedController,
        _player_index: u16,
        sink: RumbleSink,
    ) -> Result<Box<dyn VirtualPad>, BackendError> {
        if target == EmulatedController::Dualshock4 {
            return super::windows_ds4::create(_player_index, sink);
        }
        let client = Client::connect().map_err(driver_error)?;
        let driver = Native {
            target: Xbox360Wired::new(client, vigem_client::TargetId::XBOX360_WIRED),
            stopped: Arc::new(Mutex::new(false)),
        };
        Ok(Box::new(Pad::create(driver, sink)?))
    }
}
impl<D: Driver> VirtualPad for Pad<D> {
    fn update(&mut self, report: &XReport) -> Result<(), BackendError> {
        if self
            .feedback
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            return Err(BackendError::Stopped);
        }
        self.driver.update(&XGamepad {
            buttons: report.buttons.into(),
            left_trigger: report.left_trigger,
            right_trigger: report.right_trigger,
            thumb_lx: report.left.0,
            thumb_ly: report.left.1,
            thumb_rx: report.right.0,
            thumb_ry: report.right.1,
        })
    }
    fn player_index(&self) -> u16 {
        self.index
    }
}
impl<D: Driver> Drop for Pad<D> {
    fn drop(&mut self) {
        if let Err(error) = self.driver.unplug() {
            tracing::warn!(%error, "cannot unplug virtual controller");
        }
        if let Some(worker) = self.feedback.take()
            && worker.join().is_err()
        {
            tracing::warn!("virtual controller notification thread panicked");
        }
    }
}
pub(super) fn driver_error(error: vigem_client::Error) -> BackendError {
    BackendError::Driver(error.to_string())
}

#[allow(unsafe_code)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CancelIoEx(file: *mut std::ffi::c_void, overlapped: *mut std::ffi::c_void) -> i32;
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Trace {
        attached: bool,
        updates: Vec<XGamepad>,
        unplugged: usize,
    }
    struct Fake {
        trace: Arc<Mutex<Trace>>,
        ready_error: bool,
    }
    impl Driver for Fake {
        fn plug(&mut self) -> Result<(), BackendError> {
            self.trace.lock().unwrap().attached = true;
            Ok(())
        }
        fn ready(&mut self) -> Result<(), BackendError> {
            if self.ready_error {
                Err(BackendError::Stopped)
            } else {
                Ok(())
            }
        }
        fn index(&mut self) -> Result<u16, BackendError> {
            Ok(2)
        }
        fn update(&mut self, report: &XGamepad) -> Result<(), BackendError> {
            self.trace.lock().unwrap().updates.push(*report);
            Ok(())
        }
        fn feedback(&mut self, _: RumbleSink) -> Result<Option<JoinHandle<()>>, BackendError> {
            Ok(None)
        }
        fn unplug(&mut self) -> Result<(), BackendError> {
            let mut trace = self.trace.lock().unwrap();
            if trace.attached {
                trace.attached = false;
                trace.unplugged += 1;
            }
            Ok(())
        }
    }
    #[test]
    fn driver_reports_preserve_axes_and_cleanup_after_normal_or_failed_preparation() {
        let trace = Arc::new(Mutex::new(Trace::default()));
        let mut pad = Pad::create(
            Fake {
                trace: trace.clone(),
                ready_error: false,
            },
            Arc::new(|_| {}),
        )
        .unwrap();
        pad.update(&XReport {
            buttons: 0x1000,
            left_trigger: 255,
            right_trigger: 17,
            left: (-32768, 32767),
            right: (123, -456),
        })
        .unwrap();
        assert_eq!(pad.player_index(), 2);
        let report = trace.lock().unwrap().updates[0];
        assert_eq!(report.buttons.raw, 0x1000);
        assert_eq!(
            (
                report.thumb_lx,
                report.thumb_ly,
                report.thumb_rx,
                report.thumb_ry
            ),
            (-32768, 32767, 123, -456)
        );
        assert_eq!((report.left_trigger, report.right_trigger), (255, 17));
        drop(pad);
        assert_eq!(trace.lock().unwrap().unplugged, 1);
        assert!(
            Pad::create(
                Fake {
                    trace: trace.clone(),
                    ready_error: true
                },
                Arc::new(|_| {})
            )
            .is_err()
        );
        assert_eq!(trace.lock().unwrap().unplugged, 2);
        assert!(!trace.lock().unwrap().attached);
    }
}
