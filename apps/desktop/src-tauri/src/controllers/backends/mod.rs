//! Virtual output is isolated from SDL and launch policy. Drivers can be
//! replaced without changing mapping, session decisions or physical input.
use std::sync::Arc;

use super::mapping::XReport;
pub use vgames_core::manifest::EmulatedController;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
mod linux_feedback;
#[cfg(windows)]
pub mod windows;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("Virtual controller I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("The virtual controller worker has stopped.")]
    Stopped,
    #[error("Virtual controller state is unavailable.")]
    State,
    #[error("The virtual controller driver is unavailable: {0}")]
    Driver(String),
    #[error("The restricted macOS controller entitlement is missing.")]
    EntitlementMissing,
    #[error("This virtual controller target is not supported on this platform.")]
    UnsupportedTarget,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rumble {
    pub large: u16,
    pub small: u16,
    pub duration_ms: u32,
    pub delay_ms: u16,
}
pub type RumbleSink = Arc<dyn Fn(Rumble) + Send + Sync>;

pub trait VirtualPad: Send {
    fn update(&mut self, report: &XReport) -> Result<(), BackendError>;
    fn player_index(&self) -> u16;
}
pub trait VirtualPadBackend: Send + Sync {
    fn create(
        &self,
        target: EmulatedController,
        player_index: u16,
        rumble: RumbleSink,
    ) -> Result<Box<dyn VirtualPad>, BackendError>;
}

#[cfg(windows)]
mod windows_ds4;

#[cfg(any(windows, test))]
mod windows_ds4_report;
