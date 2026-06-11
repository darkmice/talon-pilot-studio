//! 自绘品牌化更新弹窗 —— 替代 tauri-plugin-dialog 的原生 NSAlert(无法居中、图标布局固定)。
//!
//! 设计:壳自己开一个小 WebView 窗口承载一段 HTML(Logo 居中、文字居中、进度条、
//! 稍后/立即更新)。不依赖前端仓 web-next,登录前(登录页)也能弹。
//!
//! HTML 承载方式:**自定义 URI scheme 协议** `tpupdater://localhost/`。
//! (起初用 data: URL,但 macOS WKWebView 对 data:URL 窗口渲染不可靠——空白白屏;
//! 自定义协议带正确 Content-Type、是真实 local origin,跨平台稳。)HTML 渲染后存进
//! managed state `UpdaterHtml`,协议 handler 读出来伺服。
//!
//! 通信免 IPC / 免 capability:
//!   - 后端 → 弹窗:`webview.eval(js)` 推下载进度 / 错误(eval 不需要 capability)。
//!   - 弹窗 → 后端:按钮把 `location` 指到 `tpupdate://install|later`(注意是 tpupdate,
//!     与内容协议 tpupdater 不同、未注册,故不会被 webview 当本地资源解析),
//!     `on_navigation` 拦截拿到动作后 return false 取消真实跳转。

use base64::Engine;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_updater::Update;

/// 弹窗 logo:128×128(11KB),作为 HTML 内联图(data:image)嵌入,够清晰。
const LOGO_PNG: &[u8] = include_bytes!("../icons/128x128.png");
const DIALOG_LABEL: &str = "tp-updater";
/// 内容协议名(渲染弹窗 HTML)。注册在 tauri::Builder。
pub const URI_SCHEME: &str = "tpupdater";

/// 待显示的弹窗 HTML —— show_update_dialog 渲染后存这里,URI scheme handler 读出伺服。
/// 作为 Tauri managed state(setup 里 app.manage)。
#[derive(Default)]
pub struct UpdaterHtml(pub Mutex<Option<String>>);

/// `tpupdater://` 协议 handler:返回当前 pending 的弹窗 HTML(text/html)。
/// 注册:`Builder::register_uri_scheme_protocol(URI_SCHEME, updater_ui::serve)`。
pub fn serve<R: Runtime>(
    ctx: tauri::UriSchemeContext<'_, R>,
    _req: tauri::http::Request<Vec<u8>>,
) -> tauri::http::Response<Vec<u8>> {
    let html = ctx
        .app_handle()
        .state::<UpdaterHtml>()
        .0
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default();
    tauri::http::Response::builder()
        .header("Content-Type", "text/html; charset=utf-8")
        .body(html.into_bytes())
        .unwrap_or_else(|_| tauri::http::Response::new(Vec::new()))
}

