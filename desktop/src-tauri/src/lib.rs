use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{Emitter, Manager};
use walkdir::WalkDir;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SaveEntry {
    mode: String,
    name: String,
    path: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteManifest {
    exists: bool,
    bytes: Option<u64>,
    updated_at: Option<String>,
    save_mode: String,
    save_name: String,
    device_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteSaveList {
    saves: Vec<RemoteManifest>,
    total_bytes: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteSaveLibrary {
    saves: Vec<RemoteManifest>,
    total_bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadSession {
    upload_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadStatus {
    upload_id: String,
    total_bytes: u64,
    chunk_size: u64,
    chunk_count: u64,
    save_mode: String,
    save_name: String,
    zip_sha256: String,
    completed_chunks: Vec<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct CachedFileEntry {
    path: String,
    bytes: u64,
    modified_ns: u64,
    sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CacheMetadata {
    schema_version: u8,
    save_mode: String,
    save_name: String,
    fingerprint: String,
    zip_sha256: String,
    zip_bytes: u64,
    files: Vec<CachedFileEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResumeState {
    schema_version: u8,
    endpoint_hash: String,
    sync_key_hash: String,
    save_mode: String,
    save_name: String,
    fingerprint: String,
    zip_sha256: String,
    zip_bytes: u64,
    chunk_size: u64,
    chunk_count: u64,
    upload_id: String,
}

struct PreparedArchive {
    path: PathBuf,
    file_count: u64,
    bytes: u64,
    fingerprint: String,
    zip_sha256: String,
    cache_dir: PathBuf,
    reused: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadProgress {
    percent: u8,
    message: String,
}

fn emit_upload_progress(app: &tauri::AppHandle, percent: u8, message: impl Into<String>) {
    let _ = app.emit(
        "upload-progress",
        UploadProgress {
            percent,
            message: message.into(),
        },
    );
}

fn emit_download_progress(app: &tauri::AppHandle, percent: u8, message: impl Into<String>) {
    let _ = app.emit(
        "download-progress",
        UploadProgress {
            percent,
            message: message.into(),
        },
    );
}

fn retryable_upload_status(status: StatusCode) -> bool {
    status.is_server_error()
        || status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
}

fn request_error_details(error: &reqwest::Error) -> String {
    let mut details = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        details.push_str(": ");
        details.push_str(&cause.to_string());
        source = cause.source();
    }
    details
}

fn stage_percent(completed: u64, total: u64) -> u8 {
    if total == 0 {
        return 100;
    }
    (((completed.min(total) as u128) * 100) / total as u128) as u8
}

fn home_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    let value = std::env::var_os("USERPROFILE");
    #[cfg(not(target_os = "windows"))]
    let value = std::env::var_os("HOME");
    value
        .map(PathBuf::from)
        .ok_or_else(|| "无法找到用户目录".to_string())
}

fn is_directory(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false)
}

fn is_ignored_metadata_directory(name: &str) -> bool {
    name.eq_ignore_ascii_case("__MACOSX")
}

fn process_list_contains_game(text: &str) -> bool {
    text.lines().any(|line| {
        let executable = line
            .split(',')
            .next()
            .unwrap_or(line)
            .trim()
            .trim_matches('"')
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        matches!(
            executable.as_str(),
            "projectzomboid.exe" | "projectzomboid64.exe" | "projectzomboid"
        )
    })
}

fn game_is_running() -> bool {
    #[cfg(target_os = "windows")]
    let output = Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .output();
    #[cfg(not(target_os = "windows"))]
    let output = Command::new("ps").args(["-A", "-o", "comm="]).output();

    let Ok(output) = output else { return false };
    process_list_contains_game(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(test)]
mod tests {
    use super::{
        completed_upload_bytes, is_ignored_metadata_directory, normalize_endpoint,
        process_list_contains_game, resume_matches, retryable_upload_status, save_fingerprint,
        scan_save_files, sha256_text, stage_percent, PreparedArchive, ResumeState,
    };
    use reqwest::StatusCode;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn sync_client_is_not_the_game() {
        assert!(!process_list_contains_game(
            "\"zomboid-save-sync.exe\",\"37316\",\"Console\",\"1\",\"40,324 K\""
        ));
    }

    #[test]
    fn detects_windows_and_macos_game_processes() {
        assert!(process_list_contains_game(
            "\"ProjectZomboid64.exe\",\"1234\",\"Console\",\"1\",\"500,000 K\""
        ));
        assert!(process_list_contains_game(
            "/Applications/ProjectZomboid.app/Contents/MacOS/ProjectZomboid"
        ));
    }

    #[test]
    fn ignores_macos_archive_metadata_directory_only() {
        assert!(is_ignored_metadata_directory("__MACOSX"));
        assert!(is_ignored_metadata_directory("__macosx"));
        assert!(!is_ignored_metadata_directory("MACOSX"));
        assert!(!is_ignored_metadata_directory("MySave"));
    }

    #[test]
    fn retries_temporary_upload_failures_only() {
        assert!(retryable_upload_status(StatusCode::REQUEST_TIMEOUT));
        assert!(retryable_upload_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(retryable_upload_status(StatusCode::BAD_GATEWAY));
        assert!(!retryable_upload_status(StatusCode::BAD_REQUEST));
        assert!(!retryable_upload_status(StatusCode::UNAUTHORIZED));
    }

    #[test]
    fn calculates_each_stage_percentage() {
        assert_eq!(stage_percent(0, 1_000), 0);
        assert_eq!(stage_percent(250, 1_000), 25);
        assert_eq!(stage_percent(999, 1_000), 99);
        assert_eq!(stage_percent(1_000, 1_000), 100);
        assert_eq!(stage_percent(2_000, 1_000), 100);
    }

    fn test_directory(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "zomboid-sync-{label}-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn save_fingerprint_is_stable_and_changes_with_file_set() {
        let root = test_directory("fingerprint");
        fs::write(root.join("b.txt"), b"beta").unwrap();
        fs::write(root.join("a.txt"), b"alpha").unwrap();
        let first = scan_save_files(&root, &[]).unwrap();
        assert_eq!(
            first
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["a.txt", "b.txt"]
        );
        let first_fingerprint = save_fingerprint(&first).unwrap();
        let reused = scan_save_files(&root, &first).unwrap();
        assert_eq!(first, reused);
        fs::write(root.join("c.txt"), b"gamma").unwrap();
        let changed = scan_save_files(&root, &reused).unwrap();
        assert_ne!(first_fingerprint, save_fingerprint(&changed).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resume_state_is_bound_without_storing_plain_sync_key() {
        let sync_key = "secret-sync-key-that-is-long-enough";
        let archive = PreparedArchive {
            path: PathBuf::from("save.zip"),
            file_count: 1,
            bytes: 10,
            fingerprint: "f".repeat(64),
            zip_sha256: "a".repeat(64),
            cache_dir: PathBuf::from("cache"),
            reused: true,
        };
        let state = ResumeState {
            schema_version: 1,
            endpoint_hash: sha256_text(&normalize_endpoint("HTTPS://Example.COM/")),
            sync_key_hash: sha256_text(sync_key),
            save_mode: "Sandbox".into(),
            save_name: "World".into(),
            fingerprint: archive.fingerprint.clone(),
            zip_sha256: archive.zip_sha256.clone(),
            zip_bytes: archive.bytes,
            chunk_size: 8,
            chunk_count: 2,
            upload_id: "1".repeat(32),
        };
        assert!(resume_matches(
            &state,
            "https://example.com",
            sync_key,
            "Sandbox",
            "World",
            &archive,
            8,
            2
        ));
        assert!(!resume_matches(
            &state,
            "https://other.example",
            sync_key,
            "Sandbox",
            "World",
            &archive,
            8,
            2
        ));
        assert!(!resume_matches(
            &state,
            "https://example.com",
            "different-secret-key-that-is-long",
            "Sandbox",
            "World",
            &archive,
            8,
            2
        ));
        assert!(!serde_json::to_string(&state).unwrap().contains(sync_key));
    }

    #[test]
    fn resumed_progress_counts_non_contiguous_chunks_and_last_chunk_bytes() {
        let chunks = [1_u64, 2_u64].into_iter().collect();
        assert_eq!(completed_upload_bytes(&chunks, 20, 8, 3), 12);
        let first_only = [0_u64].into_iter().collect();
        assert_eq!(completed_upload_bytes(&first_only, 20, 8, 3), 8);
    }
}

fn unique_temp_path(prefix: &str) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    std::env::temp_dir().join(format!("{prefix}-{}-{now}.zip", std::process::id()))
}

fn unique_staging_path(parent: &Path, name: &str) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    parent.join(format!(
        ".{name}.zomboid-download-{}-{now}",
        std::process::id()
    ))
}

fn sha256_text(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("打开文件计算摘要失败: {error}"))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 256 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("读取文件计算摘要失败: {error}"))?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn modified_ns(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "缓存路径无父目录".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("创建缓存目录失败: {error}"))?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let content =
        serde_json::to_vec_pretty(value).map_err(|error| format!("序列化缓存失败: {error}"))?;
    fs::write(&tmp, content).map_err(|error| format!("写入缓存失败: {error}"))?;
    replace_file_with_rollback(&tmp, path, "提交缓存")
}

fn replace_file_with_rollback(source: &Path, target: &Path, label: &str) -> Result<(), String> {
    let backup = target.with_extension(format!(
        "{}.old",
        target.extension().unwrap_or_default().to_string_lossy()
    ));
    let had_target = target.exists();
    if backup.exists() {
        fs::remove_file(&backup).map_err(|error| format!("清理旧缓存交换文件失败: {error}"))?;
    }
    if had_target {
        fs::rename(target, &backup).map_err(|error| format!("准备{label}失败: {error}"))?;
    }
    if let Err(error) = fs::rename(source, target) {
        if had_target {
            let _ = fs::rename(&backup, target);
        }
        return Err(format!("{label}失败: {error}"));
    }
    if had_target {
        fs::remove_file(&backup)
            .map_err(|error| format!("{label}成功，但清理旧缓存失败: {error}"))?;
    }
    Ok(())
}

#[cfg(test)]
fn scan_save_files(
    source: &Path,
    previous: &[CachedFileEntry],
) -> Result<Vec<CachedFileEntry>, String> {
    scan_save_files_with_progress(source, previous, |_, _| {})
}

fn scan_save_files_with_progress<F>(
    source: &Path,
    previous: &[CachedFileEntry],
    mut on_progress: F,
) -> Result<Vec<CachedFileEntry>, String>
where
    F: FnMut(usize, usize),
{
    let previous_by_path: std::collections::HashMap<&str, &CachedFileEntry> = previous
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let mut paths = Vec::new();
    for entry in WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !is_ignored_metadata_directory(&entry.file_name().to_string_lossy()))
    {
        let entry = entry.map_err(|error| format!("扫描存档失败: {error}"))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(source)
            .map_err(|error| format!("生成存档路径失败: {error}"))?
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = entry
            .metadata()
            .map_err(|error| format!("读取存档元数据失败: {error}"))?;
        paths.push((entry.into_path(), relative, metadata));
    }
    let total = paths.len();
    on_progress(0, total);
    let mut result = Vec::with_capacity(total);
    for (index, (path, relative, metadata)) in paths.into_iter().enumerate() {
        let bytes = metadata.len();
        let modified_ns = modified_ns(&metadata);
        let reusable = previous_by_path
            .get(relative.as_str())
            .filter(|old| old.bytes == bytes && old.modified_ns == modified_ns)
            .map(|old| old.sha256.clone());
        let sha256 = match reusable {
            Some(value) => value,
            None => sha256_file(&path)?,
        };
        result.push(CachedFileEntry {
            path: relative,
            bytes,
            modified_ns,
            sha256,
        });
        on_progress(index + 1, total);
    }
    result.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(result)
}

fn save_fingerprint(files: &[CachedFileEntry]) -> Result<String, String> {
    let serialized =
        serde_json::to_vec(files).map_err(|error| format!("生成存档指纹失败: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(serialized)))
}

fn cache_identity(save_mode: &str, save_name: &str) -> String {
    sha256_text(&format!("{save_mode}\0{save_name}"))
}

fn normalize_endpoint(endpoint: &str) -> String {
    let trimmed = endpoint.trim().trim_end_matches('/');
    reqwest::Url::parse(trimmed)
        .map(|url| url.to_string().trim_end_matches('/').to_string())
        .unwrap_or_else(|_| trimmed.to_string())
}

fn prepare_archive(
    app: &tauri::AppHandle,
    source: &Path,
    save_mode: &str,
    save_name: &str,
) -> Result<PreparedArchive, String> {
    let cache_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("获取应用数据目录失败: {error}"))?
        .join("save-archives")
        .join(cache_identity(save_mode, save_name));
    fs::create_dir_all(&cache_dir).map_err(|error| format!("创建存档缓存目录失败: {error}"))?;
    let metadata_path = cache_dir.join("cache.json");
    let archive_path = cache_dir.join("save.zip");
    let old: Option<CacheMetadata> = read_json(&metadata_path);
    emit_upload_progress(app, 0, "正在检查本地压缩缓存...");
    let mut last_scan_percent = None;
    let files = scan_save_files_with_progress(
        source,
        old.as_ref()
            .map(|value| value.files.as_slice())
            .unwrap_or(&[]),
        |completed, total| {
            let percent = stage_percent(completed as u64, total as u64);
            if last_scan_percent != Some(percent) {
                emit_upload_progress(
                    app,
                    percent,
                    format!("正在扫描存档：{completed}/{total} 个文件"),
                );
                last_scan_percent = Some(percent);
            }
        },
    )?;
    let fingerprint = save_fingerprint(&files)?;
    if let Some(metadata) = old.as_ref().filter(|value| {
        value.schema_version == 1
            && value.save_mode == save_mode
            && value.save_name == save_name
            && value.fingerprint == fingerprint
            && archive_path.is_file()
            && fs::metadata(&archive_path).map(|info| info.len()).ok() == Some(value.zip_bytes)
    }) {
        if sha256_file(&archive_path)? == metadata.zip_sha256 {
            emit_upload_progress(app, 100, "存档未变化，跳过压缩");
            return Ok(PreparedArchive {
                path: archive_path,
                file_count: files.len() as u64,
                bytes: metadata.zip_bytes,
                fingerprint,
                zip_sha256: metadata.zip_sha256.clone(),
                cache_dir,
                reused: true,
            });
        }
    }

    emit_upload_progress(app, 0, "缓存不可用，正在压缩存档...");
    let temporary_archive = cache_dir.join(format!("save.{}.tmp", std::process::id()));
    let mut last_percent = None;
    let file_count = zip_directory_with_progress(
        source,
        &temporary_archive,
        |completed_bytes, total_bytes, completed_files, total_files| {
            let percent = stage_percent(completed_bytes, total_bytes);
            if last_percent != Some(percent) {
                emit_upload_progress(
                    app,
                    percent,
                    format!("正在压缩存档：{completed_files}/{total_files} 个文件"),
                );
                last_percent = Some(percent);
            }
        },
    )?;
    let bytes = fs::metadata(&temporary_archive)
        .map_err(|error| format!("读取压缩包大小失败: {error}"))?
        .len();
    let zip_sha256 = sha256_file(&temporary_archive)?;
    replace_file_with_rollback(&temporary_archive, &archive_path, "提交压缩缓存")?;
    write_json_atomic(
        &metadata_path,
        &CacheMetadata {
            schema_version: 1,
            save_mode: save_mode.to_string(),
            save_name: save_name.to_string(),
            fingerprint: fingerprint.clone(),
            zip_sha256: zip_sha256.clone(),
            zip_bytes: bytes,
            files,
        },
    )?;
    emit_upload_progress(app, 100, format!("存档压缩完成：{bytes} 字节"));
    Ok(PreparedArchive {
        path: archive_path,
        file_count,
        bytes,
        fingerprint,
        zip_sha256,
        cache_dir,
        reused: false,
    })
}

fn resume_matches(
    state: &ResumeState,
    endpoint: &str,
    sync_key: &str,
    save_mode: &str,
    save_name: &str,
    archive: &PreparedArchive,
    chunk_size: u64,
    chunk_count: u64,
) -> bool {
    state.schema_version == 1
        && state.endpoint_hash == sha256_text(&normalize_endpoint(endpoint))
        && state.sync_key_hash == sha256_text(sync_key)
        && state.save_mode == save_mode
        && state.save_name == save_name
        && state.fingerprint == archive.fingerprint
        && state.zip_sha256 == archive.zip_sha256
        && state.zip_bytes == archive.bytes
        && state.chunk_size == chunk_size
        && state.chunk_count == chunk_count
}

fn completed_upload_bytes(
    completed_chunks: &std::collections::HashSet<u64>,
    total_bytes: u64,
    chunk_size: u64,
    chunk_count: u64,
) -> u64 {
    completed_chunks.iter().fold(0_u64, |total, index| {
        if *index >= chunk_count {
            return total;
        }
        let bytes = if *index == chunk_count - 1 {
            total_bytes.saturating_sub(index.saturating_mul(chunk_size))
        } else {
            chunk_size
        };
        total.saturating_add(bytes)
    })
}

fn query_upload_status(
    client: &Client,
    endpoint: &str,
    sync_key: &str,
    upload_id: &str,
) -> Result<Option<UploadStatus>, String> {
    let response = authorized(
        client,
        &api_url(endpoint, &format!("v1/uploads/{upload_id}")),
        sync_key,
    )
    .send()
    .map_err(|error| format!("查询上传进度失败: {}", request_error_details(&error)))?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let status = response.status();
    let body = response.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "查询上传进度失败（HTTP {}）: {body}",
            status.as_u16()
        ));
    }
    serde_json::from_str(&body)
        .map(Some)
        .map_err(|error| format!("上传进度格式错误: {error}"))
}

fn clean_component<'a>(value: &'a str, label: &str) -> Result<&'a str, String> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains(':')
    {
        return Err(format!("无效的 {label}"));
    }
    Ok(value)
}

fn zip_directory_with_progress<F>(
    source: &Path,
    destination: &Path,
    mut on_progress: F,
) -> Result<u64, String>
where
    F: FnMut(u64, u64, u64, u64),
{
    let mut files = Vec::new();
    let mut total_bytes = 0_u64;
    for entry in WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !is_ignored_metadata_directory(&entry.file_name().to_string_lossy()))
    {
        let entry = entry.map_err(|error| format!("读取存档失败: {error}"))?;
        if entry.path().is_file() {
            let bytes = entry
                .metadata()
                .map_err(|error| format!("读取存档文件大小失败: {error}"))?
                .len();
            total_bytes = total_bytes.saturating_add(bytes);
            files.push((entry.into_path(), bytes));
        }
    }

    let file = File::create(destination).map_err(|error| format!("创建压缩包失败: {error}"))?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut file_count = 0;
    let mut completed_bytes = 0_u64;
    let total_files = files.len() as u64;
    on_progress(0, total_bytes, 0, total_files);

    for (path, bytes) in files {
        let relative = path
            .strip_prefix(source)
            .map_err(|error| format!("生成存档路径失败: {error}"))?;
        let name = relative.to_string_lossy().replace('\\', "/");
        writer
            .start_file(name, options)
            .map_err(|error| format!("写入压缩包失败: {error}"))?;
        let mut input = File::open(&path).map_err(|error| format!("读取存档文件失败: {error}"))?;
        io::copy(&mut input, &mut writer).map_err(|error| format!("压缩存档失败: {error}"))?;
        file_count += 1;
        completed_bytes = completed_bytes.saturating_add(bytes);
        on_progress(completed_bytes, total_bytes, file_count, total_files);
    }

    writer
        .finish()
        .map_err(|error| format!("完成压缩失败: {error}"))?;
    Ok(file_count)
}

