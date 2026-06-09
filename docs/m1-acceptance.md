# M1 验收（macOS arm64）

## 路径一：全新机器（无 tp-agent）

1. 确认无 tp-agent：`which tp-agent` 应无输出
2. 启动客户端，调用 `agent_install` → 返回 `/usr/local/bin/tp-agent`
3. `tp-agent --version` 可执行（codesign ad-hoc 重签生效，未被 Gatekeeper 杀）
4. 调用完整 OAuth 登录 `agent_login_browser`（WebView 内授权）→ `AgentStatus.enrolled == true`
5. 云端 edge nodes 出现本机 → 可派工

## 路径二：已装 tp-agent

1. 预置：已 `tp-agent login` 过、daemon 在跑
2. 启动客户端，调用 `agent_status` → 直接 `running:true, enrolled:true`（复用，不重装）
3. 若客户端登录账号 Y ≠ 当前 active X：登录后 `active_tenant_id` 切到 Y（self-enroll 按 (tenant,machine_id) 幂等复用 edge_node_id，不起第二个 daemon）
4. 全程未起第二个 daemon（`pgrep -fl tp-agent` 只一个 run-daemon 进程）

## 实现说明

- **C5 登录**：计划原定先用 `--key` 兜底路线，实际实现了完整 WebView OAuth 闭环 `agent_login_browser`（spawn `tp-agent login --force --suppress-browser --print-auth-url`，从 stdout 流式拿 `TP_AUTH_URL=`，在新 WebView 窗口打开授权页，tp-agent 自己的 loopback callback 完成授权+self-enroll）。`agent_login(api_key)` 作为开发态/CI 兜底保留。
- **额外完成（计划外收尾）**：窗口尺寸按屏幕 80% 自适应+居中、macOS Overlay 标题栏避让（Sidebar 等贴角组件 padding-top）、后端地址壳注入（env > `~/.config/talon-pilot-studio/config.toml` > 默认 `https://agents.deeplan.ai`）、客户端请求头 `x-talon-client` / `-version` / `-os`、后端 CORS 放行。
