use std::thread;

use tokio::runtime::Runtime;

pub(crate) fn spawn_async_task<T>(on_runtime_error: impl FnOnce(String) + Send + 'static, task: T)
where
    T: FnOnce(Runtime) + Send + 'static,
{
    thread::spawn(move || {
        let rt = match Runtime::new() {
            Ok(rt) => rt,
            Err(err) => {
                on_runtime_error(err.to_string());
                return;
            }
        };
        task(rt);
    });
}
