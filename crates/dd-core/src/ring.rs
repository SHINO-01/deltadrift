//! Bounded, wait-free, single-producer / single-consumer ring buffer.
//!
//! Design (Disruptor / Rigtorp style):
//! * Capacity is a compile-time power of two, so indexing is a mask.
//! * Indices increase monotonically and wrap with `usize` arithmetic.
//! * Producer and consumer cursors live on separate padded cache lines.
//! * Each side caches the other's cursor and only re-reads the shared atomic
//!   when the cached value says the ring looks full/empty. In steady state a
//!   push or pop touches one remote cache line per *batch*, not per item.
//! * Storage is allocated once in [`ring`]; push/pop never allocate.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use crate::pad::CachePadded;

struct Shared<T, const N: usize> {
    /// Next index the consumer will read. Written by consumer only.
    head: CachePadded<AtomicUsize>,
    /// Next index the producer will write. Written by producer only.
    tail: CachePadded<AtomicUsize>,
    /// Set when the producer is dropped; lets consumers drain and exit.
    closed: CachePadded<AtomicBool>,
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
}

// SAFETY: slots in [head, tail) are only read by the consumer and slots outside
// it only written by the producer; ownership hand-off is ordered by the
// Release store / Acquire load on `tail` (publish) and `head` (reclaim).
unsafe impl<T: Send, const N: usize> Sync for Shared<T, N> {}
// SAFETY: as above; the struct owns its `T`s.
unsafe impl<T: Send, const N: usize> Send for Shared<T, N> {}

impl<T, const N: usize> Shared<T, N> {
    const MASK: usize = N - 1;

    #[inline(always)]
    fn slot(&self, idx: usize) -> *mut MaybeUninit<T> {
        // SAFETY: `idx & MASK < N == slots.len()`.
        unsafe { self.slots.get_unchecked(idx & Self::MASK).get() }
    }
}

impl<T, const N: usize> Drop for Shared<T, N> {
    fn drop(&mut self) {
        if !core::mem::needs_drop::<T>() {
            return;
        }
        let tail = *self.tail.get_mut();
        let mut h = *self.head.get_mut();
        while h != tail {
            // SAFETY: every slot in [head, tail) holds an initialised value
            // that has not been read; we have exclusive access in Drop.
            unsafe { (*self.slot(h)).assume_init_drop() };
            h = h.wrapping_add(1);
        }
    }
}

/// Write half. Not `Clone`: exactly one producer may exist.
pub struct Producer<T, const N: usize> {
    shared: Arc<Shared<T, N>>,
    tail: usize,
    cached_head: usize,
}

/// Read half. Not `Clone`: exactly one consumer may exist.
pub struct Consumer<T, const N: usize> {
    shared: Arc<Shared<T, N>>,
    head: usize,
    cached_tail: usize,
}

/// Allocates a ring of capacity `N` (must be a power of two) and splits it.
pub fn ring<T: Send, const N: usize>() -> (Producer<T, N>, Consumer<T, N>) {
    const { assert!(N.is_power_of_two() && N >= 2, "ring capacity must be a power of two >= 2") };
    let slots = (0..N)
        .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
        .collect::<Box<[_]>>();
    let shared = Arc::new(Shared {
        head: CachePadded::new(AtomicUsize::new(0)),
        tail: CachePadded::new(AtomicUsize::new(0)),
        closed: CachePadded::new(AtomicBool::new(false)),
        slots,
    });
    (
        Producer { shared: Arc::clone(&shared), tail: 0, cached_head: 0 },
        Consumer { shared, head: 0, cached_tail: 0 },
    )
}

impl<T, const N: usize> Producer<T, N> {
    pub const CAPACITY: usize = N;

    /// Attempts to enqueue. Returns the value back if the ring is full.
    #[inline]
    pub fn push(&mut self, value: T) -> Result<(), T> {
        let t = self.tail;
        if t.wrapping_sub(self.cached_head) == N {
            self.cached_head = self.shared.head.load(Ordering::Acquire);
            if t.wrapping_sub(self.cached_head) == N {
                return Err(value);
            }
        }
        // SAFETY: slot `t` is outside [head, tail) so the consumer is not
        // touching it, and the Acquire on `head` ordered its last read before us.
        unsafe { (*self.shared.slot(t)).write(value) };
        self.tail = t.wrapping_add(1);
        self.shared.tail.store(self.tail, Ordering::Release);
        Ok(())
    }

