use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::FusedStream;
use pktmon::filter::{PktMonFilter, TransportProtocol};
use pktmon::{Capture, Packet};

use crate::capture::{CaptureBackend, CaptureError, PORT_RANGE, Result};

pub struct PktmonBackend {
    stream: Box<dyn FusedStream<Item = Packet> + Unpin + Send>,
}

impl PktmonBackend {
    pub fn new() -> Result<Self> {
        // Both of these fail with an access-denied style error when the
        // process is not elevated, which is surfaced to the app as the
        // "capture could not start" status.
        let mut capture = Capture::new().map_err(|e| CaptureError::Capture(e.into()))?;

        for port in [PORT_RANGE.0, PORT_RANGE.1] {
            let filter = PktMonFilter {
                name: format!("UDP {port}"),
                transport_protocol: Some(TransportProtocol::UDP),
                port: port.into(),
                ..PktMonFilter::default()
            };
            capture.add_filter(filter).map_err(|e| CaptureError::Filter(e.into()))?;
        }

        // The original code unwrapped this; a failure here is a real error and
        // has to reach the app instead of panicking inside a windowless
        // process.
        let stream = capture.stream().map_err(|e| CaptureError::Capture(e.into()))?;
        Ok(Self {
            stream: Box::new(stream.boxed().fuse()),
        })
    }
}

#[async_trait]
impl CaptureBackend for PktmonBackend {
    async fn next_packet(&mut self) -> Result<Vec<u8>> {
        futures::select! {
            // `PacketPayload::to_vec` is misleadingly named: it returns a
            // reference to the payload, so it has to be cloned out.
            packet = self.stream.select_next_some() => Ok(packet.payload.to_vec().clone()),
            complete => Err(CaptureError::CaptureClosed),
        }
    }
}
