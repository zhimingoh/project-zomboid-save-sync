import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

const $ = (id) => document.getElementById(id);
const endpoint = $("endpoint");
const syncKey = $("sync-key");
const deviceName = $("device-name");
const saveRoot = $("save-root");
const saveSelect = $("save-select");
const status = $("status");
const statusFill = $("status-fill");
const statusText = $("status-text");
const uploadButton = $("upload");
const downloadButton = $("download");
const refreshRemoteButton = $("refresh-remote");
const remoteHeading = $("remote-heading");
const remoteList = $("remote-list");
const remotePrevButton = $("remote-prev");
const remoteNextButton = $("remote-next");
const remotePageLabel = $("remote-page");
let remoteSaves = [];
let remoteTotalBytes = 0;
let remotePage = 0;
let selectedRemoteKey = "";
let uploadInProgress = false;

const saved = JSON.parse(localStorage.getItem("zomboid-sync-settings") || "{}");
endpoint.value = saved.endpoint || "";
syncKey.value = saved.syncKey || "";
deviceName.value = saved.deviceName || `${navigator.platform} 设备`;

function saveSettings() {
  localStorage.setItem("zomboid-sync-settings", JSON.stringify({
    endpoint: endpoint.value.trim(),
    syncKey: syncKey.value.trim(),
    deviceName: deviceName.value.trim(),
  }));
}

function setStatus(message, type = "") {
  statusText.textContent = message;
  status.className = `status ${type}`;
  if (type !== "progress") statusFill.style.width = "0%";
}

function setProgress(percent, message) {
  const safePercent = Math.max(0, Math.min(100, Number(percent) || 0));
  status.className = "status progress";
  statusFill.style.width = `${safePercent}%`;
  statusText.textContent = `${message} ${safePercent}%`;
}

function setUploadInProgress(value) {
  uploadInProgress = value;
  setAllButtonsDisabled(value);
  if (!value) renderRemoteList();
}