    /// Spins (via `wait`) until there is room, then enqueues.
    #[inline]
    pub fn push_with<W: crate::wait::Wait>(&mut self, mut value: T, wait: &mut W) {
        loop {
            match self.push(value) {
                Ok(()) => {
                    wait.reset();
                    return;
                }
                Err(v) => {
                    value = v;
                    wait.idle();
                }
            }
        }
    }

    /// Number of items the producer can currently see as queued (upper bound).
    pub fn len(&self) -> usize {
        self.tail.wrapping_sub(self.shared.head.load(Ordering::Acquire))
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T, const N: usize> Drop for Producer<T, N> {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
    }
}

impl<T, const N: usize> Consumer<T, N> {
    pub const CAPACITY: usize = N;

    /// Items readable now, re-reading the producer cursor only when the cached
    /// view holds fewer than `want` (one Acquire load per batch, not per item).
    #[inline(always)]
    fn available(&mut self, want: usize) -> usize {
        let cached = self.cached_tail.wrapping_sub(self.head);
        if cached >= want {
            return cached;
        }
        self.cached_tail = self.shared.tail.load(Ordering::Acquire);
        self.cached_tail.wrapping_sub(self.head)
    }

    /// Dequeues one item, if any.
    #[inline]
    pub fn pop(&mut self) -> Option<T> {
        if self.available(1) == 0 {
            return None;
        }
        let h = self.head;
        // SAFETY: `h` is in [head, tail); the producer published it with a
        // Release store that our Acquire load of `tail` synchronised with.
        let v = unsafe { (*self.shared.slot(h)).assume_init_read() };
        self.head = h.wrapping_add(1);
        self.shared.head.store(self.head, Ordering::Release);
        Some(v)
    }

    /// Dequeues up to `max` items, invoking `f` for each, and publishes the
    /// consumer cursor once for the whole batch. Returns the count consumed.
    #[inline]
    pub fn consume<F: FnMut(T)>(&mut self, max: usize, mut f: F) -> usize {
        let n = self.available(max).min(max);
        if n == 0 {
            return 0;
        }
        let mut h = self.head;
        for _ in 0..n {
            // SAFETY: see `pop`; all `n` slots are in [head, tail).
            f(unsafe { (*self.shared.slot(h)).assume_init_read() });
            h = h.wrapping_add(1);
        }
        self.head = h;
        self.shared.head.store(h, Ordering::Release);
        n
    }

    /// True once the producer has been dropped. Items may still be queued:
    /// drain until `pop` returns `None` *after* observing this flag.
    #[inline]
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire)
    }
}

impl<T, const N: usize> core::fmt::Debug for Producer<T, N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Producer").field("capacity", &N).field("tail", &self.tail).finish()
    }
}

impl<T, const N: usize> core::fmt::Debug for Consumer<T, N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Consumer").field("capacity", &N).field("head", &self.head).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wait::BusySpin;

    #[test]
    fn fifo_and_full_empty() {
        let (mut p, mut c) = ring::<u32, 4>();
        assert_eq!(c.pop(), None);
        for i in 0..4 {
            p.push(i).unwrap();
        }
        assert_eq!(p.push(99), Err(99));
        assert_eq!(c.pop(), Some(0));
        p.push(4).unwrap();
        let mut got = vec![];
        assert_eq!(c.consume(usize::MAX, |v| got.push(v)), 4);
        assert_eq!(got, [1, 2, 3, 4]);
        assert_eq!(c.pop(), None);
    }

    #[test]
    fn drops_unconsumed_items() {
        let marker = Arc::new(());
        {
            let (mut p, _c) = ring::<Arc<()>, 8>();
            for _ in 0..5 {
                p.push(Arc::clone(&marker)).unwrap();
            }
        }
        assert_eq!(Arc::strong_count(&marker), 1);
    }

    #[test]
    fn close_signal() {
        let (p, c) = ring::<u8, 2>();
        assert!(!c.is_closed());
        drop(p);
        assert!(c.is_closed());
    }

    /// Cross-thread stress: strict ordering, no loss, no duplication,
    /// across many wrap-arounds of a tiny ring.
    #[test]
    fn cross_thread_sequence() {
        const COUNT: u64 = 2_000_000;
        let (mut p, mut c) = ring::<u64, 64>();
        let prod = std::thread::spawn(move || {
            let mut w = BusySpin;
            for i in 0..COUNT {
                p.push_with(i, &mut w);
            }
        });
        let mut expected = 0u64;
        while expected < COUNT {
            c.consume(32, |v| {
                assert_eq!(v, expected);
                expected += 1;
            });
            core::hint::spin_loop();
        }
        prod.join().unwrap();
        assert!(c.is_closed());
        assert_eq!(c.pop(), None);
    }
}
