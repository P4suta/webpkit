//! A counting global allocator for deterministic peak-memory metrics: it delegates
//! to the system allocator while tracking the live and peak REQUESTED byte counts.
//! Requested bytes (sum of `Layout::size`) are platform-independent for deterministic,
//! allocation-order-stable code, so the peak is a reproducible integer metric.
use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};
use std::alloc::System;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static BASELINE: AtomicUsize = AtomicUsize::new(0);

/// A [`GlobalAlloc`] that forwards to [`System`] while tracking live/peak requested bytes.
#[derive(Default)]
pub struct Counting;

impl Counting {
    /// Construct the allocator (usable as a `#[global_allocator]` static initializer).
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

// SAFETY: forwards every call to the System allocator unchanged; the atomic
// bookkeeping does not affect the returned pointers.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) };
    }
}

/// Reset the peak watermark to the current live total; call immediately before a measured op.
pub fn reset_peak() {
    let live = LIVE.load(Ordering::Relaxed);
    BASELINE.store(live, Ordering::Relaxed);
    PEAK.store(live, Ordering::Relaxed);
}

/// Peak ADDITIONAL requested bytes since the last [`reset_peak`] (the op's working-set high-water mark).
#[must_use]
pub fn peak_since_reset() -> usize {
    PEAK.load(Ordering::Relaxed)
        .saturating_sub(BASELINE.load(Ordering::Relaxed))
}
