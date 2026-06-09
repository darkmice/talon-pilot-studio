mod agent;
mod config;

use std::sync::Mutex;

/// 进行中的 OAuth 登录子进程 pid 登记表。agent_login_browser spawn 子进程时记下,
/// 用户关授权窗 / 点「取消」时据此 kill —— 否则 tp-agent pair-poll 会一直轮询到自身
/// 超时(数分钟),前端永久 loading。同一时刻只允许一个登录流程,Option 足够。
static LOGIN_PID: Mutex<Option<u32>> = Mutex::new(None);

/// 跨平台杀进程(取消登录用)。unix 用 SIGTERM(kill 命令避免引 nix);Windows 用 taskkill。
fn kill_pid(pid: u32) {
    #[cfg(not(target_os = "windows"))]
    {
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status();
    }
}

/// 取消进行中的 WebView 登录:kill 已登记的 tp-agent 子进程。
/// 前端「取消」按钮 + 授权窗 close 事件都调它。无进行中登录时 no-op。
#[tauri::command]
fn agent_login_cancel() {
    let pid = LOGIN_PID.lock().ok().and_then(|g| *g);
    if let Some(pid) = pid {
        kill_pid(pid);
    }
}

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
    // on_child_spawned 把子进程 pid 登记到 LOGIN_PID,供取消用。
    let join = tauri::async_runtime::spawn_blocking(move || {
        let mut url_tx = Some(url_tx);
        agent::login_with_browser(
            api_base_url.as_deref(),
            web_base_url.as_deref(),
            |url| {
                if let Some(tx) = url_tx.take() {
                    let _ = tx.send(url);
                }
            },
            |pid| {
                if let Ok(mut g) = LOGIN_PID.lock() {
                    *g = Some(pid);
                }
            },
        )
    });

    // 等授权 URL 就绪 → 开 WebView 窗口加载它(tp-agent loopback callback 会完成闭环)。
    // 注意:开窗错误**不能立即 ? 早返回** —— 那样会跳过下面的 join.await,泄漏 spawn_blocking
    // 任务(tp-agent 子进程没人收、跑到 10min 超时)。先记下窗口错误,等 join 收掉任务再决定。
    let mut window_err: Option<String> = None;
    match url_rx.await {
        Ok(url) => {
            match url.parse::<tauri::Url>() {
                Ok(parsed) => {
                    match WebviewWindowBuilder::new(
                        &app,
                        "agent-auth",
                        WebviewUrl::External(parsed),
                    )
                    .title("Talon Pilot — 登录授权")
                    .inner_size(480.0, 720.0)
                    .build()
                    {
                        Ok(win) => {
                            // 用户关授权窗 = 取消登录:kill tp-agent 子进程,否则它会
                            // pair-poll 轮询到超时,join.await 一直阻塞、前端永久 loading。
                            win.on_window_event(|ev| {
                                if let tauri::WindowEvent::CloseRequested { .. } = ev {
                                    let pid = LOGIN_PID.lock().ok().and_then(|g| *g);
                                    if let Some(pid) = pid {
                                        kill_pid(pid);
                                    }
                                }
                            });
                        }
                        Err(e) => {
                            window_err = Some(format!("open auth window: {e}"));
                        }
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
    // 被取消(关窗/取消按钮 kill 子进程)时,login_with_browser 的 child.wait() 立即
    // 返回,join 在此收到一个 Err(进程被信号终止),正常 unwind,不再卡住。
    let result = join.await.map_err(|e| format!("login task join: {e}"))?;

    // 登录流程结束,清掉 pid 登记(避免悬挂的旧 pid 被后续误 kill)。
    if let Ok(mut g) = LOGIN_PID.lock() {
        *g = None;
    }

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
            agent_login_browser,
            agent_login_cancel
        ])
        .setup(|app| {
            // 初始窗口尺寸按屏幕自适应:宽高都取屏幕 80%(与屏幕同比例),并居中。
            // tauri.conf.json 的 1280×800 是静态兜底(读不到屏幕时用)。
            use tauri::{LogicalPosition, LogicalSize, Manager};
            if let Some(win) = app.get_webview_window("main") {
                // 窗口在 config 里 visible:false 启动 —— 先按屏幕算好尺寸+位置,再 show,
                // 避免先以兜底 1280×800 闪一下再跳到目标尺寸/位置。
                if let Ok(Some(monitor)) = win.current_monitor() {
                    // monitor.size()/position() 都是物理像素;按 scale_factor 统一转逻辑
                    // 像素再算,高分屏(retina scale=2)才不会算出半个屏幕大小/错位。
                    let scale = monitor.scale_factor();
                    let screen = monitor.size().to_logical::<f64>(scale);
                    // 屏幕左上角在「虚拟桌面」中的坐标 —— 多显示器/带 dock 偏移时居中靠它,
                    // 不能假设原点是 (0,0)(副屏 origin 可能是 1920,0 之类)。
                    let origin = monitor.position().to_logical::<f64>(scale);

                    // 与屏幕同比例:宽高都取屏幕 80%,窗口形状跟屏幕一致。
                    let width = (screen.width * 0.8).round().max(960.0);
                    let height = (screen.height * 0.8).round().max(600.0);

                    // 手动算居中坐标(origin + (屏 - 窗)/2),不依赖 win.center() ——
                    // center() 在多屏/Overlay 标题栏下有时不按本屏工作区算,会偏。
                    let x = origin.x + (screen.width - width) / 2.0;
                    let y = origin.y + (screen.height - height) / 2.0;

                    let _ = win.set_size(LogicalSize::new(width, height));
                    let _ = win.set_position(LogicalPosition::new(x, y));
                }
                // 无论是否拿到 monitor(读不到就用兜底尺寸)都要 show,否则窗口永久隐藏。
                let _ = win.show();
            }
            Ok(())
        })
        .on_page_load(|webview, _payload| {
            // 注入后端地址给前端(window.__TP_API_BASE__)。地址真相源在壳侧:
            // env > 用户配置(~/.config/talon-pilot-studio/config.toml) > 内置默认。
            // 前端只读这个注入值、不内置地址(用户要求:支持后续自配云端/内网部署)。
            // on_page_load(Started)早于页面脚本,前端首个 API 请求前已就位。
            // serde_json 序列化成 JS 字面量,安全转义防注入。
            let api_base = config::resolve_api_base();
            if let Ok(lit) = serde_json::to_string(&api_base) {
                let _ = webview.eval(&format!("window.__TP_API_BASE__ = {lit};"));
            }
            // 注入客户端运行环境给前端(window.__TP_CLIENT_INFO__):版本号(编译期 Cargo
            // 版本)+ OS。前端给每个云端请求带 X-Talon-Client* header,后端按此识别客户端、
            // 记日志便于排查(哪个版本/什么系统来的请求)。版本来自壳真值,不靠 UA 猜。
            let client_info = serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "os": std::env::consts::OS,  // macos / windows / linux
            });
            if let Ok(lit) = serde_json::to_string(&client_info) {
                let _ = webview.eval(&format!("window.__TP_CLIENT_INFO__ = {lit};"));
            }
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
