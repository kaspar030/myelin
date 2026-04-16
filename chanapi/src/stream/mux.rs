//! Wire-level API multiplexing: `api_id` routing for multiple APIs over one byte stream.
//!
//! This module adds runtime `api_id`-based routing so multiple independent
//! APIs can share a single framed byte stream. Each message is prefixed with
//! a `u16` API identifier before the per-API payload. The framing layer
//! handles length-delimiting of the entire message (including the `api_id`
//! prefix).
//!
//! ## Wire format
//!
//! ```text
//! [framing header (managed by Framer)]
//! [u16 api_id LE][per-API payload: optional u8 slot_id + encoded request/response]
//! [framing footer if any]
//! ```
//!
//! ## Architecture
//!
//! - **Server side**: [`ApiRouter`] maps `api_id → Box<dyn ApiHandler>`.
//!   It peels off the 2-byte `api_id` prefix, dispatches to the matching
//!   handler, and prepends `api_id` to the response.
//! - **Client side**: [`prefix_api_id`] and [`strip_api_id`] are helper
//!   functions that prepend/strip the `api_id` to/from raw byte slices.
//!   These compose underneath `StreamTransport` — the typed client already
//!   implements `ClientTransport` over its own transport.
//!
//! ## Relationship to `compose_service!`
//!
//! - `compose_service!` is compile-time composition: one enum, one dispatch,
//!   no `api_id` overhead. Best for local transports and when all APIs are
//!   known at compile time.
//! - Wire-level multiplexing is runtime composition: each API is independent,
//!   `api_id` is on the wire. Better for stream transports where APIs may be
//!   added dynamically or where you want to avoid one mega-enum.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

/// The size of the `api_id` prefix in bytes.
pub const API_ID_LEN: usize = 2;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from the API multiplexing layer.
#[derive(Debug)]
pub enum MuxError {
    /// No handler registered for this `api_id`.
    UnknownApiId(u16),
    /// Frame too short to contain an `api_id` (< 2 bytes).
    FrameTooShort,
    /// Error from a per-API handler.
    Handler(Box<dyn std::error::Error + Send + Sync>),
}

impl core::fmt::Display for MuxError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MuxError::UnknownApiId(id) => write!(f, "unknown api_id: 0x{id:04x}"),
            MuxError::FrameTooShort => write!(f, "frame too short for api_id (< 2 bytes)"),
            MuxError::Handler(e) => write!(f, "handler error: {e}"),
        }
    }
}

impl std::error::Error for MuxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MuxError::Handler(e) => Some(&**e),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ServiceApiId — marker trait
// ---------------------------------------------------------------------------

/// Trait for services that declare a wire-level API identifier.
///
/// Each service that participates in wire-level multiplexing must define
/// a unique `API_ID`. This is what the proc macro would eventually generate
/// (e.g., via a hash of the trait name).
pub trait ServiceApiId {
    /// The unique wire-level API identifier for this service.
    const API_ID: u16;
}

// ---------------------------------------------------------------------------
// ApiHandler — per-API request handler
// ---------------------------------------------------------------------------

/// A handler for a single API's requests.
///
/// Operates on raw bytes (after `api_id` has been stripped). The handler is
/// responsible for its own encoding/decoding of the per-API payload.
///
/// This trait is object-safe (returns `Pin<Box<dyn Future>>`) to support
/// runtime registration of handlers from independent crates.
pub trait ApiHandler: Send + Sync {
    /// Handle a request and produce a response.
    ///
    /// `request` is the raw bytes after the `api_id` prefix has been
    /// stripped. Returns the raw response bytes (the mux layer will prepend
    /// the `api_id`).
    fn handle<'a>(
        &'a self,
        request: &'a [u8],
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>>
                + Send
                + 'a,
        >,
    >;
}

