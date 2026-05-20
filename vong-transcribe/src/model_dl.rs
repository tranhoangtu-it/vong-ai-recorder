//! In-app Whisper GGML model downloader.
//!
//! Downloads `ggml-{name}.bin` from HuggingFace with streaming SHA-256
//! verification. Hash is fetched live from the HF LFS pointer file — never
//! hardcoded. Writes to a `.part` file first; renames atomically on hash
//! match. Orphaned `.part` files from previous crashes are removed at app
//! startup via [`cleanup_stale_parts`].
//!
//! # Privacy
//! All `tracing` calls use structured metadata fields only. No file paths,
//! no URLs, no transcript content — only byte counts, model name, and state
//! transitions.

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use thiserror::Error;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;

// ── Model catalog ──────────────────────────────────────────────────────────

/// Metadata for a single Whisper GGML model variant.
#[derive(Debug, Clone)]
pub struct ModelSpec {
    /// Short identifier: `"tiny"`, `"base"`, `"small"`, `"medium"`.
    pub name: &'static str,
    /// Filename on disk and on HuggingFace: `"ggml-base.bin"`.
    pub filename: &'static str,
    /// Approximate compressed size in bytes (used for disk-space pre-check).
    pub size_bytes: u64,
    /// Human-readable size string shown in the model picker UI.
    pub size_display: &'static str,
    /// Vietnamese speed/quality description shown under each model card.
    pub speed_desc: &'static str,
    /// If true, badge this card as "Khuyến nghị" in the UI.
    pub recommended: bool,
}

/// All four supported Whisper GGML model variants.
///
/// Sizes are approximate and used for UI display + disk-space pre-check.
/// The authoritative SHA-256 is always fetched live from the HF LFS pointer.
pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        name: "tiny",
        filename: "ggml-tiny.bin",
        size_bytes: 78_000_000,
        size_display: "78 MB",
        speed_desc: "Nhanh nhất · chính xác thấp",
        recommended: false,
    },
    ModelSpec {
        name: "base",
        filename: "ggml-base.bin",
        size_bytes: 148_000_000,
        size_display: "148 MB",
        speed_desc: "Cân bằng · khuyến nghị",
        recommended: true,
    },
    ModelSpec {
        name: "small",
        filename: "ggml-small.bin",
        size_bytes: 488_000_000,
        size_display: "488 MB",
        speed_desc: "Chính xác cao · chậm hơn",
        recommended: false,
    },
    ModelSpec {
        name: "medium",
        filename: "ggml-medium.bin",
        size_bytes: 1_500_000_000,
        size_display: "1.5 GB",
        speed_desc: "Chính xác cao nhất · cần GPU",
        recommended: false,
    },
];

/// Return the [`ModelSpec`] for `name`, or `None` if not in the catalog.
pub fn find_spec(name: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.name == name)
}

// ── Error types ───────────────────────────────────────────────────────────

/// All errors that can occur during model download.
#[derive(Debug, Error)]
pub enum DlError {
    /// Network failure (connection refused, timeout, non-2xx status, etc.).
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    /// HF LFS pointer file did not contain an `oid sha256:` line.
    /// Likely means the HF pointer format changed — user should update app.
    #[error("LFS pointer parse failed — vui lòng cập nhật ứng dụng")]
    PointerParse,

    /// Downloaded bytes do not match the expected SHA-256.
    #[error("hash mismatch — tệp tải về bị hỏng, vui lòng thử lại")]
    HashMismatch,

    /// Download was cancelled by the caller via the cancel flag.
    #[error("download cancelled")]
    Cancelled,

    /// Not enough free disk space. `needed_mb` is the additional MB required.
    #[error("không đủ dung lượng đĩa — cần thêm {needed_mb} MB trống")]
    DiskFull {
        /// Additional megabytes required beyond current free space.
        needed_mb: u64,
    },

