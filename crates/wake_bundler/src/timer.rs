//! Native phase measurements are unavailable in the import-free browser compiler.
//! The browser adapter measures wall time around the Worker request instead. Internal native-only
//! counters use zero on WASM and must not be presented as browser timing measurements.
pub(crate) struct BuildTimer {
    #[cfg(not(target_arch = "wasm32"))]
    started: std::time::Instant,
}

impl BuildTimer {
    pub(crate) fn now() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            started: std::time::Instant::now(),
        }
    }

    pub(crate) fn elapsed(&self) -> std::time::Duration {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.started.elapsed()
        }
        #[cfg(target_arch = "wasm32")]
        {
            std::time::Duration::ZERO
        }
    }
}
