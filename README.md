# Project Zomboid Save Sync

[中文](#中文) | [English](#english)

An unofficial, self-hosted save synchronization tool for Project Zomboid.

> This project is not affiliated with or endorsed by The Indie Stone. Project Zomboid is a trademark of its respective owner.

## 中文

### 为什么做这个工具

Project Zomboid 目前没有官方云存档。玩家在 Windows 和 macOS 之间切换设备时，需要手动复制体积很大、并且会持续增长的存档目录。

Project Zomboid Save Sync 提供一个简单的桌面客户端，把选中的单个存档压缩后上传到你自己的 VPS。另一台电脑使用相同的同步密钥，即可从 VPS 下载该存档。

这是一个自托管工具，不提供公共托管云服务。存储空间、流量、域名和 VPS 成本由部署者自行承担。

### 当前功能

- 支持 Windows 和 macOS 的 Project Zomboid 存档目录。
- 自动检测 `Zomboid/Saves`，也可以手动选择目录。
- 识别 `Sandbox`、`Apocalypse` 等模式下的独立存档。
- 手动上传和下载，避免后台同步造成存档冲突。
- 显示同步密钥下的全部 VPS 存档、大小、更新时间和来源设备。
- VPS 存档按每页 5 条分页显示，并展示当前版本与回滚版本占用的总容量。
- 可在二次确认后删除指定 VPS 存档的当前版本和回滚版本。
- 不同模式或不同名称的存档会独立保存；只有模式和名称都相同才视为覆盖。
- 覆盖同一个存档时必须明确确认；取消确认不会上传。
- 游戏运行时禁止上传和下载。
- 上传前压缩存档，并按 256 KiB 分片上传。
- 分阶段显示检查、压缩、上传和服务器保存进度。
- 网络中断时自动重试分片。
- 每个独立存档保留当前版本和上一个回滚版本。
- 自动忽略 macOS 生成的 `__MACOSX` 元数据目录。

### 下载 Windows 客户端

从 [GitHub Releases](https://github.com/zhimingoh/project-zomboid-save-sync/releases/latest) 下载最新的 Windows x64 安装包。

macOS 客户端源码已经兼容，但 macOS 安装包需要在 macOS 上构建和签名，目前 Release 仅提供 Windows 安装包。

### 使用方法

1. 在自己的 VPS 上部署本仓库中的 `server/`。
2. 在桌面客户端中填写 VPS 的 HTTPS API 地址。
3. 点击“生成密钥”，妥善保存同步密钥。
4. 两台电脑必须使用完全相同的同步密钥。
5. 在源电脑选择存档并上传。
6. 上传完成后，再到目标电脑下载。
7. 上传或下载期间不要启动游戏。

默认存档路径：

- Windows：`%USERPROFILE%\Zomboid\Saves`
- macOS：`~/Zomboid/Saves`

同步密钥相当于该存档空间的密码。丢失密钥后无法找回对应存档；泄露密钥会让其他人访问该存档。请只通过 HTTPS 连接服务器。

### VPS 要求

- Linux VPS，示例命令适用于 Ubuntu/Debian。
- Node.js 24 或更高版本。
- 一个域名和 HTTPS，或者一个 Cloudflare Tunnel。
- 足够的磁盘空间和流量。

服务端不需要安装 npm 依赖。默认配置中，单个同步空间最多接受 10 GiB 的压缩快照，并在磁盘剩余空间低于 1 GiB 时拒绝新上传。

一个同步密钥可以保存多个存档。每个存档最多保留当前快照和上一个快照，因此实际磁盘占用可能接近所有压缩存档总大小的两倍。未完成上传也会临时占用空间。

### 部署服务端

以下示例把项目部署到 `/opt/zomboid-sync`，把存档数据放到 `/srv/zomboid-sync/data`。

```bash
sudo useradd --system --home /opt/zomboid-sync --shell /usr/sbin/nologin zomboid-sync || true
sudo git clone https://github.com/zhimingoh/project-zomboid-save-sync.git /opt/zomboid-sync
sudo mkdir -p /srv/zomboid-sync/data
sudo chown -R zomboid-sync:zomboid-sync /srv/zomboid-sync/data

sudo install -m 0644 \
  /opt/zomboid-sync/server/deploy/zomboid-sync.service \
  /etc/systemd/system/zomboid-sync.service

sudo systemctl daemon-reload
sudo systemctl enable --now zomboid-sync
sudo systemctl status zomboid-sync
curl http://127.0.0.1:8787/health
```

可在 `server/deploy/zomboid-sync.service` 中调整：

- `PORT`：监听端口，默认 `8787`。
- `SYNC_DATA_DIR`：存档数据目录。
- `SYNC_MAX_BYTES`：单个压缩快照的最大字节数。
- `SYNC_MIN_FREE_BYTES`：服务器必须保留的最小可用空间。

修改后执行：

```bash
sudo systemctl daemon-reload
sudo systemctl restart zomboid-sync
journalctl -u zomboid-sync -f
```

### 方案 A：Nginx + HTTPS

复制示例配置并替换域名和证书路径：

```bash
sudo cp \
  /opt/zomboid-sync/server/deploy/nginx.conf.example \
  /etc/nginx/sites-available/zomboid-sync
sudo editor /etc/nginx/sites-available/zomboid-sync
sudo ln -s /etc/nginx/sites-available/zomboid-sync /etc/nginx/sites-enabled/zomboid-sync
sudo nginx -t
sudo systemctl reload nginx
```

客户端 API 地址填写：

```text
https://sync.example.com
```

不要把 `8787` 端口直接暴露到公网。使用防火墙只开放 SSH、HTTP 和 HTTPS。

### 方案 B：Cloudflare Tunnel

如果 VPS 没有可用的公网端口，可以让 Cloudflare Tunnel 直接代理本机 API：

```bash
cloudflared tunnel login
cloudflared tunnel create zomboid-save-sync
cloudflared tunnel route dns zomboid-save-sync sync.example.com
```

创建 `/etc/cloudflared/config.yml`：

```yaml
tunnel: YOUR_TUNNEL_ID
credentials-file: /root/.cloudflared/YOUR_TUNNEL_ID.json

ingress:
  - hostname: sync.example.com
    service: http://127.0.0.1:8787
  - service: http_status:404
```

然后安装并启动 Tunnel 服务：

```bash
sudo cloudflared service install
sudo systemctl enable --now cloudflared
curl https://sync.example.com/health
```

客户端 API 地址填写 `https://sync.example.com`。

### 更新部署

```bash
cd /opt/zomboid-sync
sudo git pull --ff-only
sudo systemctl restart zomboid-sync
curl http://127.0.0.1:8787/health
```

更新代码前建议先备份 `/srv/zomboid-sync/data`。

### 从源码运行桌面客户端

需要 Node.js、Rust 和 Tauri 2 的系统依赖。

```powershell
cd desktop
npm ci
npm run tauri -- dev
```

构建 Windows NSIS 安装包：

```powershell
cd desktop
npm ci
npm run tauri -- build --bundles nsis
```

安装包输出到 `desktop/src-tauri/target/release/bundle/nsis/`。

### MVP 限制

- 一个同步密钥可以包含多个独立远程存档。
- 不会自动合并两台电脑上的不同进度。
- 下载会覆盖同名本地存档，但会先创建 `.backup` 备份。
- 服务端存档没有额外加密；安全性依赖 HTTPS、同步密钥和 VPS 本身。
- 没有账号系统、配额管理后台、自动清理策略或公共云托管服务。

## English

### Why this project exists

Project Zomboid does not currently provide official cloud saves. Moving between Windows and macOS normally requires copying a large save directory manually, and that directory keeps growing as the world is explored.

Project Zomboid Save Sync is a small self-hosted desktop tool. It compresses one selected save, uploads it to your own VPS, and lets another computer download it with the same sync key.

This project does not provide a hosted cloud service. The person deploying the server is responsible for storage, bandwidth, domain, and VPS costs.

### Features

- Windows and macOS save path support.
- Automatic `Zomboid/Saves` detection and manual directory selection.
- Individual saves under modes such as `Sandbox` and `Apocalypse`.
- Manual upload and download to reduce save conflicts.
- Displays every VPS save for the sync key, including size, update time, and source device.
- Shows five VPS saves per page and the combined storage used by current and rollback versions.
- Deletes a selected VPS save and its rollback version only after explicit confirmation.
- Saves with different modes or names are stored independently; only an exact mode and name match is an overwrite.
- Requires explicit confirmation before overwriting the exact same save.
- Operations are blocked while Project Zomboid is running.
- ZIP compression and 256 KiB chunked uploads.
- Separate progress for process checking, compression, upload, and server storage.
- Automatic chunk retries after temporary network failures.
- Current and previous snapshots for each individual save.
- Ignores macOS `__MACOSX` metadata directories.

### Download the Windows client

Download the latest Windows x64 installer from [GitHub Releases](https://github.com/zhimingoh/project-zomboid-save-sync/releases/latest).

The source supports macOS, but a distributable macOS build must be built and signed on macOS. The current Release contains only the Windows installer.

### Basic usage

1. Deploy `server/` on your own VPS.
2. Enter the HTTPS API URL in the desktop client.
3. Generate and safely store a sync key.
4. Use exactly the same key on both computers.
5. Upload the save from the source computer.
6. Download it on the destination computer after the upload completes.
7. Keep the game closed during upload and download.

Default save paths:

- Windows: `%USERPROFILE%\Zomboid\Saves`
- macOS: `~/Zomboid/Saves`

The sync key acts as the password for a save space. It cannot be recovered if lost, and anyone who obtains it can access that save. Always place the API behind HTTPS.

### VPS requirements

- A Linux VPS. Commands below target Ubuntu/Debian.
- Node.js 24 or newer.
- A domain with HTTPS, or a Cloudflare Tunnel.
- Enough disk space and bandwidth for growing saves.

The server has no npm runtime dependencies. By default, one compressed snapshot may be up to 10 GiB, and uploads are rejected when less than 1 GiB remains on disk.

A sync key may contain multiple saves. Each save can retain its current and previous snapshots, so storage can approach twice the combined compressed size of all saves. Incomplete uploads also consume temporary space.

### Deploy the server

This example installs the application at `/opt/zomboid-sync` and stores data at `/srv/zomboid-sync/data`:

```bash
sudo useradd --system --home /opt/zomboid-sync --shell /usr/sbin/nologin zomboid-sync || true
sudo git clone https://github.com/zhimingoh/project-zomboid-save-sync.git /opt/zomboid-sync
sudo mkdir -p /srv/zomboid-sync/data
sudo chown -R zomboid-sync:zomboid-sync /srv/zomboid-sync/data

sudo install -m 0644 \
  /opt/zomboid-sync/server/deploy/zomboid-sync.service \
  /etc/systemd/system/zomboid-sync.service

sudo systemctl daemon-reload
sudo systemctl enable --now zomboid-sync
sudo systemctl status zomboid-sync
curl http://127.0.0.1:8787/health
```

The service file exposes these settings:

- `PORT`: listening port, default `8787`.
- `SYNC_DATA_DIR`: snapshot storage directory.
- `SYNC_MAX_BYTES`: maximum compressed snapshot size.
- `SYNC_MIN_FREE_BYTES`: disk space that must remain free.

After changing the service file:

```bash
sudo systemctl daemon-reload
sudo systemctl restart zomboid-sync
journalctl -u zomboid-sync -f
```

### Option A: Nginx with HTTPS

Copy the example, then replace its domain and certificate paths:

```bash
sudo cp \
  /opt/zomboid-sync/server/deploy/nginx.conf.example \
  /etc/nginx/sites-available/zomboid-sync
sudo editor /etc/nginx/sites-available/zomboid-sync
sudo ln -s /etc/nginx/sites-available/zomboid-sync /etc/nginx/sites-enabled/zomboid-sync
sudo nginx -t
sudo systemctl reload nginx
```

Use `https://sync.example.com` as the desktop API URL. Do not expose port `8787` directly to the internet; restrict it with the VPS firewall.

### Option B: Cloudflare Tunnel

When no public application port is available, proxy the local API through Cloudflare Tunnel:

```bash
cloudflared tunnel login
cloudflared tunnel create zomboid-save-sync
cloudflared tunnel route dns zomboid-save-sync sync.example.com
```

Create `/etc/cloudflared/config.yml`:

```yaml
tunnel: YOUR_TUNNEL_ID
credentials-file: /root/.cloudflared/YOUR_TUNNEL_ID.json

ingress:
  - hostname: sync.example.com
    service: http://127.0.0.1:8787
  - service: http_status:404
```

Install and start the tunnel:

```bash
sudo cloudflared service install
sudo systemctl enable --now cloudflared
curl https://sync.example.com/health
```

Use `https://sync.example.com` as the desktop API URL.

### Updating a deployment

```bash
cd /opt/zomboid-sync
sudo git pull --ff-only
sudo systemctl restart zomboid-sync
curl http://127.0.0.1:8787/health
```

Back up `/srv/zomboid-sync/data` before updating.

### Desktop development and builds

Install Node.js, Rust, and the Tauri 2 system prerequisites:

```powershell
cd desktop
npm ci
npm run tauri -- dev
```

Build the Windows NSIS installer:

```powershell
cd desktop
npm ci
npm run tauri -- build --bundles nsis
```

The installer is written to `desktop/src-tauri/target/release/bundle/nsis/`.

### MVP limitations

- Multiple independent remote saves per sync key.
- No automatic merge between diverged saves.
- Download replaces the matching local save after creating a `.backup` directory.
- Server-side snapshots are not additionally encrypted. Security relies on HTTPS, the sync key, and the VPS.
- No user accounts, administration UI, automated retention policy, or hosted cloud service.

## Project layout

- `desktop/`: Tauri 2 desktop application using Rust and Vite.
- `server/`: dependency-free Node.js HTTP API.
- `server/deploy/`: systemd and Nginx deployment examples.
- `server/test/`: API integration tests.

## Tests

```powershell
cd server
npm test

cd ..\desktop\src-tauri
cargo test --lib
```