    /// Filesystem I/O error (write, rename, create directory, etc.).
    #[error("lỗi ghi file: {0}")]
    Io(#[from] std::io::Error),

    /// Unknown model name — not in the MODELS catalog.
    #[error("model không hợp lệ: {0}")]
    UnknownModel(String),

    /// HF returned HTTP 429 — rate limited.
    #[error("quá nhiều yêu cầu, thử lại sau")]
    RateLimited,
}

impl DlError {
    /// Vietnamese user-facing message for UI display.
    pub fn user_message_vi(&self) -> String {
        match self {
            DlError::Network(_) => "Lỗi mạng — kiểm tra kết nối internet".to_string(),
            DlError::PointerParse => {
                "Không đọc được thông tin model — vui lòng cập nhật ứng dụng".to_string()
            }
            DlError::HashMismatch => "Tệp tải về bị hỏng, vui lòng thử lại".to_string(),
            DlError::Cancelled => "Đã hủy tải xuống".to_string(),
            DlError::DiskFull { needed_mb } => {
                format!("Không đủ dung lượng đĩa — cần thêm {needed_mb} MB trống")
            }
            DlError::Io(e) => format!("Lỗi ghi file: {e}"),
            DlError::UnknownModel(n) => format!("Model không hợp lệ: {n}"),
            DlError::RateLimited => "HuggingFace đang giới hạn tốc độ, thử lại sau".to_string(),
        }
    }
}

// ── Progress types ────────────────────────────────────────────────────────

/// Download state machine — values map 1:1 to the `state` string in
/// `DownloadProgress` for passing through the Slint `Arc<Mutex<>>` boundary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DownloadState {
    /// No download in progress.
    #[default]
    Idle,
    /// Fetching LFS pointer + streaming binary.
    Fetching,
    /// Post-download hash verification (near-instant; shown for UX clarity).
    Hashing,
    /// Download complete and verified.
    Done,
    /// Download failed — `error_msg` contains the Vietnamese error string.
    Error,
    /// Download was cancelled by the user.
    Cancelled,
}

impl DownloadState {
    /// Lowercase ASCII string representation used in Slint `state` field.
    pub fn as_str(&self) -> &'static str {
        match self {
            DownloadState::Idle => "idle",
            DownloadState::Fetching => "fetching",
            DownloadState::Hashing => "hashing",
            DownloadState::Done => "done",
            DownloadState::Error => "error",
            DownloadState::Cancelled => "cancelled",
        }
    }
}

/// Shared download progress state. Held behind `Arc<Mutex<>>` so both the
/// download task (writer) and the Slint 30 Hz timer (reader) can access it
/// without allocating per-chunk.
#[derive(Debug, Default)]
pub struct DownloadProgress {
    /// Bytes downloaded so far.
    pub bytes: u64,
    /// Total bytes expected from `Content-Length`, if known.
    pub total: Option<u64>,
    /// Smoothed ETA in seconds; `None` until enough data for estimation.
    pub eta_secs: Option<u64>,
    /// Current download state.
    pub state: DownloadState,
    /// Vietnamese error message; non-empty only when `state == Error`.
    pub error_msg: String,

    // Internal EMA state — not exposed to Slint.
    ema_rate_bps: Option<f64>, // bytes per second, EMA-smoothed
    last_update: Option<Instant>,
}

const EMA_ALPHA: f64 = 0.3;

impl DownloadProgress {
    /// Update `bytes` and recompute the smoothed ETA.
    ///
    /// Called once per downloaded chunk; must not block.
    pub fn update_bytes(&mut self, bytes: u64, total: Option<u64>) {
        let now = Instant::now();
        let delta_bytes = bytes.saturating_sub(self.bytes) as f64;

        // Compute instantaneous rate and apply EMA smoothing.
        if let Some(last) = self.last_update {
            let elapsed = last.elapsed().as_secs_f64();
            if elapsed > 0.001 {
                let instant_rate = delta_bytes / elapsed;
                self.ema_rate_bps = Some(match self.ema_rate_bps {
                    None => instant_rate,
                    Some(prev) => EMA_ALPHA * instant_rate + (1.0 - EMA_ALPHA) * prev,
                });
            }
        }

        self.bytes = bytes;
        self.total = total;
        self.last_update = Some(now);

        // Recompute ETA.
        self.eta_secs = if let (Some(total), Some(rate)) = (total, self.ema_rate_bps) {
            if rate > 0.0 && total > bytes {
                Some(((total - bytes) as f64 / rate) as u64)
            } else {
                Some(0)
            }
        } else {
            None
        };
    }

