---
title: 压缩缓存与跨重启断点续传测试报告
module: save-transfer
change_id: 001-resumable-upload-cache
tags: [upload, cache, resume, test]
created: 2026-08-12
updated: 2026-08-12
status: active
summary: >
  Rust 单元测试、Node 集成测试、前端构建和 Release 编译全部通过，未发现阻塞缺陷。
---

# 压缩缓存与跨重启断点续传 测试报告

> 关联模块：[spec.md](../../spec.md)
> 关联 Change：[change.md](change.md)
> 测试日期：2026-08-12
> 测试环境：Windows x64；Rust stable MSVC；Node.js；Tauri 2；Vite 7

## 测试总结

| 指标 | 数值 |
|------|------|
| Rust 单元测试总数 | 8 |
| Rust 单元测试通过 | 8 |
| Rust 单元测试失败 | 0 |
| Node 集成测试总数 | 8 |
| Node 集成测试通过 | 8 |
| Node 集成测试失败 | 0 |
| 构建验证 | 2/2 通过 |
| 总体通过率 | 100% |

## 单元测试详情

### ✅ 通过的测试

| 测试文件 | 测试用例 | 对应 TODO |
|----------|----------|-----------|
| `desktop/src-tauri/src/lib.rs` | 存档清单排序稳定、未变化项复用、文件集合变化改变指纹 | S2、G1 |
| `desktop/src-tauri/src/lib.rs` | 恢复状态绑定 endpoint、密钥摘要、存档和 ZIP，序列化不含明文密钥 | S1、S4、G1 |
| `desktop/src-tauri/src/lib.rs` | 非连续已完成分片与不足整片的最后一片按真实字节计算 | S7、C1、G1 |
| `desktop/src-tauri/src/lib.rs` | 游戏进程、`__MACOSX`、重试状态与阶段百分比回归 | S7、C1 |
| `server/test/api.test.mjs` | 分片状态查询、重复分片、跨密钥隔离与恢复完成 | S5、S6、S7、G2 |
| `server/test/api.test.mjs` | 远程快照版本变化时拒绝旧会话完成 | S5、S7、G2 |
| `server/test/api.test.mjs` | 同一远程存档并发完成请求串行发布，仅一个成功 | S5、G2 |
| `server/test/api.test.mjs` | 过期上传会话自动删除 | S8、G2 |
| `server/test/api.test.mjs` | 缺少 ZIP SHA-256 的旧客户端保持可上传 | S5、G2 |

### ❌ 失败的测试

无。

## 集成 / 场景验证详情

### 场景 1：服务端断点恢复协议
- **操作步骤**：创建三分片会话，上传并重复上传第一片，查询状态，再上传缺失分片并完成。
- **期望结果**：状态仅报告第一片；重复写保持幂等；最终快照与原始内容一致。
- **实际结果**：✅ 符合预期。
- **证据**：`npm test` 中 `reports completed chunks and isolates resumable sessions by sync key` 通过。

### 场景 2：远程并发保护
- **操作步骤**：创建覆盖会话后由另一请求更新同名快照，再完成旧会话。
- **期望结果**：返回 HTTP 409，当前快照保持较新内容。
- **实际结果**：✅ 符合预期。
- **证据**：`rejects completion when the remote snapshot changed after session creation` 通过。

### 场景 3：过期会话清理
- **操作步骤**：使用 5 ms 保留期创建会话，等待后查询。
- **期望结果**：会话被清理并返回 404。
- **实际结果**：✅ 符合预期。
- **证据**：`removes expired incomplete upload sessions` 通过。

### 场景 4：生产构建
- **操作步骤**：运行 `cargo check --release`、`npm run build` 和 JS 语法检查。
- **期望结果**：Rust Release 与前端生产构建成功。
- **实际结果**：✅ 符合预期。

### 场景 5：敏感信息检查
- **操作步骤**：扫描仓库中的已知 VPS 密码、IP、GitHub token 和测试同步密钥。
- **期望结果**：真实凭据不存在；测试密钥仅出现在单元测试夹具中。
- **实际结果**：✅ 符合预期。

## 未覆盖的测试场景

- 尚未在真实 macOS 设备编译运行；这是项目既有平台验证限制，不影响 Windows 实现测试结论。
- 未通过真实网络强制中断桌面进程后再启动做 GUI 级续传演示；客户端持久状态匹配与服务端部分分片恢复已分别由单元和集成测试覆盖。
- 未连接未升级的线上旧 VPS 执行端到端上传；客户端对 404 状态查询退化为新会话的代码路径已检查，服务端旧客户端兼容有自动化测试。

## 遗留问题

- 无。首轮代码审查发现的问题已修复，Release 编译无警告。
