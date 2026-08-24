# Agent Skills 调研：鸿蒙 App 开发 / Rust / 网络编程

> 调研日期：2026-08-21
> 数据来源：本地已安装 skill 列表 + `npx skills find`（skills.sh 开放生态）
> 用途：供其它模型/Agent 参考的 skill 选型与安装指南

---

## 一、需求背景

| 领域 | 需求 |
|---|---|
| 鸿蒙（HarmonyOS） | App 编写规范、调试、最佳实践 |
| Rust | 编码规范、最佳实践 |
| 网络编程 | Rust 网络编程（tokio/async）最佳实现 |

---

## 二、本地已安装的相关 Skill

| Skill | 覆盖范围 | 触发场景 |
|---|---|---|
| `rust-coding-guidelines` | Rust 命名规范、格式化（rustfmt）、clippy lint、代码风格、最佳实践、代码审查 | 询问 Rust 代码风格/命名/格式化/clippy 时自动生效 |
| `android-native-dev` | Android 原生开发（Material Design 3、Kotlin/Compose），**不含鸿蒙** | Android 开发参考 |
| `flutter-dev` / `ios-application-dev` / `react-native-dev` | 其它移动端平台，**不含鸿蒙** | 各平台开发参考 |

**结论**：本地无鸿蒙专属 skill、无网络编程专属 skill。

---

## 三、鸿蒙（HarmonyOS）可安装 Skill

| Skill 标识 | 安装量 | 用途 |
|---|---|---|
| `web-infra-dev/midscene-skills@harmonyos-device-automation` | 2K | 鸿蒙真机 UI 自动化（调试/测试），字节 midscene 团队出品 |
| `majiayu000/spellbook@harmonyos-app` | 451 | 鸿蒙 App 开发 |
| `fadinglight9291117/arkts_skills@harmonyos-build-deploy` | 245 | ArkTS 构建与部署 |
| `linhay/harmony-next.skills@harmony-next` | 242 | HarmonyOS NEXT 开发 |
| `coreylyn/harmonyos-skills@harmonyos-dev` | 158 | 鸿蒙开发规范 |
| `coreylyn/harmonyos-skills@harmonyos-review` | 97 | 鸿蒙代码评审 |
| `openharmonyinsight/openharmony-skills@android-to-harmonyos-migration-workflow` | 84 | Android → 鸿蒙迁移工作流 |

### 安装命令

```bash
# 鸿蒙真机自动化调试（推荐）
npx skills add web-infra-dev/midscene-skills@harmonyos-device-automation --directory ~/.qoder/skills -y

# 鸿蒙 App 开发
npx skills add majiayu000/spellbook@harmonyos-app --directory ~/.qoder/skills -y

# ArkTS 构建部署
npx skills add fadinglight9291117/arkts_skills@harmonyos-build-deploy --directory ~/.qoder/skills -y

# HarmonyOS NEXT
npx skills add linhay/harmony-next.skills@harmony-next --directory ~/.qoder/skills -y

# 鸿蒙开发规范 + 代码评审
npx skills add coreylyn/harmonyos-skills@harmonyos-dev --directory ~/.qoder/skills -y
npx skills add coreylyn/harmonyos-skills@harmonyos-review --directory ~/.qoder/skills -y

# Android 迁移鸿蒙
npx skills add openharmonyinsight/openharmony-skills@android-to-harmonyos-migration-workflow --directory ~/.qoder/skills -y
```

### 详情页

- https://skills.sh/web-infra-dev/midscene-skills/harmonyos-device-automation
- https://skills.sh/majiayu000/spellbook/harmonyos-app
- https://skills.sh/fadinglight9291117/arkts_skills/harmonyos-build-deploy
- https://skills.sh/linhay/harmony-next.skills/harmony-next
- https://skills.sh/coreylyn/harmonyos-skills/harmonyos-dev
- https://skills.sh/coreylyn/harmonyos-skills/harmonyos-review

