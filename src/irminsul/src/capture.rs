//! Packet capture.
//!
//! Only the Windows `pktmon` backend is kept: it is part of the operating
//! system, so unlike Npcap it needs no third-party driver or separate
//! installation.  It does require the process to be elevated, which is why the
//! RyukinLedger app starts this program with the `runas` verb.

#[cfg(windows)]
mod pktmon_backend;

use std::fmt::{Display, Formatter};

use anyhow::Error;
use async_trait::async_trait;

/// The UDP port range used by the game servers.
pub const PORT_RANGE: (u16, u16) = (22101, 22102);

#[derive(Debug)]
pub enum CaptureError {
    /// The capture filter could not be installed.
    Filter(Error),
    /// The capture session could not be created (usually missing privileges).
    Capture(Error),
    /// The capture session itself ended.
    CaptureClosed,
}

impl Display for CaptureError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::Filter(e) => write!(f, "could not install the packet capture filter: {e}"),
            CaptureError::Capture(e) => write!(f, "{e}"),
            CaptureError::CaptureClosed => write!(f, "the packet capture session ended"),
        }
    }
}

impl std::error::Error for CaptureError {}

pub type Result<T> = std::result::Result<T, CaptureError>;

#[async_trait]
pub trait CaptureBackend: Send {
    async fn next_packet(&mut self) -> Result<Vec<u8>>;
}

/// Start capturing the game's traffic.
pub fn create_capture() -> Result<Box<dyn CaptureBackend>> {
    #[cfg(windows)]
    {
        Ok(Box::new(pktmon_backend::PktmonBackend::new()?))
    }
    #[cfg(not(windows))]
    {
        Err(CaptureError::Capture(anyhow::anyhow!(
            "packet capture is only implemented on Windows"
        )))
    }
}
