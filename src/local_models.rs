//! HuggingFace GGUF model discovery/download and local llama.cpp runtime helpers.
//!
//! This module intentionally keeps the inference runtime out-of-process: oxi starts a
//! `llama-server` child process and talks to its OpenAI-compatible `/v1` API just like it
//! does for Ollama/LM Studio.

use std::fs;
use std::io::{Cursor, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LocalModelManifest {
    #[serde(default)]
    pub models: Vec<DownloadedModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DownloadedModel {
    pub id: String,
    pub repo: String,
    pub filename: String,
    pub path: String,
    #[serde(default)]
    pub bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HfModelHit {
    #[serde(rename = "modelId")]
    pub model_id: String,
    #[serde(default)]
    pub downloads: Option<u64>,
    #[serde(default)]
    pub likes: Option<u64>,
}

#[derive(Debug, Clone)]
pub enum LocalModelMsg {
    Search(Result<Vec<HfModelHit>, String>),
    Files {
        repo: String,
        result: Result<Vec<String>, String>,
    },
    DownloadProgress {
        id: String,
        downloaded: u64,
        total: Option<u64>,
    },
    DownloadDone(Result<DownloadedModel, String>),
    RuntimeInstallProgress {
        downloaded: u64,
        total: Option<u64>,
    },
    RuntimeInstallDone(Result<String, String>),
    RemoteRuntimeInstallDone(Result<String, String>),
    RemoteDownloadDone(Result<DownloadedModel, String>),
    RemoteStartDone {
        model: DownloadedModel,
        result: Result<String, String>,
    },
    RemoteStopDone(Result<String, String>),
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("oxi")
        .join("local-models")
}

pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}

pub fn runtime_dir() -> PathBuf {
    data_dir().join("runtime")
}

pub fn bundled_llama_server_path() -> PathBuf {
    runtime_dir().join(if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    })
}

pub fn runtime_log_path() -> PathBuf {
    runtime_dir().join("llama-server.log")
}

pub fn installed_runtime_path() -> Option<PathBuf> {
    let p = bundled_llama_server_path();
    if p.is_file() { Some(p) } else { None }
}

fn manifest_path() -> PathBuf {
    data_dir().join("manifest.json")
}

pub fn load_manifest() -> LocalModelManifest {
    let path = manifest_path();
    fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_manifest(manifest: &LocalModelManifest) -> Result<(), String> {
    fs::create_dir_all(data_dir()).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(manifest).map_err(|e| e.to_string())?;
    fs::write(manifest_path(), json).map_err(|e| e.to_string())
}

pub fn upsert_downloaded(model: DownloadedModel) -> Result<(), String> {
    let mut manifest = load_manifest();
    manifest.models.retain(|m| m.id != model.id);
    manifest.models.push(model);
    manifest.models.sort_by(|a, b| a.id.cmp(&b.id));
    save_manifest(&manifest)
}

pub fn remove_downloaded(id: &str) -> Result<(), String> {
    let mut manifest = load_manifest();
    let removed: Vec<_> = manifest
        .models
        .iter()
        .filter(|m| m.id == id)
        .cloned()
        .collect();
    manifest.models.retain(|m| m.id != id);
    for m in removed {
        let _ = fs::remove_file(m.path);
    }
    save_manifest(&manifest)
}

pub async fn search_hf_models(
    client: &reqwest::Client,
    query: &str,
) -> Result<Vec<HfModelHit>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let res = client
        .get("https://huggingface.co/api/models")
        .query(&[("search", q), ("filter", "gguf"), ("limit", "20")])
        .send()
        .await
        .map_err(|e| format!("HF search failed: {e}"))?;
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HF search HTTP {status}: {}", snippet(&text)));
    }
    serde_json::from_str(&text).map_err(|e| format!("HF search parse failed: {e}"))
}

pub async fn list_gguf_files(client: &reqwest::Client, repo: &str) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    struct Info {
        #[serde(default)]
        siblings: Vec<Sibling>,
    }
    #[derive(Deserialize)]
    struct Sibling {
        rfilename: String,
    }

    let repo = repo.trim();
    let url = format!("https://huggingface.co/api/models/{repo}");
    let res = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("HF files failed: {e}"))?;
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HF files HTTP {status}: {}", snippet(&text)));
    }
    let info: Info =
        serde_json::from_str(&text).map_err(|e| format!("HF files parse failed: {e}"))?;
    let mut files: Vec<String> = info
        .siblings
        .into_iter()
        .map(|s| s.rfilename)
        .filter(|f| f.to_ascii_lowercase().ends_with(".gguf"))
        .collect();
    files.sort_by(|a, b| quant_rank(a).cmp(&quant_rank(b)).then_with(|| a.cmp(b)));
    Ok(files)
}

