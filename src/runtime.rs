//! The app-wide Tokio runtime and HTTP client.
//!
//! Every multi-thread `Runtime::new()` starts one worker per core, so building one per task
//! (update check, model downloads, one-shot completions, manager threads) multiplied idle
//! threads. Background work shares this runtime instead: long-lived managers keep their own
//! OS thread and `block_on` here, one-shot tasks `block_on` from a short-lived thread.
//!
//! The shared [`http_client`] must only be driven from this runtime: pooled connections belong
//! to the runtime that opened them and break once that runtime is dropped.

use std::sync::OnceLock;
use std::time::Duration;

use tokio::runtime::Runtime;

/// Network I/O and SSE parsing are light; a few workers cover every concurrent run.
const MAX_WORKER_THREADS: usize = 4;

static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// The shared runtime, built on first use.
pub fn runtime() -> Result<&'static Runtime, String> {
    RUNTIME
        .get_or_init(|| {
            let workers = std::thread::available_parallelism()
                .map_or(2, |n| n.get())
                .clamp(2, MAX_WORKER_THREADS);
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(workers)
                .thread_name("oxi-rt")
                .enable_all()
                .build()
                .map_err(|e| format!("tokio: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Drive `future` to completion on the shared runtime from synchronous code: a plain thread,
/// a `spawn_blocking` closure or a [`block_in_place`] section (never from inside an async task).
pub fn block_on<F: std::future::Future>(future: F) -> Result<F::Output, String> {
    Ok(runtime()?.block_on(future))
}

/// Shared client for plain requests (no per-request TLS, proxy or DNS overrides), so repeated
/// calls to the same host reuse pooled TLS connections. Cloning is cheap.
pub fn http_client() -> reqwest::Client {
    HTTP_CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default()
        })
        .clone()
}

/// Run blocking work from async code without stalling a worker: on a multi-thread runtime the
/// worker's other tasks move elsewhere first. Elsewhere (plain threads, current-thread test
/// runtimes, where `block_in_place` would panic) `f` just runs.
pub fn block_in_place<R>(f: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_is_shared_and_usable_from_many_threads() {
        let a = runtime().unwrap() as *const Runtime;
        let b = runtime().unwrap() as *const Runtime;
        assert_eq!(a, b);
        let handles: Vec<_> = (0..8)
            .map(|i| std::thread::spawn(move || runtime().unwrap().block_on(async move { i * 2 })))
            .collect();
        let sum: i32 = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(sum, 56);
    }
}