fn zip_directory(source: &Path, destination: &Path) -> Result<u64, String> {
    zip_directory_with_progress(source, destination, |_, _, _, _| {})
}

fn extract_zip_with_progress<F>(
    archive_path: &Path,
    destination: &Path,
    mut on_progress: F,
) -> Result<(), String>
where
    F: FnMut(usize, usize),
{
    let file = File::open(archive_path).map_err(|error| format!("打开下载文件失败: {error}"))?;
    let mut archive = ZipArchive::new(file).map_err(|error| format!("压缩包损坏: {error}"))?;
    let total_entries = archive.len();
    on_progress(0, total_entries);
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("读取压缩包失败: {error}"))?;
        let Some(relative) = entry.enclosed_name() else {
            return Err("压缩包包含非法路径".to_string());
        };
        let output = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&output).map_err(|error| format!("创建存档目录失败: {error}"))?;
            on_progress(index + 1, total_entries);
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("创建存档目录失败: {error}"))?;
        }
        let mut target = File::create(&output).map_err(|error| format!("写入存档失败: {error}"))?;
        io::copy(&mut entry, &mut target).map_err(|error| format!("解压存档失败: {error}"))?;
        on_progress(index + 1, total_entries);
    }
    Ok(())
}

fn api_url(endpoint: &str, path: &str) -> String {
    format!(
        "{}/{}",
        endpoint.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn authorized(
    client: &Client,
    endpoint: &str,
    sync_key: &str,
) -> reqwest::blocking::RequestBuilder {
    client
        .get(endpoint)
        .header("Authorization", format!("Bearer {sync_key}"))
}

#[tauri::command]
fn detect_save_root() -> Result<Option<String>, String> {
    let root = home_dir()?.join("Zomboid").join("Saves");
    Ok(is_directory(&root).then(|| root.to_string_lossy().to_string()))
}

#[tauri::command]
fn pick_directory() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("选择 Zomboid/Saves 目录")
        .pick_folder()
        .map(|path| path.to_string_lossy().to_string())
}

#[tauri::command]
fn list_saves(save_root: String) -> Result<Vec<SaveEntry>, String> {
    let root = PathBuf::from(&save_root);
    if !is_directory(&root) {
        return Err("Saves 目录不存在".to_string());
    }
    let mut result = Vec::new();
    let modes = fs::read_dir(&root).map_err(|error| format!("读取 Saves 目录失败: {error}"))?;
    for mode_entry in modes {
        let mode_entry = mode_entry.map_err(|error| format!("读取存档模式失败: {error}"))?;
        let mode_path = mode_entry.path();
        if !mode_path.is_dir() {
            continue;
        }
        let mode = mode_entry.file_name().to_string_lossy().to_string();
        if is_ignored_metadata_directory(&mode) {
            continue;
        }
        for save_entry in
            fs::read_dir(&mode_path).map_err(|error| format!("读取 {mode} 失败: {error}"))?
        {
            let save_entry = save_entry.map_err(|error| format!("读取存档记录失败: {error}"))?;
            let save_path = save_entry.path();
            if save_path.is_dir()
                && !is_ignored_metadata_directory(&save_entry.file_name().to_string_lossy())
            {
                result.push(SaveEntry {
                    mode: mode.clone(),
                    name: save_entry.file_name().to_string_lossy().to_string(),
                    path: save_path.to_string_lossy().to_string(),
                });
            }
        }
    }
    result.sort_by(|left, right| (&left.mode, &left.name).cmp(&(&right.mode, &right.name)));
    Ok(result)
}

#[tauri::command]
fn list_remote_saves(endpoint: String, sync_key: String) -> Result<RemoteSaveLibrary, String> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|error| format!("创建网络客户端失败: {error}"))?;
    let response = authorized(&client, &api_url(&endpoint, "v1/saves"), &sync_key)
        .send()
        .map_err(|error| format!("读取 VPS 存档信息失败: {}", request_error_details(&error)))?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "读取 VPS 存档信息失败（HTTP {}）: {body}",
            status.as_u16()
        ));
    }
    let list: RemoteSaveList =
        serde_json::from_str(&body).map_err(|error| format!("VPS 存档列表格式错误: {error}"))?;
    Ok(RemoteSaveLibrary {
        saves: list.saves,
        total_bytes: list.total_bytes,
    })
}