pub async fn install_llama_server(
    client: &reqwest::Client,
    tx: std::sync::mpsc::Sender<LocalModelMsg>,
) -> Result<String, String> {
    fs::create_dir_all(runtime_dir()).map_err(|e| e.to_string())?;
    let url = llama_cpp_release_url()?;
    let res = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("runtime download failed: {e}"))?;
    let status = res.status();
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(format!(
            "runtime download HTTP {status}: {}",
            snippet(&text)
        ));
    }
    let total = res.content_length();
    let mut bytes = Vec::new();
    let mut downloaded = 0u64;
    let mut stream = res.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        bytes.extend_from_slice(&chunk);
        let _ = tx.send(LocalModelMsg::RuntimeInstallProgress { downloaded, total });
    }
    clear_runtime_payload()?;
    extract_llama_server_archive(url, &bytes)?;
    create_runtime_library_aliases()?;
    let path = bundled_llama_server_path();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = fs::metadata(&path)
            .map_err(|e| e.to_string())?
            .permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&path, perm).map_err(|e| e.to_string())?;
    }
    Ok(path.to_string_lossy().to_string())
}

pub async fn download_gguf(
    client: &reqwest::Client,
    repo: &str,
    filename: &str,
    tx: std::sync::mpsc::Sender<LocalModelMsg>,
) -> Result<DownloadedModel, String> {
    fs::create_dir_all(models_dir()).map_err(|e| e.to_string())?;
    let safe_repo = repo.replace(['/', '\\', ':'], "__");
    let target_dir = models_dir().join(safe_repo);
    fs::create_dir_all(&target_dir).map_err(|e| e.to_string())?;
    let target = target_dir.join(Path::new(filename).file_name().unwrap_or_default());
    let tmp = target.with_extension("download");
    let url = format!("https://huggingface.co/{repo}/resolve/main/{filename}");
    let res = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("download failed: {e}"))?;
    let status = res.status();
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(format!("download HTTP {status}: {}", snippet(&text)));
    }
    let total = res.content_length();
    let id = format!("{repo}/{filename}");
    let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut downloaded = 0u64;
    let mut stream = res.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        let _ = tx.send(LocalModelMsg::DownloadProgress {
            id: id.clone(),
            downloaded,
            total,
        });
    }
    drop(file);
    fs::rename(&tmp, &target).map_err(|e| e.to_string())?;
    let model = DownloadedModel {
        id,
        repo: repo.to_string(),
        filename: filename.to_string(),
        path: target.to_string_lossy().to_string(),
        bytes: downloaded,
    };
    upsert_downloaded(model.clone())?;
    Ok(model)
}

pub fn spawn_llama_server(
    runtime_path: &str,
    model_path: &str,
    port: u16,
    context: usize,
    gpu_layers: i32,
) -> Result<Child, String> {
    let installed = installed_runtime_path();
    let installed_str = installed.as_ref().map(|p| p.to_string_lossy().to_string());
    let bin = if !runtime_path.trim().is_empty() {
        runtime_path.trim().to_string()
    } else if let Some(p) = installed_str {
        p
    } else {
        "llama-server".to_string()
    };
    let bin_ref = bin.as_str();

    if let Some(parent) = runtime_log_path().parent() {
        let _ = fs::create_dir_all(parent);
    }
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(runtime_log_path())
        .map_err(|e| format!("Could not open llama-server log: {e}"))?;
    let log2 = log
        .try_clone()
        .map_err(|e| format!("Could not clone llama-server log: {e}"))?;

    let mut cmd = Command::new(bin_ref);
    cmd.args([
        "-m",
        model_path,
        "--host",
        "127.0.0.1",
        "--port",
        &port.to_string(),
        "-c",
        &context.to_string(),
    ]);
    if gpu_layers != 0 {
        cmd.args(["-ngl", &gpu_layers.to_string()]);
    }
    if installed_runtime_path().is_some() && runtime_path.trim().is_empty() {
        cmd.current_dir(runtime_dir());
        let old_path = std::env::var("PATH").unwrap_or_default();
        cmd.env(
            "PATH",
            format!("{}:{old_path}", runtime_dir().to_string_lossy()),
        );
        #[cfg(target_os = "macos")]
        cmd.env("DYLD_LIBRARY_PATH", runtime_dir());
        #[cfg(all(unix, not(target_os = "macos")))]
        cmd.env("LD_LIBRARY_PATH", runtime_dir());
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2));
    cmd.spawn().map_err(|e| {
        if runtime_path.trim().is_empty() {
            format!("Could not start bundled/PATH llama-server: {e}. Use Install runtime or set llama-server path. Log: {}", runtime_log_path().display())
        } else {
            format!("Could not start llama-server at `{}`: {e}. Log: {}", runtime_path.trim(), runtime_log_path().display())
        }
    })
}

