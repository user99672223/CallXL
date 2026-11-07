use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cxx::SharedPtr;
use tokio::sync::mpsc;
use webrtc_sys::data_channel::{self as dcm, ffi as dc};

use crate::peer::{MediaEvent, PeerId, push};

pub use dc::DataState;

static NEXT_CHANNEL: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct DataChannelOptions {
    pub ordered: bool,
    pub reliable: bool,
    /// Pre-negotiated stream id (both sides create the channel with the same id).
    pub negotiated_id: Option<i32>,
}

impl Default for DataChannelOptions {
    fn default() -> Self {
        Self { ordered: true, reliable: true, negotiated_id: None }
    }
}

impl DataChannelOptions {
    pub(crate) fn to_init(&self) -> dc::DataChannelInit {
        dc::DataChannelInit {
            ordered: self.ordered,
            has_max_retransmit_time: false,
            max_retransmit_time: 0,
            has_max_retransmits: !self.reliable,
            max_retransmits: 0,
            protocol: String::new(),
            negotiated: self.negotiated_id.is_some(),
            id: self.negotiated_id.unwrap_or(-1),
            has_priority: false,
            priority: dc::Priority::Low,
        }
    }
}

pub enum DataChannelEvent {
    State(DataState),
    Message { data: Vec<u8>, binary: bool },
    BufferedAmount(u64),
}

struct Observer {
    peer: PeerId,
    channel: u64,
    tx: mpsc::Sender<MediaEvent>,
}

impl dcm::DataChannelObserver for Observer {
    fn on_state_change(&self, state: DataState) {
        push(&self.tx, MediaEvent::Data { peer: self.peer, channel: self.channel, event: DataChannelEvent::State(state) });
    }
    fn on_message(&self, data: &[u8], binary: bool) {
        push(
            &self.tx,
            MediaEvent::Data {
                peer: self.peer,
                channel: self.channel,
                event: DataChannelEvent::Message { data: data.to_vec(), binary },
            },
        );
    }
    fn on_buffered_amount_change(&self, _sent: u64) {
        // Buffered amount is polled by the sender; no event needed per packet.
    }
}

/// An SCTP data channel on an existing PeerConnection (DTLS-encrypted, §6).
pub struct DataChannel {
    handle: u64,
    dc: SharedPtr<dc::DataChannel>,
}
// SAFETY: libwebrtc data channels are thread-safe proxies.
unsafe impl Send for DataChannel {}
unsafe impl Sync for DataChannel {}

impl DataChannel {
    pub(crate) fn attach(dc: SharedPtr<dc::DataChannel>, peer: PeerId, tx: mpsc::Sender<MediaEvent>) -> Self {
        let handle = NEXT_CHANNEL.fetch_add(1, Ordering::Relaxed);
        if let Some(d) = dc.as_ref() {
            d.register_observer(Box::new(dcm::DataChannelObserverWrapper::new(Arc::new(Observer {
                peer,
                channel: handle,
                tx,
            }))));
        }
        Self { handle, dc }
    }

    pub fn handle(&self) -> u64 {
        self.handle
    }

    pub fn label(&self) -> String {
        self.dc.as_ref().map(|d| d.label()).unwrap_or_default()
    }

    pub fn state(&self) -> DataState {
        self.dc.as_ref().map(|d| d.state()).unwrap_or(DataState::Closed)
    }

    pub fn is_open(&self) -> bool {
        self.state() == DataState::Open
    }

    /// Queues `data`; returns false if the channel is not open or its buffer is full.
    pub fn send(&self, data: &[u8], binary: bool) -> bool {
        let Some(d) = self.dc.as_ref() else { return false };
        d.send(&dc::DataBuffer { ptr: data.as_ptr(), len: data.len(), binary })
    }

    pub fn buffered_amount(&self) -> u64 {
        self.dc.as_ref().map(|d| d.buffered_amount()).unwrap_or(0)
    }

    pub fn close(&self) {
        if let Some(d) = self.dc.as_ref() {
            d.close();
        }
    }
}

impl Drop for DataChannel {
    fn drop(&mut self) {
        if let Some(d) = self.dc.as_ref() {
            d.unregister_observer();
        }
    }
}
