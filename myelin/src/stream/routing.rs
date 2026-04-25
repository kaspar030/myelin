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

use core::future::Future;
use core::mem::MaybeUninit;
use core::ptr::addr_of_mut;
use core::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, Ordering};
use core::task::Poll;

use atomic_waker::AtomicWaker;

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

/// Per-slot storage: waker + reply buffer + generation counter.
///
/// The `data` buffer uses `UnsafeCell` because only one side writes at a
/// time (deliver writes, then recv_reply reads), with the `len` atomic
/// providing the synchronization barrier. The `waker` uses [`AtomicWaker`]
/// for safe concurrent register/wake.
struct SlotCell<const BUF: usize> {
    /// Reply data buffer, written by `deliver()`, read by `recv_reply()`.
    data: core::cell::UnsafeCell<[u8; BUF]>,
    /// Length of valid data in the buffer.
    /// 0 means empty (no reply yet). Set atomically by `deliver()`.
    len: AtomicU16,
    /// Waker for the task waiting on this slot's reply.
    waker: AtomicWaker,
    /// Generation counter: incremented each time the slot is freed.
    /// Used by `deliver()` to detect stale deliveries (ABA prevention).
    generation: AtomicU8,
}

impl<const BUF: usize> SlotCell<BUF> {
    #[allow(clippy::new_without_default)]
    fn new() -> Self {
        Self {
            data: core::cell::UnsafeCell::new([0u8; BUF]),
            len: AtomicU16::new(0),
            waker: AtomicWaker::new(),
            generation: AtomicU8::new(0),
        }
    }