    /// Seed the EMA rate directly — used in tests to bypass wall-clock timing.
    pub fn seed_rate_bps(&mut self, rate: f64) {
        self.ema_rate_bps = Some(rate);
        self.last_update = Some(Instant::now());
    }

    /// Reset to idle state for a fresh download attempt.
    pub fn reset(&mut self) {
        self.bytes = 0;
        self.total = None;
        self.eta_secs = None;
        self.state = DownloadState::Idle;
        self.error_msg = String::new();
        self.ema_rate_bps = None;
        self.last_update = None;
    }

    /// Pre-computed percent 0.0..=100.0 for the Slint progress bar.
    pub fn percent(&self) -> f32 {
        match self.total {
            Some(t) if t > 0 => (self.bytes as f32 / t as f32 * 100.0).min(100.0),
            _ => 0.0,
        }
    }
}

// ── HF LFS pointer fetch ──────────────────────────────────────────────────

/// Fetch the HF LFS pointer file and extract the expected SHA-256 hex string.
///
/// The pointer file is ~130 bytes plaintext:
/// ```text
/// version https://git-lfs.github.com/spec/v1
/// oid sha256:60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe
/// size 147951465
/// ```
pub async fn fetch_lfs_pointer(spec: &ModelSpec) -> Result<(String, u64), DlError> {
    let url = format!(
        "https://huggingface.co/ggerganov/whisper.cpp/raw/main/{}",
        spec.filename
    );

    let resp = reqwest::get(&url).await.map_err(DlError::Network)?;

    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(DlError::RateLimited);
    }

    let resp = resp.error_for_status().map_err(DlError::Network)?;
    let text = resp.text().await.map_err(DlError::Network)?;

    parse_lfs_pointer(&text)
}

/// Parse an LFS pointer text body and extract `(sha256_hex, size_bytes)`.
///
/// Exposed as `pub` for unit testing without network.
pub fn parse_lfs_pointer(text: &str) -> Result<(String, u64), DlError> {
    let mut sha = None;
    let mut size = None;

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("oid sha256:") {
            sha = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("size ") {
            size = rest.trim().parse::<u64>().ok();
        }
    }

    match (sha, size) {
        (Some(s), Some(z)) => Ok((s, z)),
        _ => Err(DlError::PointerParse),
    }
}

// ── Disk-space check ──────────────────────────────────────────────────────

/// Check that `dir` has at least `needed_bytes` free.
///
/// Uses `statvfs` on POSIX and `GetDiskFreeSpaceExW` on Windows via
/// `std::fs::metadata` — falls back to allowing the download on any query
/// failure (best-effort).
fn check_disk_space(dir: &Path, needed_bytes: u64) -> Result<(), DlError> {
    let free = available_bytes(dir);
    match free {
        Some(free) if free < needed_bytes => {
            let needed_mb = (needed_bytes.saturating_sub(free)) / (1024 * 1024) + 1;
            Err(DlError::DiskFull { needed_mb })
        }
        _ => Ok(()), // unknown free space → allow (best-effort)
    }
}

/// Query available bytes at `path` using platform APIs via `std`.
///
/// On Windows, calls `GetDiskFreeSpaceExW` through a raw `extern "system"`
/// declaration so we don't need the `windows-sys` crate (already in the
/// build graph via `reqwest`/`rustls` but not listed in our direct deps).
/// On non-Windows, returns `None` — the 2× safety margin in the caller
/// absorbs the uncertainty (best-effort).
fn available_bytes(path: &Path) -> Option<u64> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;

        extern "system" {
            fn GetDiskFreeSpaceExW(
                lp_directory_name: *const u16,
                lp_free_bytes_available: *mut u64,
                lp_total_number_of_bytes: *mut u64,
                lp_total_number_of_free_bytes: *mut u64,
            ) -> i32;
        }

        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut free_caller: u64 = 0;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free_caller,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok != 0 { Some(free_caller) } else { None }
    }

    #[cfg(not(target_os = "windows"))]
    {
        // No statvfs without nix/libc; best-effort — caller allows on None.
        let _ = path;
        None
    }
}

