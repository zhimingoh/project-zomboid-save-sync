import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";

async function startServer(t, prefix, options = {}) {
  const dataDir = await mkdtemp(path.join(os.tmpdir(), prefix));
  process.env.SYNC_DATA_DIR = dataDir;
  process.env.SYNC_MAX_BYTES = "1000000";
  process.env.SYNC_MIN_FREE_BYTES = "0";
  process.env.SYNC_UPLOAD_RETENTION_MS = String(options.uploadRetentionMs || 7 * 24 * 60 * 60 * 1000);
  process.env.SYNC_UPLOAD_CLEANUP_INTERVAL_MS = "0";
  const { createSyncServer } = await import(`../src/server.mjs?test=${Date.now()}-${Math.random()}`);
  const server = createSyncServer();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(async () => {
    await new Promise((resolve) => server.close(resolve));
    await rm(dataDir, { recursive: true, force: true });
  });
  return { dataDir, base: `http://127.0.0.1:${server.address().port}` };
}

function auth(key) {
  return { Authorization: `Bearer ${key}` };
}

async function directUpload(base, key, body, saveMode, saveName, overwriteConfirmed = false) {
  return fetch(`${base}/v1/snapshot`, {
    method: "PUT",
    headers: {
      ...auth(key),
      "content-type": "application/zip",
      "content-length": String(body.length),
      "x-save-mode": saveMode,
      "x-save-name": saveName,
      "x-device-name": "test-device",
      "x-overwrite-confirmed": String(overwriteConfirmed),
    },
    body,
  });
}

async function download(base, key, saveMode, saveName, version = "current") {
  const query = new URLSearchParams({ saveMode, saveName, version });
  const response = await fetch(`${base}/v1/snapshot?${query}`, { headers: auth(key) });
  assert.equal(response.status, 200);
  return Buffer.from(await response.arrayBuffer());
}

async function initChunked(base, key, body, saveMode, saveName, chunkSize = 8, overwriteConfirmed = false) {
  return fetch(`${base}/v1/uploads`, {
    method: "POST",
    headers: { ...auth(key), "content-type": "application/json" },
    body: JSON.stringify({
      totalBytes: body.length,
      chunkSize,
      chunkCount: Math.ceil(body.length / chunkSize),
      saveMode,
      saveName,
      deviceName: "test",
      overwriteConfirmed,
      zipSha256: crypto.createHash("sha256").update(body).digest("hex"),
    }),
  });
}

async function putChunk(base, key, uploadId, body, chunkSize, index) {
  const chunk = body.subarray(index * chunkSize, Math.min(body.length, (index + 1) * chunkSize));
  return fetch(`${base}/v1/uploads/${uploadId}/chunks/${index}`, {
    method: "PUT",
    headers: {
      ...auth(key),
      "content-length": String(chunk.length),
      "x-chunk-sha256": crypto.createHash("sha256").update(chunk).digest("hex"),
    },
    body: chunk,
  });
}

test("stores different saves independently and only overwrites an exact identity", async (t) => {
  const { base } = await startServer(t, "zomboid-multi-");
  const key = "a".repeat(32);
  const apocalypse = Buffer.from("apocalypse save");
  const sandbox = Buffer.from("sandbox save");

  assert.equal((await directUpload(base, key, apocalypse, "Apocalypse", "2026-06-10_18-56-30")).status, 200);
  assert.equal((await directUpload(base, key, sandbox, "Sandbox", "2026-08-08_13-43-57")).status, 200);

  const list = await (await fetch(`${base}/v1/saves`, { headers: auth(key) })).json();
  assert.equal(list.saves.length, 2);
  assert.deepEqual(
    list.saves.map((save) => `${save.saveMode}/${save.saveName}`).sort(),
    ["Apocalypse/2026-06-10_18-56-30", "Sandbox/2026-08-08_13-43-57"],
  );
  assert.equal((await download(base, key, "Apocalypse", "2026-06-10_18-56-30")).toString(), apocalypse.toString());
  assert.equal((await download(base, key, "Sandbox", "2026-08-08_13-43-57")).toString(), sandbox.toString());

  const replacement = Buffer.from("sandbox replacement");
  const rejected = await directUpload(base, key, replacement, "Sandbox", "2026-08-08_13-43-57");
  assert.equal(rejected.status, 409);
  assert.equal((await rejected.json()).error, "overwrite_confirmation_required");
  assert.equal((await directUpload(base, key, replacement, "Sandbox", "2026-08-08_13-43-57", true)).status, 200);
  assert.equal((await download(base, key, "Sandbox", "2026-08-08_13-43-57")).toString(), replacement.toString());
  assert.equal((await download(base, key, "Sandbox", "2026-08-08_13-43-57", "previous")).toString(), sandbox.toString());

  const library = await (await fetch(`${base}/v1/saves`, { headers: auth(key) })).json();
  assert.equal(library.totalBytes, apocalypse.length + replacement.length + sandbox.length);
  const deleteQuery = new URLSearchParams({ saveMode: "Apocalypse", saveName: "2026-06-10_18-56-30" });
  const deleted = await fetch(`${base}/v1/saves?${deleteQuery}`, { method: "DELETE", headers: auth(key) });
  assert.equal(deleted.status, 200);
  const deletedBody = await deleted.json();
  assert.equal(deletedBody.deleted, true);
  assert.equal(deletedBody.saves.length, 1);
  assert.equal(deletedBody.totalBytes, replacement.length + sandbox.length);
  assert.equal((await fetch(`${base}/v1/snapshot?${deleteQuery}`, { headers: auth(key) })).status, 404);
});

