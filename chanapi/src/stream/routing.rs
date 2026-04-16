//! Reply routing layer: how responses get matched to callers.
//!
//! Two strategies are provided:
//!
//! - [`Sequential`] — one request at a time, zero overhead, no wire-format
//!   headers. The next reply goes to the single waiting caller.
//!
//! - [`MuxedSlots<N, BUF>`](MuxedSlots) — up to `N` concurrent in-flight
//!   requests over a single byte stream. Each frame is prefixed with a 1-byte
//!   slot ID. Slot allocation uses an atomic bitmap (no allocator needed).

use core::cell::UnsafeCell;
use core::future::Future;
use core::sync::atomic::{AtomicU32, AtomicU16, Ordering};
use core::task::{Poll, Waker};

/// Reply routing strategy.
///
/// Implementations define how outgoing requests are tagged and how incoming
/// replies are dispatched to the correct caller.
pub trait ReplyRouter {
    /// Number of bytes prepended to each frame for routing metadata.
    ///
    /// `0` for [`Sequential`], `1` for [`MuxedSlots`].
    const HEADER_LEN: usize;

    /// Opaque slot handle returned by [`acquire`](Self::acquire).
    /// Freed on drop (cancel-safe).
    type SlotHandle<'a>: RouterSlotHandle
    where
        Self: 'a;

    /// Error type for slot allocation failures.
    type Error;

    /// Acquire a routing slot.
    ///
    /// For [`Sequential`], always succeeds immediately.
    /// For [`MuxedSlots`], waits asynchronously if all slots are occupied.
    fn acquire(&self) -> impl Future<Output = Result<Self::SlotHandle<'_>, Self::Error>>;

    /// Try to acquire a slot without waiting.
    ///
    /// Returns `None` if all slots are in use.
    fn try_acquire(&self) -> Option<Self::SlotHandle<'_>>;

    /// Write this slot's routing header into `buf[..HEADER_LEN]`.
    fn write_header(slot: &Self::SlotHandle<'_>, buf: &mut [u8]);

    /// Parse the slot ID from an incoming frame header `buf[..HEADER_LEN]`.
    fn parse_header(buf: &[u8]) -> u8;

    /// Deliver a reply payload to the given slot.
    ///
    /// Called by the cooperative demux reader. If the slot has been freed
    /// (guard dropped due to cancellation), the delivery is silently
    /// discarded.
    fn deliver(&self, slot_id: u8, payload: &[u8]);
}

/// A routing slot handle: carries the slot ID and can await a reply.
pub trait RouterSlotHandle {
    /// The slot index (written into frame headers).
    fn slot_id(&self) -> u8;

    /// Wait for a reply to be delivered to this slot.
    ///
    /// Returns the reply payload bytes.
    fn recv_reply(&self) -> impl Future<Output = &[u8]>;
}

// ---------------------------------------------------------------------------
// Sequential — zero overhead, one request at a time
// ---------------------------------------------------------------------------

/// Sequential reply routing: one request at a time, no multiplexing.
///
/// Zero-sized type — adds no overhead and no wire-format headers. The
/// transport simply sends a request and reads the next frame as the reply.
#[derive(Debug, Default, Clone, Copy)]
pub struct Sequential;

impl ReplyRouter for Sequential {
    const HEADER_LEN: usize = 0;
    type SlotHandle<'a> = SequentialSlot;
    type Error = core::convert::Infallible;

    async fn acquire(&self) -> Result<SequentialSlot, core::convert::Infallible> {
        Ok(SequentialSlot)
    }

    fn try_acquire(&self) -> Option<SequentialSlot> {
        Some(SequentialSlot)
    }

    fn write_header(_slot: &SequentialSlot, _buf: &mut [u8]) {
        // No header bytes.
    }

    fn parse_header(_buf: &[u8]) -> u8 {
        0
    }

    fn deliver(&self, _slot_id: u8, _payload: &[u8]) {
        // Sequential mode: replies are read inline, not delivered through slots.
    }
}

/// Slot handle for [`Sequential`] routing — a unit struct.
pub struct SequentialSlot;

impl RouterSlotHandle for SequentialSlot {
    fn slot_id(&self) -> u8 {
        0
    }

