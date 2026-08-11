import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";

async function startServer(t, prefix) {
  const dataDir = await mkdtemp(path.join(os.tmpdir(), prefix));
  process.env.SYNC_DATA_DIR = dataDir;
  process.env.SYNC_MAX_BYTES = "1000000";
  process.env.SYNC_MIN_FREE_BYTES = "0";
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
  const init = await fetch(`${base}/v1/uploads`, {
    method: "POST",
    headers: { ...headers, "content-type": "application/json" },
    body: JSON.stringify({ totalBytes: body.length, chunkSize: 8, chunkCount: 3, saveMode: "Apocalypse", saveName: "World B", deviceName: "test" }),
  });
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
