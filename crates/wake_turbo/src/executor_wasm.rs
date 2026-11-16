//! Sequential execution inside the owning browser Worker. Compiler tasks and their identities
//! remain unchanged; this backend creates no OS threads and requires no shared WebAssembly memory.
use std::sync::{Arc, OnceLock};

pub struct Executor;

pub fn global_executor() -> Arc<Executor> {
    static GLOBAL: OnceLock<Arc<Executor>> = OnceLock::new();
    Arc::clone(GLOBAL.get_or_init(|| Arc::new(Executor)))
}

impl Executor {
    pub fn new(_num_threads: usize) -> Self {
        Self
    }
    pub fn with_default_threads() -> Self {
        Self
    }
    pub fn num_threads(&self) -> usize {
        1
    }
    pub fn parallel<T, F>(&self, jobs: Vec<F>) -> Vec<T>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        jobs.into_iter().map(|job| job()).collect()
    }
}
