//! Embassy-based local transport: static `Channel` for requests, `Signal` array for replies.
//!
//! Moves Rust types directly — no serialization. All storage is static.
//!
//! ## Usage
//!
//! ```ignore
//! use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
//!
//! // Static storage for the transport:
//! static TRANSPORT: EmbassyLocal<CriticalSectionRawMutex, MyReq, MyResp, 4, 8> =
//!     EmbassyLocal::new();
//!
//! // In client task:
//! let client = TRANSPORT.client();
//! // In server task:
//! let mut server = TRANSPORT.server();
//! ```

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;

use core::sync::atomic::{AtomicU8, Ordering};

use crate::transport::{ClientTransport, ServerTransport};

/// Static embassy local transport.
///
/// - `M`: `RawMutex` implementation (e.g., `CriticalSectionRawMutex`)
/// - `Req`: request enum
/// - `Resp`: response enum
/// - `MAX_CALLERS`: number of concurrent reply slots (pre-allocated)
/// - `CHANNEL_DEPTH`: depth of the request channel
pub struct EmbassyLocal<M: RawMutex, Req, Resp, const MAX_CALLERS: usize, const CHANNEL_DEPTH: usize>
{
    /// Request channel: carries (request, reply_slot_index).
    requests: Channel<M, (Req, u8), CHANNEL_DEPTH>,
    /// Pre-allocated reply signals, one per caller slot.
    replies: [Signal<M, Resp>; MAX_CALLERS],
    /// Bitmask of free reply slots (supports up to 8 callers).
    free_slots: AtomicU8,
}

impl<M: RawMutex, Req, Resp, const MAX_CALLERS: usize, const CHANNEL_DEPTH: usize>
    EmbassyLocal<M, Req, Resp, MAX_CALLERS, CHANNEL_DEPTH>
{
    /// Create a new transport. All storage is inline — use in a `static`.
    pub const fn new() -> Self {
        assert!(MAX_CALLERS <= 8, "MAX_CALLERS must be <= 8");
        Self {
            requests: Channel::new(),
            replies: [const { Signal::new() }; MAX_CALLERS],
            // All slots free: lower MAX_CALLERS bits set.
            free_slots: AtomicU8::new((1u8 << MAX_CALLERS as u8) - 1),
        }
    }

    /// Get a client handle (cheap reference, can be copied/shared).
    pub fn client(&self) -> EmbassyClient<'_, M, Req, Resp, MAX_CALLERS, CHANNEL_DEPTH> {
        EmbassyClient { transport: self }
    }

    /// Get a server handle.
    pub fn server(&self) -> EmbassyServer<'_, M, Req, Resp, MAX_CALLERS, CHANNEL_DEPTH> {
        EmbassyServer { transport: self }
    }

    /// Try to acquire a free reply slot. Returns the slot index.
    fn acquire_slot(&self) -> Option<u8> {
        loop {
            let free = self.free_slots.load(Ordering::Acquire);
            if free == 0 {
                return None;
            }
            let slot = free.trailing_zeros() as u8;
            let mask = 1u8 << slot;
            if self
                .free_slots
                .compare_exchange_weak(free, free & !mask, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                // Reset the signal so we don't see a stale value.
                self.replies[slot as usize].reset();
                return Some(slot);
            }
        }
    }

    /// Release a reply slot back to the pool.
    fn release_slot(&self, slot: u8) {
        let mask = 1u8 << slot;
        self.free_slots.fetch_or(mask, Ordering::Release);
    }
}

// -- Error --

/// Errors from the embassy local transport.
#[derive(Debug)]
pub enum EmbassyLocalError {
    /// No reply slots available (all MAX_CALLERS are in use).
    NoReplySlot,
}

impl core::fmt::Display for EmbassyLocalError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EmbassyLocalError::NoReplySlot => write!(f, "no reply slot available"),
        }
    }
}

// -- Client --

/// Client handle to an embassy local transport.
pub struct EmbassyClient<
    'a,
    M: RawMutex,
    Req,
    Resp,
    const MAX_CALLERS: usize,
    const CHANNEL_DEPTH: usize,
> {
    transport: &'a EmbassyLocal<M, Req, Resp, MAX_CALLERS, CHANNEL_DEPTH>,
}

// Manually implement Clone/Copy since derive can't handle the generics easily.
impl<'a, M: RawMutex, Req, Resp, const MAX_CALLERS: usize, const CHANNEL_DEPTH: usize> Clone
    for EmbassyClient<'a, M, Req, Resp, MAX_CALLERS, CHANNEL_DEPTH>
{
    fn clone(&self) -> Self {
        *self
    }
}

impl<'a, M: RawMutex, Req, Resp, const MAX_CALLERS: usize, const CHANNEL_DEPTH: usize> Copy
    for EmbassyClient<'a, M, Req, Resp, MAX_CALLERS, CHANNEL_DEPTH>
{
}

impl<'a, M: RawMutex, Req, Resp, const MAX_CALLERS: usize, const CHANNEL_DEPTH: usize>
    ClientTransport<Req, Resp> for EmbassyClient<'a, M, Req, Resp, MAX_CALLERS, CHANNEL_DEPTH>
{
    type Error = EmbassyLocalError;

    async fn call(&self, req: Req) -> Result<Resp, Self::Error> {
        let slot = self
            .transport
            .acquire_slot()
            .ok_or(EmbassyLocalError::NoReplySlot)?;

        // Send request with reply slot index.
        self.transport.requests.send((req, slot)).await;

        // Await the reply.
        let resp = self.transport.replies[slot as usize].wait().await;

        // Release the slot.
        self.transport.release_slot(slot);

        Ok(resp)
    }
}

// -- Server --

/// Server handle to an embassy local transport.
pub struct EmbassyServer<
    'a,
    M: RawMutex,
    Req,
    Resp,
    const MAX_CALLERS: usize,
    const CHANNEL_DEPTH: usize,
> {
    transport: &'a EmbassyLocal<M, Req, Resp, MAX_CALLERS, CHANNEL_DEPTH>,
}

/// Reply token for embassy transport — just the slot index.
pub struct EmbassyReplyToken(u8);

impl<'a, M: RawMutex, Req, Resp, const MAX_CALLERS: usize, const CHANNEL_DEPTH: usize>
    ServerTransport<Req, Resp>
    for EmbassyServer<'a, M, Req, Resp, MAX_CALLERS, CHANNEL_DEPTH>
{
    type Error = EmbassyLocalError;
    type ReplyToken = EmbassyReplyToken;

    async fn recv(&mut self) -> Result<(Req, Self::ReplyToken), Self::Error> {
        let (req, slot) = self.transport.requests.receive().await;
        Ok((req, EmbassyReplyToken(slot)))
    }

    async fn reply(&self, token: Self::ReplyToken, resp: Resp) -> Result<(), Self::Error> {
        self.transport.replies[token.0 as usize].signal(resp);
        Ok(())
    }
}
