//! Whisper.cpp GGML model catalog, download, and manifest persistence for local voice
//! dictation. Unlike [`crate::local_models`] (which searches arbitrary HF repos for GGUF
//! LLMs), there is one canonical upstream repo for these models, so this is a small fixed
//! catalog rather than a search UI.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VoiceModelManifest {
    #[serde(default)]
    pub models: Vec<DownloadedVoiceModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DownloadedVoiceModel {
    /// Catalog id (e.g. `"base.en"`), also used as the manifest key.
    pub id: String,
    pub filename: String,
    pub path: String,
    #[serde(default)]
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceModelCatalogEntry {
    pub id: &'static str,
    pub filename: &'static str,
    pub label: &'static str,
    pub approx_mb: u32,
}

/// Curated subset of `ggerganov/whisper.cpp`'s GGML model releases, ordered smallest first.
pub const VOICE_MODEL_CATALOG: &[VoiceModelCatalogEntry] = &[
    VoiceModelCatalogEntry {
        id: "tiny.en",
        filename: "ggml-tiny.en.bin",
        label: "Tiny (English only)",
        approx_mb: 75,
    },
    VoiceModelCatalogEntry {
        id: "base.en",
        filename: "ggml-base.en.bin",
        label: "Base (English only)",
        approx_mb: 142,
    },
    VoiceModelCatalogEntry {
        id: "small.en",
        filename: "ggml-small.en.bin",
        label: "Small (English only)",
        approx_mb: 466,
    },
    VoiceModelCatalogEntry {
        id: "medium.en",
        filename: "ggml-medium.en.bin",
        label: "Medium (English only)",
        approx_mb: 1500,
    },
    VoiceModelCatalogEntry {
        id: "tiny",
        filename: "ggml-tiny.bin",
        label: "Tiny (multilingual)",
        approx_mb: 75,
    },
    VoiceModelCatalogEntry {
        id: "base",
        filename: "ggml-base.bin",
        label: "Base (multilingual)",
        approx_mb: 142,
    },
    VoiceModelCatalogEntry {
        id: "small",
        filename: "ggml-small.bin",
        label: "Small (multilingual)",
        approx_mb: 466,
    },
    VoiceModelCatalogEntry {
        id: "medium",
        filename: "ggml-medium.bin",
        label: "Medium (multilingual)",
        approx_mb: 1500,
    },
    VoiceModelCatalogEntry {
        id: "large-v3",
        filename: "ggml-large-v3.bin",
        label: "Large v3 (multilingual)",
        approx_mb: 2900,
    },
];

const HF_REPO: &str = "ggerganov/whisper.cpp";

#[derive(Debug, Clone)]
pub enum VoiceModelMsg {
    DownloadProgress {
        id: String,
        downloaded: u64,
        total: Option<u64>,
    },
    DownloadDone(Result<DownloadedVoiceModel, String>),
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("oxi")
        .join("voice-models")
}

fn manifest_path() -> PathBuf {
    data_dir().join("manifest.json")
}

pub fn load_manifest() -> VoiceModelManifest {
    let path = manifest_path();
    fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_manifest(manifest: &VoiceModelManifest) -> Result<(), String> {
    fs::create_dir_all(data_dir()).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(manifest).map_err(|e| e.to_string())?;
    fs::write(manifest_path(), json).map_err(|e| e.to_string())
}

pub fn upsert_downloaded(model: DownloadedVoiceModel) -> Result<(), String> {
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

pub async fn download_model(
    client: &reqwest::Client,
    entry: &VoiceModelCatalogEntry,
    tx: std::sync::mpsc::Sender<VoiceModelMsg>,
) -> Result<DownloadedVoiceModel, String> {
    let dir = data_dir().join("models");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(entry.filename);
    let tmp = target.with_extension("download");
    let url = format!(
        "https://huggingface.co/{HF_REPO}/resolve/main/{}",
        entry.filename
    );
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
    let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut downloaded = 0u64;
    let mut stream = res.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        let _ = tx.send(VoiceModelMsg::DownloadProgress {
            id: entry.id.to_string(),
            downloaded,
            total,
        });
    }
    drop(file);
    fs::rename(&tmp, &target).map_err(|e| e.to_string())?;
    let model = DownloadedVoiceModel {
        id: entry.id.to_string(),
        filename: entry.filename.to_string(),
        path: target.to_string_lossy().to_string(),
        bytes: downloaded,
    };
    upsert_downloaded(model.clone())?;
    Ok(model)
}

fn snippet(s: &str) -> String {
    let mut out: String = s.chars().take(240).collect();
    if s.chars().count() > 240 {
        out.push('…');
    }
    out
}