---

## 四、Rust 可安装 Skill

| Skill 标识 | 安装量 | 用途 |
|---|---|---|
| `wshobson/agents@rust-async-patterns` | 17.3K | **Rust 异步编程模式（tokio/async），最贴合网络编程需求** |
| `apollographql/skills@rust-best-practices` | 16.1K | Rust 通用最佳实践，Apollo GraphQL 团队出品 |
| `github/awesome-copilot@rust-mcp-server-generator` | 9.1K | Rust MCP Server 生成 |
| `affaan-m/ecc@rust-testing` | 8.4K | Rust 测试实践 |
| `affaan-m/ecc@rust-patterns` | 8K | Rust 设计模式 |
| `actionbook/rust-skills@m15-anti-pattern` | 7.5K | Rust 反模式规避 |
| `jeffallan/claude-skills@rust-engineer` | 5K | Rust 工程师角色 skill |

### 安装命令

```bash
# Rust 异步/网络编程（推荐）
npx skills add wshobson/agents@rust-async-patterns --directory ~/.qoder/skills -y

# Rust 通用最佳实践（推荐）
npx skills add apollographql/skills@rust-best-practices --directory ~/.qoder/skills -y

# 其它
npx skills add affaan-m/ecc@rust-testing --directory ~/.qoder/skills -y
npx skills add affaan-m/ecc@rust-patterns --directory ~/.qoder/skills -y
npx skills add actionbook/rust-skills@m15-anti-pattern --directory ~/.qoder/skills -y
```

### 详情页

- https://skills.sh/wshobson/agents/rust-async-patterns
- https://skills.sh/apollographql/skills/rust-best-practices
- https://skills.sh/affaan-m/ecc/rust-testing
- https://skills.sh/affaan-m/ecc/rust-patterns
- https://skills.sh/actionbook/rust-skills/m15-anti-pattern

---

## 五、网络编程

**结论：skills.sh 生态中没有针对通用网络编程的专属 skill。**

- `npx skills find network programming` 的返回结果均为区块链/特定项目相关（axiom-networking、surfpool、vara-agent-network），与通用网络编程无关
- **替代覆盖方案**：Rust 网络编程的核心是 tokio/async 生态，`rust-async-patterns` skill 基本可覆盖（异步 IO、TCP/UDP、并发模型、错误处理等）
- 应用层实时通信（SSE/WebSocket）可参考本地已装的 `fullstack-dev` skill

---

## 六、非 Skill 形式的替代方案

1. **自建私有 skill**：使用 `create-skill` 将团队内部的鸿蒙/Rust 编码规范、调试手册沉淀为私有 skill，适合有内部规范的团队
2. **官方文档直接检索**：
   - 鸿蒙：HarmonyOS 开发者官网（ArkTS 编码规范、ArkUI 声明式开发、DevEco Studio 调试指南）
   - Rust 网络编程：tokio 官方教程（tokio.rs）、Rust 异步编程指南（rust-lang.github.io/async-book）
3. **组合用法**：安装社区 skill 作为通用基线 + 自建 skill 承载团队私有规范

---

## 七、选型建议

| 场景 | 推荐组合 |
|---|---|
| 鸿蒙 App 开发 + 调试 | `harmonyos-dev` + `harmonyos-review` + `harmonyos-device-automation` |
| Android 团队转鸿蒙 | `android-to-harmonyos-migration-workflow` + `harmony-next` |
| Rust 网络编程 | 本地 `rust-coding-guidelines` + `rust-async-patterns` + `rust-best-practices` |
| 团队有私有规范 | 上述组合 + `create-skill` 自建内部规范 skill |

---

## 八、Skills CLI 速查

```bash
npx skills find <关键词>          # 搜索 skill
npx skills add <owner/repo@skill> --directory ~/.qoder/skills -y   # 安装
npx skills check                  # 检查更新
npx skills update                 # 更新全部
```

浏览地址：https://skills.sh/
