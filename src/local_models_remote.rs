//! Remote SSH helpers for oxi-managed HuggingFace GGUF runtimes.

use crate::compute;
use crate::local_models::DownloadedModel;
use crate::settings::SshConfig;

// Keep the SSH-managed runtime and downloads in their own namespace. In particular, an SSH
// target may be this same machine (a common way to test Remote HF); sharing `local-models`
// then made the remote switch kill/read the Local HF process, pid file, and log.
const REMOTE_BASE: &str = "$HOME/.local/share/oxi/remote-hf";

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn runtime_url_for(os: &str, arch: &str) -> Result<&'static str, String> {
    match (os, arch) {
        ("darwin", "arm64") | ("darwin", "aarch64") => Ok(
            "https://github.com/ggml-org/llama.cpp/releases/download/b9910/llama-b9910-bin-macos-arm64.tar.gz",
        ),
        ("darwin", "x86_64") => Ok(
            "https://github.com/ggml-org/llama.cpp/releases/download/b9910/llama-b9910-bin-macos-x64.tar.gz",
        ),
        ("linux", "x86_64") | ("linux", "amd64") => Ok(
            "https://github.com/ggml-org/llama.cpp/releases/download/b9910/llama-b9910-bin-ubuntu-x64.tar.gz",
        ),
        ("linux", "aarch64") | ("linux", "arm64") => Ok(
            "https://github.com/ggml-org/llama.cpp/releases/download/b9910/llama-b9910-bin-ubuntu-arm64.tar.gz",
        ),
        _ => Err(format!(
            "No llama.cpp runtime mapping for remote {os}/{arch}"
        )),
    }
}

async fn exec_ok(cfg: &SshConfig, password: &str, command: &str) -> Result<String, String> {
    let out = compute::ssh_exec(cfg, password, command)
        .await
        .map_err(|e| e.to_string())?;
    if out.status == 0 {
        return Ok(out.stdout);
    }

    // Some SSH servers occasionally return only status 1 after a long command, with both
    // output streams empty (for example when the transport closes just as the process exits).
    // Preserve the useful status, but don't render misleading empty stdout/stderr sections.
    let stdout = out.stdout.trim();
    let stderr = out.stderr.trim();
    let mut detail = format!("remote command failed with exit status {}", out.status);
    if !stderr.is_empty() {
        detail.push_str("\nstderr:\n");
        detail.push_str(stderr);
    }
    if !stdout.is_empty() {
        detail.push_str("\nstdout:\n");
        detail.push_str(stdout);
    }
    if stdout.is_empty() && stderr.is_empty() {
        detail.push_str(
            "\nThe SSH host returned no diagnostic output. Check that llama-server is still running and retry.",
        );
    }
    Err(detail)
}

pub async fn install_runtime(cfg: &SshConfig, password: &str) -> Result<String, String> {
    let probe = exec_ok(
        cfg,
        password,
        "printf '%s %s' \"$(uname -s | tr '[:upper:]' '[:lower:]')\" \"$(uname -m)\"",
    )
    .await?;
    let mut parts = probe.split_whitespace();
    let os = parts.next().unwrap_or_default();
    let arch = parts.next().unwrap_or_default();
    let url = runtime_url_for(os, arch)?;
    let cmd = format!(
        r#"set -eu
base={base}
rt="$base/runtime"
mkdir -p "$rt"
find "$rt" -maxdepth 1 -type f ! -name 'llama-server.log' -delete 2>/dev/null || true
archive="$rt/runtime.archive"
if command -v curl >/dev/null 2>&1; then
  curl -L --fail -o "$archive" {url}
elif command -v wget >/dev/null 2>&1; then
  wget -O "$archive" {url}
else
  echo "curl or wget is required on the remote host" >&2
  exit 127
fi
case "$archive" in
  *.zip) unzip -oq "$archive" -d "$rt/extract" ;;
  *) mkdir -p "$rt/extract"; tar -xzf "$archive" -C "$rt/extract" ;;
