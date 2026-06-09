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
    // 注意:开窗错误**不能立即 ? 早返回** —— 那样会跳过下面的 join.await,泄漏 spawn_blocking
    // 任务(tp-agent 子进程没人收、跑到 10min 超时)。先记下窗口错误,等 join 收掉任务再决定。
    let mut window_err: Option<String> = None;
    match url_rx.await {
        Ok(url) => {
            match url.parse::<tauri::Url>() {
                Ok(parsed) => {
                    if let Err(e) = WebviewWindowBuilder::new(
                        &app,
                        "agent-auth",
                        WebviewUrl::External(parsed),
                    )
                    .title("Talon Pilot — 登录授权")
                    .inner_size(480.0, 720.0)
                    .build()
                    {
                        window_err = Some(format!("open auth window: {e}"));
                    }
                }
                Err(e) => {
                    window_err = Some(format!("invalid auth url from tp-agent: {e}"));
                }
            }
        }
        Err(_) => {
            // sender 被 drop = login_with_browser 没拿到 URL 就结束了；
            // 下面 join 会拿到它的具体错误,这里不抢着报。
        }
    }

    // 无论开窗成功失败,都要 await join 收掉 login 任务(否则子进程泄漏)。
    let result = join.await.map_err(|e| format!("login task join: {e}"))?;

    // 登录流程结束,关掉授权窗口(成功失败都关)。
    if let Some(w) = app.get_webview_window("agent-auth") {
        let _ = w.close();
    }

    // 开窗本身失败时优先报它(子进程已被 join 收掉,不再泄漏)。
    if let Some(e) = window_err {
        return Err(e);
    }
    result
}

/// macOS 专用注入:Overlay 标题栏(透明、无原生标题栏)下,web-next 顶部没有可拖区域。
/// 叠一条固定在顶部的透明拖动条,**手动绑 mousedown 调 Tauri 2 的 startDragging()**
/// (方案 B,绕过 data-tauri-drag-region 属性机制 + CSS -webkit-app-region 的 macOS
/// WKWebView 冲突坑;需 capabilities 有 core:window:allow-start-dragging)。
/// 左上 80px 留给交通灯不绑拖动;双击切最大化。
///
/// **顶部避让高度不在这里做** —— 交给 web-next 前端按平台条件化 CSS(见 web-next
/// 的 titlebar 适配),因为 Windows/Linux 用系统原生标题栏(在窗口外),内容不需避让;
/// 只有 macOS Overlay 才需要内容下移。本脚本只负责"让那条透明区域能拖窗口"。
/// Windows/Linux 有原生标题栏可直接拖,不注入本脚本。
#[cfg(target_os = "macos")]
const TITLEBAR_DRAG_SCRIPT: &str = r#"
(function () {
  var ID = '__tp_titlebar_drag__';
  var BAR_H = 28;     // 拖动条高度,与前端 macOS 避让 padding 对齐
  var LIGHTS_W = 80;  // 左上交通灯宽度,这一段不触发拖动
  function getWin() {
    try { return window.__TAURI__.window.getCurrentWindow(); } catch (e) { return null; }
  }
  function ensure() {
    if (document.getElementById(ID)) return;
    if (!document.body) return;
    var bar = document.createElement('div');
    bar.id = ID;
    bar.style.cssText = [
      'position:fixed','top:0','left:0','right:0','height:' + BAR_H + 'px',
      'z-index:2147483647','user-select:none','-webkit-user-select:none'
    ].join(';');
    bar.addEventListener('mousedown', function (e) {
      if (e.buttons !== 1) return;            // 仅左键
      if (e.clientX < LIGHTS_W) return;       // 交通灯区域放行,不抢点击
      var win = getWin();
      if (!win) return;
      if (e.detail === 2) { win.toggleMaximize(); return; }  // 双击切最大化
      e.preventDefault();
      win.startDragging();
    });
    document.body.appendChild(bar);
  }
  function boot() {
    ensure();
    try {
      var obs = new MutationObserver(ensure);
      obs.observe(document.documentElement, { childList: true, subtree: true });
    } catch (e) {}
  }
  // __TAURI__ 由 initialization script 注入,顶层脚本执行时可能还没就绪,轮询等一下
  if (window.__TAURI__ && window.__TAURI__.window) {
    boot();
  } else {
    var tries = 0;
    var t = setInterval(function () {
      if ((window.__TAURI__ && window.__TAURI__.window) || tries++ > 50) {
        clearInterval(t); boot();
      }
    }, 100);
  }
})();
"#;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            agent_status,
            agent_install,
            agent_login,
            agent_login_browser
        ])
        .on_page_load(|webview, _payload| {
            // 拖动条只 macOS 需要(Overlay 透明标题栏无原生可拖区)。
            // Windows/Linux 用系统原生标题栏,直接可拖,不注入。
            #[cfg(target_os = "macos")]
            {
                let _ = webview.eval(TITLEBAR_DRAG_SCRIPT);
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
