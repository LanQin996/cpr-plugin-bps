# CPR BPS 原生插件

将 `bps-local-public` 的默认 BPS WebSocket 生成链路移植到 Rust 插件，直接连接 BPS。**不依赖 Go、`bps-local`、本地 HTTP 服务或其他桥接进程。**

```text
Responses 客户端 → Codex Proxy RS → 本插件 → BPS WebSocket
                      │              │
                  Key / ACL / 租约   请求编码、工具转换、OAuth
                  计量 / 代理        事件校验、历史回放
```

## 实现范围

- 原生 BPS `response.create`、Excel 客户端 profile、认证子协议、请求 ID 和 resume token
- 独立 BPS 设备授权、PKCE 验证、OAuth JSON 导入、有效期检查和退出登录
- 多账号：每个显式宿主账号对应一份 BPS OAuth；不接管其他 OpenAI 账号
- Responses 文本、完整历史、加密推理状态、配置更新、HTTPS 图片输入
- function/custom/namespace 工具目录、`run_officejs` 信封、原生 `update_plan` 适配
- 工具调用只交回客户端，不执行 OfficeJS、Shell 或任何工具代码
- 工具终态校验、按 Key/账号/凭据版本/会话隔离的有界原生回放缓存
- `json_object` / `json_schema`：先缓冲最终文本，再校验和重建事件；不是上游 constrained decoding
- 流式 SSE 与 `stream:false` 最终 JSON、用量/缓存/推理 Token、唯一终态与连接回收
- BPS 控制帧、私有 cursor 过滤；握手拒绝、流内失败、断连与取消不自动重放

当前版本对齐原项目默认 **WS** 路径，不包含独立 CLI 的本地 HTTP 服务器、启动脚本、legacy SSE transport、实验性 ARC 初始化、图像中继托管或协议诊断命令。没有管理页面；配置使用宿主配置表单，登录使用插件 CLI。

## 兼容要求

- 宿主声明范围：`>=3.33.0, <4.0.0`；还需支持清单 v2、RPC v7、`upstream_adapter` v5、`model_catalog` v2
- SDK 和打包 CLI 固定于 `codex-proxy-rs` 提交 `9b4a042be5c96e66e635d5af76047e9e3cbee462`
- 安装包平台：Linux x86_64、Linux aarch64、macOS aarch64，以包内 target 为准
- Windows 可编译和测试源码，但当前官方宿主打包合同不支持 Windows 安装包

Issue #278 发布时的 `v3.14.0` 不等于当前插件合同。本插件没有宣称兼容早期 RPC。

## 安装和配置

