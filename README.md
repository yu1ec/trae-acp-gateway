# Trae ACP Gateway

> **重要说明（2026-10-22）**：CLI 形态（`trae_acp_gateway` 可执行文件）已停止后续开发，CI/CD 不再构建或发布 CLI 二进制。桌面 App 是唯一继续维护和发布的交付形态；CLI 源码与本地构建能力暂时保留，但不再接收新功能，也不再发布更新。现有 CLI 用户请迁移到 App。

OpenAI / Anthropic / Gemini 多协议兼容的本地网关，通过 ACP 协议对接 Trae CLI agent。项目包含两个交付形态：

- **App**（`Trae ACP Gateway`，推荐并持续维护）：基于 Tauri 2 的桌面托盘应用，内置网关并提供设置与日志界面
- **CLI**（`trae_acp_gateway`，已停止维护）：命令行守护进程，监听 `127.0.0.1` 提供多种 LLM HTTP 表面（见下）；仅保留源码与本地构建说明

## 环境要求

- **TRAE CLI**（`traecli`）：网关的实际 agent，需登录 TRAE 账号。启动前会自动检测；未安装时命令行形态会提示是否运行官方安装脚本，桌面 App 会弹窗询问。
  - macOS / Linux：`sh -c "$(curl -fsSL https://trae.cn/trae-cli/install_v2.sh)"`
  - Windows：`irm https://trae.cn/trae-cli/install_v2.ps1 | iex`
  - 文档：<https://docs.trae.cn/cli_about-trae-code-cli-2>
