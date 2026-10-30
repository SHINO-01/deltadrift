//! Hot-path primitives shared by every pipeline stage.
//!
//! Nothing in this crate allocates after construction. All message types are
//! `Copy`, `#[repr(C, align(64))]` and exactly one cache line wide.

pub mod affinity;
pub mod calendar;
pub mod clock;
pub mod decimal;
pub mod pad;
pub mod ring;
pub mod rng;
pub mod types;
pub mod wait;

pub use pad::CachePadded;
pub use ring::{Consumer, Producer, ring};
pub use types::*;