/// 预览更新弹窗(不依赖网络/真实版本检查)——设计走查 / 自测用。
/// 启动时设 `TP_PREVIEW_UPDATER=1` 触发(见 lib.rs setup)。「立即更新」在预览态无真实
/// 安装动作(无 Update),只用来看样式与文案;真实更新流程走 show_update_dialog。
pub fn preview_dialog(app: &AppHandle) -> Result<(), String> {
    if app.get_webview_window(DIALOG_LABEL).is_some() {
        return Ok(());
    }
    let html = render_html(
        env!("CARGO_PKG_VERSION"),
        "0.1.7", // 预览演示用:展示"新版本 > 当前"的样子
        "## 本次更新\n\n\
         - 修复 **tp-agent** 安装后检测不到的问题(`install`/`locate` 路径闭环)\n\
         - 帮装/登录命令移出主线程,加下载超时与进度,断网不再卡死\n\
         - 新增 daemon 管理:启动/停止/重启,多账号切换后自动生效\n\
         - 更新弹窗改为品牌化样式,支持 **Markdown 渲染**与滚动渐隐\n\
         - 自动更新与 tp-agent 下载支持镜像加速(大陆直连更稳)\n\
         - 开启内容安全策略(CSP)、系统托盘、单实例、窗口状态记忆\n\n\
         [查看完整发布说明](https://github.com/darkmice/talon-pilot-studio/releases)",
    );
    *app.state::<UpdaterHtml>()
        .0
        .lock()
        .map_err(|_| "updater html state poisoned".to_string())? = Some(html);
    let url = format!("{URI_SCHEME}://localhost/")
        .parse::<tauri::Url>()
        .map_err(|e| format!("build preview url: {e}"))?;
    // 预览态:无真实 Update。「立即更新」模拟一遍下载进度(不真装/不重启),好让设计
    // 走查能看到完整进度条 UX;「稍后」关窗;外链走系统浏览器——与真实弹窗一致。
    let app_handle = app.clone();
    let started = Arc::new(AtomicBool::new(false));
    WebviewWindowBuilder::new(app, DIALOG_LABEL, WebviewUrl::CustomProtocol(url))
        .title("Talon Pilot Studio 更新")
        .inner_size(420.0, 540.0)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .center()
        .always_on_top(true)
        .on_navigation(move |url| {
            if url.scheme() == "tpupdate" {
                match url.host_str().unwrap_or("") {
                    "install" => {
                        if !started.swap(true, Ordering::SeqCst) {
                            preview_simulate_install(app_handle.clone());
                        }
                    }
                    "later" => {
                        if let Some(w) = app_handle.get_webview_window(DIALOG_LABEL) {
                            let _ = w.close();
                        }
                    }
                    _ => {}
                }
                return false;
            }
            route_non_action(&app_handle, url)
        })
        .build()
        .map_err(|e| format!("open preview dialog: {e}"))?;
    Ok(())
}

/// 预览态模拟下载进度:走查用,不真下载/不重启。0→100 推进度条后显示完成。
fn preview_simulate_install(app: AppHandle) {
    tauri::async_runtime::spawn_blocking(move || {
        let eval = |js: String| {
            if let Some(w) = app.get_webview_window(DIALOG_LABEL) {
                let _ = w.eval(&js);
            }
        };
        eval("window.__tpStart && window.__tpStart()".into());
        for pct in (0..=100).step_by(5) {
            std::thread::sleep(std::time::Duration::from_millis(120));
            eval(format!("window.__tpProgress && window.__tpProgress({pct})"));
        }
        eval("window.__tpDone && window.__tpDone()".into());
    });
}

/// 展示更新弹窗。`update` 是 updater 已查到的新版本,`current` 是当前版本号。
/// 创建失败(极少)时退回直接安装的逻辑由调用方决定;这里只负责弹窗 + 驱动下载。
pub fn show_update_dialog(app: &AppHandle, update: Update, current: String) -> Result<(), String> {
    // 已经有一个更新弹窗就别开第二个(后台检查 + 手动检查可能撞车)。
    if app.get_webview_window(DIALOG_LABEL).is_some() {
        return Ok(());
    }

    let new_version = update.version.clone();
    let notes = update.body.clone().unwrap_or_default();
    let html = render_html(&current, &new_version, &notes);
    // 存进 state 供协议 handler 伺服(state 在 setup 里已 manage)。
    *app.state::<UpdaterHtml>()
        .0
        .lock()
        .map_err(|_| "updater html state poisoned".to_string())? = Some(html);

    let url = format!("{URI_SCHEME}://localhost/")
        .parse::<tauri::Url>()
        .map_err(|e| format!("build updater dialog url: {e}"))?;

    // Update 要在「立即更新」点击时(on_navigation 回调,Fn + Send + 'static)被异步任务用,
    // 用 Arc 共享;started 防重复点击触发多次下载。
    let update = Arc::new(update);
    let started = Arc::new(AtomicBool::new(false));
    let app_handle = app.clone();

    WebviewWindowBuilder::new(app, DIALOG_LABEL, WebviewUrl::CustomProtocol(url))
        .title("Talon Pilot Studio 更新")
        .inner_size(420.0, 540.0)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .center()
        .always_on_top(true)
        .on_navigation(move |url| nav_handler(&app_handle, &started, &update, url))
        .build()
        .map_err(|e| format!("open updater dialog: {e}"))?;

    Ok(())
}

/// 弹窗内容自身的 origin 主机名:macOS 是 `tpupdater://localhost`,
/// Windows/Android 是 `http(s)://tpupdater.localhost`(自定义协议跨平台改写)。
fn is_dialog_origin(url: &tauri::Url) -> bool {
    let scheme = url.scheme();
    let host = url.host_str().unwrap_or("");
    scheme == URI_SCHEME || host == format!("{URI_SCHEME}.localhost")
}