- [Rust](https://rustup.rs/) 工具链（建议 stable）
- **打包 App 额外需要**：
  - [Tauri CLI](https://v2.tauri.app/start/prerequisites/)：`cargo install tauri-cli --locked`
  - macOS：Xcode Command Line Tools
  - Windows：WebView2、Visual Studio C++ Build Tools
  - Linux：webkit2gtk 等 Tauri 依赖（见官方文档）

## 使用 Makefile

项目根目录提供统一构建入口，执行 `make help` 查看全部目标。

```bash
# 首次：拉取依赖、安装 tauri-cli、添加 rustup target
make init

# 构建 CLI（已停止维护，仅用于本地源码构建）
make build-cli

# 构建本机 App（Trae ACP Gateway 安装包）
make build-app

# 指定平台（需在对应操作系统上执行）
make build-app OS=macos ARCH=universal BUNDLES=app,dmg
make build-app OS=linux ARCH=arm64 BUNDLES=deb
make build-app OS=windows ARCH=x86_64 BUNDLES=msi

# 开发模式 / 运行 CLI（CLI 已停止维护）
make dev-app
make run-cli
```

常用变量：

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `OS` | 自动检测 | `macos` / `linux` / `windows` |
| `ARCH` | 自动检测 | `arm64` / `x86_64` / `universal`（仅 macOS） |
| `BUNDLES` | 按 OS 默认 | 如 `app,dmg`、`deb,appimage`、`msi,nsis` |
| `SIGN` | `0` | `1` 启用代码签名，本地测试保持 `0` |

> Tauri App 需在目标操作系统上构建（macOS 上可构建 `universal` 通用二进制）。

## HTTP API（多协议）

网关绑定 `127.0.0.1`，**不校验** `Authorization` / API Key。每个 HTTP 请求会 spawn 一次 Trae agent、执行一轮 ACP `session/prompt`，然后结束进程。

| 协议 | 方法 | 路径 |
|------|------|------|
| OpenAI Chat | POST | `/v1/chat/completions` |
| OpenAI Models | GET | `/v1/models` |
| OpenAI Responses | POST | `/v1/responses` |
| OpenAI Responses 查询 | GET | `/v1/responses/{id}` |
| Anthropic Messages | POST | `/v1/messages` |
| Gemini generateContent | POST | `/v1beta/models/{model}:generateContent` |
| Gemini 流式 | POST | `/v1beta/models/{model}:streamGenerateContent` |
| Gemini Models | GET | `/v1beta/models` |

流式：`stream: true`（OpenAI Chat / Responses）、Anthropic `stream: true`、Gemini 使用 `streamGenerateContent` 端点（SSE `data:` JSON）。

### 能力边界（Agent 网关，非原始 LLM API）

- 请求里的 **`model` 不会切换 Trae 内部模型**（与原先 Chat 行为一致）；模型列表来自 agent 的 `configOptions`。
- 客户端 **`tools` / `functions` / `tool_choice` 等会被忽略**；响应中**不会**伪造 `tool_calls` / `tool_use` / `functionCall`。Trae 在 agent 内部自行跑工具（权限由 `--sandbox` 控制），最终只返回助手文本（流式时可带 `reasoning_content` / thinking）。
- 不支持：`n>1`、logprobs、audio、Batch、Files、Embeddings、Vertex AI 项目路径、`count_tokens` 等。
- **OpenAI Responses** 的 `previous_response_id`：网关用**内存**保存最近响应的 transcript 以续写；进程重启后丢失，且不等同于 OpenAI 官方的加密状态存储。
- 文本与 **base64 图片**（Anthropic image source、OpenAI `input_image` data URL、Gemini `inlineData`）会转为 ACP prompt 块；其它 part 类型忽略。

## 打包 CLI（已停止维护）

> CLI 已停止后续开发，Release 不再提供预编译二进制。以下内容仅用于从源码本地构建；功能不再更新。

在项目根目录执行：

```bash
cargo build --release
```

产物路径：

```
target/release/trae_acp_gateway
```

可选：使用 `strip = true` 的 release profile 已启用，二进制体积更小。

### 运行示例

```bash
# 直接运行
./target/release/trae_acp_gateway

# 或使用便捷脚本（若未编译会先构建）
./run.sh

# 自定义参数
./target/release/trae_acp_gateway --port 9000 --workdir /path/to/project
```

常用参数：

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--port` | `60111` | 监听端口（仅 localhost）；被占用时启动直接失败并报错，不会自动切换端口 |
| `--workdir` | 网关进程当前目录 | 客户端未上报 workdir 时使用的 Agent 工作目录（未配置时使用网关自己的 cwd） |
| `--allowed-workdir-root` | `$HOME` | 允许客户端上报的 workdir 根目录；上报路径规范化后必须位于其下 |
| `--trae-cmd` | `traecli` | Agent 可执行文件 |
| `--trae-args` | `acp,serve` | 传给 Agent 的参数（逗号分隔） |
| `--sandbox` | `true` | 是否保持 Agent 沙箱限制 |
| `--debug` | — | 打印与 Agent 的 JSON-RPC 原始日志 |
| `--auto-check-update` | — | 启动 gateway 时在后台检查 GitHub Releases 更新 |
| `--no-auto-check-update` | — | 禁用自动检查（覆盖 settings 文件） |

### 动态 workdir

每个请求都会单独解析 workdir，优先级从高到低：

1. 请求头 `X-Trae-Workdir`（Claude Code 可用 `ANTHROPIC_CUSTOM_HEADERS` 发送）
2. 请求体字段：OpenAI `workdir`、Responses `workdir` / `metadata.workdir`
3. Claude Code 注入的 `# Environment` 块中的 `- Primary working directory: ...`
4. system prompt 中的 `Working directory: ...`
5. `--workdir` 配置值；未配置时使用网关进程的当前目录

上报路径必须是已存在的绝对目录，且规范化（解析符号链接）后位于 `--allowed-workdir-root` 下；
不满足时显式来源返回 400，自动解析来源则回退到 `--workdir`（未配置时为网关进程当前目录）。解析结果及来源会以 `debug` 级别记录。

```jsonc
// <project>/.claude/settings.json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:60111",
    "ANTHROPIC_CUSTOM_HEADERS": "X-Trae-Workdir: /Users/me/project"
  }
}
```

### 检查与自更新（CLI，已停止维护）

> Release 已不再发布 CLI 二进制，因此 CLI 自更新无法获取新版本。App 的自动更新不受影响。

CLI 可从 GitHub Releases 检查并替换自身二进制：

```bash
# 检查是否有新版本（有更新时 exit code 为 1，便于脚本检测）
./target/release/trae_acp_gateway update check

# 下载并替换当前二进制
./target/release/trae_acp_gateway update apply
./target/release/trae_acp_gateway update apply --yes
```

持久化设置（是否启动时自动检查）保存在：

| 平台 | 路径 |
|------|------|
| macOS / Linux | `~/.config/trae-acp-gateway/settings.json` |
| Windows | `%APPDATA%\trae-acp-gateway\settings.json` |

示例：

```json
{
  "auto_check_update": false
}
```

启动 gateway 时若启用自动检查，会发送系统通知（macOS / Linux / Windows）并在终端提示；不会自动下载。

更新源：[yu1ec/trae-acp-gateway](https://github.com/yu1ec/trae-acp-gateway) GitHub Releases。

## 打包 App

App 源码位于 `app/` 目录，使用 Tauri 2 打包。需在 `app/` 下执行命令（该目录包含 `tauri.conf.json`）。

### 安装 Tauri CLI（首次）

```bash
cargo install tauri-cli --locked
```

### 发布构建（含安装包）

```bash
cd app
cargo tauri build
```

产物路径（macOS 示例）：

```
target/release/bundle/macos/Trae ACP Gateway.app   # 应用包
target/release/bundle/dmg/Trae ACP Gateway_0.1.0_<arch>.dmg  # DMG 安装镜像
```

> 注意：Tauri 使用 workspace 根目录的 `target/`，因此产物在**项目根目录**的 `target/release/bundle/` 下，而非 `app/target/`。

### 仅编译二进制（不生成安装包）

```bash
cd app
cargo tauri build --no-bundle
```

产物：

```
target/release/trae-acp-gateway-app
```

### 开发模式（热重载）

前端使用 Vite + React（开发端口 **3847**）。首次需安装 npm 依赖：

```bash
make init          # 或 cd app/frontend && npm install
make dev-app       # 启动 Tauri + Vite 联调
```

也可手动：

```bash
cd app
cargo tauri dev
```

`cargo tauri dev` 会自动运行 `npm run dev`（Vite @ localhost:3847）并热重载 Rust 侧。

### 自动更新

App 设置页提供：

- **Check for updates on startup**：启动时在后台检查 GitHub Releases，发现新版本会发送系统通知
- **Check now**：手动检查，发现新版本时同样发送系统通知
- **安装 vX.Y.Z 并重启**：下载 updater 产物并校验 minisign 签名，然后原地替换应用、自动重启；不需要手动下载安装包或拖拽 DMG
- **加速下载**：优先走 `ghfast.top` / `gh-proxy.com` 镜像，下载失败会自动回退到 GitHub 直连

> 从下一个版本起，Release 会附带 `latest.json` 和更新包签名。已发布的 0.10.0 不支持应用内更新，首次升级仍需手动安装一次；之后即可在应用内完成更新。

### 系统权限（macOS）

网关本身只需要读写自己的工作目录和配置。下面这些权限都是**可选**的，只有 Agent 需要访问受保护目录时才需要：

| 权限 | 什么时候需要 | 设置位置 |
|------|--------------|----------|
| 完全磁盘访问权限 | 项目位于「桌面」「文稿」「下载」、外接磁盘或其它受 TCC 保护的目录 | 系统设置 → 隐私与安全性 → 完全磁盘访问权限 |
| 文件与文件夹 | 只想授权某一个受保护目录，而不是全盘 | 系统设置 → 隐私与安全性 → 文件与文件夹 |
| 通知 | 启动时检查到新版本、Gateway 异常退出时发送通知 | 系统设置 → 通知 |
| 媒体与 Apple 音乐 | **不需要**。Agent 扫描受保护目录时 macOS 可能误弹，点「不允许」即可 | 设置页可「清除授权记录」 |

设置页的 **System permissions** 面板会显示当前完全磁盘访问状态，并提供「打开设置」「在 Finder 中显示」「重新检测」「清除媒体库授权记录」按钮。首次启动如果检测到尚未授权，会弹一次提示，之后不再打扰。

> TCC 授权按代码签名的 designated requirement 记录。发布包使用固定自签名证书 `Trae ACP Gateway Signing`，requirement 只包含 bundle id 和证书指纹，因此升级后已授予的权限不会重置；ad-hoc 签名会因为 cdhash 每次变化而让用户重新授权。

App 配置保存在 `{app_data_dir}/config.json`，字段 `auto_check_update`（默认 `false`）。

App 固定使用 `traecli acp serve` 启动 agent，不再提供 Trae command / Trae args 输入框。设置页只显示是否检测到 `traecli`：未检测到时可点击按钮运行官方安装脚本，安装完成后状态会自动刷新；安装失败时会给出官网安装说明链接，可手动安装。

App 不单独提供 Sandbox 开关：沙箱与审批策略由 `traecli` 自身的权限模式决定，网关只负责拒绝 agent 越出沙箱的权限请求。如需使用 `--permission-mode` 等参数调整权限模式，请改用命令行形态的 `--trae-args`：

- `--trae-args=acp,serve`（默认）：Workspace Edit / Auto-Review 等 traecli 自身默认模式
- `--trae-args=--permission-mode,auto,acp,serve`：Auto-Review 模式，仍在沙箱内，关键操作由模型自动评审
- `--trae-args=--permission-mode,bypass_permissions,acp,serve` 或 `--trae-args=acp,serve,-y`：Full Access，关闭沙箱和审批，风险最高

### 思考面板

设置页勾选 **启用思考面板** 并保存后，可从设置页的「打开思考面板」或托盘菜单的 **Open Thoughts** 打开一个置顶悬浮面板。面板实时滚动显示 ACP `agent_thought_chunk` 思考流，支持面板内调整透明度，以及通过图钉按钮切换是否置顶。

把面板拖到任意显示器的左边缘或右边缘即自动吸附：鼠标移开后只留一条可悬停的细条，悬停细条会重新滑出。从吸附状态拖动面板会自动解除吸附，拖回左/右边缘则重新吸附，因此不需要额外的开关按钮。

多个客户端会话并发时，同一会话的思考使用相同颜色和短标签，交错输出也能一眼看出归属。Claude Code 会自动通过 `X-Claude-Code-Session-Id` 上报会话 ID；其他客户端可使用 `X-Session-Id` / `X-Conversation-Id`，或 Anthropic 请求体的 `metadata.session_id`。未上报会话 ID 的请求标记为「未标记」。

思考流通过独立 side channel 展示，不会伪造成 Anthropic thinking 块写回响应，因此不会影响 Claude Code 的工具执行；即使客户端没有开启 thinking，面板也能看到 agent 的真实思考输出。

### 单独构建前端

```bash
make build-frontend   # 输出到 app/ui/
```

### 常用构建选项

```bash
# macOS 通用二进制（先安装两个真实 target，universal-apple-darwin 仅用于 Tauri）
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo tauri build --target universal-apple-darwin

# 指定安装包类型（macOS 可选 app、dmg）
cargo tauri build --bundles dmg

# 跳过代码签名（本地测试）
cargo tauri build --no-sign
```

## 发布到 GitHub Releases

推送 `v*` 格式的 tag 时，GitHub Actions 会自动在三端构建 App 安装包并发布到 Releases。CLI 已停止发布，CI 不再编译 CLI 二进制。

### 发布步骤

1. 按变更类型自动递增版本号（推荐）：

```bash
# 修复 bug:   0.1.2 -> 0.1.3
make release TYPE=patch

# 新功能:     0.1.2 -> 0.2.0
make release TYPE=minor

# 破坏性变更: 0.1.2 -> 1.0.0
make release TYPE=major

# 仅预览，不改文件
make release TYPE=patch DRY_RUN=1
```

也支持别名：`fix` / `bug` / `feature` / `feat` / `breaking`，以及中文 `修复` / `功能` / `大版本`。

命令会更新所有版本文件，并输出你需要手动执行的 `git commit` / `git tag` / `git push` 命令。

如需指定确切版本，仍可使用：

```bash
make bump-version VERSION=0.1.2
make check-version TAG=0.1.2   # 发布前本地校验，与 CI 一致
```

2. 按脚本输出的命令提交并推送 tag（示例）：

```bash
git add -A
git commit -m "chore: release v0.1.3"
git tag v0.1.3
git push origin HEAD --tags
```

3. 在 GitHub **Actions** 页查看构建进度，完成后在 **Releases** 页下载产物

### 发布前准备：签名密钥（只需一次）

macOS 代码签名和自动更新各需要一套密钥。密钥只生成一次，之后每版复用；**丢失后无法补发能被旧版本验证的更新**。

```bash
# 1. macOS 固定自签名证书（生成 .signing/cert.p12 等文件）
bash scripts/macos-signing-cert.sh

# 2. Tauri updater minisign 密钥对（公钥会自动写入 app/tauri.conf.json）
bash scripts/updater-key.sh

# 3. 写入 GitHub Secrets
gh secret set MACOS_CERT_P12 < .signing/cert.p12.b64
gh secret set MACOS_CERT_PASSWORD < .signing/p12-password.txt
gh secret set MACOS_SIGNING_IDENTITY --body "Trae ACP Gateway Signing"
gh secret set TAURI_SIGNING_PRIVATE_KEY < .signing/updater.key
gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD < .signing/updater-password.txt
```

`.signing/` 已加入 `.gitignore`。请把整个目录离线备份（U 盘或密码管理器），不要提交到仓库。

`app/tauri.conf.json` 已开启 `createUpdaterArtifacts`，所以 `make build-app` 会自动读取 `.signing/updater.key` 为更新包签名；找不到密钥时会直接报错并提示先运行 `make updater-key`。

本地签名构建：

```bash
bash scripts/macos-signing-cert.sh --import   # 导入 login keychain
APPLE_SIGNING_IDENTITY="Trae ACP Gateway Signing" make build-app SIGN=1
```

### 发布产物

| 平台 | App（Tauri 默认 bundle） | App（稳定名，供手动下载） |
|------|--------------------------|---------------------------|
| macOS | `.app` + DMG（Universal） | `Trae-ACP-Gateway-macos-universal.dmg` |
| Linux | DEB + AppImage | `Trae-ACP-Gateway-linux-x86_64.AppImage` |
| Windows | MSI + NSIS | `Trae-ACP-Gateway-windows-x86_64.msi` |

Release 还会附带 `latest.json` 以及各平台的 updater 产物（`.app.tar.gz`、`.nsis.zip`、`.AppImage.tar.gz`）和 `.sig` 签名，供应用内自动更新使用。

> 仓库需在 **Settings → Actions → General → Workflow permissions** 中启用 **Read and write permissions**，否则无法创建 Release。
>
> GitHub API 未认证时限流 60 次/小时；可设置环境变量 `GITHUB_TOKEN` 提高限额。

## 安装发布包

### macOS

发布包使用**固定自签名证书**（`Trae ACP Gateway Signing`）签名，未经过 Apple 公证。首次打开时 Gatekeeper 会拦截，按下面任一方式放行即可（只需一次）：

- 到「系统设置 → 隐私与安全性」，在页面底部点击「仍要打开」
- 或在终端执行：

```bash
xattr -dr com.apple.quarantine "/Applications/Trae ACP Gateway.app"
open "/Applications/Trae ACP Gateway.app"
```

如果提示「已损坏，无法打开」，同样用上面的 `xattr` 命令清除隔离属性后再打开。

> 签名身份跨版本固定，升级后「完全磁盘访问权限」等 TCC 授权不会重置。首次打开后建议把 App 拖进 `/Applications`，从 App Translocation 路径运行会导致授权和就地更新失效。

> 首次运行时如果弹出「“Trae ACP Gateway” 想访问 Apple Music、你的音乐与视频活动以及你的媒体资料库」，**请点击「不允许」**。网关本身不需要媒体库权限，这个弹窗是沙箱内 agent 执行的 `find` 等搜索命令扫到受保护目录时由 macOS 触发的。清除已保存的授权可执行 `tccutil reset MediaLibrary com.trae.acp-gateway`。

### Linux

```bash
# AppImage
chmod +x Trae-ACP-Gateway-linux-x86_64.AppImage
./Trae-ACP-Gateway-linux-x86_64.AppImage

# 或 DEB
sudo apt install ./Trae-ACP-Gateway_*_amd64.deb
```

### Windows

MSI 可直接安装。若 SmartScreen 提示「已保护你的电脑」，点击「更多信息 → 仍要运行」。

### CLI（已停止发布）

CLI 二进制不再随 Release 发布。如仍需使用，请参考「打包 CLI（已停止维护）」从源码本地构建；建议改用桌面 App。

## 项目结构

```
.
├── src/              # 网关核心库（App 复用）与 CLI 入口（CLI 已停止维护）
├── app/              # Tauri 桌面应用
│   ├── src/          # App Rust 代码
│   ├── frontend/     # React 源码（Vite）
│   ├── ui/           # 前端构建产物（gitignore）
│   └── tauri.conf.json
├── .github/workflows/  # CI / Release 工作流
├── run.sh            # CLI 快速启动脚本
├── Makefile          # 统一构建入口
└── Cargo.toml        # Workspace 配置
```
