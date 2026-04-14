//! Error types for chanapi service calls.

use core::fmt;

/// Error returned to a client when a service call fails.
///
/// `E` is the transport-specific error type.
#[derive(Debug)]
pub enum CallError<E> {
    /// The transport failed (channel closed, I/O error, etc.)
    Transport(E),
    /// The service dropped the request without replying.
    Cancelled,
}

impl<E: fmt::Display> fmt::Display for CallError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallError::Transport(e) => write!(f, "transport error: {e}"),
            CallError::Cancelled => write!(f, "request cancelled by service"),
        }
    }
}
