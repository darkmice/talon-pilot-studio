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
#[tauri::command]
fn agent_login(api_key: String) -> Result<agent::AgentStatus, String> {
    agent::login_with_key(&api_key)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            agent_status,
            agent_install,
            agent_login
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