安装包从本仓库的 [GitHub Releases](https://github.com/LanQin996/cpr-plugin-bps/releases/latest) 下载，不是 Actions 附件或 GitHub 自动生成的 Source code。也可在宿主中选择 GitHub 来源 `LanQin996/cpr-plugin-bps` 安装。

1. 在宿主创建专用 **OpenAI URL + Key** 占位账号，协议选择 Responses、HTTP transport。它只用于宿主的选号、ACL 和租约，不用于 BPS 认证。不要复用普通 OpenAI 生产账号。
2. 占位账号可以使用固定目标 `https://bps.openai.com/basispoints/api/` 和非真实的占位 Key。不要对占位账号执行原生连通性测试或模型自动探测。
3. 安装与宿主运行平台匹配的 `.tar.gz`，接受插件完整信任。源码目录和 `.exe` 不是安装包。
4. 参考 `config.example.json`，填写实际账号 ID 和该 BPS 账号有权使用的模型，保存并启用：

```json
{
  "accounts": [
    { "accountId": "YOUR_HOST_ACCOUNT_ID", "models": ["YOUR_AUTHORIZED_MODEL_ID"] }
  ]
}
```

5. 保持默认的 `upstream` / `reject` 绑定，通过宿主账号组和 Key 权限控制谁可使用这些账号。适配模型目录不支持带 Key/账号组条件的适配器绑定；不要用这类绑定代替正常账号 ACL。
6. 按下一节完成独立 BPS 登录，再通过宿主发起请求。

最多配置 16 个账号，每个最多 16 个模型；序列化模型目录还须不超过 64 KiB。不会自动探测、替换或降级模型。

**维护顺序：先停用 BPS 占位账号并排空请求，再停用或卸载插件。** 插件停用后，宿主不会把占位账号永久标记为“只能由插件执行”。

### 代理

生成请求通过 `host.upstream.websocket.*`，使用所选宿主账号的代理和部署默认出口，代理失败不回退直连。设备授权通过宿主受管 HTTP，使用部署默认出口。两者可能不同，需分别确保可访问对应站点。

## 原生登录

将下面的 `INSTANCE_ID` 替换为宿主实际插件实例 ID。多个账号时加 `--account YOUR_HOST_ACCOUNT_ID`，只有一个时可省略。

```bash
codex-proxy-rs plugin INSTANCE_ID login-start --account YOUR_HOST_ACCOUNT_ID
```

打开输出的授权 URL，输入设备码并在浏览器批准。按输出的轮询间隔执行：

```bash
codex-proxy-rs plugin INSTANCE_ID login-poll --account YOUR_HOST_ACCOUNT_ID
codex-proxy-rs plugin INSTANCE_ID status --account YOUR_HOST_ACCOUNT_ID
```

`login-poll` 每次查询一次，待批准时可再次执行；授权流程 15 分钟有效。不输出 Access Token 或 Refresh Token，也不读取浏览器凭据、Codex 的 `auth.json` 或系统凭据库。

也可以通过 `import-oauth --oauth '<JSON>'` 导入已授权的独立 BPS OAuth 文档。参数是 **JSON 内容，不是文件路径**；不要将真实文档写进脚本、Git 或共享日志，优先使用设备授权以避免 Shell 历史暴露。支持顶层 `access_token` 或 `tokens.access_token`，不能同时出现；不接受 API Key 代替 OAuth。

```bash
codex-proxy-rs plugin INSTANCE_ID logout --account YOUR_HOST_ACCOUNT_ID
```

OAuth 保存在插件私有 `oauth` 状态命名空间，按宿主账号隔离并使用版本 CAS。宿主备份也可能包含这些敏感数据，应按凭据保护。和参考项目一样，**不自动使用 Refresh Token 刷新**；AT 到期后需重新授权或导入。`status` 只检查本地元数据，不证明 BPS 生成权限。

## 请求示例

客户端调用宿主，不直接访问 BPS：

```bash
curl -N "$CPR_BASE_URL/v1/responses" \
  -H "Authorization: Bearer $CPR_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"model":"YOUR_AUTHORIZED_MODEL_ID","input":"只回复 OK","stream":true,"reasoning":{"effort":"medium"}}'
```

不支持 `previous_response_id` / `item_reference`；每轮提交完整历史。进程内回放缓存上限为 1024 条 / 16 MiB，重启后可以从完整的客户端工具调用和结果重建传输历史。回放绑定 Key、账号、宿主凭据版本、BPS OAuth 版本与 Token 摘要。会话使用 `session_id`、`prompt_cache_key` 和首项历史共同划分；没有稳定会话标识且首项完全相同的请求无法区分为两个会话，调用方应提供稳定标识。

其他限制：

- BPS 内部接口及模型权限可能变化；OAuth 成功、WS 握手 101、HTTP 200 都不是生成成功，必须检查终态
- `tool_choice` 仅支持 `auto` / `none`；已知 hosted tools 会被省略并在协议提示中说明，不会伪装执行
- 图片仅支持无嵌入凭据的 HTTPS URL；不支持 data/base64、文件 ID、原始图片托管
- reasoning 支持 low/medium/high/xhigh；minimal/none 降为 low，max/ultra 等归为 xhigh，与参考项目一致
- BPS 上游不接受的客户端字段按参考项目白名单处理；不承诺 temperature、service_tier 等字段生效
- 不支持 background、WebSocket 预热、BPS 自动重连或断点续传；每次生成建立独立连接
- 单个上游事件不超过约 16 MiB，编码后的事件还受宿主 RPC 分块和信用窗口限制；默认 SDK 单块预算 1 MiB
- 用量只提交上游已知数值，未知保持未知；费用由宿主价格配置计算，不代表 BPS 官方账单

## 开发与打包

使用 Rust **1.99.1**，依赖由 `Cargo.lock` 固定，无 Node 或 Go 构建步骤。

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
```

使用 Python 3.11+ 和同提交的官方 CLI 打包，不手写包摘要或平台元数据：

```bash
rustup target add x86_64-unknown-linux-gnu
python scripts/package.py --target x86_64-unknown-linux-gnu --install-cli
```

上述普通构建需要对应平台的链接工具。在 Windows 上交叉构建 Linux，另准备 `cargo-zigbuild` 和 Zig（也可使用 Python `ziglang` 包）：

```bash
cargo install cargo-zigbuild --locked
python -m pip install ziglang
python scripts/package.py --target x86_64-unknown-linux-gnu --zig --install-cli
```

`--zig` 使用 glibc 2.28 基线；输出在 `dist/`，包括 `.tar.gz` 和 `.sha256`。脚本不会安装插件到宿主，也不会修改宿主账号、启用配置或发布 Release。CI 配置提供三个平台的原生构建任务；本地交叉编译不等于已在对应系统运行验证。

构建后可运行 `python scripts/verify_packages.py`，核对归档摘要、包内资源、二进制架构及其与当前默认 `target/` 构建产物的一致性。

### 发布到 GitHub

普通 main / PR 构建只做检查和上传 Actions 附件，不创建 Release。推送与 `Cargo.toml`、`plugin.json` 版本一致的 `v<version>` 标签，才会触发 `.github/workflows/release.yml`：

1. 校验标签与源码版本，分别检查、测试和构建两个 Linux 平台
2. 校验两个 `.tar.gz`、两个 `.sha256` 和包内架构、摘要
3. 创建草稿并上传完整附件，全部成功后公开 Release

无预发行后缀的标签发布为正式版并设置 Latest；含后缀的标签发布为预发行版。手动重跑时必须选择已有版本标签，不能选择 main。已公开版本不会被覆盖；只有未公开的草稿允许补传附件。发布说明位于 `release/notes.md`。

## 源码入口

| 路径 | 职责 |
| --- | --- |
| `plugin.json` / `src/lib.rs` | 能力、私有状态和 `PluginBuilder` 注册 |
| `src/auth.rs` | 设备授权、PKCE、导入、有效期和状态 CAS |
| `src/adapter.rs` | BPS WebSocket、认证头、控制信息和连接生命周期 |
| `src/protocol/` | BPS 请求、工具信封、历史、结构化输出和终态转换 |
| `src/events.rs` | Responses wire、标准事实、用量和失败 |
| `tests/` | 纯协议与真实插件子进程的模拟宿主测试 |
| `scripts/package.py` | 编译和官方 CLI 打包 |

移植来源及原有许可说明见 `NOTICE` 和 `licenses/bps-local.txt`。参考源码保存在本地忽略目录 `.references/`，不参与构建或运行。

测试中的宿主和 BPS 数据均为模拟夹具，不使用真实账号。真实 BPS 授权、模型生成、目标宿主安装和费用验证仍需在实际部署中验收。
