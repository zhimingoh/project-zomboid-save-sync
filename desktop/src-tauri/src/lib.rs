use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::Emitter;
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
        is_ignored_metadata_directory, process_list_contains_game, retryable_upload_status,
        stage_percent,
    };
    use reqwest::StatusCode;

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
}

fn unique_temp_path(prefix: &str) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    std::env::temp_dir().join(format!("{prefix}-{}-{now}.zip", std::process::id()))
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

fn extract_zip(archive_path: &Path, destination: &Path) -> Result<(), String> {
    let file = File::open(archive_path).map_err(|error| format!("打开下载文件失败: {error}"))?;
    let mut archive = ZipArchive::new(file).map_err(|error| format!("压缩包损坏: {error}"))?;
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
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("创建存档目录失败: {error}"))?;
        }
        let mut target = File::create(&output).map_err(|error| format!("写入存档失败: {error}"))?;
        io::copy(&mut entry, &mut target).map_err(|error| format!("解压存档失败: {error}"))?;
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

    emit_upload_progress(&app, 0, "正在扫描并压缩存档...");
    let archive_path = unique_temp_path("zomboid-upload");
    let mut last_compression_percent = None;
    let file_count = zip_directory_with_progress(
        &source,
        &archive_path,
        |completed_bytes, total_bytes, completed_files, total_files| {
            let percent = stage_percent(completed_bytes, total_bytes);
            if last_compression_percent != Some(percent) {
                emit_upload_progress(
                    &app,
                    percent,
                    format!("正在压缩存档：{completed_files}/{total_files} 个文件"),
                );
                last_compression_percent = Some(percent);
            }
        },
    )?;
    let bytes = fs::metadata(&archive_path)
        .map_err(|error| format!("读取压缩包大小失败: {error}"))?
        .len();
    emit_upload_progress(&app, 100, format!("存档压缩完成：{bytes} 字节"));
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|error| format!("创建网络客户端失败: {error}"))?;
    const CHUNK_SIZE: u64 = 256 * 1024;
    let chunk_count = bytes.div_ceil(CHUNK_SIZE);
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
            "overwriteConfirmed": overwrite_confirmed
        }))
        .send()
        .map_err(|error| format!("创建上传会话失败: {error}"))?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    if !status.is_success() {
        let _ = fs::remove_file(&archive_path);
        return Err(format!(
            "创建上传会话失败（HTTP {}）: {body}",
            status.as_u16()
        ));
    }
    let session: UploadSession =
        serde_json::from_str(&body).map_err(|error| format!("上传会话格式错误: {error}"))?;
    emit_upload_progress(&app, 0, format!("正在上传：0/{chunk_count} 块"));
    let mut file = File::open(&archive_path).map_err(|error| format!("打开压缩包失败: {error}"))?;
    let mut buffer = vec![0_u8; CHUNK_SIZE as usize];
    for index in 0..chunk_count {
        let expected = if index == chunk_count - 1 {
            (bytes - index * CHUNK_SIZE) as usize
        } else {
            CHUNK_SIZE as usize
        };
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
                    stage_percent((index * CHUNK_SIZE).min(bytes), bytes),
                    format!("第 {} 块上传中断，正在重试 {retry}/3...", index + 1),
                );
                thread::sleep(Duration::from_secs(1 << (retry - 1)));
            }
            match client
                .put(api_url(
                    &endpoint,
                    &format!("v1/uploads/{}/chunks/{index}", session.upload_id),
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
            let _ = fs::remove_file(&archive_path);
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
        let uploaded_bytes = ((index + 1) * CHUNK_SIZE).min(bytes);
        let percent = stage_percent(uploaded_bytes, bytes);
        emit_upload_progress(
            &app,
            percent,
            format!("正在上传：{}/{} 块", index + 1, chunk_count),
        );
    }
    emit_upload_progress(&app, 0, "服务器正在保存存档...");
    let response = client
        .post(api_url(
            &endpoint,
            &format!("v1/uploads/{}/complete", session.upload_id),
        ))
        .header("Authorization", format!("Bearer {sync_key}"))
        .send()
        .map_err(|error| format!("完成上传失败: {error}"))?;
    let response_status = response.status();
    let response_body = response.text().unwrap_or_default();
    let _ = fs::remove_file(archive_path);
    if !response_status.is_success() {
        return Err(format!(
            "完成上传失败（HTTP {}）: {response_body}",
            response_status.as_u16()
        ));
    }
    emit_upload_progress(&app, 100, "服务器保存完成");
    Ok(format!("上传完成：{save_mode}/{save_name}，{file_count} 个文件，压缩后 {bytes} 字节，共 {chunk_count} 块"))
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

#[tauri::command]
fn download_save(
    save_root: String,
    endpoint: String,
    sync_key: String,
    save_mode: String,
    save_name: String,
) -> Result<String, String> {
    if game_is_running() {
        return Err("检测到 Project Zomboid 正在运行，请先退出游戏".to_string());
    }
    let root = PathBuf::from(&save_root);
    if !is_directory(&root) {
        return Err("Saves 目录不存在".to_string());
    }
    let mode = clean_component(&save_mode, "远程存档模式")?.to_string();
    let name = clean_component(&save_name, "远程存档名称")?.to_string();
    let client = Client::builder()
        .build()
        .map_err(|error| format!("创建网络客户端失败: {error}"))?;
    let mut url = reqwest::Url::parse(&api_url(&endpoint, "v1/snapshot"))
        .map_err(|error| format!("VPS API 地址无效: {error}"))?;
    url.query_pairs_mut()
        .append_pair("saveMode", &mode)
        .append_pair("saveName", &name);
    let mut response = authorized(&client, url.as_str(), &sync_key)
        .send()
        .map_err(|error| format!("下载失败: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(format!("下载失败（HTTP {}）: {body}", status.as_u16()));
    }
    let archive_path = unique_temp_path("zomboid-download");
    let mut archive =
        File::create(&archive_path).map_err(|error| format!("创建下载临时文件失败: {error}"))?;
    io::copy(&mut response, &mut archive).map_err(|error| format!("保存下载文件失败: {error}"))?;

    let target = root.join(&mode).join(&name);
    let backup = root.join(&mode).join(format!("{name}.backup"));
    let had_existing = target.exists();
    if had_existing {
        if backup.exists() {
            fs::remove_dir_all(&backup).map_err(|error| format!("清理旧备份失败: {error}"))?;
        }
        fs::rename(&target, &backup).map_err(|error| format!("备份本地存档失败: {error}"))?;
    }
    let result = extract_zip(&archive_path, &target);
    let _ = fs::remove_file(&archive_path);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&target);
        if had_existing {
            let _ = fs::rename(&backup, &target);
        }
        return Err(error);
    }
    Ok(format!(
        "下载完成：{mode}/{name}，本地旧存档已备份为 {name}.backup"
    ))
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