esac
find "$rt/extract" -type f \( -name 'llama-server' -o -name 'llama-server.exe' -o -name '*.dylib' -o -name '*.so' -o -name '*.so.*' -o -name '*.dll' \) -exec sh -c 'for f do cp "$f" "$0/$(basename "$f")"; done' "$rt" {{}} +
rm -rf "$rt/extract" "$archive"
chmod +x "$rt/llama-server" 2>/dev/null || true
cd "$rt"
find . -maxdepth 1 -type f -name '*.dylib' -print | while IFS= read -r p; do
  f=${{p#./}}
  stem=${{f%.dylib}}
  prefix=${{stem%%.*}}
  rest=${{stem#*.}}
  major=${{rest%%.*}}
  [ "$stem" != "$rest" ] || continue
  case "$major" in ''|*[!0-9]*) continue ;; esac
  short="$prefix.$major.dylib"
  [ "$short" = "$f" ] || ln -sf "$f" "$short"
done
find . -maxdepth 1 -type f -name '*.so.*' -print | while IFS= read -r p; do
  f=${{p#./}}
  short=$(printf '%s\n' "$f" | sed -E 's/^(.*\.so\.[0-9]+)\..*$/\1/')
  [ "$short" = "$f" ] || ln -sf "$f" "$short"
done
printf '%s/llama-server' "$rt"
"#,
        base = REMOTE_BASE,
        url = sh_quote(url),
    );
    exec_ok(cfg, password, &cmd)
        .await
        .map(|s| s.trim().to_string())
}

pub async fn download_model(
    cfg: &SshConfig,
    password: &str,
    repo: &str,
    filename: &str,
) -> Result<DownloadedModel, String> {
    let id = format!("{repo}/{filename}");
    let safe_repo = repo.replace(['/', '\\', ':'], "__");
    let base_name = std::path::Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(filename);
    let url = format!("https://huggingface.co/{repo}/resolve/main/{filename}");
    let cmd = format!(
        r#"set -eu
base={base}
# Keep shell-quoted user-controlled components outside double quotes. Putting
# one inside `"$base/..."` made its quote characters part of the filename.
dir="$base/models"/{safe_repo}
mkdir -p "$dir"
out="$dir"/{base_name}
part="$out.download"
if command -v curl >/dev/null 2>&1; then
  curl -L --fail -C - -o "$part" {url}
elif command -v wget >/dev/null 2>&1; then
  wget -c -O "$part" {url}
else
  echo "curl or wget is required on the remote host" >&2
  exit 127
fi
mv "$part" "$out"
bytes=$(wc -c < "$out" | tr -d ' ')
printf '%s\n%s' "$out" "$bytes"
"#,
        base = REMOTE_BASE,
        safe_repo = sh_quote(&safe_repo),
        base_name = sh_quote(base_name),
        url = sh_quote(&url),
    );
    let out = exec_ok(cfg, password, &cmd).await?;
    let mut lines = out.lines();
    let path = lines.next().unwrap_or_default().to_string();
    let bytes = lines
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    Ok(DownloadedModel {
        id,
        repo: repo.to_string(),
        filename: filename.to_string(),
        path,
        bytes,
    })
}

/// Result of [`list_models`]: GGUF files on the SSH host, plus what's running there.
#[derive(Debug, Clone)]
pub struct RemoteModelList {
    pub models: Vec<DownloadedModel>,
    /// `-m <path>` of the llama-server currently running on the host (per its pid
    /// file). A non-empty path is guaranteed to have a row in `models` (synthesized
    /// when the file sits outside the oxi models dir) so the UI can offer Stop.
    /// Empty string = a server is running but its model could not be identified.
    pub running_path: Option<String>,
}

/// List GGUF models present under the oxi models dir on the SSH host, and detect a
/// llama-server left running there (e.g. from a previous oxi session).
pub async fn list_models(cfg: &SshConfig, password: &str) -> Result<RemoteModelList, String> {
    let cmd = format!(
        r#"set -eu
base={base}
rt="$base/runtime"
pid_file="$rt/llama-server.pid"
running_args=""
if [ -f "$pid_file" ] && kill -0 "$(cat "$pid_file")" 2>/dev/null; then
  running_args=$(ps -o args= -p "$(cat "$pid_file")" 2>/dev/null || true)
fi
# Recover from a stale/overwritten pid file by inspecting only Oxi's managed binary.
if [ -z "$running_args" ]; then
  running_args=$(ps -axo command= 2>/dev/null | grep -F "$rt/llama-server" | grep -F -- '--port {port}' | head -n 1 || true)
fi
if [ -n "$running_args" ]; then
  printf 'running\t%s\n' "$running_args"
fi
dir="$base/models"
if [ -d "$dir" ]; then
  # Avoid GNU-only -mindepth: remote hosts may use BSD find (macOS).
  # The final two patterns also discover files written by older Oxi builds which
  # accidentally included literal single quotes around the repo/file components.
  find "$dir" -type f \( -name '*.gguf' -o -name '*.GGUF' -o -name "*.gguf'" -o -name "*.GGUF'" \) | while IFS= read -r f; do
    bytes=$(wc -c < "$f" | tr -d ' ')
    printf 'model\t%s\t%s\n' "$f" "$bytes"
  done
fi
"#,
        base = REMOTE_BASE,
        port = cfg.remote_runtime_port,
    );
    let out = exec_ok(cfg, password, &cmd).await?;
    let mut models = Vec::new();
    let mut running_path: Option<String> = None;
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("model\t") {
            let Some((path, bytes)) = rest.rsplit_once('\t') else {
                continue;
            };
            let bytes = bytes.trim().parse::<u64>().unwrap_or(0);
            if let Some(m) = model_from_remote_path(path, bytes) {
                models.push(m);
            }
        } else if let Some(args) = line.strip_prefix("running\t") {
            running_path = Some(model_arg_from_args(args).unwrap_or_default());
        }
    }
    // A running model always gets a row, even when its file sits outside the oxi
    // models dir, so the panel can show it with a Stop button.
    if let Some(p) = running_path.clone()
        && !p.is_empty()
        && !models.iter().any(|m| m.path == p)
        && let Some(m) = model_from_remote_path(&p, 0)
    {
        models.push(m);
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.path == b.path);
    Ok(RemoteModelList {
        models,
        running_path,
    })
}

/// Reconstruct a [`DownloadedModel`] from a file path on the SSH host.
/// `download_model` flattens "org/model" into "org__model" for the directory name;
/// undo the first separator to recover the repo id.
fn model_from_remote_path(path: &str, bytes: u64) -> Option<DownloadedModel> {
    let p = std::path::Path::new(path);
    let raw_filename = p.file_name().and_then(|n| n.to_str())?;
    let raw_safe_repo = p
        .parent()
        .and_then(|d| d.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    // Oxi briefly produced paths such as models/'org__repo'/'model.gguf'. Keep
    // the real path for launch/delete, but remove those accidental quotes from
    // the model identity shown in the UI.
    let filename = raw_filename
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(raw_filename);
    let safe_repo = raw_safe_repo
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(raw_safe_repo);
    let repo = safe_repo.replacen("__", "/", 1);
    Some(DownloadedModel {
        id: format!("{repo}/{filename}"),
        repo,
        filename: filename.to_string(),
        path: path.to_string(),
        bytes,
    })
}

/// Extract the `-m <path>` argument from a llama-server command line.
/// Paths with spaces aren't recoverable from `ps` output; callers treat a miss as
/// "running, model unknown".
fn model_arg_from_args(args: &str) -> Option<String> {
    let mut it = args.split_whitespace();
    while let Some(a) = it.next() {
        if a == "-m" || a == "--model" {
            return it.next().map(|s| s.to_string());
        }
    }
    None
}

/// Delete a downloaded model file on the SSH host (and its repo dir if now empty).
pub async fn delete_model(cfg: &SshConfig, password: &str, path: &str) -> Result<(), String> {
    let cmd = format!(
        r#"set -eu
f={path}
rm -f -- "$f"
rmdir -- "$(dirname "$f")" 2>/dev/null || true
"#,
        path = sh_quote(path),
    );
    exec_ok(cfg, password, &cmd).await.map(|_| ())
}

pub async fn start_model(
    cfg: &SshConfig,
    password: &str,
    model_path: &str,
    repo: &str,
    filename: &str,
    context: usize,
    gpu_layers: i32,
) -> Result<String, String> {
    let port = cfg.remote_runtime_port;
    let ngl = if gpu_layers != 0 {
        format!(" -ngl {}", gpu_layers)
    } else {
        String::new()
    };
    let safe_repo = repo.replace(['/', '\\', ':'], "__");
    let base_name = std::path::Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(filename);
    let cmd = format!(
        r#"set -eu
base={base}
rt="$base/runtime"
log="$rt/llama-server.log"
pid="$rt/llama-server.pid"
model={model}
# `set -e` can otherwise terminate on a host-specific shell/process utility failure
# without producing any output. Always return enough context for the composer notice.
on_exit() {{
  oxi_status=$?
  trap - 0
  if [ "$oxi_status" -ne 0 ]; then
    echo "Remote model switch failed (status $oxi_status)." >&2
    if [ -f "$pid" ]; then
      server_pid=$(cat "$pid" 2>/dev/null || true)
      echo "Recorded llama-server pid: ${{server_pid:-none}}" >&2
      if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then
        echo "The recorded llama-server process is still running." >&2
      else
        echo "The recorded llama-server process is not running." >&2
      fi
    fi
    if [ -f "$log" ]; then
      echo "Last llama-server log lines ($log):" >&2
      tail -n 120 "$log" >&2 || true
    else
      echo "llama-server log does not exist: $log" >&2
    fi
  fi
  exit "$oxi_status"
}}
trap on_exit 0
if [ ! -f "$model" ]; then
  candidate="$base/models"/{safe_repo}/{base_name}
  if [ -f "$candidate" ]; then
    model="$candidate"
  else
    echo "Model file not found on remote host." >&2
    echo "Tried saved path: $model" >&2
    echo "Tried remote download path: $candidate" >&2
    echo "Download the model while Local HF is set to Remote SSH, then press Play again." >&2
    exit 1
  fi
fi
# The pid file can become stale after a failed replacement. Stop every Oxi-managed
# llama-server on this runtime port, but signal each process only once. llama-server may
# spend several seconds freeing a large model; launching after the old 5-second wait made
# the first switch lose the port race while a second attempt appeared to work.
managed_pids() {{
  ps -axo pid=,command= 2>/dev/null | while read -r old_pid old_args; do
    case "$old_args" in
      "$rt/llama-server "*"--port {port}"*) printf '%s\n' "$old_pid" ;;
    esac
  done
}}
old_pids=$(managed_pids)
for old_pid in $old_pids; do
  kill "$old_pid" 2>/dev/null || true
done
i=0
while [ "$i" -lt 30 ] && [ -n "$(managed_pids)" ]; do
  i=$((i + 1))
  sleep 1
done
# Do not let a wedged old runtime make every replacement fail forever.
remaining_pids=$(managed_pids)
for old_pid in $remaining_pids; do
  kill -9 "$old_pid" 2>/dev/null || true
done
if [ -n "$remaining_pids" ]; then
  sleep 1
fi
rm -f "$pid"
cd "$rt"
# Self-heal macOS/Linux runtime library aliases in case the runtime was installed
# by an older oxi build. llama-server's LC_LOAD_DYLIB may ask for e.g.
# libllama-common.0.dylib while the archive contains libllama-common.0.0.9910.dylib.
find . -maxdepth 1 -type f -name '*.dylib' -print | while IFS= read -r p; do
  f=${{p#./}}
  stem=${{f%.dylib}}
  prefix=${{stem%%.*}}
  rest=${{stem#*.}}
  major=${{rest%%.*}}
  [ "$stem" != "$rest" ] || continue
  case "$major" in ''|*[!0-9]*) continue ;; esac
  short="$prefix.$major.dylib"
  [ "$short" = "$f" ] || ln -sf "$f" "$short"
done
find . -maxdepth 1 -type f -name '*.so.*' -print | while IFS= read -r p; do
  f=${{p#./}}
  short=$(printf '%s\n' "$f" | sed -E 's/^(.*\.so\.[0-9]+)\..*$/\1/')
  [ "$short" = "$f" ] || ln -sf "$f" "$short"
done
: > "$log"
(DYLD_LIBRARY_PATH="$rt:${{DYLD_LIBRARY_PATH:-}}" LD_LIBRARY_PATH="$rt:${{LD_LIBRARY_PATH:-}}" PATH="$rt:$PATH" nohup "$rt/llama-server" -m "$model" --host 127.0.0.1 --port {port} -c {ctx}{ngl} >> "$log" 2>&1 & echo $! > "$pid")
# Wait until llama-server is actually reachable. Starting the process can succeed while
# the port is not open yet (or it can fail a few seconds later while loading the model).
# Returning early makes the SSH tunnel probe fail with "ConnectFailed".
i=0
while [ "$i" -lt 90 ]; do
  if ! kill -0 "$(cat "$pid")" 2>/dev/null; then
    tail -n 120 "$log" >&2 || true
    exit 1
  fi
  if command -v curl >/dev/null 2>&1; then
    # Do not require llama.cpp to echo the absolute model path: versions differ and
    # often expose an alias/name instead. Process ownership plus a healthy API response
    # proves this newly launched managed server owns the endpoint.
    health=$(curl -sS --max-time 2 "http://127.0.0.1:{port}/health" 2>/dev/null || true)
    models=$(curl -sS --max-time 2 "http://127.0.0.1:{port}/v1/models" 2>/dev/null || true)
    if [ -n "$health" ] || [ -n "$models" ]; then
      printf 'Remote llama-server ready with %s on 127.0.0.1:%s. Log: %s' "$model" {port} "$log"
      exit 0
    fi
  elif command -v nc >/dev/null 2>&1 && nc -z 127.0.0.1 {port} >/dev/null 2>&1; then
    # With no curl, process ownership plus an open port is the best available check.
    printf 'Remote llama-server ready on 127.0.0.1:%s. Log: %s' {port} "$log"
    exit 0
  fi
  i=$((i + 1))
  sleep 1
done
echo "llama-server started but did not open 127.0.0.1:{port} within 90s" >&2
tail -n 120 "$log" >&2 || true
exit 1
"#,
        base = REMOTE_BASE,
        model = sh_quote(model_path),
        safe_repo = sh_quote(&safe_repo),
        base_name = sh_quote(base_name),
        port = port,
        ctx = context,
        ngl = ngl,
    );
    exec_ok(cfg, password, &cmd)
        .await
        .map(|s| s.trim().to_string())
}

pub async fn stop_model(cfg: &SshConfig, password: &str) -> Result<String, String> {
    let cmd = format!(
        r#"set -eu
rt={base}/runtime
pid="$rt/llama-server.pid"
port={port}
stopped=0
if [ -f "$pid" ] && kill -0 "$(cat "$pid")" 2>/dev/null; then
  kill "$(cat "$pid")" 2>/dev/null || true
  stopped=1
fi
# Also stop an older managed process if its pid file was overwritten or stale.
ps -axo pid=,command= 2>/dev/null | while read -r old_pid old_args; do
  case "$old_args" in
    "$rt/llama-server "*"--port $port"*) kill "$old_pid" 2>/dev/null || true ;;
  esac
done
rm -f "$pid"
# Report stopped even when only the stale process scan found it; this command is idempotent.
echo stopped
"#,
        base = REMOTE_BASE,
        port = cfg.remote_runtime_port,
    );
    exec_ok(cfg, password, &cmd)
        .await
        .map(|s| s.trim().to_string())
}

pub fn password_for_remotehf() -> String {
    let creds = compute::load_ssh_credentials();
    creds
        .get(crate::settings::LlmProviderKind::RemoteHf.slug())
        // Seamless migration: older builds stored Remote HF credentials under Local HF.
        .or_else(|| creds.get(crate::settings::LlmProviderKind::LocalHf.slug()))
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_path_recovers_repo_and_filename() {
        let m = model_from_remote_path(
            "/home/u/.local/share/oxi/local-models/models/unsloth__Qwen3-GGUF/q4.gguf",
            42,
        )
        .unwrap();
        assert_eq!(m.repo, "unsloth/Qwen3-GGUF");
        assert_eq!(m.filename, "q4.gguf");
        assert_eq!(m.id, "unsloth/Qwen3-GGUF/q4.gguf");
        assert_eq!(m.bytes, 42);
    }

    #[test]
    fn remote_path_recovers_identity_from_legacy_quoted_components() {
        let m = model_from_remote_path(
            "/Users/manu/.local/share/oxi/local-models/models/'deepreinforce-ai__Ornith-GGUF'/'ornith-Q4.gguf'",
            99,
        )
        .unwrap();
        assert_eq!(m.repo, "deepreinforce-ai/Ornith-GGUF");
        assert_eq!(m.filename, "ornith-Q4.gguf");
        assert_eq!(m.id, "deepreinforce-ai/Ornith-GGUF/ornith-Q4.gguf");
        assert!(m.path.ends_with("/'ornith-Q4.gguf'"));
    }

    #[test]
    fn model_arg_parses_short_and_long_flags() {
        assert_eq!(
            model_arg_from_args("/rt/llama-server -m /models/a.gguf --port 8080"),
            Some("/models/a.gguf".to_string())
        );
        assert_eq!(
            model_arg_from_args("llama-server --model /m/b.gguf -c 8192"),
            Some("/m/b.gguf".to_string())
        );
        assert_eq!(model_arg_from_args("llama-server --port 8080"), None);
    }
}
