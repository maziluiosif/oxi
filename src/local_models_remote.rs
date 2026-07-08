//! Remote SSH helpers for oxi-managed HuggingFace GGUF runtimes.

use crate::compute;
use crate::local_models::DownloadedModel;
use crate::settings::SshConfig;

const REMOTE_BASE: &str = "$HOME/.local/share/oxi/local-models";

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
        Ok(out.stdout)
    } else {
        Err(format!(
            "remote command failed with {}\nstdout:\n{}\nstderr:\n{}",
            out.status, out.stdout, out.stderr
        ))
    }
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
dir="$base/models/{safe_repo}"
mkdir -p "$dir"
out="$dir/{base_name}"
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
if [ ! -f "$model" ]; then
  candidate="$base/models/{safe_repo}/{base_name}"
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
if [ -f "$pid" ] && kill -0 "$(cat "$pid")" 2>/dev/null; then
  kill "$(cat "$pid")" 2>/dev/null || true
  sleep 1
fi
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
  if command -v nc >/dev/null 2>&1 && nc -z 127.0.0.1 {port} >/dev/null 2>&1; then
    printf 'Remote llama-server ready on 127.0.0.1:%s. Log: %s' {port} "$log"
    exit 0
  fi
  if command -v curl >/dev/null 2>&1 && curl -sS --max-time 1 -o /dev/null "http://127.0.0.1:{port}/health" >/dev/null 2>&1; then
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
if [ -f "$pid" ] && kill -0 "$(cat "$pid")" 2>/dev/null; then
  kill "$(cat "$pid")"
  rm -f "$pid"
  echo stopped
else
  echo not running
fi
"#,
        base = REMOTE_BASE,
    );
    exec_ok(cfg, password, &cmd)
        .await
        .map(|s| s.trim().to_string())
}

pub fn password_for_localhf() -> String {
    let creds = compute::load_ssh_credentials();
    creds
        .get(crate::settings::LlmProviderKind::LocalHf.slug())
        .unwrap_or_default()
        .to_string()
}
