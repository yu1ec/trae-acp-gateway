# Trae ACP Gateway

OpenAI 兼容的本地网关，通过 ACP 协议对接 Trae CLI agent。项目包含两个交付形态：

- **CLI**（`trae_acp_gateway`）：命令行守护进程，监听 `127.0.0.1` 提供 `/v1/chat/completions` 等接口
- **App**（`Trae ACP Gateway`）：基于 Tauri 2 的桌面托盘应用，内置网关并提供设置与日志界面

## 环境要求

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

# 构建 CLI
make build-cli

# 构建本机 App（Trae ACP Gateway 安装包）
make build-app

# 指定平台（需在对应操作系统上执行）
make build-app OS=macos ARCH=universal BUNDLES=app,dmg
make build-app OS=linux ARCH=arm64 BUNDLES=deb
make build-app OS=windows ARCH=x86_64 BUNDLES=msi

# 开发模式 / 运行 CLI
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

## 打包 CLI

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
| `--port` | `8080` | 监听端口（仅 localhost） |
| `--workdir` | `~/trae-acp-gateway` | Agent 工作目录（macOS/Linux: `$HOME/trae-acp-gateway`，Windows: `%USERPROFILE%\\trae-acp-gateway`；不存在时自动创建） |
| `--trae-cmd` | `traecli` | Agent 可执行文件 |
| `--trae-args` | `acp,serve` | 传给 Agent 的参数（逗号分隔） |
| `--sandbox` | `true` | 是否保持 Agent 沙箱限制 |
| `--debug` | — | 打印与 Agent 的 JSON-RPC 原始日志 |

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

### 单独构建前端

```bash
make build-frontend   # 输出到 app/ui/
```

### 常用构建选项

```bash
# macOS 通用二进制（需同时安装 aarch64 与 x86_64 target）
cargo tauri build --target universal-apple-darwin

# 指定安装包类型（macOS 可选 app、dmg）
cargo tauri build --bundles dmg

# 跳过代码签名（本地测试）
cargo tauri build --no-sign
```

## 项目结构

```
.
├── src/              # CLI 核心库与入口（trae_acp_gateway）
├── app/              # Tauri 桌面应用
│   ├── src/          # App Rust 代码
│   ├── frontend/     # React 源码（Vite）
│   ├── ui/           # 前端构建产物（gitignore）
│   └── tauri.conf.json
├── run.sh            # CLI 快速启动脚本
├── Makefile          # 统一构建入口
└── Cargo.toml        # Workspace 配置
```
