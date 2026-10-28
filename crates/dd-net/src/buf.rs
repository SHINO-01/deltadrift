//! Fixed-capacity linear byte buffers.
//!
//! Layout: `[consumed | data: start..end | spare: end..cap]`. Consumers drop
//! from the front with `consume`; producers append at `end`. Compaction
//! (moving data to offset 0) is explicit so the io_uring driver can forbid it
//! while the kernel owns the spare region.

use std::ptr::NonNull;

/// Receive buffer whose spare region may be written by the kernel (io_uring)
/// while the data region is borrowed. Backed by a raw allocation so that
/// handing the kernel a pointer into the tail never conflicts with Rust
/// references to the head.
pub(crate) struct RecvBuf {
    ptr: NonNull<u8>,
    cap: usize,
    start: usize,
    end: usize,
}

// SAFETY: RecvBuf exclusively owns its allocation; moving it between threads is fine.
unsafe impl Send for RecvBuf {}

impl RecvBuf {
    pub(crate) fn new(cap: usize) -> Self {
        assert!(cap > 0);
        let raw = Box::into_raw(vec![0u8; cap].into_boxed_slice());
        // SAFETY: Box::into_raw never returns null.
        let ptr = unsafe { NonNull::new_unchecked(raw.cast::<u8>()) };
        Self { ptr, cap, start: 0, end: 0 }
    }

    #[inline(always)]
    pub(crate) fn data_mut(&mut self) -> &mut [u8] {
        // SAFETY: start..end is initialised, in bounds, and not visible to the
        // kernel (I/O only ever targets end..cap).
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr().add(self.start), self.end - self.start) }
    }

    #[inline(always)]
    pub(crate) fn consume(&mut self, n: usize) {
        debug_assert!(n <= self.end - self.start);
        self.start += n;
    }

    /// Moves live data to offset 0. Caller guarantees no I/O targets the buffer.
    #[inline]
    pub(crate) fn compact(&mut self) {
        if self.start == 0 {
            return;
        }
        let len = self.end - self.start;
        // SAFETY: both ranges are in bounds; `copy` handles overlap.
        unsafe { std::ptr::copy(self.ptr.as_ptr().add(self.start), self.ptr.as_ptr(), len) };
        self.start = 0;
        self.end = len;
    }

    #[inline(always)]
    pub(crate) fn spare(&self) -> usize {
        self.cap - self.end
    }

    /// Raw pointer to the spare region, for the kernel to write into.
    #[cfg(target_os = "linux")]
    #[inline(always)]
    pub(crate) fn tail_ptr(&mut self) -> *mut u8 {
        // SAFETY: end <= cap, so this is in bounds or one-past-the-end.
        unsafe { self.ptr.as_ptr().add(self.end) }
    }

    #[inline(always)]
    pub(crate) fn tail_mut(&mut self) -> &mut [u8] {
        // SAFETY: end..cap is in bounds and initialised (zeroed at allocation).
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr().add(self.end), self.cap - self.end) }
    }

    /// Marks `n` bytes of the spare region as written.
    #[inline(always)]
    pub(crate) fn advance(&mut self, n: usize) {
        assert!(n <= self.spare(), "advance past capacity");
        self.end += n;
    }
}

impl Drop for RecvBuf {
    fn drop(&mut self) {
        // SAFETY: reconstructs exactly the Box<[u8]> leaked in `new`.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(self.ptr.as_ptr(), self.cap)) });
    }
}

/// Safe linear buffer for decrypted plaintext awaiting WebSocket framing.
pub(crate) struct ByteBuf {
    buf: Box<[u8]>,
    start: usize,
    end: usize,
}

impl ByteBuf {
    pub(crate) fn new(cap: usize) -> Self {
        Self { buf: vec![0u8; cap].into_boxed_slice(), start: 0, end: 0 }
    }

    #[inline(always)]
    pub(crate) fn data(&self) -> &[u8] {
        &self.buf[self.start..self.end]
    }

    #[inline(always)]
    pub(crate) fn consume(&mut self, n: usize) {
        self.start += n;
        if self.start == self.end {
            self.start = 0;
            self.end = 0;
        }
    }

    /// Appends, compacting first if needed. Returns false if it cannot fit.
    #[inline]
    pub(crate) fn extend(&mut self, src: &[u8]) -> bool {
        if self.buf.len() - self.end < src.len() {
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
            if self.buf.len() - self.end < src.len() {
                return false;
            }
        }
        self.buf[self.end..self.end + src.len()].copy_from_slice(src);
        self.end += src.len();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recvbuf_lifecycle() {
        let mut b = RecvBuf::new(8);
        b.tail_mut()[..5].copy_from_slice(b"hello");
        b.advance(5);
        assert_eq!(b.data_mut(), b"hello");
        b.consume(2);
        assert_eq!(b.data_mut(), b"llo");
        assert_eq!(b.spare(), 3);
        b.compact();
        assert_eq!((b.data_mut().len(), b.spare()), (3, 5));
        assert_eq!(b.data_mut(), b"llo");
    }

    #[test]
    fn bytebuf_compacts_on_extend() {
        let mut b = ByteBuf::new(6);
        assert!(b.extend(b"abcd"));
        b.consume(3);
        assert!(b.extend(b"efgh"));
        assert_eq!(b.data(), b"defgh");
        assert!(!b.extend(b"xy"));
    }
}