#[tauri::command]
fn delete_remote_save(
    endpoint: String,
    sync_key: String,
    save_mode: String,
    save_name: String,
) -> Result<String, String> {
    clean_component(&save_mode, "远程存档模式")?;
    clean_component(&save_name, "远程存档名称")?;
    let mut url = reqwest::Url::parse(&api_url(&endpoint, "v1/saves"))
        .map_err(|error| format!("VPS API 地址无效: {error}"))?;
    url.query_pairs_mut()
        .append_pair("saveMode", &save_mode)
        .append_pair("saveName", &save_name);
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|error| format!("创建网络客户端失败: {error}"))?;
    let response = client
        .delete(url)
        .header("Authorization", format!("Bearer {sync_key}"))
        .send()
        .map_err(|error| format!("删除 VPS 存档失败: {}", request_error_details(&error)))?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "删除 VPS 存档失败（HTTP {}）: {body}",
            status.as_u16()
        ));
    }
    Ok(format!("已删除 VPS 存档：{save_mode}/{save_name}"))
}

#[allow(dead_code)]
fn upload_save_legacy(
    save_path: String,
    save_mode: String,
    save_name: String,
    endpoint: String,
    sync_key: String,
    device_name: String,
) -> Result<String, String> {
    if game_is_running() {
        return Err("检测到 Project Zomboid 正在运行，请先退出游戏".to_string());
    }
    clean_component(&save_mode, "存档模式")?;
    clean_component(&save_name, "存档名称")?;
    let source = PathBuf::from(&save_path);
    if !is_directory(&source) {
        return Err("本地存档目录不存在".to_string());
    }

    let archive_path = unique_temp_path("zomboid-upload");
    let file_count = zip_directory(&source, &archive_path)?;
    let bytes = fs::metadata(&archive_path)
        .map_err(|error| format!("读取压缩包大小失败: {error}"))?
        .len();
    let client = Client::builder()
        .build()
        .map_err(|error| format!("创建网络客户端失败: {error}"))?;
    let file = File::open(&archive_path).map_err(|error| format!("打开压缩包失败: {error}"))?;
    let response = client
        .put(api_url(&endpoint, "v1/snapshot"))
        .header("Authorization", format!("Bearer {sync_key}"))
        .header("Content-Type", "application/zip")
        .header("Content-Length", bytes)
        .header("X-Save-Mode", &save_mode)
        .header("X-Save-Name", &save_name)
        .header("X-Device-Name", &device_name)
        .body(reqwest::blocking::Body::new(file))
        .send()
        .map_err(|error| format!("上传失败: {error}"))?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    let _ = fs::remove_file(archive_path);
    if !status.is_success() {
        return Err(format!("上传失败（HTTP {}）: {body}", status.as_u16()));
    }
    Ok(format!(
        "上传完成：{save_mode}/{save_name}，{file_count} 个文件，压缩后 {bytes} 字节"
    ))
}

