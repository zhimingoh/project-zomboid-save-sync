import crypto from "node:crypto";
import { createServer } from "node:http";
import { createReadStream, createWriteStream, existsSync, statfsSync } from "node:fs";
import { mkdir, open, readFile, readdir, rename, rm, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const port = Number(process.env.PORT || 8787);
const dataDir = path.resolve(process.env.SYNC_DATA_DIR || "./data");
const maxSnapshotBytes = Number(process.env.SYNC_MAX_BYTES || 10 * 1024 * 1024 * 1024);
const minFreeBytes = Number(process.env.SYNC_MIN_FREE_BYTES || 1024 * 1024 * 1024);
const maxChunkBytes = Number(process.env.SYNC_MAX_CHUNK_BYTES || 16 * 1024 * 1024);

function corsHeaders() {
  return {
    "access-control-allow-origin": "*",
    "access-control-allow-headers": "authorization, content-type, x-save-mode, x-save-name, x-device-name, x-game-version, x-chunk-sha256, x-overwrite-confirmed",
    "access-control-allow-methods": "GET, POST, PUT, DELETE, OPTIONS",
  };
}

function json(res, status, body) {
  const payload = JSON.stringify(body);
  res.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
    "cache-control": "no-store",
    ...corsHeaders(),
  });
  res.end(payload);
}

function syncKeyFromRequest(req) {
  const value = req.headers.authorization || "";
  if (!value.startsWith("Bearer ")) return null;
  const key = value.slice("Bearer ".length).trim();
  return key.length >= 24 ? key : null;
}

function spaceDir(syncKey) {
  const id = crypto.createHash("sha256").update(syncKey).digest("hex");
  return path.join(dataDir, id);
}

async function ensureSpace(syncKey) {
  const dir = spaceDir(syncKey);
  await mkdir(path.join(dir, "saves"), { recursive: true });
  return dir;
}

function freeBytes() {
  const stats = statfsSync(dataDir);
  return Number(stats.bavail) * Number(stats.bsize);
}

function safeHeader(value, fallback) {
  if (typeof value !== "string") return fallback;
  const trimmed = value.trim();
  return trimmed.length > 200 ? trimmed.slice(0, 200) : trimmed || fallback;
}

function saveId(saveMode, saveName) {
  return crypto.createHash("sha256").update(`${saveMode}\0${saveName}`).digest("hex");
}

function targetSaveDir(space, saveMode, saveName) {
  return path.join(space, "saves", saveId(saveMode, saveName));
}

async function readManifest(dir) {
  try {
    return JSON.parse(await readFile(path.join(dir, "manifest.json"), "utf8"));
  } catch (error) {
    if (error.code === "ENOENT") return { exists: false };
    throw error;
  }
}

async function writeManifest(dir, manifest) {
  await mkdir(dir, { recursive: true });
  const tmp = path.join(dir, `manifest.${process.pid}.tmp`);
  await writeFile(tmp, JSON.stringify(manifest, null, 2), "utf8");
  await rename(tmp, path.join(dir, "manifest.json"));
}

async function migrateLegacySave(space) {
  const legacyManifest = await readManifest(space);
  if (!legacyManifest.exists || !existsSync(path.join(space, "current.zip"))) return;
  const saveMode = safeHeader(legacyManifest.saveMode, "Sandbox");
  const saveName = safeHeader(legacyManifest.saveName, "MigratedSave");
  const target = targetSaveDir(space, saveMode, saveName);
  await mkdir(target, { recursive: true });
  if (!existsSync(path.join(target, "current.zip"))) {
    await rename(path.join(space, "current.zip"), path.join(target, "current.zip"));
    if (existsSync(path.join(space, "previous.zip"))) {
      await rename(path.join(space, "previous.zip"), path.join(target, "previous.zip"));
    }
    await writeManifest(target, { ...legacyManifest, saveMode, saveName, exists: true });
  }
  await rm(path.join(space, "manifest.json"), { force: true });
}