    async fn recv_reply(&self) -> &[u8] {
        // Never called in sequential mode — replies are read directly by the
        // transport. This exists only to satisfy the trait.
        &[]
    }
}

// ---------------------------------------------------------------------------
// MuxedSlots<N, BUF> — concurrent multiplexed routing
// ---------------------------------------------------------------------------

/// Per-slot storage: waker + reply buffer.
struct SlotCell<const BUF: usize> {
    /// Reply data buffer, written by `deliver()`, read by `recv_reply()`.
    data: UnsafeCell<[u8; BUF]>,
    /// Length of valid data in the buffer.
    /// 0 means empty (no reply yet). Set atomically by `deliver()`.
    len: AtomicU16,
    /// Waker for the task waiting on this slot's reply.
    waker: UnsafeCell<Option<Waker>>,
}

impl<const BUF: usize> SlotCell<BUF> {
    const fn new() -> Self {
        Self {
            data: UnsafeCell::new([0u8; BUF]),
            len: AtomicU16::new(0),
            waker: UnsafeCell::new(None),
        }
    }
}

/// Multiplexed reply router supporting `N` concurrent in-flight requests.
///
/// Wire format: each frame is prefixed with a 1-byte slot ID. Slot
/// allocation uses an [`AtomicU32`] bitmap — no allocator needed.
///
/// # Type Parameters
///
/// - `N`: maximum concurrent slots (must be ≤ 32).
/// - `BUF`: per-slot reply buffer size in bytes.
///
/// # Cancel Safety
///
/// The slot guard ([`MuxedSlotGuard`]) frees its slot on drop, ensuring
/// that cancelled callers do not leak slots. If a reply arrives for a
/// freed slot, it is silently discarded.
pub struct MuxedSlots<const N: usize, const BUF: usize> {
    /// Bitmap: bit `i` is set when slot `i` is allocated.
    bitmap: AtomicU32,
    /// Per-slot state.
    slots: [SlotCell<BUF>; N],
    /// Waker for tasks blocked in `acquire()` waiting for a free slot.
    alloc_waker: UnsafeCell<Option<Waker>>,
}

// SAFETY: All UnsafeCell access is guarded by the atomic bitmap.
// - `SlotCell::data` and `SlotCell::waker` for slot `i` are only written
//   when bit `i` is set in the bitmap, and only by the holder of
//   `MuxedSlotGuard` (for waker) or `deliver()` (for data, which also
//   checks the bit).
// - `alloc_waker` is written only by tasks in `acquire()` under the
//   condition that no slot is free.
unsafe impl<const N: usize, const BUF: usize> Send for MuxedSlots<N, BUF> {}
unsafe impl<const N: usize, const BUF: usize> Sync for MuxedSlots<N, BUF> {}

impl<const N: usize, const BUF: usize> MuxedSlots<N, BUF> {
    // Compile-time assertion: N must be ≤ 32 (AtomicU32 bitmap).
    const _ASSERT_N_LE_32: () = assert!(N <= 32, "MuxedSlots: N must be <= 32");

    /// Create a new muxed router with all slots free.
    pub const fn new() -> Self {
        // Trigger the compile-time assertion.
        let () = Self::_ASSERT_N_LE_32;

        // Use a const block to initialize the array without Copy bound on SlotCell.
        // SAFETY: [SlotCell::new(); N] doesn't work because SlotCell isn't Copy,
        // so we use unsafe transmute from MaybeUninit.
        let slots = {
            let mut arr: [core::mem::MaybeUninit<SlotCell<BUF>>; N] =
                unsafe { core::mem::MaybeUninit::uninit().assume_init() };
            let mut i = 0;
            while i < N {
                arr[i] = core::mem::MaybeUninit::new(SlotCell::new());
                i += 1;
            }
            // SAFETY: All elements are now initialized.
            unsafe { core::mem::transmute_copy::<_, [SlotCell<BUF>; N]>(&arr) }
        };

        Self {
            bitmap: AtomicU32::new(0),
            slots,
            alloc_waker: UnsafeCell::new(None),
        }
    }

    /// The valid-slot mask: bits `0..N` are set.
    const fn valid_mask() -> u32 {
        if N == 32 {
            u32::MAX
        } else {
            (1u32 << N) - 1
        }
    }

    /// Try to allocate a slot via CAS on the bitmap.
    /// Returns `Some(index)` on success, `None` if all slots are occupied.
    fn try_alloc(&self) -> Option<u8> {
        loop {
            let current = self.bitmap.load(Ordering::Acquire);
            // Find first zero bit within the valid range.
            let free = (!current) & Self::valid_mask();
            if free == 0 {
                return None;
            }
            let idx = free.trailing_zeros() as u8;
            let new = current | (1u32 << idx);
            match self.bitmap.compare_exchange_weak(
                current,
                new,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(idx),
                Err(_) => continue, // Retry on contention.
            }
        }
    }