test("assembles a chunked upload into the selected save", async (t) => {
  const { base } = await startServer(t, "zomboid-chunks-");
  const key = "b".repeat(32);
  const body = Buffer.from("abcdefghijklmnopqrst");
  const headers = auth(key);
  const init = await initChunked(base, key, body, "Apocalypse", "World B");
  assert.equal(init.status, 201);
  const { uploadId } = await init.json();
  for (let index = 0; index < 3; index += 1) {
    const chunk = body.subarray(index * 8, Math.min(body.length, (index + 1) * 8));
    const response = await fetch(`${base}/v1/uploads/${uploadId}/chunks/${index}`, {
      method: "PUT",
      headers: {
        ...headers,
        "content-length": String(chunk.length),
        "x-chunk-sha256": crypto.createHash("sha256").update(chunk).digest("hex"),
      },
      body: chunk,
    });
    assert.equal(response.status, 200);
  }
  assert.equal((await fetch(`${base}/v1/uploads/${uploadId}/complete`, { method: "POST", headers })).status, 200);
  assert.equal((await download(base, key, "Apocalypse", "World B")).toString(), body.toString());
});

test("accepts legacy chunked clients that do not send a zip digest", async (t) => {
  const { base } = await startServer(t, "zomboid-legacy-chunks-");
  const key = "9".repeat(32);
  const body = Buffer.from("legacy-client-body");
  const init = await fetch(`${base}/v1/uploads`, {
    method: "POST",
    headers: { ...auth(key), "content-type": "application/json" },
    body: JSON.stringify({
      totalBytes: body.length,
      chunkSize: 8,
      chunkCount: Math.ceil(body.length / 8),
      saveMode: "Sandbox",
      saveName: "LegacyChunkWorld",
    }),
  });
  assert.equal(init.status, 201);
  const { uploadId } = await init.json();
  for (let index = 0; index < Math.ceil(body.length / 8); index += 1) {
    assert.equal((await putChunk(base, key, uploadId, body, 8, index)).status, 200);
  }
  assert.equal((await fetch(`${base}/v1/uploads/${uploadId}/complete`, { method: "POST", headers: auth(key) })).status, 200);
  assert.equal((await download(base, key, "Sandbox", "LegacyChunkWorld")).toString(), body.toString());
});

test("reports completed chunks and isolates resumable sessions by sync key", async (t) => {
  const { base } = await startServer(t, "zomboid-resume-");
  const key = "d".repeat(32);
  const otherKey = "e".repeat(32);
  const body = Buffer.from("resume-this-upload");
  const init = await initChunked(base, key, body, "Sandbox", "ResumeWorld");
  assert.equal(init.status, 201);
  const { uploadId } = await init.json();
  assert.equal((await putChunk(base, key, uploadId, body, 8, 0)).status, 200);
  assert.equal((await putChunk(base, key, uploadId, body, 8, 0)).status, 200);

  const status = await fetch(`${base}/v1/uploads/${uploadId}`, { headers: auth(key) });
  assert.equal(status.status, 200);
  const session = await status.json();
  assert.deepEqual(session.completedChunks, [0]);
  assert.equal(session.zipSha256, crypto.createHash("sha256").update(body).digest("hex"));
  assert.equal((await fetch(`${base}/v1/uploads/${uploadId}`, { headers: auth(otherKey) })).status, 404);

  for (const index of [1, 2]) assert.equal((await putChunk(base, key, uploadId, body, 8, index)).status, 200);
  assert.equal((await fetch(`${base}/v1/uploads/${uploadId}/complete`, { method: "POST", headers: auth(key) })).status, 200);
  assert.equal((await download(base, key, "Sandbox", "ResumeWorld")).toString(), body.toString());
});

