//! 「浏览器」连接器桌面侧:开关驱动 tp-agent relay,铺编译版扩展,装 native
//! messaging host。供 web-next 的「设置 → 连接器 → 浏览器」一键启用。
//!
//! 浏览器驱动本体跑在本机 edge(tp-agent relay + Chrome 扩展),桌面壳只做:
//!   1. enable/disable:写 tp-agent 开关标记并起 relay(`tp-agent browser enable`)。
//!   2. 铺扩展:把随 app 打包的编译版扩展拷到 app 数据目录,供用户「加载已解压」。
//!   3. 装 native host:写 Chrome NativeMessagingHosts manifest,指向 tp-agent 旁边
//!      的 tp-agent-browser-native-host 二进制(扩展靠它发现 relay 的 ws+token)。
//!   4. 状态:relay 是否在线 / 扩展是否已连(查 tp-agent)。
//!
//! 待发布管线补齐(见 docs/browser-connector-desktop.md):编译版扩展打包进 bundle
//! 资源、扩展 ID 固定(manifest "key")、native host 二进制随 tp-agent 发布。

use crate::agent;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::Manager;

/// 固定扩展 ID —— 需在 browser-extension/manifest.json 里用固定 "key" 锁定后填这里,
/// 否则 native host 的 allowed_origins 对不上,扩展无法调 native host 发现 relay。
/// 见 docs/browser-connector-desktop.md「扩展 ID 固定」。
const EXTENSION_ID: &str = "REPLACE_WITH_PINNED_EXTENSION_ID";
const NATIVE_HOST_NAME: &str = "com.talonpilot.browser_connector";

#[derive(Serialize)]
pub struct BrowserConnectorStatus {
    /// 用户是否已开启(tp-agent 开关标记存在)。
    pub enabled: bool,
    /// 本机 relay 是否在线。
    pub relay_live: bool,
    /// Chrome 扩展是否已连上 relay。
    pub extension_connected: bool,
    /// 已铺好的编译版扩展目录(供「加载已解压的扩展程序」)。
    pub extension_dir: Option<String>,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// tp-agent 浏览器开关标记(与 tp-agent 约定一致)。
fn enabled_marker() -> Option<PathBuf> {
    home().map(|h| h.join(".local/share/tp-agent/browser-relay.enabled"))
}

/// 跑 `tp-agent <args>`,best-effort(失败/未装返回 Err)。
fn run_tp(args: &[&str]) -> Result<std::process::Output, String> {
    let bin = agent::locate_tp_agent().ok_or_else(|| "tp-agent 未安装".to_string())?;
    Command::new(&bin)
        .args(args)
        .output()
        .map_err(|e| format!("spawn tp-agent {}: {e}", args.join(" ")))
}

/// 该 app 铺扩展的目标目录:<app_data_dir>/chrome-extension
/// (macOS = ~/Library/Application Support/<bundle id>/chrome-extension)。
pub fn extension_target_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|d| d.join("chrome-extension"))
}

