//! Error types and result helpers for myelin service calls.

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

/// Convert a transport `Result<T, E>` into the appropriate user-facing type.
///
/// - For `E = Infallible`: returns `T` directly (the call can't fail).
/// - For other `E`: returns `Result<T, CallError<E>>`.
///
/// Used by generated client methods to adapt return types based on transport.
pub trait TransportResult<T>: Sized {
    /// The user-facing return type.
    type Output;

    /// Convert a transport result into the user-facing type.
    fn into_output(result: Result<T, Self>) -> Self::Output;
}

// -- Infallible: unwrap directly --

impl<T> TransportResult<T> for core::convert::Infallible {
    type Output = T;

    fn into_output(result: Result<T, Self>) -> T {
        match result {
            Ok(val) => val,
            Err(inf) => match inf {},
        }
    }
}
