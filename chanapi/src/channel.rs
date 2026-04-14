//! Core channel types for request/response communication between tasks.

/// A oneshot-style reply slot. The service task sends its response here.
///
/// This is the building block for cancel safety: dropping the `ReplySender`
/// without sending is observable by the caller (they get an error), and
/// dropping the caller's receive half is observable by the service (the
/// send fails).
pub struct ReplySender<T> {
    _phantom: core::marker::PhantomData<T>,
}

/// The caller's half — awaits the response from the service.
pub struct ReplyReceiver<T> {
    _phantom: core::marker::PhantomData<T>,
}

/// Create a linked reply pair.
pub fn reply_pair<T>() -> (ReplySender<T>, ReplyReceiver<T>) {
    (
        ReplySender { _phantom: core::marker::PhantomData },
        ReplyReceiver { _phantom: core::marker::PhantomData },
    )
}
