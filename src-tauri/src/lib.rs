mod agent;
mod browser;
mod config;
mod updater_ui;
mod window_state;

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
/// (async):kill/taskkill 是 spawn 子进程并等退出,不能占主线程。
#[tauri::command(async)]
fn agent_login_cancel() {
    let pid = LOGIN_PID.lock().ok().and_then(|g| *g);
    if let Some(pid) = pid {
        kill_pid(pid);
    }
}

/// 查询本机 tp-agent 状态（spawn `tp-agent status --json`）。
/// (async):Tauri 2 非 async 命令在主线程跑,spawn+wait 子进程会卡 UI。
#[tauri::command(async)]
fn agent_status() -> Result<agent::AgentStatus, String> {
    agent::fetch_status()
}

/// 「浏览器」连接器状态:relay 在线 / 扩展已连 / 已铺扩展目录(供 web 连接器显示)。
#[tauri::command(async)]
fn browser_connector_status(app: tauri::AppHandle) -> Result<browser::BrowserConnectorStatus, String> {
    Ok(browser::status(browser::extension_target_dir(&app)))
}

/// 开/关浏览器连接器:开 = 铺扩展 + 装 native host + 起 relay;关 = 停 relay 开关。
#[tauri::command(async)]
fn browser_connector_set_enabled(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<browser::BrowserConnectorStatus, String> {
    browser::set_enabled(&app, enabled)
}

/// 在 Chrome 打开 chrome://extensions(供安装引导)。
#[tauri::command(async)]
fn open_chrome_extensions_page() -> Result<(), String> {
    browser::open_chrome_extensions_page()
}

/// 帮装 tp-agent(跨平台,从配置镜像 / GitHub Release 下载)。返回安装路径。
/// 下载在 blocking 线程跑(此前在主线程同步下载,慢网整窗冻结、断网永久挂起),
/// 进度经 `agent-install-progress` 事件发前端:payload {downloaded, total},
/// total 可能为 null(服务器没报 Content-Length)。事件按 ≥256KB 间隔节流。
#[tauri::command]
async fn agent_install(app: tauri::AppHandle) -> Result<String, String> {
    use tauri::Emitter;

    let join = tauri::async_runtime::spawn_blocking(move || {
        let mut last_emitted = 0u64;
        agent::install_tp_agent(move |downloaded, total| {
            if downloaded - last_emitted < 256 * 1024 && Some(downloaded) != total {
                return;
            }
            last_emitted = downloaded;
            let _ = app.emit(
                "agent-install-progress",
                serde_json::json!({ "downloaded": downloaded, "total": total }),
            );
        })
    });
    join.await
        .map_err(|e| format!("install task join: {e}"))?
        .map(|p| p.display().to_string())
}

/// 用 api_key 完成 tp-agent 登录 + self-enroll，返回登录后状态。
/// (开发态兜底 / CI；正常 UI 走 `agent_login_browser` 完整 OAuth。)
#[tauri::command(async)]
fn agent_login(api_key: String) -> Result<agent::AgentStatus, String> {
    agent::login_with_key(&api_key)
}

/// 启动 tp-agent daemon(幂等)。
#[tauri::command(async)]
fn agent_start() -> Result<agent::AgentStatus, String> {
    agent::start_daemon()
}

/// 停止 tp-agent daemon。
#[tauri::command(async)]
fn agent_stop() -> Result<agent::AgentStatus, String> {
    agent::stop_daemon()
}

/// 重启 tp-agent daemon —— 多账号登录新账号后,daemon 在跑时要重启才生效,
/// 前端在 `agent_login_browser` 成功且 status.running 时引导用户调这个。
#[tauri::command(async)]
fn agent_restart() -> Result<agent::AgentStatus, String> {
    agent::restart_daemon()
}

/// 弹系统「选择文件夹」对话框,返回选中目录的绝对路径(取消则 None)。
/// 给本地项目创建向导用,替代手填绝对路径。
/// (async):blocking_pick_folder 会把原生面板派发到主线程并阻塞调用线程,
/// 必须在 worker 线程上调(非 async 命令在主线程跑会死锁)。
#[tauri::command(async)]
fn pick_folder(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    app.dialog()
        .file()
        .set_title("选择项目文件夹")
        .blocking_pick_folder()
        .and_then(|fp| fp.into_path().ok())
        .map(|p| p.display().to_string())
}

/// 用系统默认浏览器打开外链。仅允许 http/https —— 防前端被诱导打开 file:// 或
/// 任意 scheme(可能触发本地处理器)。
#[tauri::command(async)]
fn open_external(app: tauri::AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("拒绝打开非 http(s) 链接: {url}"));
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

/// 在系统文件管理器中定位(选中)某个文件/目录(「在 Finder 中显示」)。
#[tauri::command(async)]
fn reveal_in_dir(app: tauri::AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .reveal_item_in_dir(path)
        .map_err(|e| e.to_string())
}

/// 弹原生系统通知(任务完成/需要验收等)。best-effort:未授权/失败返回 Err,
/// 前端忽略即可(通知失败不该影响主流程)。
#[tauri::command(async)]
fn notify(app: tauri::AppHandle, title: String, body: String) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|e| e.to_string())
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