    /// Initialise a `SlotCell` in place at `ptr` without ever materialising
    /// a whole `SlotCell<BUF>` on the stack.
    ///
    /// The naïve `ptr.write(SlotCell::new())` would still build the fresh
    /// cell (including its `[u8; BUF]` data buffer) in a local first, which
    /// is exactly the stack-overflow hazard we are trying to avoid for large
    /// `BUF`. This helper zeros the allocation byte-by-byte and then overlays
    /// a fresh `AtomicWaker` — a small, `BUF`-independent local.
    ///
    /// # Safety
    ///
    /// `ptr` must point to a properly-aligned, writable region of size
    /// `size_of::<SlotCell<BUF>>()` that the caller treats as uninitialised.
    /// After this call the region contains a fully-initialised `SlotCell<BUF>`
    /// and ownership of that value transfers to the caller.
    unsafe fn init_in_place(ptr: *mut SlotCell<BUF>) {
        // Zero the whole region. This is a valid initialiser for:
        //   - `data: UnsafeCell<[u8; BUF]>` — zeroed byte array is fine,
        //   - `len: AtomicU16` — `AtomicU16::new(0)` is all-zeros,
        //   - `generation: AtomicU8` — `AtomicU8::new(0)` is all-zeros.
        // The `waker: AtomicWaker` field is also zeroed, but `AtomicWaker`'s
        // internal `Option<Waker>` niche layout is not guaranteed to match
        // an all-zero `None`, so we overwrite it explicitly below.
        //
        // SAFETY: the caller guarantees `ptr` is properly aligned and points
        // to a writable region of `size_of::<SlotCell<BUF>>()` bytes.
        unsafe {
            core::ptr::write_bytes(ptr as *mut u8, 0, core::mem::size_of::<SlotCell<BUF>>());
        }
        // Overwrite the waker field with a properly-constructed
        // `AtomicWaker`. This is a small, fixed-size local that does not
        // scale with `BUF`, so the stack cost is trivial.
        //
        // SAFETY: same as above — `ptr` is valid for writes to `(*ptr).waker`.
        unsafe {
            addr_of_mut!((*ptr).waker).write(AtomicWaker::new());
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
/// freed slot (or a reallocated slot with a different generation), it is
/// silently discarded.
///
/// # Concurrency
///
/// The routing layer itself is fully concurrent and safe for multi-threaded
/// executors. However, the current [`StreamTransport`](super::transport::StreamTransport)
/// integration uses blocking `std::io::Read`/`Write`, which means the
/// cooperative demux loop in `call()` does not yield between reads. True
/// concurrent multiplexing requires an async framing layer (e.g.,
/// `tokio::io::AsyncRead`). The routing layer is ready for that — the
/// transport integration is the current bottleneck.
/// # Waiter behavior
///
/// `acquire()` uses an [`AtomicWaker`] which supports a single waiter.
/// If multiple tasks call `acquire()` concurrently when all slots are
/// full, only one waiter is guaranteed to be woken when a slot frees.
/// With the current blocking I/O transport (which uses `RefCell` and is
/// inherently single-task), this is not a practical limitation. For
/// future multi-task usage with async I/O, callers should use
/// `try_acquire()` with their own retry/backoff logic, or the transport
/// should serialize access to `acquire()`.
pub struct MuxedSlots<const N: usize, const BUF: usize> {
    /// Bitmap: bit `i` is set when slot `i` is allocated.
    bitmap: AtomicU32,
    /// Per-slot state.
    slots: [SlotCell<BUF>; N],
    /// Waker for tasks blocked in `acquire()` waiting for a free slot.
    alloc_waker: AtomicWaker,
}

// SAFETY: All fields are Send+Sync:
// - AtomicU32, AtomicWaker: Send+Sync by definition.
// - SlotCell::data (UnsafeCell<[u8; BUF]>): access is ordered by the
//   atomic `len` field — deliver() writes data then sets len (Release),
//   recv_reply() reads len (Acquire) then reads data. The bitmap ensures
//   only one "owner" (guard holder or deliver) accesses a slot's data at
//   a time, and the generation counter prevents ABA races on slot reuse.
unsafe impl<const N: usize, const BUF: usize> Send for MuxedSlots<N, BUF> {}
unsafe impl<const N: usize, const BUF: usize> Sync for MuxedSlots<N, BUF> {}

impl<const N: usize, const BUF: usize> MuxedSlots<N, BUF> {
    // Compile-time assertion: N must be ≤ 32 (AtomicU32 bitmap).
    const _ASSERT_N_LE_32: () = assert!(N <= 32, "MuxedSlots: N must be <= 32");

    /// Create a new muxed router with all slots free.
    ///
    /// This constructor builds the slot array on the stack before the
    /// result is moved into its final location. For small configurations
    /// that is fine, but the stack frame grows as `N * BUF`. If
    /// `N * BUF` is more than a few hundred kilobytes (e.g. `N = 32`,
    /// `BUF = 65_536` → 2 MiB), prefer [`new_boxed`](Self::new_boxed) —
    /// it writes each slot directly into a heap allocation and keeps
    /// stack use bounded.
    pub fn new() -> Self {
        // Trigger the compile-time assertion.
        let () = Self::_ASSERT_N_LE_32;

        Self {
            bitmap: AtomicU32::new(0),
            slots: core::array::from_fn(|_| SlotCell::new()),
            alloc_waker: AtomicWaker::new(),
        }
    }

    /// Heap-allocate and construct a `MuxedSlots<N, BUF>` directly in its
    /// final `Box`, without placing the `N × BUF` slot array on the stack.
    ///
    /// Prefer this over [`new`](Self::new) when `N * BUF` is large enough
    /// to risk a stack overflow during construction — typical runtime
    /// worker-thread stacks are only a couple of megabytes, and
    /// `MuxedSlots<32, 65_536>` alone is 2 MiB.
    ///
    /// The resulting `Box<MuxedSlots<N, BUF>>` has the same observable
    /// state as `Box::new(MuxedSlots::new())`; the only difference is
    /// that no intermediate is built on the stack.
    ///
    /// See [`StreamTransport::with_boxed_router`](crate::stream::StreamTransport::with_boxed_router)
    /// for the constructor that consumes the result.
    pub fn new_boxed() -> Box<Self> {
        // Trigger the compile-time assertion.
        let () = Self::_ASSERT_N_LE_32;

        let mut uninit: Box<MaybeUninit<Self>> = Box::new_uninit();
        let ptr: *mut Self = uninit.as_mut_ptr();
        // SAFETY: `uninit` owns a properly-aligned, writable allocation
        // large enough to hold a `MuxedSlots<N, BUF>`. We initialise each
        // field exactly once via `addr_of_mut!(...).write(...)` and then
        // hand the allocation to `Box::from_raw` as a fully-initialised
        // `Self`. None of the writes below can panic, so we do not need
        // a panic guard to avoid leaking partially-initialised fields.
        unsafe {
            addr_of_mut!((*ptr).bitmap).write(AtomicU32::new(0));
            addr_of_mut!((*ptr).alloc_waker).write(AtomicWaker::new());

            let slots_ptr: *mut SlotCell<BUF> =
                addr_of_mut!((*ptr).slots) as *mut SlotCell<BUF>;
            for i in 0..N {
                // `init_in_place` writes directly into the heap slot,
                // avoiding any `BUF`-scaled stack intermediate.
                SlotCell::<BUF>::init_in_place(slots_ptr.add(i));
            }

            let raw = Box::into_raw(uninit) as *mut Self;
            Box::from_raw(raw)
        }
    }

    /// The valid-slot mask: bits `0..N` are set.
    const fn valid_mask() -> u32 {
        if N == 32 { u32::MAX } else { (1u32 << N) - 1 }
    }

    /// Try to allocate a slot via CAS on the bitmap.
    /// Returns `Some((index, generation))` on success, `None` if all slots
    /// are occupied.
    fn try_alloc(&self) -> Option<(u8, u8)> {
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
                Ok(_) => {
                    let gen_val = self.slots[idx as usize].generation.load(Ordering::Acquire);
                    return Some((idx, gen_val));
                }
                Err(_) => continue, // Retry on contention.
            }
        }
    }

    /// Free a slot: clear its bitmap bit, increment generation, and wake
    /// any task waiting in `acquire()`.
    fn free_slot(&self, idx: u8) {
        // Reset the slot's data length so stale data isn't visible.
        self.slots[idx as usize].len.store(0, Ordering::Release);

        // Increment the generation counter (ABA prevention).
        self.slots[idx as usize]
            .generation
            .fetch_add(1, Ordering::Release);

        // Clear the bitmap bit.
        self.bitmap.fetch_and(!(1u32 << idx), Ordering::Release);

        // Wake a task waiting for a free slot.
        self.alloc_waker.wake();
    }

    /// Check if a slot has a pending reply and return it.
    ///
    /// Returns `Some(data)` if the slot has received a reply (len > 0),
    /// `None` otherwise. Used by the cooperative demux in the transport.
    ///
    /// # Safety requirement (not enforced)
    ///
    /// The caller must hold a valid `MuxedSlotGuard` for `slot_id` (i.e.,
    /// the slot must be allocated). This is the transport's responsibility.
    pub fn try_recv_slot(&self, slot_id: u8) -> Option<&[u8]> {
        if (slot_id as usize) >= N {
            return None;
        }
        let slot = &self.slots[slot_id as usize];
        let len = slot.len.load(Ordering::Acquire);
        if len > 0 {
            // SAFETY: data was written by deliver() with Release on len.
            // We loaded len with Acquire → happens-before is established.
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
        if let Some((idx, gen_val)) = self.try_alloc() {
            return Ok(MuxedSlotGuard {
                router: self,
                index: idx,
                generation: gen_val,
            });
        }

        // Slow path: all slots occupied — poll until one frees up.
        core::future::poll_fn(|cx| {
            // Register waker *before* checking, to avoid lost-wakeup race.
            self.alloc_waker.register(cx.waker());

            if let Some((idx, gen_val)) = self.try_alloc() {
                return Poll::Ready(Ok(MuxedSlotGuard {
                    router: self,
                    index: idx,
                    generation: gen_val,
                }));
            }
            Poll::Pending
        })
        .await
    }

    fn try_acquire(&self) -> Option<MuxedSlotGuard<'_, N, BUF>> {
        self.try_alloc().map(|(idx, gen_val)| MuxedSlotGuard {
            router: self,
            index: idx,
            generation: gen_val,
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

        // Snapshot the generation before writing. After writing, re-check
        // to detect if the slot was freed and reallocated between our
        // bitmap check and now (ABA prevention).
        let gen_before = slot.generation.load(Ordering::Acquire);

        let len = payload.len().min(BUF);

        // SAFETY: The bitmap bit is set, meaning a MuxedSlotGuard exists.
        // The guard's recv_reply() reads data only after len is set
        // (Acquire on len). We write data first, then set len (Release).
        unsafe {
            let data = &mut *slot.data.get();
            data[..len].copy_from_slice(&payload[..len]);
        }

        // Re-check generation: if it changed, the slot was freed and
        // possibly reallocated. Discard by NOT setting len.
        let gen_after = slot.generation.load(Ordering::Acquire);
        if gen_before != gen_after {
            return;
        }

        slot.len.store(len as u16, Ordering::Release);

        // Wake the task waiting on this slot.
        slot.waker.wake();
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
    /// Generation at the time this slot was allocated.
    #[allow(dead_code)]
    generation: u8,
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
                // (Release). We loaded len with Acquire → happens-before.
                let arr = unsafe { &*slot.data.get() };
                let data = &arr[..len as usize];
                Poll::Ready(data)
            } else {
                // Register waker so deliver() can wake us.
                slot.waker.register(cx.waker());

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

    #[test]
    fn generation_counter_prevents_stale_delivery() {
        let router = MuxedSlots::<1, 64>::new();

        // Allocate, note generation.
        let slot1 = router.try_acquire().unwrap();
        let id = slot1.slot_id();
        let gen1 = slot1.generation;

        // Free and reallocate — generation should increment.
        drop(slot1);
        let slot2 = router.try_acquire().unwrap();
        assert_eq!(slot2.slot_id(), id); // Same index.
        assert_eq!(slot2.generation, gen1.wrapping_add(1));

        // A stale deliver() that checks gen_before but the slot was freed
        // and reallocated would see a different generation. We can't easily
        // trigger the race in a unit test, but we verify the generation
        // counter increments on free.
        drop(slot2);
        let gen_now = router.slots[id as usize].generation.load(Ordering::Relaxed);
        assert_eq!(gen_now, gen1.wrapping_add(2));
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
    async fn single_waiter_chain_works() {
        // With AtomicWaker, one waiter at a time is supported. When a slot
        // frees, the waiter is woken, acquires, frees, wakes the next, etc.
        let router = Arc::new(MuxedSlots::<1, 64>::new());

        // Acquire the only slot.
        let slot = router.acquire().await.unwrap();

        // Spawn a single waiter.
        let r2 = router.clone();
        let handle = tokio::spawn(async move {
            let s = r2.acquire().await.unwrap();
            let id = s.slot_id();
            drop(s);
            id
        });

        // Give the waiter a chance to register.
        tokio::task::yield_now().await;

        // Free the slot — this wakes the single waiter.
        drop(slot);

        let id = handle.await.unwrap();
        assert_eq!(id, 0);
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

    // -- new_boxed --

    #[test]
    fn new_boxed_produces_equivalent_state() {
        // Heap-construct and verify the resulting router is functionally
        // identical to one built via `new()`.
        let router: Box<MuxedSlots<4, 64>> = MuxedSlots::<4, 64>::new_boxed();

        // Bitmap starts empty.
        assert_eq!(router.bitmap.load(Ordering::Relaxed), 0);

        // All slots initially zeroed.
        for i in 0..4 {
            assert_eq!(router.slots[i].len.load(Ordering::Relaxed), 0);
            assert_eq!(router.slots[i].generation.load(Ordering::Relaxed), 0);
        }

        // Allocate all four, get distinct ids, then free by dropping.
        let s0 = router.try_acquire().expect("slot 0");
        let s1 = router.try_acquire().expect("slot 1");
        let s2 = router.try_acquire().expect("slot 2");
        let s3 = router.try_acquire().expect("slot 3");
        assert!(router.try_acquire().is_none());

        let mut ids = [s0.slot_id(), s1.slot_id(), s2.slot_id(), s3.slot_id()];
        ids.sort();
        assert_eq!(ids, [0, 1, 2, 3]);

        // Deliver + recv round-trip on one slot to exercise the data buffer.
        router.deliver(s1.slot_id(), b"hello");
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let reply = rt.block_on(s1.recv_reply());
        assert_eq!(reply, b"hello");

        drop(s0);
        drop(s1);
        drop(s2);
        drop(s3);
        assert_eq!(router.bitmap.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn new_boxed_on_restricted_stack() {
        // 32 × 128 KiB = 4 MiB of slot data — vastly more than a 1 MiB
        // worker stack. This *must* succeed via `new_boxed` (building the
        // same router with `new()` on this thread would overflow).
        std::thread::Builder::new()
            .stack_size(1 << 20) // 1 MiB
            .spawn(|| {
                let router: Box<MuxedSlots<32, 131_072>> =
                    MuxedSlots::<32, 131_072>::new_boxed();
                // Touch the router to make sure the optimiser does not
                // elide the allocation.
                assert_eq!(router.bitmap.load(Ordering::Relaxed), 0);
                let slot = router.try_acquire().expect("slot on big router");
                assert!(slot.slot_id() < 32);
            })
            .expect("spawn restricted-stack thread")
            .join()
            .expect("restricted-stack thread panicked (likely stack overflow)");
    }
}