/// 非动作导航的统一路由(真实弹窗与预览弹窗共用):
/// - 弹窗自身 origin(初始加载):放行;
/// - http/https(更新说明里的外链):**系统浏览器打开**,弹窗内不跳转(否则弹窗被替换);
/// - 其余:放行。
fn route_non_action(app: &AppHandle, url: &tauri::Url) -> bool {
    use tauri_plugin_opener::OpenerExt;

    if is_dialog_origin(url) {
        return true; // 弹窗自身内容,放行加载
    }
    if matches!(url.scheme(), "http" | "https") {
        let _ = app.opener().open_url(url.as_str(), None::<&str>);
        return false;
    }
    true
}

/// 真实弹窗导航处理:`tpupdate://install|later` 为动作(拦截不跳转),其余走 route_non_action。
fn nav_handler(
    app: &AppHandle,
    started: &Arc<AtomicBool>,
    update: &Arc<Update>,
    url: &tauri::Url,
) -> bool {
    if url.scheme() == "tpupdate" {
        match url.host_str().unwrap_or("") {
            "install" => {
                if !started.swap(true, Ordering::SeqCst) {
                    start_download(app.clone(), update.clone());
                }
            }
            "later" => {
                if let Some(w) = app.get_webview_window(DIALOG_LABEL) {
                    let _ = w.close();
                }
            }
            _ => {}
        }
        return false; // 动作协议不真正跳转
    }
    route_non_action(app, url)
}

/// 启动下载 + 安装,进度经 eval 推给弹窗;完成后重启,失败则在弹窗内显示错误。
fn start_download(app: AppHandle, update: Arc<Update>) {
    tauri::async_runtime::spawn(async move {
        let eval = |js: String| {
            if let Some(w) = app.get_webview_window(DIALOG_LABEL) {
                let _ = w.eval(&js);
            }
        };
        eval("window.__tpStart && window.__tpStart()".into());

        // 进度回调:累加已下载字节,按整数百分比节流 eval(content_len 缺失则只报"下载中")。
        let mut downloaded: u64 = 0;
        let mut last_pct: i64 = -1;
        let app_for_progress = app.clone();
        let on_chunk = move |chunk: usize, total: Option<u64>| {
            downloaded += chunk as u64;
            let pct = match total {
                Some(t) if t > 0 => ((downloaded * 100) / t) as i64,
                _ => -1,
            };
            if pct != last_pct {
                last_pct = pct;
                if let Some(w) = app_for_progress.get_webview_window(DIALOG_LABEL) {
                    let _ = w.eval(format!("window.__tpProgress && window.__tpProgress({pct})"));
                }
            }
        };

        match update.download_and_install(on_chunk, || {}).await {
            Ok(()) => {
                eval("window.__tpDone && window.__tpDone()".into());
                // 给弹窗一瞬间渲染"完成,正在重启",再 relaunch 完成替换。
                tauri::process::restart(&app.env());
            }
            Err(e) => {
                // JSON 转义错误文本,安全塞进 JS 字符串。
                let msg = serde_json::to_string(&format!("{e}"))
                    .unwrap_or_else(|_| "\"更新失败\"".to_string());
                eval(format!("window.__tpError && window.__tpError({msg})"));
                // 也广播一个事件,便于将来前端/日志感知(无监听则无副作用)。
                let _ = app.emit("updater-error", format!("{e}"));
            }
        }
    });
}