fn llama_cpp_release_url() -> Result<&'static str, String> {
    // Pinned to a recent llama.cpp build because newer model families (e.g. Gemma 4)
    // need newer architecture support than older runtime releases.
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok(
            "https://github.com/ggml-org/llama.cpp/releases/download/b9910/llama-b9910-bin-macos-arm64.tar.gz",
        ),
        ("macos", "x86_64") => Ok(
            "https://github.com/ggml-org/llama.cpp/releases/download/b9910/llama-b9910-bin-macos-x64.tar.gz",
        ),
        ("linux", "aarch64") => Ok(
            "https://github.com/ggml-org/llama.cpp/releases/download/b9910/llama-b9910-bin-ubuntu-arm64.tar.gz",
        ),
        ("linux", "x86_64") => Ok(
            "https://github.com/ggml-org/llama.cpp/releases/download/b9910/llama-b9910-bin-ubuntu-x64.tar.gz",
        ),
        ("windows", "x86_64") => Ok(
            "https://github.com/ggml-org/llama.cpp/releases/download/b9910/llama-b9910-bin-win-cpu-x64.zip",
        ),
        (os, arch) => Err(format!(
            "No bundled llama-server build for {os}/{arch}. Set a custom llama-server path."
        )),
    }
}

fn extract_llama_server_archive(url: &str, bytes: &[u8]) -> Result<(), String> {
    if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
        extract_llama_server_tgz(bytes)
    } else {
        extract_llama_server_zip(bytes)
    }
}

fn clear_runtime_payload() -> Result<(), String> {
    let dir = runtime_dir();
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let remove = name == "llama-server"
            || name == "llama-server.exe"
            || name.ends_with(".dylib")
            || name.ends_with(".so")
            || name.contains(".so.")
            || name.ends_with(".dll");
        if remove {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}

fn should_extract_runtime_file(base: &str, wanted: &str) -> bool {
    base == wanted
        || base.ends_with(".dylib")
        || base.ends_with(".so")
        || base.contains(".so.")
        || base.ends_with(".dll")
}

fn extract_llama_server_zip(bytes: &[u8]) -> Result<(), String> {
    let wanted = if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    };
    extract_llama_server_zip_to(bytes, &runtime_dir(), wanted)
}

fn extract_llama_server_zip_to(
    bytes: &[u8],
    output_dir: &Path,
    wanted: &str,
) -> Result<(), String> {
    let reader = Cursor::new(bytes);
    let mut zip =
        zip::ZipArchive::new(reader).map_err(|e| format!("runtime zip open failed: {e}"))?;
    fs::create_dir_all(output_dir).map_err(|e| e.to_string())?;
    let mut found_server = false;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| e.to_string())?;
        if file.is_dir() {
            continue;
        }
        let name = file.name().replace('\\', "/");
        let Some(base) = name.rsplit('/').next() else {
            continue;
        };
        // llama-server depends on sibling dynamic libraries in llama.cpp binary archives.
        // Extract executable/library payloads into oxi's runtime folder.
        if !should_extract_runtime_file(base, wanted) {
            continue;
        }
        let out = output_dir.join(base);
        let mut target = fs::File::create(&out).map_err(|e| e.to_string())?;
        std::io::copy(&mut file, &mut target).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            let mode = if base == wanted { 0o755 } else { 0o644 };
            let _ = fs::set_permissions(&out, fs::Permissions::from_mode(mode));
        }
        if base == wanted {
            found_server = true;
        }
    }
    if found_server {
        Ok(())
    } else {
        Err(format!("runtime zip does not contain {wanted}"))
    }
}