fn upload_save_blocking(
    app: tauri::AppHandle,
    save_path: String,
    save_mode: String,
    save_name: String,
    endpoint: String,
    sync_key: String,
    device_name: String,
    overwrite_confirmed: bool,
) -> Result<String, String> {
    emit_upload_progress(&app, 0, "正在检查游戏状态...");
    if game_is_running() {
        return Err("检测到 Project Zomboid 正在运行，请先退出游戏".to_string());
    }
    emit_upload_progress(&app, 100, "游戏状态检查完成");
    clean_component(&save_mode, "存档模式")?;
    clean_component(&save_name, "存档名称")?;
    let source = PathBuf::from(&save_path);
    if !is_directory(&source) {
        return Err("本地存档目录不存在".to_string());
    }

    let archive = prepare_archive(&app, &source, &save_mode, &save_name)?;
    let bytes = archive.bytes;
    let file_count = archive.file_count;
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|error| format!("创建网络客户端失败: {error}"))?;
    const CHUNK_SIZE: u64 = 256 * 1024;
    let chunk_count = bytes.div_ceil(CHUNK_SIZE);
    let resume_path = archive.cache_dir.join("resume.json");
    let saved_resume: Option<ResumeState> = read_json(&resume_path);
    let mut active_resume = saved_resume.filter(|state| {
        resume_matches(
            state,
            &endpoint,
            &sync_key,
            &save_mode,
            &save_name,
            &archive,
            CHUNK_SIZE,
            chunk_count,
        )
    });
    let mut completed_chunks = std::collections::HashSet::new();
    if let Some(state) = active_resume.as_ref() {
        emit_upload_progress(&app, 0, "发现可续传任务，正在查询 VPS...");
        match query_upload_status(&client, &endpoint, &sync_key, &state.upload_id) {
            Ok(Some(status))
                if status.upload_id == state.upload_id
                    && status.total_bytes == bytes
                    && status.chunk_size == CHUNK_SIZE
                    && status.chunk_count == chunk_count
                    && status.save_mode == save_mode
                    && status.save_name == save_name
                    && status.zip_sha256 == archive.zip_sha256 =>
            {
                completed_chunks.extend(status.completed_chunks.into_iter());
            }
            Ok(_) => active_resume = None,
            Err(error) => {
                if error.contains("HTTP 404") {
                    active_resume = None;
                } else {
                    return Err(error);
                }
            }
        }
    }

    if active_resume.is_none() {
        emit_upload_progress(&app, 0, "正在创建上传会话...");
        let response = client
            .post(api_url(&endpoint, "v1/uploads"))
            .header("Authorization", format!("Bearer {sync_key}"))
            .json(&serde_json::json!({
                "totalBytes": bytes,
                "chunkSize": CHUNK_SIZE,
                "chunkCount": chunk_count,
                "saveMode": save_mode,
                "saveName": save_name,
                "deviceName": device_name,
                "gameVersion": "Unknown",
                "overwriteConfirmed": overwrite_confirmed,
                "zipSha256": archive.zip_sha256
            }))
            .send()
            .map_err(|error| format!("创建上传会话失败: {error}"))?;
        let status = response.status();
        let body = response.text().unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "创建上传会话失败（HTTP {}）: {body}",
                status.as_u16()
            ));
        }
        let session: UploadSession =
            serde_json::from_str(&body).map_err(|error| format!("上传会话格式错误: {error}"))?;
        active_resume = Some(ResumeState {
            schema_version: 1,
            endpoint_hash: sha256_text(&normalize_endpoint(&endpoint)),
            sync_key_hash: sha256_text(&sync_key),
            save_mode: save_mode.clone(),
            save_name: save_name.clone(),
            fingerprint: archive.fingerprint.clone(),
            zip_sha256: archive.zip_sha256.clone(),
            zip_bytes: bytes,
            chunk_size: CHUNK_SIZE,
            chunk_count,
            upload_id: session.upload_id,
        });
        write_json_atomic(&resume_path, active_resume.as_ref().unwrap())?;
    }
    let upload_id = active_resume.as_ref().unwrap().upload_id.clone();
    let initially_completed =
        completed_upload_bytes(&completed_chunks, bytes, CHUNK_SIZE, chunk_count);
    emit_upload_progress(
        &app,
        stage_percent(initially_completed, bytes),
        format!(
            "正在上传：已完成 {}/{} 块",
            completed_chunks.len(),
            chunk_count
        ),
    );
    let mut file = File::open(&archive.path).map_err(|error| format!("打开压缩包失败: {error}"))?;
    let mut buffer = vec![0_u8; CHUNK_SIZE as usize];
    for index in 0..chunk_count {
        if completed_chunks.contains(&index) {
            continue;
        }
        let expected = if index == chunk_count - 1 {
            (bytes - index * CHUNK_SIZE) as usize
        } else {
            CHUNK_SIZE as usize
        };
        file.seek(SeekFrom::Start(index * CHUNK_SIZE))
            .map_err(|error| format!("定位上传分块失败: {error}"))?;
        file.read_exact(&mut buffer[..expected])
            .map_err(|error| format!("读取上传分块失败: {error}"))?;
        let chunk = buffer[..expected].to_vec();
        let digest = format!("{:x}", Sha256::digest(&chunk));
        const MAX_ATTEMPTS: u8 = 4;
        let mut uploaded = false;
        let mut last_error = String::new();
        let mut attempts_made = 0;
        for attempt in 1..=MAX_ATTEMPTS {
            attempts_made = attempt;
            if attempt > 1 {
                let retry = attempt - 1;
                emit_upload_progress(
                    &app,
                    stage_percent(
                        completed_upload_bytes(&completed_chunks, bytes, CHUNK_SIZE, chunk_count),
                        bytes,
                    ),
                    format!("第 {} 块上传中断，正在重试 {retry}/3...", index + 1),
                );
                thread::sleep(Duration::from_secs(1 << (retry - 1)));
            }
            match client
                .put(api_url(
                    &endpoint,
                    &format!("v1/uploads/{upload_id}/chunks/{index}"),
                ))
                .header("Authorization", format!("Bearer {sync_key}"))
                .header("Content-Type", "application/octet-stream")
                .header("Content-Length", expected)
                .header("X-Chunk-Sha256", &digest)
                .body(chunk.clone())
                .send()
            {
                Ok(response) if response.status().is_success() => {
                    uploaded = true;
                    break;
                }
                Ok(response) => {
                    let response_status = response.status();
                    let response_body = response.text().unwrap_or_default();
                    last_error = format!("HTTP {}: {response_body}", response_status.as_u16());
                    if !retryable_upload_status(response_status) {
                        break;
                    }
                }
                Err(error) => last_error = request_error_details(&error),
            }
        }
        if !uploaded {
            let retry_message = if attempts_made == MAX_ATTEMPTS {
                "，重试 3 次后仍未成功"
            } else {
                ""
            };
            return Err(format!(
                "上传第 {} 块失败{retry_message}: {last_error}",
                index + 1
            ));
        }
        completed_chunks.insert(index);
        let uploaded_bytes =
            completed_upload_bytes(&completed_chunks, bytes, CHUNK_SIZE, chunk_count);
        let percent = stage_percent(uploaded_bytes, bytes);
        emit_upload_progress(
            &app,
            percent,
            format!(
                "正在上传：已完成 {}/{} 块",
                completed_chunks.len(),
                chunk_count
            ),
        );
    }
    emit_upload_progress(&app, 0, "服务器正在保存存档...");
    let response = client
        .post(api_url(
            &endpoint,
            &format!("v1/uploads/{upload_id}/complete"),
        ))
        .header("Authorization", format!("Bearer {sync_key}"))
        .send()
        .map_err(|error| format!("完成上传失败: {error}"))?;
    let response_status = response.status();
    let response_body = response.text().unwrap_or_default();
    if !response_status.is_success() {
        if response_status == StatusCode::CONFLICT
            && response_body.contains("remote_snapshot_changed")
        {
            let _ = fs::remove_file(&resume_path);
            return Err("VPS 同名存档已被其他设备更新，请刷新列表并重新确认覆盖".to_string());
        }
        if response_status == StatusCode::CONFLICT
            && response_body.contains("assembled_verification_failed")
        {
            let _ = fs::remove_file(&resume_path);
            return Err("服务器校验上传分片失败，下次上传将创建新会话".to_string());
        }
        return Err(format!(
            "完成上传失败（HTTP {}）: {response_body}",
            response_status.as_u16()
        ));
    }
    let _ = fs::remove_file(&resume_path);
    emit_upload_progress(&app, 100, "服务器保存完成");
    Ok(format!(
        "上传完成：{save_mode}/{save_name}，{file_count} 个文件，压缩后 {bytes} 字节，共 {chunk_count} 块{}",
        if archive.reused { "，已复用本地缓存" } else { "" }
    ))
}

