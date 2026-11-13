## BPS 原生插件 v0.1.0

纯 Rust 实现默认 BPS WebSocket 生成链路，不依赖 Go 或本地 HTTP 桥接服务。

- 独立设备授权、PKCE、OAuth 导入、状态检查和退出登录
- Responses 文本、完整历史、HTTPS 图片输入
- function/custom/namespace 工具、原生 update_plan 与隔离历史回放
- 结构化输出校验、流式/非流式响应、Token 用量和连接生命周期管理

### 安装

下载与 **Codex Proxy RS 宿主** 匹配的安装包及 `.sha256`：

- Linux x86_64：`local.bps-0.1.0-x86_64-unknown-linux-gnu.tar.gz`
- Linux ARM64：`local.bps-0.1.0-aarch64-unknown-linux-gnu.tar.gz`

在宿主插件管理中上传安装包，或使用 GitHub 来源 `LanQin996/cpr-plugin-bps` 安装。GitHub 自动生成的 Source code 归档不是插件安装包。

声明兼容宿主 `>=3.33.0, <4.0.0`，需要清单 v2、RPC v7 和 upstream_adapter v5。配置与登录命令见仓库 README。

### 当前边界

- 自动测试使用模拟宿主和 BPS 数据，没有进行真实账号生成验收
- 不自动刷新 OAuth；AT 到期后需重新授权或导入
- 不支持 previous_response_id，需要完整历史
- legacy SSE、实验性 ARC 和图像中继托管未包含在此版本
- 本次仅分发两个 Linux 平台，不提供 Windows 或 macOS 安装附件