    /// Free a slot: clear its bitmap bit and wake any task waiting in `acquire()`.
    fn free_slot(&self, idx: u8) {
        // Reset the slot's data length so stale data isn't visible.
        self.slots[idx as usize].len.store(0, Ordering::Release);

        // Clear the bitmap bit.
        self.bitmap.fetch_and(!(1u32 << idx), Ordering::Release);

        // Wake any task waiting for a free slot.
        // SAFETY: alloc_waker is only written by tasks in acquire() and
        // read+cleared here. Races are benign: worst case we miss a wake
        // and the task re-polls from the executor's periodic wake.
        let waker = unsafe { &mut *self.alloc_waker.get() };
        if let Some(w) = waker.take() {
            w.wake();
        }
    }
}

impl<const N: usize, const BUF: usize> MuxedSlots<N, BUF> {
    /// Check if a slot has a pending reply and return it.
    ///
    /// Returns `Some(data)` if the slot has received a reply (len > 0),
    /// `None` otherwise. Used by the cooperative demux in the transport.
    ///
    /// # Safety
    ///
    /// The caller must hold a valid `MuxedSlotGuard` for `slot_id` (i.e.,
    /// the slot must be allocated). This is not enforced at the type level
    /// here — it is the transport's responsibility.
    pub fn try_recv_slot(&self, slot_id: u8) -> Option<&[u8]> {
        if (slot_id as usize) >= N {
            return None;
        }
        let slot = &self.slots[slot_id as usize];
        let len = slot.len.load(core::sync::atomic::Ordering::Acquire);
        if len > 0 {
            // SAFETY: data was written by deliver() with Release before
            // len was set. We read len with Acquire → happens-before.
            // SAFETY: data was written by deliver() with Release before
            // len was set. We read len with Acquire → happens-before.
            let arr = unsafe { &*slot.data.get() };
            Some(&arr[..len as usize])
        } else {
            None
        }
    }
}

impl<const N: usize, const BUF: usize> Default for MuxedSlots<N, BUF> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize, const BUF: usize> ReplyRouter for MuxedSlots<N, BUF> {
    const HEADER_LEN: usize = 1;

    type SlotHandle<'a> = MuxedSlotGuard<'a, N, BUF>;
    type Error = core::convert::Infallible;

    async fn acquire(&self) -> Result<MuxedSlotGuard<'_, N, BUF>, core::convert::Infallible> {
        // Fast path: try to allocate immediately.
        if let Some(idx) = self.try_alloc() {
            return Ok(MuxedSlotGuard {
                router: self,
                index: idx,
            });
        }

        // Slow path: all slots occupied — poll until one frees up.
        core::future::poll_fn(|cx| {
            // Re-check after registering waker.
            if let Some(idx) = self.try_alloc() {
                return Poll::Ready(Ok(MuxedSlotGuard {
                    router: self,
                    index: idx,
                }));
            }
            // Store our waker so `free_slot()` can wake us.
            // SAFETY: single-writer (this task), read by `free_slot`.
            unsafe {
                *self.alloc_waker.get() = Some(cx.waker().clone());
            }
            // Re-check to avoid lost-wakeup race.
            if let Some(idx) = self.try_alloc() {
                // Clear the waker since we're not going to sleep.
                unsafe { *self.alloc_waker.get() = None; }
                return Poll::Ready(Ok(MuxedSlotGuard {
                    router: self,
                    index: idx,
                }));
            }
            Poll::Pending
        })
        .await
    }

    fn try_acquire(&self) -> Option<MuxedSlotGuard<'_, N, BUF>> {
        self.try_alloc().map(|idx| MuxedSlotGuard {
            router: self,
            index: idx,
        })
    }

    fn write_header(slot: &MuxedSlotGuard<'_, N, BUF>, buf: &mut [u8]) {
        buf[0] = slot.index;
    }

    fn parse_header(buf: &[u8]) -> u8 {
        buf[0]
    }

    fn deliver(&self, slot_id: u8, payload: &[u8]) {
        debug_assert!((slot_id as usize) < N);
        if slot_id as usize >= N {
            return;
        }

        // Check if the slot is still allocated (guard not dropped).
        let bitmap = self.bitmap.load(Ordering::Acquire);
        if bitmap & (1u32 << slot_id) == 0 {
            // Slot was freed (caller cancelled). Discard silently.
            return;
        }

        let slot = &self.slots[slot_id as usize];
        let len = payload.len().min(BUF);

        // SAFETY: We checked the bitmap bit is set, meaning a MuxedSlotGuard
        // exists for this slot. The guard's recv_reply() reads data only after
        // len is set (Acquire ordering). We write data first, then set len
        // (Release ordering).
        unsafe {
            let data = &mut *slot.data.get();
            data[..len].copy_from_slice(&payload[..len]);
        }
        slot.len.store(len as u16, Ordering::Release);

        // Wake the task waiting on this slot.
        // SAFETY: the waker is set by recv_reply() (the guard owner).
        unsafe {
            if let Some(w) = (*slot.waker.get()).take() {
                w.wake();
            }
        }
    }
}