fn extract_llama_server_tgz(bytes: &[u8]) -> Result<(), String> {
    fs::create_dir_all(runtime_dir()).map_err(|e| e.to_string())?;
    let gz = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(gz);
    let wanted = if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    };
    let mut found_server = false;
    let entries = archive
        .entries()
        .map_err(|e| format!("runtime tar open failed: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| e.to_string())?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path().map_err(|e| e.to_string())?;
        let Some(base) = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
        else {
            continue;
        };
        if !should_extract_runtime_file(&base, wanted) {
            continue;
        }
        let out = runtime_dir().join(&base);
        let mut target = fs::File::create(&out).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut target).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            let mode = if base == wanted { 0o755 } else { 0o644 };
            let _ = fs::set_permissions(&out, fs::Permissions::from_mode(mode));
        }
        if base == wanted {
            found_server = true;
        }
    }
    if found_server {
        Ok(())
    } else {
        Err(format!("runtime tar.gz does not contain {wanted}"))
    }
}

fn create_runtime_library_aliases() -> Result<(), String> {
    let dir = runtime_dir();
    if !dir.is_dir() {
        return Ok(());
    }
    let mut aliases: Vec<(PathBuf, PathBuf)> = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(alias) = macos_dylib_compat_name(name) {
            aliases.push((path.clone(), dir.join(alias)));
        }
        if let Some(alias) = linux_so_compat_name(name) {
            aliases.push((path.clone(), dir.join(alias)));
        }
    }
    for (target, alias) in aliases {
        if alias == target || alias.exists() {
            continue;
        }
        #[cfg(unix)]
        {
            if let Some(file_name) = target.file_name()
                && std::os::unix::fs::symlink(file_name, &alias).is_ok()
            {
                continue;
            }
        }
        let _ = fs::copy(&target, &alias);
    }
    Ok(())
}

fn macos_dylib_compat_name(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".dylib")?;
    let (prefix, patch) = stem.rsplit_once('.')?;
    if !patch.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (prefix2, minor) = prefix.rsplit_once('.')?;
    if !minor.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (prefix3, major) = prefix2.rsplit_once('.')?;
    if !major.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("{prefix3}.{major}.dylib"))
}

fn linux_so_compat_name(name: &str) -> Option<String> {
    let marker = ".so.";
    let idx = name.find(marker)?;
    let version = &name[idx + marker.len()..];
    let major = version.split('.').next()?;
    if major.is_empty() || !major.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("{}.so.{major}", &name[..idx]))
}

fn quant_rank(s: &str) -> usize {
    let l = s.to_ascii_lowercase();
    for (i, q) in ["q4_k_m", "q5_k_m", "q6_k", "q8_0", "q4_0", "q3_k_m", "f16"]
        .iter()
        .enumerate()
    {
        if l.contains(q) {
            return i;
        }
    }
    99
}

fn snippet(s: &str) -> String {
    let mut out: String = s.chars().take(240).collect();
    if s.chars().count() > 240 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock before Unix epoch")
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("oxi-{label}-{}-{nonce}", std::process::id()));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn runtime_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, contents) in entries {
            writer.start_file(*name, options).expect("start ZIP file");
            writer.write_all(contents).expect("write ZIP file");
        }
        writer.finish().expect("finish ZIP").into_inner()
    }

    #[test]
    fn extracts_server_and_sibling_libraries_from_nested_zip() {
        let archive = runtime_zip(&[
            ("llama/bin/llama-server.exe", b"server"),
            ("llama/bin/ggml.dll", b"library"),
            ("llama/bin/README.txt", b"ignored"),
        ]);
        let output = TestDir::new("runtime-zip");

        extract_llama_server_zip_to(&archive, &output.0, "llama-server.exe")
            .expect("extract runtime ZIP");

        assert_eq!(
            fs::read(output.0.join("llama-server.exe")).unwrap(),
            b"server"
        );
        assert_eq!(fs::read(output.0.join("ggml.dll")).unwrap(), b"library");
        assert!(!output.0.join("README.txt").exists());
        assert!(!output.0.join("llama").exists());
    }

    #[test]
    fn runtime_zip_requires_server_binary() {
        let archive = runtime_zip(&[("llama/bin/ggml.dll", b"library")]);
        let output = TestDir::new("runtime-zip-missing-server");

        let error = extract_llama_server_zip_to(&archive, &output.0, "llama-server.exe")
            .expect_err("ZIP without server must fail");

        assert!(error.contains("does not contain llama-server.exe"));
    }

    #[test]
    fn corrupt_runtime_zip_is_rejected() {
        let output = TestDir::new("runtime-zip-corrupt");

        let error = extract_llama_server_zip_to(b"not a ZIP", &output.0, "llama-server.exe")
            .expect_err("corrupt ZIP must fail");

        assert!(error.contains("runtime zip open failed"));
    }
}