fn relay_live() -> bool {
    let Ok(out) = run_tp(&["browser", "status", "--json"]) else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .ok()
        .and_then(|v| v.get("live").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

fn extension_connected() -> bool {
    // `tp-agent browser call browser.relay.health` → {ok, result:{connected,...}}
    let Ok(out) = run_tp(&["browser", "call", "browser.relay.health"]) else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .ok()
        .and_then(|v| {
            v.get("result")
                .and_then(|r| r.get("connected"))
                .and_then(|b| b.as_bool())
        })
        .unwrap_or(false)
}

pub fn status(extension_dir: Option<PathBuf>) -> BrowserConnectorStatus {
    BrowserConnectorStatus {
        enabled: enabled_marker().map(|p| p.exists()).unwrap_or(false),
        relay_live: relay_live(),
        extension_connected: extension_connected(),
        extension_dir: extension_dir
            .filter(|p| p.exists())
            .map(|p| p.display().to_string()),
    }
}

/// 开/关浏览器连接器。开:铺扩展 + 装 native host + `browser enable` + 起 daemon;
/// 关:`browser disable`。返回最新状态。
pub fn set_enabled(app: &tauri::AppHandle, enabled: bool) -> Result<BrowserConnectorStatus, String> {
    let target = extension_target_dir(app);
    if enabled {
        // 1. 铺编译版扩展(打包资源 → app 数据目录)。资源未打包时报清楚的错。
        if let Some(target) = target.as_ref() {
            let src = app
                .path()
                .resource_dir()
                .ok()
                .map(|d| d.join("chrome-extension"));
            match src {
                Some(src) if src.is_dir() => copy_dir_all(&src, target)?,
                _ => {
                    return Err(
                        "随 app 打包的浏览器扩展资源缺失(发布管线需打包 browser-extension 产物);\
                         见 docs/browser-connector-desktop.md"
                            .to_string(),
                    )
                }
            }
        }
        // 2. 装 native messaging host manifest(指向 tp-agent 旁的 native host 二进制)。
        install_native_host()?;
        // 3. 开 relay 开关 + 起 daemon(幂等)。
        run_tp(&["browser", "enable"])?;
        let _ = run_tp(&["start"]);
    } else {
        run_tp(&["browser", "disable"])?;
    }
    Ok(status(target))
}

/// 写 Chrome NativeMessagingHosts manifest,path 指向 tp-agent 同目录下的
/// tp-agent-browser-native-host(随 tp-agent 发布)。
fn install_native_host() -> Result<(), String> {
    let bin = agent::locate_tp_agent().ok_or_else(|| "tp-agent 未安装".to_string())?;
    let native_host = bin
        .parent()
        .map(|d| d.join(native_host_bin_name()))
        .ok_or_else(|| "无法定位 native host 二进制目录".to_string())?;
    let manifest = serde_json::json!({
        "name": NATIVE_HOST_NAME,
        "description": "Talon Pilot Browser Connector",
        "path": native_host.display().to_string(),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{EXTENSION_ID}/")],
    });
    let dir = native_messaging_hosts_dir()
        .ok_or_else(|| "无法定位 Chrome NativeMessagingHosts 目录".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let dest = dir.join(format!("{NATIVE_HOST_NAME}.json"));
    std::fs::write(&dest, serde_json::to_vec_pretty(&manifest).unwrap_or_default())
        .map_err(|e| format!("write native host manifest {}: {e}", dest.display()))?;
    Ok(())
}

fn native_host_bin_name() -> &'static str {
    if cfg!(windows) {
        "tp-agent-browser-native-host.exe"
    } else {
        "tp-agent-browser-native-host"
    }
}

/// Chrome 的 NativeMessagingHosts 目录(mac / linux;Windows 走注册表,暂未支持)。
fn native_messaging_hosts_dir() -> Option<PathBuf> {
    let home = home()?;
    #[cfg(target_os = "macos")]
    {
        Some(home.join("Library/Application Support/Google/Chrome/NativeMessagingHosts"))
    }
    #[cfg(target_os = "linux")]
    {
        Some(home.join(".config/google-chrome/NativeMessagingHosts"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = home;
        None
    }
}

/// 在 Chrome 打开 chrome://extensions(chrome:// 不能走 open_external 的 http 校验)。
pub fn open_chrome_extensions_page() -> Result<(), String> {
    let url = "chrome://extensions";
    #[cfg(target_os = "macos")]
    let r = Command::new("open").args(["-a", "Google Chrome", url]).spawn();
    #[cfg(target_os = "linux")]
    let r = Command::new("google-chrome").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let r = Command::new("cmd").args(["/C", "start", "chrome", url]).spawn();
    r.map(|_| ()).map_err(|e| format!("打开 Chrome 扩展页失败: {e}"))
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("create {}: {e}", dst.display()))?;
    for entry in std::fs::read_dir(src).map_err(|e| format!("read {}: {e}", src.display()))? {
        let entry = entry.map_err(|e| format!("read entry: {e}"))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| format!("copy {}: {e}", from.display()))?;
        }
    }
    Ok(())
}
