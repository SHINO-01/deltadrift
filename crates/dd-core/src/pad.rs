use core::ops::{Deref, DerefMut};

/// Pads and aligns `T` so it never shares a cache line with a neighbour.
///
/// 128 bytes rather than 64: x86 prefetches adjacent line pairs, and Arm
/// Neoverse cores are frequently paired with 128-byte-granular interconnects.
/// The extra 64 bytes only applies to a handful of control atomics.
#[derive(Debug, Default)]
#[repr(C, align(128))]
pub struct CachePadded<T>(pub T);

impl<T> CachePadded<T> {
    pub const fn new(v: T) -> Self {
        Self(v)
    }
}

impl<T> Deref for CachePadded<T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for CachePadded<T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}