#[tauri::command]
async fn upload_save(
    app: tauri::AppHandle,
    save_path: String,
    save_mode: String,
    save_name: String,
    endpoint: String,
    sync_key: String,
    device_name: String,
    overwrite_confirmed: bool,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        upload_save_blocking(
            app,
            save_path,
            save_mode,
            save_name,
            endpoint,
            sync_key,
            device_name,
            overwrite_confirmed,
        )
    })
    .await
    .map_err(|error| format!("上传任务异常结束: {error}"))?
}

fn download_save_blocking(
    app: tauri::AppHandle,
    save_root: String,
    endpoint: String,
    sync_key: String,
    save_mode: String,
    save_name: String,
    overwrite_confirmed: bool,
) -> Result<String, String> {
    emit_download_progress(&app, 0, "正在检查游戏状态...");
    if game_is_running() {
        return Err("检测到 Project Zomboid 正在运行，请先退出游戏".to_string());
    }
    emit_download_progress(&app, 100, "游戏状态检查完成");
    let root = PathBuf::from(&save_root);
    if !is_directory(&root) {
        return Err("Saves 目录不存在".to_string());
    }
    let mode = clean_component(&save_mode, "远程存档模式")?.to_string();
    let name = clean_component(&save_name, "远程存档名称")?.to_string();
    let mode_directory = root.join(&mode);
    let target = mode_directory.join(&name);
    let had_existing = target.exists();
    if had_existing && !overwrite_confirmed {
        return Err("本地已存在同名存档，需要确认覆盖后才能下载".to_string());
    }
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(60 * 60))
        .build()
        .map_err(|error| format!("创建网络客户端失败: {error}"))?;
    let mut url = reqwest::Url::parse(&api_url(&endpoint, "v1/snapshot"))
        .map_err(|error| format!("VPS API 地址无效: {error}"))?;
    url.query_pairs_mut()
        .append_pair("saveMode", &mode)
        .append_pair("saveName", &name);
    emit_download_progress(&app, 0, "正在连接 VPS...");
    let mut response = authorized(&client, url.as_str(), &sync_key)
        .send()
        .map_err(|error| format!("下载失败: {}", request_error_details(&error)))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(format!("下载失败（HTTP {}）: {body}", status.as_u16()));
    }
    let total_bytes = response.content_length().unwrap_or(0);
    let archive_path = unique_temp_path("zomboid-download");
    let mut archive =
        File::create(&archive_path).map_err(|error| format!("创建下载临时文件失败: {error}"))?;
    let mut buffer = vec![0_u8; 256 * 1024];
    let mut downloaded_bytes = 0_u64;
    emit_download_progress(&app, 0, format!("正在下载：0/{total_bytes} 字节"));
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|error| format!("下载数据失败: {error}"))?;
        if read == 0 {
            break;
        }
        archive
            .write_all(&buffer[..read])
            .map_err(|error| format!("保存下载文件失败: {error}"))?;
        downloaded_bytes = downloaded_bytes.saturating_add(read as u64);
        let percent = if total_bytes == 0 {
            0
        } else {
            stage_percent(downloaded_bytes, total_bytes)
        };
        emit_download_progress(
            &app,
            percent,
            if total_bytes == 0 {
                format!("正在下载：{downloaded_bytes} 字节")
            } else {
                format!("正在下载：{downloaded_bytes}/{total_bytes} 字节")
            },
        );
    }
    archive
        .flush()
        .map_err(|error| format!("保存下载文件失败: {error}"))?;
    emit_download_progress(&app, 100, format!("下载完成：{downloaded_bytes} 字节"));

    fs::create_dir_all(&mode_directory)
        .map_err(|error| format!("创建存档模式目录失败: {error}"))?;
    let staging = unique_staging_path(&mode_directory, &name);
    let mut last_extract_percent = None;
    let result = extract_zip_with_progress(&archive_path, &staging, |completed, total| {
        let percent = stage_percent(completed as u64, total as u64);
        if last_extract_percent != Some(percent) {
            emit_download_progress(
                &app,
                percent,
                format!("正在解压存档：{completed}/{total} 项"),
            );
            last_extract_percent = Some(percent);
        }
    });
    let _ = fs::remove_file(&archive_path);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    emit_download_progress(&app, 0, "正在覆盖本地存档...");
    let replaced = unique_staging_path(&mode_directory, &format!("{name}-replaced"));
    if had_existing {
        fs::rename(&target, &replaced).map_err(|error| {
            let _ = fs::remove_dir_all(&staging);
            format!("准备覆盖本地同名存档失败: {error}")
        })?;
    }
    if let Err(error) = fs::rename(&staging, &target) {
        let _ = fs::remove_dir_all(&staging);
        if had_existing {
            let _ = fs::rename(&replaced, &target);
        }
        return Err(format!("写入下载存档失败: {error}"));
    }
    if had_existing {
        fs::remove_dir_all(&replaced)
            .map_err(|error| format!("新存档已覆盖完成，但清理旧存档临时目录失败: {error}"))?;
    }
    emit_download_progress(&app, 100, "本地存档覆盖完成");
    emit_download_progress(&app, 100, "存档解压完成");
    Ok(format!("下载完成：{mode}/{name}"))
}

#[tauri::command]
async fn download_save(
    app: tauri::AppHandle,
    save_root: String,
    endpoint: String,
    sync_key: String,
    save_mode: String,
    save_name: String,
    overwrite_confirmed: bool,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        download_save_blocking(
            app,
            save_root,
            endpoint,
            sync_key,
            save_mode,
            save_name,
            overwrite_confirmed,
        )
    })
    .await
    .map_err(|error| format!("下载任务异常结束: {error}"))?
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            detect_save_root,
            pick_directory,
            list_saves,
            list_remote_saves,
            delete_remote_save,
            upload_save,
            download_save
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
