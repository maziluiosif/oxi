//! Git of a remote workspace (`ssh://host/path`, see [`crate::remote`]): each entry point of
//! this module's parent asks the host's server to run it there (`fs.git`) and returns its
//! answer, so the panel, the editor's diff mode and the turn cards work unchanged.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

/// Pulls and pushes over a slow link can take a while.
const TIMEOUT: Duration = Duration::from_secs(300);

/// Run `kind` on the host of `cwd`; `None` when `cwd` is a local path.
pub(super) fn call<T: DeserializeOwned>(
    cwd: &str,
    kind: &str,
    mut args: Value,
) -> Option<Result<T, String>> {
    let (host, path) = crate::remote::split(cwd)?;
    args["kind"] = json!(kind);
    args["cwd"] = json!(path);
    Some(
        crate::remote::client::call(host, "fs.git", args, TIMEOUT).and_then(|v| {
            serde_json::from_value(v).map_err(|e| format!("bad answer from {host}: {e}"))
        }),
    )
}

fn answer<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

fn arg<T: DeserializeOwned>(params: &Value, key: &str) -> Result<T, String> {
    serde_json::from_value(params[key].clone()).map_err(|e| format!("{key}: {e}"))
}

/// `fs.git` on the server: the call [`call`] made, run here.
pub fn serve(params: &Value) -> Result<Value, String> {
    let cwd = params["cwd"].as_str().ok_or("cwd is missing")?;
    match params["kind"].as_str().unwrap_or_default() {
        "op" => answer(super::handle_op(cwd, arg(params, "op")?)),
        "view" => answer(super::view_diff(cwd, arg(params, "op")?)),
        "apply_block" => answer(super::hunk::apply_block(cwd, &arg(params, "edit")?)),
        "diff_texts" => answer(super::hunk::diff_texts(
            cwd,
            &arg::<String>(params, "relative")?,
            &arg(params, "base")?,
        )),
        "compare" => answer(super::compare(cwd, &arg::<String>(params, "base")?)),
        "changed_files" => answer(super::checkpoint::changed_files(
            Path::new(cwd),
            &arg::<String>(params, "before")?,
            &arg::<String>(params, "after")?,
        )),
        "patch" => answer(super::checkpoint::patch(
            Path::new(cwd),
            &arg::<String>(params, "before")?,
            &arg::<String>(params, "after")?,
            arg(params, "max_bytes")?,
        )),
        "restore" => answer(super::checkpoint::restore(
            Path::new(cwd),
            &arg::<String>(params, "before")?,
            &arg::<String>(params, "after")?,
            arg::<Option<String>>(params, "only")?.as_deref(),
        )),
        "plan_restore" => answer(super::checkpoint::plan_restore(
            Path::new(cwd),
            &arg::<Vec<(String, String)>>(params, "turns")?,
        )),
        "restore_turns" => answer(super::checkpoint::restore_turns(
            Path::new(cwd),
            &arg::<Vec<(String, String)>>(params, "turns")?,
            arg::<Option<String>>(params, "only")?.as_deref(),
        )),
        other => Err(format!("unknown git call: {other}")),
    }
}
