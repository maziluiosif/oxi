use std::thread;

use tokio::runtime::Runtime;

/// Run `task` on its own OS thread with the shared runtime, so it can `block_on` async work.
pub(crate) fn spawn_async_task<T>(on_runtime_error: impl FnOnce(String) + Send + 'static, task: T)
where
    T: FnOnce(&'static Runtime) + Send + 'static,
{
    thread::spawn(move || {
        let rt = match crate::runtime::runtime() {
            Ok(rt) => rt,
            Err(err) => {
                on_runtime_error(err.to_string());
                return;
            }
        };
        task(rt);
    });
}