/// 渲染弹窗 HTML。Logo 内嵌为 base64,版本号/说明经转义注入。整体居中、产品风格。
fn render_html(current: &str, new_version: &str, notes: &str) -> String {
    let logo = base64::engine::general_purpose::STANDARD.encode(LOGO_PNG);
    let notes_html = render_notes(notes);
    // 版本号是 semver,字符集安全;仍走 escape 兜底防意外内容。
    let cur = escape_html(current);
    let new = escape_html(new_version);

    format!(
        r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>更新</title>
<style>
  :root {{
    --brand: #4f46e5;       /* 与产品主色一致的靛蓝 */
    --brand-2: #6366f1;
    --ink: #0f172a;
    --muted: #64748b;
    --line: #e2e8f0;
    --bg: #ffffff;
  }}
  @media (prefers-color-scheme: dark) {{
    :root {{ --ink:#f1f5f9; --muted:#94a3b8; --line:#1e293b; --bg:#0b1220; }}
  }}
  * {{ box-sizing: border-box; }}
  html, body {{ margin: 0; height: 100%; }}
  body {{
    font-family: -apple-system, "PingFang SC", "Microsoft YaHei", system-ui, sans-serif;
    background: var(--bg); color: var(--ink);
    display: flex; flex-direction: column; align-items: center;
    text-align: center; padding: 32px 28px 24px; user-select: none;
    -webkit-user-select: none;
  }}
  .logo {{ width: 84px; height: 84px; border-radius: 20px; }}
  h1 {{ font-size: 17px; font-weight: 600; margin: 18px 0 2px; letter-spacing: .2px; }}
  .ver {{ font-size: 13px; color: var(--muted); margin: 0; }}
  .ver b {{ color: var(--ink); font-weight: 600; }}
  /* 更新说明:内容过多则滚动。scroll-mask —— 用 mask-image 线性渐变在上下边缘渐隐,
     提示"还有更多";--fade-top/--fade-bottom 由 JS 按滚动位置动态置 0(到顶不隐顶、
     到底不隐底、不溢出则全不隐)。等价 tailwindcss-scroll-mask,纯 CSS/JS 无需工具链。 */
  /* 弹性填充 logo/版本 与 按钮之间的全部空间(flex:1 + min-height:0 让 overflow 在
     flex 容器里生效)。内容少:在该空间内垂直居中(不在底部留空);内容多:`safe center`
     自动回退到顶对齐,可滚动不裁切 + scroll-mask 上下渐隐。 */
  .notes {{
    margin: 16px 0 14px; width: 100%; flex: 1 1 auto; min-height: 0; overflow-y: auto;
    display: flex; flex-direction: column; justify-content: safe center;
    font-size: 12.5px; line-height: 1.7; color: var(--muted);
    padding: 10px 12px; text-align: left;
    --fade: 22px; --fade-top: 0px; --fade-bottom: 0px;
    -webkit-mask-image: linear-gradient(to bottom, transparent 0,
      #000 var(--fade-top), #000 calc(100% - var(--fade-bottom)), transparent 100%);
    mask-image: linear-gradient(to bottom, transparent 0,
      #000 var(--fade-top), #000 calc(100% - var(--fade-bottom)), transparent 100%);
    scrollbar-width: thin; scrollbar-color: var(--line) transparent;
  }}
  .notes-body {{ width: 100%; }}
  /* Markdown 元素样式(render_notes 产出) */
  .notes-body > :first-child {{ margin-top: 0; }}
  .notes-body > :last-child {{ margin-bottom: 0; }}
  .notes-body h1, .notes-body h2, .notes-body h3 {{
    font-size: 13.5px; font-weight: 600; color: var(--ink); margin: 12px 0 6px;
  }}
  .notes-body p {{ margin: 6px 0; }}
  .notes-body ul, .notes-body ol {{ margin: 6px 0; padding-left: 20px; }}
  .notes-body li {{ margin: 3px 0; }}
  .notes-body strong, .notes-body b {{ color: var(--ink); font-weight: 600; }}
  .notes-body a {{ color: var(--brand-2); text-decoration: none; }}
  .notes-body a:hover {{ text-decoration: underline; }}
  .notes-body code {{
    background: var(--line); border-radius: 4px; padding: 1px 5px; font-size: 11.5px;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  }}
  .notes-body pre {{ background: var(--line); border-radius: 8px; padding: 10px; overflow-x: auto; }}
  .notes-body pre code {{ background: none; padding: 0; }}
  .notes-body hr {{ border: none; border-top: 1px solid var(--line); margin: 10px 0; }}
  .notes::-webkit-scrollbar {{ width: 6px; }}
  .notes::-webkit-scrollbar-thumb {{ background: var(--line); border-radius: 3px; }}
  /* 进度区:下载时显示 */
  .progress-wrap {{ width: 100%; margin: 14px 0 2px; display: none; }}
  .progress-wrap.show {{ display: block; }}
  .bar {{ height: 6px; border-radius: 999px; background: var(--line); overflow: hidden; }}
  .bar > i {{
    display: block; height: 100%; width: 0%;
    background: linear-gradient(90deg, var(--brand), var(--brand-2));
    transition: width .2s ease;
  }}
  .bar.indeterminate > i {{ width: 40% !important; animation: slide 1.1s ease-in-out infinite; }}
  @keyframes slide {{ 0%{{margin-left:-40%}} 100%{{margin-left:100%}} }}
  .progress-text {{ font-size: 12px; color: var(--muted); margin-top: 8px; }}
  .err {{ color: #dc2626; font-size: 12.5px; margin: 12px 0 0; display: none; }}
  .actions {{ display: flex; gap: 10px; width: 100%; margin-top: 18px; }}
  button {{
    flex: 1; height: 38px; border-radius: 10px; font-size: 14px; cursor: pointer;
    border: 1px solid var(--line); background: transparent; color: var(--ink);
    transition: opacity .15s, filter .15s;
  }}
  button.primary {{
    border: none; color: #fff;
    background: linear-gradient(90deg, var(--brand), var(--brand-2));
  }}
  button:disabled {{ opacity: .5; cursor: default; }}
  button:not(:disabled):hover {{ filter: brightness(1.05); }}
</style>
</head>
<body>
  <img class="logo" src="data:image/png;base64,{logo}" alt="Talon Pilot">
  <h1>Talon Pilot Studio 有新版本</h1>
  <p class="ver">发现新版本 <b>{new}</b>（当前 {cur}）</p>
  <div class="notes"><div class="notes-body">{notes_html}</div></div>

  <div class="progress-wrap" id="pw">
    <div class="bar" id="bar"><i id="barfill"></i></div>
    <div class="progress-text" id="ptext">正在下载…</div>
  </div>
  <p class="err" id="err"></p>

  <div class="actions" id="actions">
    <button id="later" onclick="location.href='tpupdate://later'">稍后</button>
    <button id="now" class="primary" onclick="location.href='tpupdate://install'">立即更新</button>
  </div>

<script>
  var pw = document.getElementById('pw');
  var bar = document.getElementById('bar');
  var fill = document.getElementById('barfill');
  var ptext = document.getElementById('ptext');
  var err = document.getElementById('err');
  var nowBtn = document.getElementById('now');
  var laterBtn = document.getElementById('later');

  // 后端调用:开始下载 —— 禁用按钮、显示进度区(先用 indeterminate 等首个百分比)。
  window.__tpStart = function () {{
    nowBtn.disabled = true; laterBtn.disabled = true;
    nowBtn.textContent = '更新中…';
    pw.classList.add('show'); bar.classList.add('indeterminate');
    ptext.textContent = '正在下载…';
  }};
  // 后端调用:进度。pct<0 表示服务器没报总长,保持 indeterminate。
  window.__tpProgress = function (pct) {{
    if (pct < 0) {{ bar.classList.add('indeterminate'); ptext.textContent = '正在下载…'; return; }}
    bar.classList.remove('indeterminate');
    fill.style.width = pct + '%';
    ptext.textContent = '正在下载… ' + pct + '%';
  }};
  // 后端调用:下载安装完成,即将重启。
  window.__tpDone = function () {{
    bar.classList.remove('indeterminate'); fill.style.width = '100%';
    ptext.textContent = '下载完成,正在重启应用…';
  }};
  // 后端调用:出错,恢复按钮让用户可重试 / 稍后。
  window.__tpError = function (msg) {{
    bar.classList.remove('indeterminate');
    pw.classList.remove('show');
    err.style.display = 'block';
    err.textContent = '更新失败:' + msg;
    nowBtn.disabled = false; laterBtn.disabled = false;
    nowBtn.textContent = '重试';
  }};

  // scroll-mask:按滚动位置动态控制上下渐隐(到顶不隐顶 / 到底不隐底 / 不溢出全不隐)。
  var notes = document.querySelector('.notes');
  function updateMask() {{
    if (!notes) return;
    var fade = getComputedStyle(notes).getPropertyValue('--fade').trim() || '22px';
    var scrollable = notes.scrollHeight > notes.clientHeight + 1;
    if (!scrollable) {{
      notes.style.setProperty('--fade-top', '0px');
      notes.style.setProperty('--fade-bottom', '0px');
      return;
    }}
    var atTop = notes.scrollTop <= 0;
    var atBottom = notes.scrollTop + notes.clientHeight >= notes.scrollHeight - 1;
    notes.style.setProperty('--fade-top', atTop ? '0px' : fade);
    notes.style.setProperty('--fade-bottom', atBottom ? '0px' : fade);
  }}
  if (notes) {{
    updateMask();
    notes.addEventListener('scroll', updateMask, {{ passive: true }});
    window.addEventListener('resize', updateMask);
  }}
</script>
</body>
</html>"#
    )
}

/// 更新说明:把 Markdown 渲染成 HTML(发布说明天然是 Markdown:列表/加粗/标题/链接)。
/// 安全:notes 来自 latest.json(我方发版生成),但仍把原始 HTML 事件降级成纯文本
/// (push_html 会转义),即便 notes 里夹了 `<script>` 也只会显示字面、不会执行。
/// 空说明返回空串。
fn render_notes(notes: &str) -> String {
    use pulldown_cmark::{html, Event, Options, Parser};

    let trimmed = notes.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);
    let parser = Parser::new_ext(trimmed, opts).map(|ev| match ev {
        // 原始 HTML(块级/内联)降级为文本 —— push_html 会转义,杜绝脚本注入。
        Event::Html(s) | Event::InlineHtml(s) => Event::Text(s),
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

/// 最小 HTML 转义(版本号等短字段用;Markdown 正文走 render_notes)。
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_html_special_chars() {
        assert_eq!(
            escape_html(r#"<script>"x"&'y'</script>"#),
            "&lt;script&gt;&quot;x&quot;&amp;&#39;y&#39;&lt;/script&gt;"
        );
    }

    #[test]
    fn empty_notes_render_empty() {
        assert_eq!(render_notes("   \n  "), "");
        assert_eq!(render_notes(""), "");
    }

    #[test]
    fn renders_markdown_list_and_emphasis() {
        let html = render_notes("- 修复 **A**\n- 新增 B");
        assert!(html.contains("<ul>"));
        assert!(html.contains("<li>修复 <strong>A</strong></li>"));
        assert!(html.contains("<li>新增 B</li>"));
    }

    #[test]
    fn renders_markdown_heading_and_link() {
        let html = render_notes("## 标题\n\n[文档](https://example.com)");
        assert!(html.contains("<h2>标题</h2>"));
        assert!(html.contains(r#"<a href="https://example.com">文档</a>"#));
    }

    #[test]
    fn html_embeds_versions_and_logo() {
        let html = render_html("0.1.0", "0.1.6", "改进若干");
        assert!(html.contains("0.1.6"));
        assert!(html.contains("0.1.0"));
        assert!(html.contains("data:image/png;base64,")); // logo 内嵌
        assert!(html.contains("tpupdate://install"));
        assert!(html.contains("tpupdate://later"));
        assert!(html.contains("改进若干"));
    }

    #[test]
    fn raw_html_in_notes_is_neutralized() {
        // notes 里夹原始 HTML/脚本:降级为文本被转义,不会以可执行标签出现。
        let html = render_notes("正常文本 <script>alert(1)</script> <img src=x onerror=alert(1)>");
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img src=x"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("正常文本"));
    }

    #[test]
    fn notes_have_scroll_mask() {
        let html = render_html("0.1.0", "0.1.6", "a\nb");
        assert!(html.contains("mask-image")); // scroll-mask 渐隐
        assert!(html.contains("--fade-top"));
        assert!(html.contains("overflow-y: auto"));
    }

    #[test]
    fn dialog_origin_recognized_cross_platform() {
        // macOS: tpupdater://localhost ; Windows: http(s)://tpupdater.localhost
        assert!(is_dialog_origin(&"tpupdater://localhost/".parse().unwrap()));
        assert!(is_dialog_origin(
            &"http://tpupdater.localhost/".parse().unwrap()
        ));
        assert!(is_dialog_origin(
            &"https://tpupdater.localhost/x".parse().unwrap()
        ));
        // 真实外链不算弹窗 origin(应被系统浏览器打开)。
        assert!(!is_dialog_origin(&"https://github.com/".parse().unwrap()));
    }
}
