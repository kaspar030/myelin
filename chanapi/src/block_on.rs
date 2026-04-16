//! Blocking executor trait for sync client wrappers.

use core::future::Future;

/// A blocking executor that can run a future to completion.
///
/// Implemented for different runtimes:
/// - Ariel OS threads: `ariel_os::thread::block_on`
/// - Embassy futures: `embassy_futures::block_on`
///
/// The implementation must poll the future to completion and return its output.
pub trait BlockOn {
    /// Run a future to completion, blocking the current thread.
    fn block_on<F: Future>(&self, fut: F) -> F::Output;
}

// Blanket impl: &B is a BlockOn if B is.
impl<B: BlockOn> BlockOn for &B {
    fn block_on<F: Future>(&self, fut: F) -> F::Output {
        (**self).block_on(fut)
    }
}