/// RAII slot handle for [`MuxedSlots`].
///
/// Holds a slot allocation. On drop, frees the slot and wakes any task
/// waiting in [`acquire()`](ReplyRouter::acquire). This makes the
/// acquire-use-drop cycle cancel-safe.
pub struct MuxedSlotGuard<'a, const N: usize, const BUF: usize> {
    router: &'a MuxedSlots<N, BUF>,
    index: u8,
}

impl<const N: usize, const BUF: usize> Drop for MuxedSlotGuard<'_, N, BUF> {
    fn drop(&mut self) {
        self.router.free_slot(self.index);
    }
}

impl<const N: usize, const BUF: usize> RouterSlotHandle for MuxedSlotGuard<'_, N, BUF> {
    fn slot_id(&self) -> u8 {
        self.index
    }

    async fn recv_reply(&self) -> &[u8] {
        core::future::poll_fn(|cx| {
            let slot = &self.router.slots[self.index as usize];
            let len = slot.len.load(Ordering::Acquire);
            if len > 0 {
                // SAFETY: data was written by deliver() before len was set
                // (Release), and we read len with Acquire, establishing
                // happens-before.
                // SAFETY: data was written by deliver() before len was
                // set (Release). We read len with Acquire → happens-before.
                let arr = unsafe { &*slot.data.get() };
                let data = &arr[..len as usize];
                Poll::Ready(data)
            } else {
                // Store waker and wait for deliver() to wake us.
                // SAFETY: only this task writes the waker for this slot.
                unsafe {
                    *slot.waker.get() = Some(cx.waker().clone());
                }
                // Re-check to avoid lost wakeup.
                let len = slot.len.load(Ordering::Acquire);
                if len > 0 {
                    let arr = unsafe { &*slot.data.get() };
                    let data = &arr[..len as usize];
                    Poll::Ready(data)
                } else {
                    Poll::Pending
                }
            }
        })
        .await
    }
}

/// Reply token for muxed routing on the server side.
///
/// Carries the `slot_id` from the incoming request so that the server
/// can tag its response with the same slot ID.
#[derive(Debug, Clone, Copy)]
pub struct MuxedReplyToken {
    /// The slot ID extracted from the incoming request.
    slot_id: u8,
}

impl MuxedReplyToken {
    /// Create a new reply token with the given slot ID.
    pub fn new(slot_id: u8) -> Self {
        Self { slot_id }
    }

    /// The slot ID to prepend to the outgoing reply.
    pub fn slot_id(&self) -> u8 {
        self.slot_id
    }
}

// ---------------------------------------------------------------------------
// Type aliases for common configurations
// ---------------------------------------------------------------------------

/// 8-slot muxed router — a common configuration.
pub type MuxedSlots8<const BUF: usize> = MuxedSlots<8, BUF>;