test("rejects completion when the remote snapshot changed after session creation", async (t) => {
  const { base } = await startServer(t, "zomboid-conflict-");
  const key = "f".repeat(32);
  const original = Buffer.from("original");
  const resumed = Buffer.from("stale resumed body");
  assert.equal((await directUpload(base, key, original, "Sandbox", "ConflictWorld")).status, 200);
  const init = await initChunked(base, key, resumed, "Sandbox", "ConflictWorld", 8, true);
  assert.equal(init.status, 201);
  const { uploadId } = await init.json();
  assert.equal((await directUpload(base, key, Buffer.from("newer"), "Sandbox", "ConflictWorld", true)).status, 200);
  for (let index = 0; index < Math.ceil(resumed.length / 8); index += 1) {
    assert.equal((await putChunk(base, key, uploadId, resumed, 8, index)).status, 200);
  }
  const completed = await fetch(`${base}/v1/uploads/${uploadId}/complete`, { method: "POST", headers: auth(key) });
  assert.equal(completed.status, 409);
  assert.equal((await completed.json()).error, "remote_snapshot_changed");
  assert.equal((await download(base, key, "Sandbox", "ConflictWorld")).toString(), "newer");
});

test("serializes concurrent publishes for the same remote save", async (t) => {
  const { base } = await startServer(t, "zomboid-concurrent-");
  const key = "2".repeat(32);
  const first = Buffer.from("first concurrent upload");
  const second = Buffer.from("second concurrent upload");
  const firstInit = await initChunked(base, key, first, "Sandbox", "ConcurrentWorld", 8);
  const secondInit = await initChunked(base, key, second, "Sandbox", "ConcurrentWorld", 8);
  const firstId = (await firstInit.json()).uploadId;
  const secondId = (await secondInit.json()).uploadId;
  for (let index = 0; index < Math.ceil(first.length / 8); index += 1) {
    assert.equal((await putChunk(base, key, firstId, first, 8, index)).status, 200);
  }
  for (let index = 0; index < Math.ceil(second.length / 8); index += 1) {
    assert.equal((await putChunk(base, key, secondId, second, 8, index)).status, 200);
  }
  const responses = await Promise.all([
    fetch(`${base}/v1/uploads/${firstId}/complete`, { method: "POST", headers: auth(key) }),
    fetch(`${base}/v1/uploads/${secondId}/complete`, { method: "POST", headers: auth(key) }),
  ]);
  assert.deepEqual(responses.map((response) => response.status).sort(), [200, 409]);
  const published = await download(base, key, "Sandbox", "ConcurrentWorld");
  assert.ok(published.equals(first) || published.equals(second));
});

test("removes expired incomplete upload sessions", async (t) => {
  const { base } = await startServer(t, "zomboid-expiry-", { uploadRetentionMs: 5 });
  const key = "1".repeat(32);
  const body = Buffer.from("expires");
  const init = await initChunked(base, key, body, "Sandbox", "ExpiredWorld");
  const { uploadId } = await init.json();
  await new Promise((resolve) => setTimeout(resolve, 15));
  const status = await fetch(`${base}/v1/uploads/${uploadId}`, { headers: auth(key) });
  assert.equal(status.status, 404);
});

test("migrates a legacy single-slot save into the multi-save layout", async (t) => {
  const { dataDir, base } = await startServer(t, "zomboid-legacy-");
  const key = "c".repeat(32);
  const keyHash = crypto.createHash("sha256").update(key).digest("hex");
  const space = path.join(dataDir, keyHash);
  await mkdir(space, { recursive: true });
  await writeFile(path.join(space, "current.zip"), "legacy-current");
  await writeFile(path.join(space, "previous.zip"), "legacy-previous");
  await writeFile(path.join(space, "manifest.json"), JSON.stringify({
    exists: true,
    bytes: 14,
    updatedAt: "2026-01-01T00:00:00.000Z",
    saveMode: "Sandbox",
    saveName: "LegacyWorld",
    deviceName: "old-client",
  }));

  const list = await (await fetch(`${base}/v1/saves`, { headers: auth(key) })).json();
  assert.equal(list.saves.length, 1);
  assert.equal(list.saves[0].saveName, "LegacyWorld");
  assert.equal((await download(base, key, "Sandbox", "LegacyWorld")).toString(), "legacy-current");
  assert.equal((await download(base, key, "Sandbox", "LegacyWorld", "previous")).toString(), "legacy-previous");
  await assert.rejects(readFile(path.join(space, "manifest.json")), { code: "ENOENT" });
});