/// 检查并(经用户确认后)应用更新。供启动后台调用与手动 `app_check_update` 命令复用。
///
/// 流程:updater.check() 拉 endpoint 的 latest.json,与本机版本比对 —— 有新版才弹**自绘
/// 品牌弹窗**(updater_ui,Logo 居中 + 进度条);用户点「立即更新」才 download_and_install
/// (updater 边下边用 pubkey 校验 minisign 签名,校验失败直接报错不会装上损坏/被篡改包)+
/// relaunch 重启完成替换。
/// 静默策略:无更新 / 检查失败(离线、GitHub 不可达)都不打扰用户,只在控制台留痕。
async fn check_for_update(app: tauri::AppHandle) {
    use tauri_plugin_updater::UpdaterExt;

    // 用户配置了 update_endpoint(镜像/内网)则覆盖 tauri.conf.json 内置 endpoint;
    // 配置无效时退回内置默认并留痕,不能让一行错配置弄瞎整个自更新。
    let builder = match config::resolve_update_endpoint()
        .map(|ep| -> Result<_, String> {
            let url: tauri::Url = ep.parse().map_err(|e| format!("{ep}: {e}"))?;
            app.updater_builder()
                .endpoints(vec![url])
                .map_err(|e| format!("{ep}: {e}"))
        })
        .transpose()
    {
        Ok(Some(b)) => b,
        Ok(None) => app.updater_builder(),
        Err(e) => {
            eprintln!("[updater] 自定义 update_endpoint 无效,改用内置默认: {e}");
            app.updater_builder()
        }
    };
    let updater = match builder.build() {
        Ok(u) => u,
        Err(e) => {
            eprintln!("[updater] 初始化失败: {e}");
            return;
        }
    };

    let update = match updater.check().await {
        Ok(Some(update)) => update,
        Ok(None) => return, // 已是最新
        Err(e) => {
            eprintln!("[updater] 检查更新失败(忽略): {e}");
            return;
        }
    };

    // 有新版本 —— 开自绘品牌弹窗(下载/安装/进度/重启都在 updater_ui 里驱动)。
    let current = app.package_info().version.to_string();
    if let Err(e) = updater_ui::show_update_dialog(&app, update, current) {
        eprintln!("[updater] 打开更新弹窗失败(忽略): {e}");
    }
}

/// 手动触发检查更新(设置页/菜单可调)。无更新或失败时静默(同后台策略)。
#[tauri::command]
async fn app_check_update(app: tauri::AppHandle) {
    check_for_update(app).await;
}

