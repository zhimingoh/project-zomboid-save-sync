---
title: 001-resumable-upload-cache 实施偏差记录
module: save-transfer
change_id: 001-resumable-upload-cache
tags: [偏差]
created: 2026-08-12
updated: 2026-08-12
status: active
summary: >
  记录持久缓存目录与旧客户端协议兼容的实施调整。
---

# 001-resumable-upload-cache 实施偏差记录

> 关联 change：[change.md](change.md)

## 偏差项

### [D-1] 缓存使用应用数据目录
- **相关 TODO**：TODO-S1、TODO-S3
- **原计划**：使用 Tauri 应用数据目录或缓存目录承载持久 ZIP。
- **实际做法**：固定使用应用数据目录下的 `save-archives`，不使用操作系统可主动清理的 cache 目录。
- **原因**：跨客户端重启断点续传依赖 ZIP 稳定存在，系统缓存目录可能被操作系统清理，无法满足恢复可靠性。
- **影响**：缓存更持久；本地磁盘生命周期必须由每存档单份替换策略及后续清理入口管理。

### [D-2] 服务端兼容无 ZIP 摘要的旧客户端
- **相关 TODO**：TODO-S5、TODO-G2
- **原计划**：新上传会话保存并校验 ZIP SHA-256。
- **实际做法**：新客户端必须发送并校验 ZIP SHA-256；旧客户端未发送该字段时，服务端继续使用总长度校验，且新客户端不会恢复这类旧会话。
- **原因**：避免升级 VPS 后当前已发布的旧客户端立即无法上传，兑现 Change 的向后兼容要求。
- **影响**：新客户端上传具备完整摘要校验；旧客户端保持原安全等级和行为。
