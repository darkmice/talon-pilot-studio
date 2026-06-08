mod agent;

/// 查询本机 tp-agent 状态（spawn `tp-agent status --json`）。
#[tauri::command]
fn agent_status() -> Result<agent::AgentStatus, String> {
    agent::fetch_status()
}

/// 帮装 tp-agent（macOS arm64，从 GitHub release）。返回安装路径。
#[tauri::command]
fn agent_install() -> Result<String, String> {
    agent::install_tp_agent().map(|p| p.display().to_string())
}

/// 用 api_key 完成 tp-agent 登录 + self-enroll，返回登录后状态。
/// (开发态兜底 / CI；正常 UI 走 `agent_login_browser` 完整 OAuth。)
#[tauri::command]
fn agent_login(api_key: String) -> Result<agent::AgentStatus, String> {
    agent::login_with_key(&api_key)
}

/// WebView 完整 OAuth 登录闭环：spawn `tp-agent login --suppress-browser --print-auth-url`，
/// 流式拿到授权 URL 后在新 WebView 窗口打开，tp-agent 自己的 loopback callback 完成授权 +
/// self-enroll，进程退出后返回 enrolled 状态。无需用户手抄 api_key。
#[tauri::command]
async fn agent_login_browser(
    app: tauri::AppHandle,
    api_base_url: Option<String>,
    web_base_url: Option<String>,
) -> Result<agent::AgentStatus, String> {
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

    // url 从 blocking 线程(流式读 tp-agent stdout)回到 async runtime,在这里开窗。
    let (url_tx, url_rx) = tokio::sync::oneshot::channel::<String>();

    // login_with_browser 阻塞(流式读 + wait 子进程,可能数分钟),放 blocking 线程。
    let join = tauri::async_runtime::spawn_blocking(move || {
        let mut url_tx = Some(url_tx);
        agent::login_with_browser(api_base_url.as_deref(), web_base_url.as_deref(), |url| {
            if let Some(tx) = url_tx.take() {
                let _ = tx.send(url);
            }
        })
    });

    // 等授权 URL 就绪 → 开 WebView 窗口加载它(tp-agent loopback callback 会完成闭环)。
    match url_rx.await {
        Ok(url) => {
            let parsed = url
                .parse::<tauri::Url>()
                .map_err(|e| format!("invalid auth url from tp-agent: {e}"))?;
            WebviewWindowBuilder::new(&app, "agent-auth", WebviewUrl::External(parsed))
                .title("Talon Pilot — 登录授权")
                .inner_size(480.0, 720.0)
                .build()
                .map_err(|e| format!("open auth window: {e}"))?;
        }
        Err(_) => {
            // sender 被 drop = login_with_browser 没拿到 URL 就结束了；
            // 下面 join 会拿到它的具体错误,这里不抢着报。
        }
    }

    // 等 login_with_browser 跑完(OAuth 完成 → self-enroll → 进程退出 → fetch_status)。
    let result = join.await.map_err(|e| format!("login task join: {e}"))?;

    // 登录流程结束,关掉授权窗口(成功失败都关)。
    if let Some(w) = app.get_webview_window("agent-auth") {
        let _ = w.close();
    }

    result
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            agent_status,
            agent_install,
            agent_login,
            agent_login_browser
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
