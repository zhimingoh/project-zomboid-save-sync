# AGENTS.md

本文档为 AI Agent 提供项目全局上下文。

<!-- eo-doc:start -->
## eo-doc 文档体系（代码侧）

代码侧文档根目录 `eo-doc/`。本表即目录索引，按任务类型读对应子目录 INDEX，不要一次性读完。

涉及代码时，`agent-handbook/INDEX.md` 是必读的代码地图指南：先扫 INDEX 定位模块，再按需读具体模块详情。

| 目录 | 用途 | 何时读 |
|------|------|--------|
| [agent-handbook/](eo-doc/agent-handbook/INDEX.md) | 代码架构、模块入口、接口索引 | 看或改代码前必读 INDEX，按需深入模块 |
| state/（待生成） | 业务规则、状态流转、系统现状 | 首次同步后了解功能现状 |
| [dev/](eo-doc/dev/INDEX.md) | 功能开发文档（spec/change/review） | 查变更进度 |
| [templates/](eo-doc/templates/) | 项目定制模板（eo-* 技能扩展点） | eo-* 技能启动时自动读取 |

项目管理侧见 `.eo-project.json` 的 `project_root` 字段。
<!-- eo-doc:end -->

<!-- eo-project:start -->
## EO-Project

本项目通过 `.eo-project.json` 关联到项目管理侧：`D:\AppCenter\ZombieSaveSync\.eo-project`

- 模式：`local`
- 项目管理侧（roadmap / backlog / decisions / lessons 等）：`D:\AppCenter\ZombieSaveSync\.eo-project`
- 代码侧文档：`eo-doc/`

### 待办提醒

当对话中出现“以后要做”、“TODO”、“先跳过”、“回头处理”等信号，或用户做了 workaround 时，主动提示是否加入项目 backlog。

### 决策同步

当对话中出现关键技术决策（选型、架构、方案取舍）时，提示是否记录到 decisions/。

### 经验教训

当用户提到踩坑、下次不这么做或学到的经验时，提示是否记录到 lessons/。
<!-- eo-project:end -->