/// 4-slot muxed router.
pub type MuxedSlots4<const BUF: usize> = MuxedSlots<4, BUF>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // -- Bitmap allocation / deallocation --

    #[test]
    fn bitmap_alloc_free() {
        let router = MuxedSlots::<4, 64>::new();

        // Allocate all 4 slots.
        let s0 = router.try_acquire().expect("slot 0");
        let s1 = router.try_acquire().expect("slot 1");
        let s2 = router.try_acquire().expect("slot 2");
        let s3 = router.try_acquire().expect("slot 3");

        // All unique.
        let mut ids = [s0.slot_id(), s1.slot_id(), s2.slot_id(), s3.slot_id()];
        ids.sort();
        assert_eq!(ids, [0, 1, 2, 3]);

        // No more slots available.
        assert!(router.try_acquire().is_none());

        // Free slot 2.
        let id2 = s2.slot_id();
        drop(s2);

        // Can allocate again — should get the freed slot.
        let s2b = router.try_acquire().expect("reacquired slot");
        assert_eq!(s2b.slot_id(), id2);

        // Still full.
        assert!(router.try_acquire().is_none());

        drop(s0);
        drop(s1);
        drop(s2b);
        drop(s3);

        // All free — bitmap should be zero.
        assert_eq!(router.bitmap.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn sequential_zero_overhead() {
        assert_eq!(Sequential::HEADER_LEN, 0);
        let seq = Sequential;
        let slot = seq.try_acquire().unwrap();
        assert_eq!(slot.slot_id(), 0);
    }

    #[test]
    fn muxed_header_round_trip() {
        let router = MuxedSlots::<8, 64>::new();
        let slot = router.try_acquire().unwrap();
        let mut buf = [0u8; 1];
        MuxedSlots::<8, 64>::write_header(&slot, &mut buf);
        let parsed = MuxedSlots::<8, 64>::parse_header(&buf);
        assert_eq!(parsed, slot.slot_id());
    }

    #[test]
    fn deliver_and_recv() {
        let router = MuxedSlots::<4, 64>::new();
        let slot = router.try_acquire().unwrap();
        let id = slot.slot_id();

        // Deliver a reply.
        router.deliver(id, b"hello");

        // recv_reply should return immediately.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let reply = rt.block_on(slot.recv_reply());
        assert_eq!(reply, b"hello");
    }

    #[test]
    fn deliver_to_freed_slot_is_discarded() {
        let router = MuxedSlots::<4, 64>::new();
        let slot = router.try_acquire().unwrap();
        let id = slot.slot_id();
        drop(slot); // Free the slot (simulating cancellation).

        // Delivering to a freed slot should not panic.
        router.deliver(id, b"orphaned reply");

        // Bitmap should be clean.
        assert_eq!(router.bitmap.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn concurrent_acquire_slots() {
        let router = Arc::new(MuxedSlots::<2, 64>::new());

        // Two tasks acquire slots concurrently. We use channels to
        // keep the slot guards alive until both IDs are reported.
        let (tx1, rx1) = tokio::sync::oneshot::channel();
        let (tx2, rx2) = tokio::sync::oneshot::channel();
        let (done_tx1, done_rx1) = tokio::sync::oneshot::channel::<()>();
        let (done_tx2, done_rx2) = tokio::sync::oneshot::channel::<()>();

        let r1 = router.clone();
        tokio::spawn(async move {
            let slot = r1.acquire().await.unwrap();
            tx1.send(slot.slot_id()).unwrap();
            // Hold the guard until told to release.
            let _ = done_rx1.await;
            drop(slot);
        });

        let r2 = router.clone();
        tokio::spawn(async move {
            let slot = r2.acquire().await.unwrap();
            tx2.send(slot.slot_id()).unwrap();
            let _ = done_rx2.await;
            drop(slot);
        });

        let id1 = rx1.await.unwrap();
        let id2 = rx2.await.unwrap();
        assert_ne!(id1, id2);

        // Release both guards.
        let _ = done_tx1.send(());
        let _ = done_tx2.send(());
    }

    #[tokio::test]
    async fn acquire_blocks_when_full_then_wakes() {
        let router = Arc::new(MuxedSlots::<1, 64>::new());

        // Acquire the only slot.
        let slot = router.acquire().await.unwrap();
        let id = slot.slot_id();

        let r2 = router.clone();
        let handle = tokio::spawn(async move {
            // This should block until the slot is freed.
            let s = r2.acquire().await.unwrap();
            s.slot_id()
        });

        // Give the spawned task a chance to block.
        tokio::task::yield_now().await;

        // Free the slot.
        drop(slot);

        // The spawned task should now complete.
        let id2 = handle.await.unwrap();
        assert_eq!(id2, id); // Should get the same slot index back.
    }

    #[tokio::test]
    async fn cancel_safety_drop_after_acquire() {
        let router = MuxedSlots::<2, 64>::new();

        {
            let _slot = router.acquire().await.unwrap();
            // Guard is dropped here — slot should be freed.
        }

        // Bitmap should be clean.
        assert_eq!(router.bitmap.load(Ordering::Relaxed), 0);

        // Can acquire again.
        let slot = router.try_acquire().unwrap();
        assert!(slot.slot_id() < 2);
    }

    #[test]
    fn muxed_reply_token_round_trip() {
        let token = MuxedReplyToken::new(7);
        assert_eq!(token.slot_id(), 7);
    }

    #[test]
    fn max_32_slots() {
        let router = MuxedSlots::<32, 16>::new();
        let mut guards = Vec::new();
        for _ in 0..32 {
            guards.push(router.try_acquire().unwrap());
        }
        assert!(router.try_acquire().is_none());

        let mut ids: Vec<u8> = guards.iter().map(|g| g.slot_id()).collect();
        ids.sort();
        let expected: Vec<u8> = (0..32).collect();
        assert_eq!(ids, expected);
    }
}