async function listSaveManifests(space) {
  await migrateLegacySave(space);
  const savesRoot = path.join(space, "saves");
  const entries = await readdir(savesRoot, { withFileTypes: true });
  const saves = [];
  for (const entry of entries) {
    if (!entry.isDirectory() || !/^[a-f0-9]{64}$/.test(entry.name)) continue;
    const manifest = await readManifest(path.join(savesRoot, entry.name));
    if (manifest.exists) saves.push(manifest);
  }
  saves.sort((left, right) => String(right.updatedAt || "").localeCompare(String(left.updatedAt || "")));
  return saves;
}

async function saveLibrary(space) {
  const saves = await listSaveManifests(space);
  let totalBytes = 0;
  for (const save of saves) {
    const dir = targetSaveDir(space, save.saveMode, save.saveName);
    for (const filename of ["current.zip", "previous.zip"]) {
      try {
        totalBytes += (await stat(path.join(dir, filename))).size;
      } catch (error) {
        if (error.code !== "ENOENT") throw error;
      }
    }
  }
  return { saves, totalBytes };
}

async function deleteSave(res, syncKey, url) {
  const saveMode = url.searchParams.get("saveMode");
  const saveName = url.searchParams.get("saveName");
  if (!saveMode || !saveName) return json(res, 400, { error: "save_identity_incomplete" });
  const mode = safeHeader(saveMode, "Sandbox");
  const name = safeHeader(saveName, "Unknown save");
  const space = await ensureSpace(syncKey);
  const target = targetSaveDir(space, mode, name);
  const manifest = await readManifest(target);
  if (!manifest.exists) return json(res, 404, { error: "save_not_found" });
  await rm(target, { recursive: true, force: true });
  return json(res, 200, { deleted: true, saveMode: mode, saveName: name, ...(await saveLibrary(space)) });
}

async function exactManifest(space, saveMode, saveName) {
  await migrateLegacySave(space);
  return readManifest(targetSaveDir(space, saveMode, saveName));
}

function validUploadId(value) {
  return typeof value === "string" && /^[a-f0-9]{32}$/.test(value);
}