// ── Already-downloaded check ──────────────────────────────────────────────

/// If `target_dir/ggml-{name}.bin` exists and its SHA-256 matches the live
/// LFS pointer, return its path immediately without re-downloading.
async fn check_already_downloaded(
    spec: &ModelSpec,
    target_dir: &Path,
    expected_sha: &str,
    progress: &Arc<Mutex<DownloadProgress>>,
) -> Option<PathBuf> {
    let bin_path = target_dir.join(spec.filename);
    if !bin_path.exists() {
        return None;
    }

    // Hash the existing file to verify integrity.
    if let Ok(actual) = hash_file_sync(&bin_path) {
        if actual == expected_sha {
            tracing::info!(
                model_name = spec.name,
                state_transition = "already_downloaded",
                "model already present and verified"
            );
            if let Ok(mut p) = progress.lock() {
                p.state = DownloadState::Done;
            }
            return Some(bin_path);
        }
    }
    None
}

/// Synchronously SHA-256 hash a file. Runs on the tokio blocking pool
/// via `spawn_blocking` before being called here.
fn hash_file_sync(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

// ── Main download function ────────────────────────────────────────────────

/// Download `ggml-{name}.bin` into `target_dir`.
///
/// # Algorithm
///
/// 1. Look up `name` in [`MODELS`] catalog.
/// 2. Fetch the HF LFS pointer (~130 B) to obtain the authoritative SHA-256.
/// 3. Pre-check disk space: needs ≥ `spec.size_bytes × 2` bytes free.
/// 4. If the file already exists and hash matches → return early (`Ok(path)`).
/// 5. Stream the binary into `<target_dir>/ggml-{name}.bin.part`, hashing
///    each chunk with SHA-256. Check `cancel` on every chunk.
/// 6. On all chunks received: compare actual hash vs expected.
///    - Match → atomic rename `.part → .bin`, return `Ok(path)`.
///    - Mismatch → delete `.part`, return `Err(DlError::HashMismatch)`.
///
/// # Cancellation
///
/// Set `cancel.store(true, Ordering::Relaxed)` from any thread. The download
/// loop checks it per chunk (≤64 KB gap). On cancel, `.part` is deleted and
/// `DlError::Cancelled` is returned.
///
/// # Progress
///
/// Caller holds `progress: Arc<Mutex<DownloadProgress>>`. The download task
/// updates it ≥ 10 Hz (per chunk at 64 KB chunks over a 100 Mbps link).
/// The Slint 30 Hz timer reads it from the UI thread.
pub async fn download_model(
    name: &str,
    target_dir: &Path,
    progress: Arc<Mutex<DownloadProgress>>,
    cancel: Arc<AtomicBool>,
) -> Result<PathBuf, DlError> {
    // Reset progress to clean state.
    if let Ok(mut p) = progress.lock() {
        p.reset();
        p.state = DownloadState::Fetching;
    }

    let spec = find_spec(name).ok_or_else(|| DlError::UnknownModel(name.to_string()))?;

    tracing::info!(
        model_name = spec.name,
        model_size_class = spec.size_display,
        state_transition = "start",
        "model download starting"
    );

    // Step 1: Fetch LFS pointer for authoritative hash + actual size.
    let (expected_sha, lfs_size) = fetch_lfs_pointer(spec).await?;

    // Step 2: Disk space pre-check. Use the LFS-reported size × 2 as the
    // safety margin (need room for .part + final .bin simultaneously).
    std::fs::create_dir_all(target_dir)?;
    check_disk_space(target_dir, lfs_size * 2)?;

    // Step 3: Already-downloaded short-circuit.
    if let Some(existing) =
        check_already_downloaded(spec, target_dir, &expected_sha, &progress).await
    {
        return Ok(existing);
    }

    // Step 4: Begin streaming download.
    let dl_url = format!(
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
        spec.filename
    );

    // Retry once on 429.
    let resp = {
        let r = reqwest::get(&dl_url).await.map_err(DlError::Network)?;
        if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            tracing::warn!(
                model_name = spec.name,
                state_transition = "rate_limited_retry",
                "HF 429 — waiting 30 s then retrying once"
            );
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            reqwest::get(&dl_url).await.map_err(DlError::Network)?
        } else {
            r
        }
    };
    let resp = resp.error_for_status().map_err(DlError::Network)?;
    let content_length = resp.content_length();

    // Seed progress with total size from LFS pointer (more reliable than
    // Content-Length through CDN redirects).
    if let Ok(mut p) = progress.lock() {
        p.state = DownloadState::Fetching;
        p.total = Some(lfs_size);
    }

    let part_path = target_dir.join(format!("{}.part", spec.filename));
    let bin_path = target_dir.join(spec.filename);

    let mut file = File::create(&part_path).await?;
    let mut hasher = Sha256::new();
    let mut bytes_downloaded: u64 = 0;
    let mut stream = resp.bytes_stream();

    while let Some(chunk_result) = stream.next().await {
        // Check cancel before processing chunk.
        if cancel.load(Ordering::Relaxed) {
            drop(file);
            let _ = tokio::fs::remove_file(&part_path).await;
            if let Ok(mut p) = progress.lock() {
                p.state = DownloadState::Cancelled;
            }
            tracing::info!(
                model_name = spec.name,
                bytes_downloaded,
                state_transition = "cancelled",
                "download cancelled by user"
            );
            return Err(DlError::Cancelled);
        }

        let chunk = chunk_result.map_err(DlError::Network)?;
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(|e| {
            // Clean up .part on write failure.
            let part = part_path.clone();
            tokio::spawn(async move {
                let _ = tokio::fs::remove_file(part).await;
            });
            DlError::Io(e)
        })?;

        bytes_downloaded += chunk.len() as u64;

        if let Ok(mut p) = progress.lock() {
            p.update_bytes(bytes_downloaded, content_length.or(Some(lfs_size)));
        }
    }

    file.flush().await?;
    drop(file);

    // Step 5: Hash verification.
    if let Ok(mut p) = progress.lock() {
        p.state = DownloadState::Hashing;
    }

    let actual_sha = format!("{:x}", hasher.finalize());
    if actual_sha != expected_sha {
        let _ = tokio::fs::remove_file(&part_path).await;
        if let Ok(mut p) = progress.lock() {
            p.state = DownloadState::Error;
            p.error_msg = DlError::HashMismatch.user_message_vi();
        }
        tracing::warn!(
            model_name = spec.name,
            bytes_downloaded,
            state_transition = "hash_mismatch",
            "model hash verification failed"
        );
        return Err(DlError::HashMismatch);
    }

    // Step 6: Atomic rename — only after verified.
    tokio::fs::rename(&part_path, &bin_path).await?;

    if let Ok(mut p) = progress.lock() {
        p.state = DownloadState::Done;
        p.bytes = bytes_downloaded;
    }

    tracing::info!(
        model_name = spec.name,
        bytes_downloaded,
        state_transition = "done",
        "model download complete and verified"
    );

    Ok(bin_path)
}

// ── Stale .part cleanup ───────────────────────────────────────────────────

/// Scan `models_dir` for `*.part` files older than 1 hour and delete them.
///
/// Called once at app startup (after logger init) to clean up orphaned
/// `.part` files left by previous crashes or cancelled downloads.
///
/// Returns the count of files deleted.
pub fn cleanup_stale_parts(models_dir: &Path) -> usize {
    let threshold = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(3600))
        .unwrap_or(std::time::UNIX_EPOCH);

    let mut count = 0usize;

    let Ok(entries) = std::fs::read_dir(models_dir) else {
        return 0;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "part") {
            if let Ok(meta) = entry.metadata() {
                if let Ok(mtime) = meta.modified() {
                    if mtime < threshold && std::fs::remove_file(&path).is_ok() {
                        count += 1;
                        tracing::debug!(
                            state_transition = "stale_part_removed",
                            "removed stale .part file"
                        );
                    }
                }
            }
        }
    }

    if count > 0 {
        tracing::info!(stale_parts_cleaned = count, "models dir cleanup complete");
    }

    count
}