function makeKey() {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function selectedSave() {
  return saveSelect.value ? JSON.parse(saveSelect.value) : null;
}

function formatBytes(bytes) {
  const value = Number(bytes);
  if (!Number.isFinite(value) || value < 0) return "大小未知";
  const units = ["B", "KiB", "MiB", "GiB"];
  let amount = value;
  let unit = 0;
  while (amount >= 1024 && unit < units.length - 1) {
    amount /= 1024;
    unit += 1;
  }
  return `${amount.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}

function selectedRemoteSave() {
  return remoteSaves.find((save) => remoteKey(save) === selectedRemoteKey) || null;
}

function remoteKey(save) {
  return `${save.saveMode}\u0000${save.saveName}`;
}

async function confirmAction(title, message) {
  return invoke("confirm_action", { title, message });
}

function setAllButtonsDisabled(value) {
  for (const button of document.querySelectorAll("button")) button.disabled = value;
}

function renderRemoteList() {
  const pageSize = 5;
  const pageCount = Math.ceil(remoteSaves.length / pageSize);
  remotePage = pageCount ? Math.min(remotePage, pageCount - 1) : 0;
  remoteHeading.textContent = `VPS 现有存档（共 ${remoteSaves.length} 个，总容量 ${formatBytes(remoteTotalBytes)}）`;
  remoteList.replaceChildren();
  const pageItems = remoteSaves.slice(remotePage * pageSize, (remotePage + 1) * pageSize);
  if (!pageItems.length) {
    const empty = document.createElement("div");
    empty.className = "remote-empty";
    empty.textContent = "VPS 上没有存档";
    remoteList.append(empty);
  } else {
    for (const save of pageItems) {
      const item = document.createElement("div");
      item.className = `remote-item${remoteKey(save) === selectedRemoteKey ? " selected" : ""}`;
      item.addEventListener("click", () => {
        selectedRemoteKey = remoteKey(save);
        renderRemoteList();
      });
      const details = document.createElement("div");
      details.className = "remote-details";
      const name = document.createElement("div");
      name.className = "remote-name";
      const updatedAt = save.updatedAt ? new Date(save.updatedAt).toLocaleString() : "时间未知";
      name.textContent = `${save.saveMode} / ${save.saveName}（上传时间：${updatedAt}）`;
      const meta = document.createElement("div");
      meta.className = "remote-meta";
      meta.textContent = `${formatBytes(save.bytes)} · ${save.deviceName || "未知设备"}`;
      details.append(name, meta);
      const deleteButton = document.createElement("button");
      deleteButton.type = "button";
      deleteButton.className = "danger";
      deleteButton.textContent = "删除";
      deleteButton.disabled = uploadInProgress;
      deleteButton.addEventListener("click", async (event) => {
        event.stopPropagation();
        await deleteRemoteSave(save);
      });
      item.append(details, deleteButton);
      remoteList.append(item);
    }
  }
  remotePageLabel.textContent = pageCount ? `第 ${remotePage + 1}/${pageCount} 页` : "第 0/0 页";
  remotePrevButton.disabled = uploadInProgress || remotePage === 0;
  remoteNextButton.disabled = uploadInProgress || remotePage >= pageCount - 1;
}

function applyRemoteLibrary(library) {
  remoteSaves = library.saves;
  remoteTotalBytes = library.totalBytes;
  if (!remoteSaves.some((save) => remoteKey(save) === selectedRemoteKey)) {
    selectedRemoteKey = remoteSaves.length ? remoteKey(remoteSaves[0]) : "";
  }
  renderRemoteList();
}

async function refreshRemote({ showStatus = true } = {}) {
  saveSettings();
  if (!endpoint.value.trim()) throw new Error("请填写 VPS API 地址");
  if (syncKey.value.trim().length < 24) throw new Error("同步密钥至少需要 24 个字符");
  const library = await invoke("list_remote_saves", {
    endpoint: endpoint.value.trim(),
    syncKey: syncKey.value.trim(),
  });
  applyRemoteLibrary(library);
  if (showStatus) setStatus(`已刷新 VPS 存档列表，共 ${library.saves.length} 个`, "success");
  return library.saves;
}

async function deleteRemoteSave(save) {
  const confirmed = await confirmAction(
    "确认删除 VPS 存档",
    `确定要永久删除 VPS 存档吗？\n\n${save.saveMode} / ${save.saveName}\n\n` +
    "该存档的当前版本和上一个回滚版本都会被删除，此操作无法撤销。",
  );
  if (!confirmed) return;
  setAllButtonsDisabled(true);
  try {
    const result = await invoke("delete_remote_save", {
      endpoint: endpoint.value.trim(),
      syncKey: syncKey.value.trim(),
      saveMode: save.saveMode,
      saveName: save.saveName,
    });
    selectedRemoteKey = "";
    await refreshRemote({ showStatus: false });
    setStatus(result, "success");
  } catch (error) {
    setStatus(String(error), "error");
  } finally {
    setAllButtonsDisabled(false);
    renderRemoteList();
  }
}

async function refreshSaves() {
  if (!saveRoot.value) return;
  try {
    const saves = await invoke("list_saves", { saveRoot: saveRoot.value });
    saveSelect.replaceChildren();
    for (const save of saves) {
      const option = document.createElement("option");
      option.value = JSON.stringify(save);
      option.textContent = `${save.mode} / ${save.name}`;
      saveSelect.append(option);
    }
    if (!saves.length) {
      saveSelect.append(new Option("没有找到存档记录", ""));
      setStatus("Saves 目录下没有找到模式/存档两层目录", "error");
    } else {
      setStatus(`找到 ${saves.length} 个存档记录`, "success");
    }
  } catch (error) {
    setStatus(String(error), "error");
  }
}

async function detectRoot() {
  try {
    const detected = await invoke("detect_save_root");
    if (!detected) throw new Error("没有找到默认 Zomboid/Saves 目录");
    saveRoot.value = detected;
    await refreshSaves();
  } catch (error) {
    setStatus(String(error), "error");
  }
}

async function pickRoot() {
  try {
    const picked = await invoke("pick_directory");
    if (picked) {
      saveRoot.value = picked;
      await refreshSaves();
    }
  } catch (error) {
    setStatus(String(error), "error");
  }
}

function validateConnection(requireSave = true) {
  saveSettings();
  if (!endpoint.value.trim()) throw new Error("请填写 VPS API 地址");
  if (syncKey.value.trim().length < 24) throw new Error("同步密钥至少需要 24 个字符");
  if (!requireSave) return null;
  const save = selectedSave();
  if (!save) throw new Error("请先选择一个存档记录");
  return save;
}

async function runAction(button, action, requireSave = true) {
  button.disabled = true;
  uploadButton.disabled = true;
  downloadButton.disabled = true;
  try {
    const save = validateConnection(requireSave);
    setProgress(0, "正在检查游戏，请不要启动游戏...");
    const result = await action(save);
    setStatus(result || "操作完成", "success");
    await refreshSaves();
    try {
      await refreshRemote({ showStatus: false });
    } catch (error) {
      console.error("Failed to refresh remote manifest", error);
    }
  } catch (error) {
    setStatus(String(error), "error");
  } finally {
    button.disabled = uploadInProgress;
    uploadButton.disabled = uploadInProgress;
    downloadButton.disabled = uploadInProgress;
  }
}

$("generate-key").addEventListener("click", () => {
  syncKey.value = makeKey();
  saveSettings();
  applyRemoteLibrary({ saves: [], totalBytes: 0 });
  setStatus("已生成同步密钥，请在另一台电脑使用同一个密钥", "success");
});
$("detect-root").addEventListener("click", detectRoot);
$("pick-root").addEventListener("click", pickRoot);
refreshRemoteButton.addEventListener("click", async () => {
  refreshRemoteButton.disabled = true;
  try {
    await refreshRemote();
  } catch (error) {
    setStatus(String(error), "error");
  } finally {
    refreshRemoteButton.disabled = false;
  }
});
uploadButton.addEventListener("click", async () => {
  let save;
  let saves;
  try {
    save = validateConnection(true);
    saves = await refreshRemote({ showStatus: false });
  } catch (error) {
    setStatus(`上传前无法确认 VPS 存档状态：${error}`, "error");
    return;
  }

  const matchingRemote = saves.find((remote) => remote.saveMode === save.mode && remote.saveName === save.name);
  let overwriteConfirmed = false;
  if (matchingRemote) {
    overwriteConfirmed = await confirmAction(
      "确认覆盖 VPS 存档",
      `VPS 上已存在同一个存档：${matchingRemote.saveMode} / ${matchingRemote.saveName}\n\n` +
      `继续上传将覆盖这个存档，并把它的当前版本移入上一个回滚版本。\n\n确定要覆盖吗？`,
    );
    if (!overwriteConfirmed) {
      setStatus("已取消上传，VPS 现有存档未被覆盖");
      return;
    }
  }

  setUploadInProgress(true);
  try {
    await runAction(uploadButton, (selected) => invoke("upload_save", {
      savePath: selected.path,
      saveMode: selected.mode,
      saveName: selected.name,
      endpoint: endpoint.value.trim(),
      syncKey: syncKey.value.trim(),
      deviceName: deviceName.value.trim(),
      overwriteConfirmed,
    }));
  } finally {
    setUploadInProgress(false);
  }
});
downloadButton.addEventListener("click", async () => {
  const remote = selectedRemoteSave();
  if (!remote) {
    setStatus("请先刷新并选择一个 VPS 存档", "error");
    return;
  }
  let overwriteConfirmed = false;
  try {
    const localSaves = await invoke("list_saves", { saveRoot: saveRoot.value });
    const matchingLocal = localSaves.find(
      (save) => save.mode === remote.saveMode && save.name === remote.saveName,
    );
    if (matchingLocal) {
      overwriteConfirmed = await confirmAction(
        "确认覆盖本地存档",
        `本地已存在同一个存档：${remote.saveMode} / ${remote.saveName}\n\n` +
        "继续下载会永久删除本地版本，并使用 VPS 版本直接覆盖，不会创建备份。\n\n确定要覆盖吗？",
      );
      if (!overwriteConfirmed) {
        setStatus("已取消下载，本地存档未被修改");
        return;
      }
    }
  } catch (error) {
    setStatus(`下载前无法检查本地存档：${error}`, "error");
    return;
  }
  await runAction(downloadButton, () => invoke("download_save", {
    saveRoot: saveRoot.value,
    endpoint: endpoint.value.trim(),
    syncKey: syncKey.value.trim(),
    saveMode: remote.saveMode,
    saveName: remote.saveName,
    overwriteConfirmed,
  }), false);
});

remotePrevButton.addEventListener("click", () => {
  if (remotePage > 0) {
    remotePage -= 1;
    renderRemoteList();
  }
});
remoteNextButton.addEventListener("click", () => {
  if ((remotePage + 1) * 5 < remoteSaves.length) {
    remotePage += 1;
    renderRemoteList();
  }
});

getCurrentWindow().onCloseRequested(async (event) => {
  if (!uploadInProgress) return;
  event.preventDefault();
  const confirmed = await confirmAction(
    "确认关闭程序",
    "存档仍在上传中。现在关闭程序会中断上传，本次上传可能无法完成。\n\n确定要关闭程序吗？",
  );
  if (confirmed) await getCurrentWindow().destroy();
}).catch((error) => {
  console.error("Failed to initialize close confirmation", error);
});

detectRoot();

listen("upload-progress", ({ payload }) => {
  setProgress(payload.percent, payload.message);
}).catch((error) => {
  console.error("Failed to initialize upload progress listener", error);
});

listen("download-progress", ({ payload }) => {
  setProgress(payload.percent, payload.message);
}).catch((error) => {
  console.error("Failed to initialize download progress listener", error);
});
