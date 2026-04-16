//! Reply routing layer: how responses get matched to callers.
//!
//! The simplest strategy is sequential — one request at a time, the next
//! reply goes to the single waiting caller. Future strategies (e.g.,
//! multiplexed slot-based routing) will add request IDs to the wire format.

/// Reply routing strategy marker.
///
/// This trait is intentionally minimal for the sequential case. When
/// multiplexed routing is added (e.g., `MuxedSlots<N>`), it will grow
/// methods for preparing request headers and parsing response headers.
pub trait ReplyRouter {}

/// Sequential reply routing: one request at a time, no multiplexing.
///
/// This is a zero-sized type — it adds no overhead and no wire-format
/// headers. The transport simply sends a request and reads the next
/// frame as the reply.
#[derive(Debug, Default, Clone, Copy)]
pub struct Sequential;

impl ReplyRouter for Sequential {}