/// 显示并聚焦主窗口(托盘点击 / 二次启动聚焦共用)。
fn show_main_window(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

/// 保存主窗口几何(物理像素),下次启动原样恢复。关窗 / 托盘退出前调用。
fn save_main_window_geometry(win: &tauri::WebviewWindow) {
    if let (Ok(pos), Ok(size)) = (win.outer_position(), win.inner_size()) {
        window_state::save(&window_state::WindowGeometry {
            x: pos.x,
            y: pos.y,
            width: size.width,
            height: size.height,
        });
    }
}

/// 系统托盘:左键点图标显示主窗口;菜单 = 显示主窗口 / 检查更新 / 退出。
/// 不改关闭行为(关窗即退出):任务跑在 tp-agent daemon 里,应用无须常驻,托盘只做快捷入口。
#[cfg(desktop)]
fn setup_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    use tauri::Manager;

    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let update = MenuItem::with_id(app, "check-update", "检查更新", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &update, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().ok_or("默认窗口图标缺失")?.clone())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, ev| match ev.id.as_ref() {
            "show" => show_main_window(app),
            "check-update" => {
                let handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    check_for_update(handle).await;
                });
            }
            "quit" => {
                // 托盘退出不经过窗口 CloseRequested,先存几何再退。
                if let Some(win) = app.get_webview_window("main") {
                    save_main_window_geometry(&win);
                }
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            // 左键单击 = 显示主窗口(Windows 习惯;macOS 菜单照常右键弹)
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // single-instance 必须最先注册:二次启动的实例在其它插件初始化前就退出,
    // 并聚焦已有实例 —— 双实例会双 updater / 双登录流程互踩。
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
        show_main_window(app);
    }));
    builder
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        // opener:外链系统浏览器打开 / 文件管理器里定位;notification:任务完成原生
        // 通知。两者壳侧只注册,由 web-next 经 window.__TAURI__ 调(capability 已放行)。
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        // 自绘更新弹窗的内容协议 + 其 HTML 状态(替代不稳的 data: URL 窗口)。
        .manage(updater_ui::UpdaterHtml::default())
        .register_uri_scheme_protocol(updater_ui::URI_SCHEME, updater_ui::serve)
        .invoke_handler(tauri::generate_handler![
            agent_status,
            agent_install,
            agent_login,
            agent_login_browser,
            agent_login_cancel,
            agent_start,
            agent_stop,
            agent_restart,
            pick_folder,
            open_external,
            reveal_in_dir,
            notify,
            browser_connector_status,
            browser_connector_set_enabled,
            open_chrome_extensions_page,
            app_check_update
        ])
        .setup(|app| {
            use tauri::{LogicalPosition, LogicalSize, Manager};
            if let Some(win) = app.get_webview_window("main") {
                // 窗口在 config 里 visible:false 启动 —— 先把几何定好再 show,
                // 避免先以兜底 1280×800 闪一下再跳。优先恢复上次保存的几何
                // (窗口中心仍落在某个显示器内才算有效);没有/失效则按屏幕 80%
                // 自适应居中(tauri.conf.json 的 1280×800 是读不到屏幕时的静态兜底)。
                let mut restored = false;
                if let Some(g) = window_state::load() {
                    let monitors: Vec<(i32, i32, u32, u32)> = win
                        .available_monitors()
                        .map(|ms| {
                            ms.iter()
                                .map(|m| {
                                    let p = m.position();
                                    let s = m.size();
                                    (p.x, p.y, s.width, s.height)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    if window_state::is_visible_on(&g, &monitors) {
                        let _ = win.set_size(tauri::PhysicalSize::new(g.width, g.height));
                        let _ = win.set_position(tauri::PhysicalPosition::new(g.x, g.y));
                        restored = true;
                    }
                }
                if !restored {
                    if let Ok(Some(monitor)) = win.current_monitor() {
                        // monitor.size()/position() 都是物理像素;按 scale_factor 统一转
                        // 逻辑像素再算,高分屏(retina scale=2)才不会算出半个屏幕大小/错位。
                        let scale = monitor.scale_factor();
                        let screen = monitor.size().to_logical::<f64>(scale);
                        // 屏幕左上角在「虚拟桌面」中的坐标 —— 多显示器/带 dock 偏移时居中
                        // 靠它,不能假设原点是 (0,0)(副屏 origin 可能是 1920,0 之类)。
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
                }
                // 无论是否拿到 monitor(读不到就用兜底尺寸)都要 show,否则窗口永久隐藏。
                let _ = win.show();

                // 关窗时保存几何。强杀进程会丢这一次的调整,可接受。
                let win_for_save = win.clone();
                win.on_window_event(move |ev| {
                    if let tauri::WindowEvent::CloseRequested { .. } = ev {
                        save_main_window_geometry(&win_for_save);
                    }
                });
            }

            #[cfg(desktop)]
            setup_tray(app)?;

            // 预览更新弹窗(设计走查/自测):TP_PREVIEW_UPDATER=1 启动即弹,不依赖网络。
            if std::env::var_os("TP_PREVIEW_UPDATER").is_some() {
                let _ = updater_ui::preview_dialog(&app.handle().clone());
            }

            // 启动后台静默检查更新。endpoint(GitHub Release latest.json)+ pubkey 在
            // tauri.conf.json;updater 用自有 minisign 公钥校验下载包完整性 —— 与 Apple
            // 代码签名无关,所以无 Apple 证书也能自更新。app 自下载替换自身不打 quarantine
            // 标记,绕过 Gatekeeper 重校(只有首次浏览器下载才会被拦)。
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                check_for_update(handle).await;
            });

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
                let _ = webview.eval(format!("window.__TP_API_BASE__ = {lit};"));
            }
            // 注入客户端运行环境给前端(window.__TP_CLIENT_INFO__):版本号(编译期 Cargo
            // 版本)+ OS。前端给每个云端请求带 X-Talon-Client* header,后端按此识别客户端、
            // 记日志便于排查(哪个版本/什么系统来的请求)。版本来自壳真值,不靠 UA 猜。
            let client_info = serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "os": std::env::consts::OS,  // macos / windows / linux
            });
            if let Ok(lit) = serde_json::to_string(&client_info) {
                let _ = webview.eval(format!("window.__TP_CLIENT_INFO__ = {lit};"));
            }
            // 拖动条只 macOS 需要(Overlay 透明标题栏无原生可拖区)。
            // Windows/Linux 用系统原生标题栏,直接可拖,不注入。
            #[cfg(target_os = "macos")]
            {
                let _ = webview.eval(TITLEBAR_DRAG_SCRIPT);
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // 退出前存窗口几何。窗口的 CloseRequested 只覆盖「点红钮关窗」;
            // macOS Cmd+Q / 托盘退出 / app.exit 不经过它(实测),都汇聚到这里。
            // 窗口已销毁时 outer_position 会失败,save 内部静默跳过,不会覆盖坏值。
            if let tauri::RunEvent::ExitRequested { .. } = event {
                use tauri::Manager;
                if let Some(win) = app.get_webview_window("main") {
                    save_main_window_geometry(&win);
                }
            }
        });
}