// Blanket impl: any `Fn(&[u8]) -> Result<Vec<u8>, ...>` can be an ApiHandler.
impl<F> ApiHandler for F
where
    F: Fn(&[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> + Send + Sync,
{
    fn handle<'a>(
        &'a self,
        request: &'a [u8],
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>>
                + Send
                + 'a,
        >,
    > {
        let result = (self)(request);
        Box::pin(async move { result })
    }
}

// ---------------------------------------------------------------------------
// ApiRouter — server-side registry
// ---------------------------------------------------------------------------

/// Server-side API router: maps `api_id → handler`.
///
/// Reads a complete frame, peels off the 2-byte `api_id` prefix, looks up
/// the handler, dispatches the remaining bytes, and prepends the `api_id`
/// to the response.
pub struct ApiRouter {
    handlers: HashMap<u16, Box<dyn ApiHandler>>,
}

impl ApiRouter {
    /// Create a new empty router.
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Register a handler for an `api_id`.
    ///
    /// Returns `&mut Self` for builder-style chaining.
    ///
    /// # Panics
    ///
    /// Does not panic on duplicate `api_id` — the new handler replaces the
    /// old one silently. Use different `api_id` values for different APIs.
    pub fn register(&mut self, api_id: u16, handler: impl ApiHandler + 'static) -> &mut Self {
        self.handlers.insert(api_id, Box::new(handler));
        self
    }

    /// Dispatch a complete frame (including `api_id` prefix).
    ///
    /// Returns the response frame (including `api_id` prefix).
    ///
    /// 1. Checks `frame.len() >= 2`, returns `FrameTooShort` if not.
    /// 2. Reads `u16::from_le_bytes(frame[0..2])` as `api_id`.
    /// 3. Looks up the handler, returns `UnknownApiId` if not found.
    /// 4. Calls `handler.handle(&frame[2..])`.
    /// 5. Prepends `api_id.to_le_bytes()` to the response bytes.
    pub async fn dispatch(&self, frame: &[u8]) -> Result<Vec<u8>, MuxError> {
        if frame.len() < API_ID_LEN {
            return Err(MuxError::FrameTooShort);
        }

        let api_id = u16::from_le_bytes([frame[0], frame[1]]);
        let payload = &frame[API_ID_LEN..];

        let handler = self
            .handlers
            .get(&api_id)
            .ok_or(MuxError::UnknownApiId(api_id))?;

        let response = handler.handle(payload).await.map_err(MuxError::Handler)?;

        let mut result = Vec::with_capacity(API_ID_LEN + response.len());
        result.extend_from_slice(&api_id.to_le_bytes());
        result.extend_from_slice(&response);
        Ok(result)
    }

    /// Returns the number of registered handlers.
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// Returns `true` if no handlers are registered.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// Check whether a handler is registered for the given `api_id`.
    pub fn has_handler(&self, api_id: u16) -> bool {
        self.handlers.contains_key(&api_id)
    }
}

impl Default for ApiRouter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Client-side helpers
// ---------------------------------------------------------------------------

/// Prepend a 2-byte little-endian `api_id` to a payload.
///
/// Returns a new `Vec<u8>` containing `[api_id LE][payload]`.
pub fn prefix_api_id(api_id: u16, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(API_ID_LEN + payload.len());
    frame.extend_from_slice(&api_id.to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// Strip the 2-byte little-endian `api_id` from a frame and verify it matches.
///
/// Returns the payload bytes (after the `api_id` prefix) on success.
///
/// # Errors
///
/// - [`MuxError::FrameTooShort`] if `frame.len() < 2`.
/// - [`MuxError::UnknownApiId`] if the parsed `api_id` does not match
///   `expected_api_id`.
pub fn strip_api_id(expected_api_id: u16, frame: &[u8]) -> Result<&[u8], MuxError> {
    if frame.len() < API_ID_LEN {
        return Err(MuxError::FrameTooShort);
    }

    let actual = u16::from_le_bytes([frame[0], frame[1]]);
    if actual != expected_api_id {
        return Err(MuxError::UnknownApiId(actual));
    }

    Ok(&frame[API_ID_LEN..])
}

/// Parse the `api_id` from a frame without verification.
///
/// Returns `(api_id, payload)`.
///
/// # Errors
///
/// - [`MuxError::FrameTooShort`] if `frame.len() < 2`.
pub fn parse_api_id(frame: &[u8]) -> Result<(u16, &[u8]), MuxError> {
    if frame.len() < API_ID_LEN {
        return Err(MuxError::FrameTooShort);
    }

    let api_id = u16::from_le_bytes([frame[0], frame[1]]);
    Ok((api_id, &frame[API_ID_LEN..]))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Helper: echo handler that returns the request bytes reversed --
    fn echo_handler(req: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(req.to_vec())
    }

    fn reverse_handler(req: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        let mut v = req.to_vec();
        v.reverse();
        Ok(v)
    }

    fn error_handler(_req: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        Err("handler failed".into())
    }

    // -- prefix_api_id --

    #[test]
    fn test_prefix_api_id() {
        let frame = prefix_api_id(0x0001, b"hello");
        assert_eq!(frame.len(), 2 + 5);
        assert_eq!(&frame[..2], &[0x01, 0x00]); // LE
        assert_eq!(&frame[2..], b"hello");
    }

    #[test]
    fn test_prefix_api_id_empty_payload() {
        let frame = prefix_api_id(0xABCD, &[]);
        assert_eq!(frame.len(), 2);
        assert_eq!(&frame, &[0xCD, 0xAB]);
    }

    #[test]
    fn test_prefix_api_id_large_id() {
        let frame = prefix_api_id(0xFFFF, &[42]);
        assert_eq!(&frame[..2], &[0xFF, 0xFF]);
        assert_eq!(frame[2], 42);
    }

    // -- strip_api_id --

    #[test]
    fn test_strip_api_id_success() {
        let frame = prefix_api_id(0x0001, b"hello");
        let payload = strip_api_id(0x0001, &frame).unwrap();
        assert_eq!(payload, b"hello");
    }

    #[test]
    fn test_strip_api_id_wrong_id() {
        let frame = prefix_api_id(0x0001, b"hello");
        let err = strip_api_id(0x0002, &frame).unwrap_err();
        match err {
            MuxError::UnknownApiId(id) => assert_eq!(id, 0x0001),
            other => panic!("expected UnknownApiId, got: {other}"),
        }
    }

    #[test]
    fn test_strip_api_id_too_short_empty() {
        let err = strip_api_id(0x0001, &[]).unwrap_err();
        assert!(matches!(err, MuxError::FrameTooShort));
    }

    #[test]
    fn test_strip_api_id_too_short_one_byte() {
        let err = strip_api_id(0x0001, &[0x01]).unwrap_err();
        assert!(matches!(err, MuxError::FrameTooShort));
    }

    #[test]
    fn test_strip_api_id_exact_header() {
        // Frame with just the api_id, no payload.
        let frame = prefix_api_id(0x0042, &[]);
        let payload = strip_api_id(0x0042, &frame).unwrap();
        assert!(payload.is_empty());
    }

    // -- parse_api_id --

    #[test]
    fn test_parse_api_id() {
        let frame = prefix_api_id(0x1234, b"data");
        let (api_id, payload) = parse_api_id(&frame).unwrap();
        assert_eq!(api_id, 0x1234);
        assert_eq!(payload, b"data");
    }

    #[test]
    fn test_parse_api_id_too_short() {
        let err = parse_api_id(&[0x01]).unwrap_err();
        assert!(matches!(err, MuxError::FrameTooShort));
    }

    // -- ApiRouter --

    #[tokio::test]
    async fn test_router_dispatch() {
        let mut router = ApiRouter::new();
        router.register(0x0001, echo_handler);

        let frame = prefix_api_id(0x0001, b"test");
        let response = router.dispatch(&frame).await.unwrap();

        // Response should have the api_id prepended.
        let (api_id, payload) = parse_api_id(&response).unwrap();
        assert_eq!(api_id, 0x0001);
        assert_eq!(payload, b"test");
    }

    #[tokio::test]
    async fn test_router_unknown_api_id() {
        let router = ApiRouter::new();

        let frame = prefix_api_id(0x0099, b"test");
        let err = router.dispatch(&frame).await.unwrap_err();
        match err {
            MuxError::UnknownApiId(id) => assert_eq!(id, 0x0099),
            other => panic!("expected UnknownApiId, got: {other}"),
        }
    }

    #[tokio::test]
    async fn test_router_multiple_apis() {
        let mut router = ApiRouter::new();
        router.register(0x0001, echo_handler);
        router.register(0x0002, reverse_handler);

        // Dispatch to echo handler.
        let frame1 = prefix_api_id(0x0001, b"abc");
        let resp1 = router.dispatch(&frame1).await.unwrap();
        let (id1, payload1) = parse_api_id(&resp1).unwrap();
        assert_eq!(id1, 0x0001);
        assert_eq!(payload1, b"abc");

        // Dispatch to reverse handler.
        let frame2 = prefix_api_id(0x0002, b"abc");
        let resp2 = router.dispatch(&frame2).await.unwrap();
        let (id2, payload2) = parse_api_id(&resp2).unwrap();
        assert_eq!(id2, 0x0002);
        assert_eq!(payload2, b"cba");
    }

    #[tokio::test]
    async fn test_router_handler_error() {
        let mut router = ApiRouter::new();
        router.register(0x0001, error_handler);

        let frame = prefix_api_id(0x0001, b"test");
        let err = router.dispatch(&frame).await.unwrap_err();
        assert!(matches!(err, MuxError::Handler(_)));
    }

    #[tokio::test]
    async fn test_router_frame_too_short() {
        let router = ApiRouter::new();

        // Empty frame.
        let err = router.dispatch(&[]).await.unwrap_err();
        assert!(matches!(err, MuxError::FrameTooShort));

        // One byte frame.
        let err = router.dispatch(&[0x01]).await.unwrap_err();
        assert!(matches!(err, MuxError::FrameTooShort));
    }

    // -- ApiRouter builder --

    #[test]
    fn test_router_builder_chain() {
        let mut router = ApiRouter::new();
        router
            .register(0x0001, echo_handler)
            .register(0x0002, reverse_handler);

        assert_eq!(router.len(), 2);
        assert!(!router.is_empty());
        assert!(router.has_handler(0x0001));
        assert!(router.has_handler(0x0002));
        assert!(!router.has_handler(0x0003));
    }

    #[test]
    fn test_router_replace_handler() {
        let mut router = ApiRouter::new();
        router.register(0x0001, echo_handler);
        router.register(0x0001, reverse_handler);

        // Should still have exactly 1 handler.
        assert_eq!(router.len(), 1);
    }

    #[test]
    fn test_router_default() {
        let router = ApiRouter::default();
        assert!(router.is_empty());
    }

    // -- Round-trip: prefix then strip --

    #[test]
    fn test_prefix_strip_round_trip() {
        let api_id = 0x1234u16;
        let payload = b"hello world";

        let frame = prefix_api_id(api_id, payload);
        let stripped = strip_api_id(api_id, &frame).unwrap();
        assert_eq!(stripped, payload);
    }

    // -- MuxError Display --

    #[test]
    fn test_mux_error_display() {
        let err = MuxError::UnknownApiId(0x0042);
        assert_eq!(format!("{err}"), "unknown api_id: 0x0042");

        let err = MuxError::FrameTooShort;
        assert_eq!(format!("{err}"), "frame too short for api_id (< 2 bytes)");

        let err = MuxError::Handler("test error".into());
        assert_eq!(format!("{err}"), "handler error: test error");
    }

    // -- ServiceApiId trait --

    struct TestService;
    impl ServiceApiId for TestService {
        const API_ID: u16 = 0x0042;
    }

    #[test]
    fn test_service_api_id() {
        assert_eq!(TestService::API_ID, 0x0042);
    }
}