async function readJsonBody(req, maxBytes = 64 * 1024) {
  const chunks = [];
  let bytes = 0;
  for await (const chunk of req) {
    bytes += chunk.length;
    if (bytes > maxBytes) throw Object.assign(new Error("request_too_large"), { statusCode: 413 });
    chunks.push(chunk);
  }
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

async function initChunkedUpload(req, res, syncKey) {
  const space = await ensureSpace(syncKey);
  const body = await readJsonBody(req);
  const totalBytes = Number(body.totalBytes);
  const chunkSize = Number(body.chunkSize);
  const chunkCount = Number(body.chunkCount);
  const saveMode = safeHeader(body.saveMode, "Sandbox");
  const saveName = safeHeader(body.saveName, "Unknown save");
  const currentManifest = await exactManifest(space, saveMode, saveName);
  if (currentManifest.exists && body.overwriteConfirmed !== true) {
    return json(res, 409, { error: "overwrite_confirmation_required", current: currentManifest });
  }
  if (!Number.isSafeInteger(totalBytes) || totalBytes < 1 || totalBytes > maxSnapshotBytes) {
    return json(res, 400, { error: "invalid_total_bytes", maxBytes: maxSnapshotBytes });
  }
  if (!Number.isSafeInteger(chunkSize) || chunkSize < 1 || chunkSize > maxChunkBytes) {
    return json(res, 400, { error: "invalid_chunk_size", maxChunkBytes });
  }
  if (!Number.isSafeInteger(chunkCount) || chunkCount < 1 || chunkCount !== Math.ceil(totalBytes / chunkSize)) {
    return json(res, 400, { error: "invalid_chunk_count" });
  }
  if (freeBytes() < Math.max(minFreeBytes, totalBytes * 2)) {
    return json(res, 507, { error: "server_storage_low" });
  }

  const uploadId = crypto.randomBytes(16).toString("hex");
  const uploadDir = path.join(space, "uploads", uploadId);
  await mkdir(uploadDir, { recursive: true });
  await writeFile(path.join(uploadDir, "upload.json"), JSON.stringify({
    totalBytes,
    chunkSize,
    chunkCount,
    saveMode,
    saveName,
    deviceName: safeHeader(body.deviceName, "Unknown device"),
    gameVersion: safeHeader(body.gameVersion, "Unknown"),
    overwriteConfirmed: body.overwriteConfirmed === true,
    createdAt: new Date().toISOString(),
  }, null, 2));
  return json(res, 201, { uploadId, chunkSize, chunkCount });
}

async function uploadChunk(req, res, syncKey, uploadId, indexText) {
  if (!validUploadId(uploadId)) return json(res, 400, { error: "invalid_upload_id" });
  const space = await ensureSpace(syncKey);
  const index = Number(indexText);
  const uploadDir = path.join(space, "uploads", uploadId);
  let metadata;
  try {
    metadata = JSON.parse(await readFile(path.join(uploadDir, "upload.json"), "utf8"));
  } catch (error) {
    if (error.code === "ENOENT") return json(res, 404, { error: "upload_not_found" });
    throw error;
  }
  if (!Number.isSafeInteger(index) || index < 0 || index >= metadata.chunkCount) {
    return json(res, 400, { error: "invalid_chunk_index" });
  }
  const expectedBytes = index === metadata.chunkCount - 1
    ? metadata.totalBytes - (metadata.chunkSize * index)
    : metadata.chunkSize;
  const contentLength = Number(req.headers["content-length"] || -1);
  if (contentLength !== expectedBytes || contentLength > maxChunkBytes) {
    return json(res, 400, { error: "invalid_chunk_length", expectedBytes });
  }

  const tmpPath = path.join(uploadDir, `${index}.${process.pid}.tmp`);
  const finalPath = path.join(uploadDir, `${index}.part`);
  const output = createWriteStream(tmpPath, { flags: "wx" });
  const hash = crypto.createHash("sha256");
  let bytes = 0;
  try {
    for await (const chunk of req) {
      bytes += chunk.length;
      if (bytes > expectedBytes) throw Object.assign(new Error("invalid_chunk_length"), { statusCode: 400 });
      hash.update(chunk);
      if (!output.write(chunk)) await new Promise((resolve) => output.once("drain", resolve));
    }
    await new Promise((resolve, reject) => {
      output.once("error", reject);
      output.end(resolve);
    });
    const digest = hash.digest("hex");
    const expectedHash = req.headers["x-chunk-sha256"];
    if (bytes !== expectedBytes || (expectedHash && expectedHash !== digest)) {
      await rm(tmpPath, { force: true });
      return json(res, 400, { error: "chunk_verification_failed" });
    }
    await rm(finalPath, { force: true });
    await rename(tmpPath, finalPath);
    return json(res, 200, { index, bytes, sha256: digest });
  } catch (error) {
    output.destroy();
    await rm(tmpPath, { force: true });
    throw error;
  }
}

async function completeChunkedUpload(res, syncKey, uploadId) {
  if (!validUploadId(uploadId)) return json(res, 400, { error: "invalid_upload_id" });
  const space = await ensureSpace(syncKey);
  const uploadDir = path.join(space, "uploads", uploadId);
  let metadata;
  try {
    metadata = JSON.parse(await readFile(path.join(uploadDir, "upload.json"), "utf8"));
  } catch (error) {
    if (error.code === "ENOENT") return json(res, 404, { error: "upload_not_found" });
    throw error;
  }
  const target = targetSaveDir(space, metadata.saveMode, metadata.saveName);
  const currentManifest = await readManifest(target);
  if (currentManifest.exists && metadata.overwriteConfirmed !== true) {
    return json(res, 409, { error: "overwrite_confirmation_required", current: currentManifest });
  }
  const entries = await readdir(uploadDir);
  for (let index = 0; index < metadata.chunkCount; index += 1) {
    if (!entries.includes(`${index}.part`)) return json(res, 409, { error: "missing_chunk", index });
  }

  const assembledPath = path.join(uploadDir, "assembled.tmp");
  const output = await open(assembledPath, "w");
  const hash = crypto.createHash("sha256");
  let bytes = 0;
  try {
    for (let index = 0; index < metadata.chunkCount; index += 1) {
      const part = await readFile(path.join(uploadDir, `${index}.part`));
      bytes += part.length;
      hash.update(part);
      await output.write(part);
    }
  } finally {
    await output.close();
  }
  if (bytes !== metadata.totalBytes) {
    await rm(assembledPath, { force: true });
    return json(res, 409, { error: "assembled_size_mismatch", expectedBytes: metadata.totalBytes, bytes });
  }

  await mkdir(target, { recursive: true });
  const currentPath = path.join(target, "current.zip");
  const previousPath = path.join(target, "previous.zip");
  if (existsSync(currentPath)) {
    await rm(previousPath, { force: true });
    await rename(currentPath, previousPath);
  }
  await rename(assembledPath, currentPath);
  const manifest = {
    exists: true,
    bytes,
    sha256: hash.digest("hex"),
    updatedAt: new Date().toISOString(),
    saveMode: metadata.saveMode,
    saveName: metadata.saveName,
    deviceName: metadata.deviceName,
    gameVersion: metadata.gameVersion,
  };
  await writeManifest(target, manifest);
  await rm(uploadDir, { recursive: true, force: true });
  return json(res, 200, manifest);
}

async function uploadSnapshot(req, res, syncKey) {
  const space = await ensureSpace(syncKey);
  const saveMode = safeHeader(req.headers["x-save-mode"], "Sandbox");
  const saveName = safeHeader(req.headers["x-save-name"], "Unknown save");
  const target = targetSaveDir(space, saveMode, saveName);
  const currentManifest = await readManifest(target);
  if (currentManifest.exists && req.headers["x-overwrite-confirmed"] !== "true") {
    return json(res, 409, { error: "overwrite_confirmation_required", current: currentManifest });
  }
  const contentLength = Number(req.headers["content-length"] || 0);
  if (contentLength > maxSnapshotBytes) return json(res, 413, { error: "snapshot_too_large", maxBytes: maxSnapshotBytes });
  if (freeBytes() < Math.max(minFreeBytes, contentLength * 2)) return json(res, 507, { error: "server_storage_low" });

  await mkdir(target, { recursive: true });
  const tmpPath = path.join(target, `current.${process.pid}.${Date.now()}.tmp`);
  const output = createWriteStream(tmpPath, { flags: "wx" });
  const hash = crypto.createHash("sha256");
  let bytes = 0;
  try {
    for await (const chunk of req) {
      bytes += chunk.length;
      if (bytes > maxSnapshotBytes) {
        output.destroy();
        await rm(tmpPath, { force: true });
        return json(res, 413, { error: "snapshot_too_large", maxBytes: maxSnapshotBytes });
      }
      hash.update(chunk);
      if (!output.write(chunk)) await new Promise((resolve) => output.once("drain", resolve));
    }
    await new Promise((resolve, reject) => {
      output.once("error", reject);
      output.end(resolve);
    });
    const currentPath = path.join(target, "current.zip");
    const previousPath = path.join(target, "previous.zip");
    if (existsSync(currentPath)) {
      await rm(previousPath, { force: true });
      await rename(currentPath, previousPath);
    }
    await rename(tmpPath, currentPath);
    const manifest = {
      exists: true,
      bytes,
      sha256: hash.digest("hex"),
      updatedAt: new Date().toISOString(),
      saveMode,
      saveName,
      deviceName: safeHeader(req.headers["x-device-name"], "Unknown device"),
      gameVersion: safeHeader(req.headers["x-game-version"], "Unknown"),
    };
    await writeManifest(target, manifest);
    return json(res, 200, manifest);
  } catch (error) {
    output.destroy();
    await rm(tmpPath, { force: true });
    throw error;
  }
}

async function selectedSave(space, url) {
  const saveMode = url.searchParams.get("saveMode");
  const saveName = url.searchParams.get("saveName");
  if ((saveMode && !saveName) || (!saveMode && saveName)) return { error: "save_identity_incomplete" };
  if (saveMode && saveName) {
    const mode = safeHeader(saveMode, "Sandbox");
    const name = safeHeader(saveName, "Unknown save");
    const manifest = await exactManifest(space, mode, name);
    return manifest.exists ? { manifest, dir: targetSaveDir(space, mode, name) } : null;
  }
  const saves = await listSaveManifests(space);
  if (!saves.length) return null;
  const manifest = saves[0];
  return { manifest, dir: targetSaveDir(space, manifest.saveMode, manifest.saveName) };
}

async function downloadSnapshot(res, syncKey, url) {
  const space = await ensureSpace(syncKey);
  const selected = await selectedSave(space, url);
  if (selected?.error) return json(res, 400, { error: selected.error });
  if (!selected) return json(res, 404, { error: "snapshot_not_found" });
  const version = url.searchParams.get("version") === "previous" ? "previous" : "current";
  const filePath = path.join(selected.dir, `${version}.zip`);
  try {
    const info = await stat(filePath);
    res.writeHead(200, {
      "content-type": "application/zip",
      "content-length": info.size,
      "content-disposition": `attachment; filename="zomboid-${version}.zip"`,
      "cache-control": "no-store",
      ...corsHeaders(),
    });
    createReadStream(filePath).pipe(res);
  } catch (error) {
    if (error.code === "ENOENT") return json(res, 404, { error: "snapshot_not_found" });
    throw error;
  }
}

export function createSyncServer() {
  return createServer(async (req, res) => {
    try {
      if (req.method === "OPTIONS") {
        res.writeHead(204, corsHeaders());
        return res.end();
      }
      if (req.url === "/health" && req.method === "GET") {
        return json(res, 200, { ok: true, service: "zomboid-save-sync", version: "0.2.0" });
      }

      const url = new URL(req.url, `http://${req.headers.host || "localhost"}`);
      const syncKey = syncKeyFromRequest(req);
      if (!syncKey) return json(res, 401, { error: "invalid_sync_key" });
      if (url.pathname === "/v1/saves" && req.method === "GET") {
        return json(res, 200, await saveLibrary(await ensureSpace(syncKey)));
      }
      if (url.pathname === "/v1/saves" && req.method === "DELETE") return deleteSave(res, syncKey, url);
      if (url.pathname === "/v1/manifest" && req.method === "GET") {
        const selected = await selectedSave(await ensureSpace(syncKey), url);
        return json(res, 200, selected && !selected.error ? selected.manifest : { exists: false });
      }
      if (url.pathname === "/v1/snapshot" && req.method === "PUT") return uploadSnapshot(req, res, syncKey);
      if (url.pathname === "/v1/snapshot" && req.method === "GET") return downloadSnapshot(res, syncKey, url);
      if (url.pathname === "/v1/uploads" && req.method === "POST") return initChunkedUpload(req, res, syncKey);
      const chunkMatch = url.pathname.match(/^\/v1\/uploads\/([a-f0-9]{32})\/chunks\/(\d+)$/);
      if (chunkMatch && req.method === "PUT") return uploadChunk(req, res, syncKey, chunkMatch[1], chunkMatch[2]);
      const completeMatch = url.pathname.match(/^\/v1\/uploads\/([a-f0-9]{32})\/complete$/);
      if (completeMatch && req.method === "POST") return completeChunkedUpload(res, syncKey, completeMatch[1]);
      return json(res, 404, { error: "not_found" });
    } catch (error) {
      console.error(error);
      if (!res.headersSent) json(res, error.statusCode || 500, { error: error.message || "internal_error" });
      else res.destroy(error);
    }
  });
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await mkdir(dataDir, { recursive: true });
  createSyncServer().listen(port, "0.0.0.0", () => {
    console.log(`Zomboid sync API listening on :${port}`);
    console.log(`Data directory: ${dataDir}`);
    console.log(`Max snapshot bytes: ${maxSnapshotBytes}`);
  });
}
