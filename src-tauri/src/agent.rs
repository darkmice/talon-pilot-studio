//! tp-agent 对接层：检测 / 状态解析 / spawn CLI。
//!
//! 设计原则（见 ADR §2.6 / §2.6.1）：
//! - 客户端不内嵌 tp-agent，通过「读文件 + spawn CLI」对接（M1 零新本地接口）。
//! - 检测「装没装」= 找可执行文件；「起没起」= `tp-agent status --json`（内部 pid+kill-0）。
//! - **绝不扫端口探测**——端口会变、扫到也认不出是不是 tp-agent。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

/// `tp-agent status --json` 的输出（对应 pilot-agent 的 StatusJson）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub lifecycle: String,
    pub enrolled: bool,
    pub active_tenant_id: Option<String>,
    pub accounts_bound: usize,
    pub accounts_enabled: usize,
    pub last_error: Option<String>,
}

/// 解析 `tp-agent status --json` 的输出。
pub fn parse_status_json(raw: &str) -> Result<AgentStatus, serde_json::Error> {
    serde_json::from_str(raw.trim())
}

/// 在 PATH / 标准位置找 tp-agent 可执行文件。
/// 检测原则（见 ADR §2.6.1）：装没装 = 找可执行文件；起没起 = `status --json`（内部 pid+kill-0）。
/// 绝不扫端口探测——端口会变、扫到也认不出是不是 tp-agent。
pub fn locate_tp_agent() -> Option<PathBuf> {
    if let Ok(p) = which::which("tp-agent") {
        return Some(p);
    }
    for cand in [
        "/usr/local/bin/tp-agent",
        "/opt/homebrew/bin/tp-agent",
        // cargo install 默认位置
        "~/.cargo/bin/tp-agent",
    ] {
        let expanded = if let Some(stripped) = cand.strip_prefix("~/") {
            match std::env::var_os("HOME") {
                Some(home) => PathBuf::from(home).join(stripped),
                None => continue,
            }
        } else {
            PathBuf::from(cand)
        };
        if expanded.exists() {
            return Some(expanded);
        }
    }
    None
}

/// 运行 `tp-agent status --json` 并解析。tp-agent 不存在时返回 Err。
pub fn fetch_status() -> Result<AgentStatus, String> {
    let bin = locate_tp_agent().ok_or_else(|| "tp-agent not installed".to_string())?;
    let out = Command::new(&bin)
        .args(["status", "--json"])
        .output()
        .map_err(|e| format!("spawn tp-agent: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "tp-agent status exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    parse_status_json(&raw).map_err(|e| format!("parse status json: {e}"))
}

/// 与 talon-pilot updater.rs 的 github_asset() 命名保持一致。
pub fn release_asset_name(os: &str, arch: &str) -> Result<&'static str, String> {
    match (os, arch) {
        ("macos", "aarch64") => Ok("tp-agent-macos-arm64.tar.gz"),
        ("macos", "x86_64") => Ok("tp-agent-macos-x64.tar.gz"),
        ("linux", "x86_64") => Ok("tp-agent-linux-x64.tar.gz"),
        ("windows", "x86_64") => Ok("tp-agent-windows-x64.zip"),
        (o, a) => Err(format!("no prebuilt tp-agent asset for {o}-{a}")),
    }
}

pub fn release_download_url(repo: &str, version: Option<&str>, asset: &str) -> String {
    match version {
        Some(v) => {
            let v = v.trim().trim_start_matches('v');
            format!("https://github.com/{repo}/releases/download/v{v}/{asset}")
        }
        None => format!("https://github.com/{repo}/releases/latest/download/{asset}"),
    }
}

/// 帮装 tp-agent 到 /usr/local/bin（macOS arm64）。M1 只实现 macOS arm64，其余平台明确报错。
pub fn install_tp_agent() -> Result<PathBuf, String> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    if os != "macos" || arch != "aarch64" {
        return Err(format!(
            "M1 only supports macos-aarch64 auto-install; got {os}-{arch}. Install tp-agent manually."
        ));
    }
    let asset = release_asset_name(os, arch)?;
    let url = release_download_url("darkmice/talon-pilot-client", None, asset);

    let tmp = std::env::temp_dir().join("tp-agent-install");
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let bytes = reqwest::blocking::get(&url)
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .map_err(|e| format!("download {url}: {e}"))?;

    // 解 tar.gz，取出 tp-agent 二进制
    let gz = flate2::read::GzDecoder::new(&bytes[..]);
    let mut ar = tar::Archive::new(gz);
    let bin_tmp = tmp.join("tp-agent");
    let mut found = false;
    for entry in ar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        if path.file_name().and_then(|n| n.to_str()) == Some("tp-agent") {
            entry.unpack(&bin_tmp).map_err(|e| e.to_string())?;
            found = true;
            break;
        }
    }
    if !found {
        return Err("tp-agent binary not found in release asset".to_string());
    }

    // chmod 755 + ad-hoc 重签（否则 Gatekeeper 杀进程）
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bin_tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    let _ = Command::new("xattr").args(["-c"]).arg(&bin_tmp).status();
    let sign = Command::new("codesign")
        .args(["-s", "-", "--force"])
        .arg(&bin_tmp)
        .status()
        .map_err(|e| format!("codesign: {e}"))?;
    if !sign.success() {
        return Err("codesign ad-hoc failed".to_string());
    }

    // 装到 /usr/local/bin（需可写；不可写时报错让用户处理权限）
    let dest = PathBuf::from("/usr/local/bin/tp-agent");
    std::fs::copy(&bin_tmp, &dest).map_err(|e| format!("install to {}: {e}", dest.display()))?;
    Ok(dest)
}

/// 用 api_key 驱动 tp-agent 完成 login + self-enroll（spawn CLI，复用现成逻辑）。
/// M1 用 `--key` 路线：WebView 内 OAuth 授权拿到 api_key 后交给这里。
/// 账号以客户端登录为准——`tp-agent login` 会把该账号设为 active（多账号场景见 ADR §3 M1.5）。
pub fn login_with_key(api_key: &str) -> Result<AgentStatus, String> {
    let bin = locate_tp_agent().ok_or_else(|| "tp-agent not installed".to_string())?;
    let out = Command::new(&bin)
        .args(["login", "--key", api_key])
        .output()
        .map_err(|e| format!("spawn tp-agent login: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "tp-agent login failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    fetch_status()
}

/// tp-agent `login --print-auth-url` 会在 pairing 就绪时往 stdout 打一行
/// `TP_AUTH_URL=<url>`。从单行里提取 url；非该前缀行返回 None。
/// 抽成纯函数以便单测（流式读管道时逐行喂进来）。
pub fn extract_auth_url(line: &str) -> Option<String> {
    line.strip_prefix("TP_AUTH_URL=")
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
}

/// WebView 登录闭环(M1 完整 OAuth,无需用户手抄 api_key):
/// 1. spawn `tp-agent login --suppress-browser --print-auth-url`(stdout piped)
/// 2. 流式逐行读 stdout,匹配 `TP_AUTH_URL=` 拿到授权 URL,经 `on_auth_url` 回调交给调用方
///    (调用方在 Tauri WebView 新窗口打开它)
/// 3. tp-agent 自己的 loopback callback 完成授权 → pair-poll 取 api_key → self-enroll → 进程退出
/// 4. 等子进程退出,成功则 `fetch_status()` 返回 enrolled 状态
///
/// `on_auth_url` 在拿到 URL 时被调用一次;若进程结束都没打出 URL 则返回 Err。
///
/// `api_base_url` / `web_base_url`:云端地址,运行时由客户端注入(ADR §2.5 C2,不编译死)。
/// 传 `None` 时回落到 tp-agent 二进制内置默认(本地 dev = localhost,release = 线上)。
pub fn login_with_browser<F>(
    api_base_url: Option<&str>,
    web_base_url: Option<&str>,
    on_auth_url: F,
) -> Result<AgentStatus, String>
where
    F: FnOnce(String),
{
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let bin = locate_tp_agent().ok_or_else(|| "tp-agent not installed".to_string())?;
    let mut cmd = Command::new(&bin);
    cmd.args(["login", "--suppress-browser", "--print-auth-url"]);
    if let Some(api) = api_base_url {
        cmd.args(["--api-base-url", api]);
    }
    if let Some(web) = web_base_url {
        cmd.args(["--web-base-url", web]);
    }
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn tp-agent login: {e}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture tp-agent stdout".to_string())?;

    // 流式读 stdout,直到拿到 TP_AUTH_URL= 那行。拿到就开窗,然后继续读完
    // (子进程会一直输出到 OAuth 完成),最后 wait。
    let reader = BufReader::new(stdout);
    let mut auth_url_sent = false;
    let mut on_auth_url = Some(on_auth_url);
    for line in reader.lines() {
        let line = line.map_err(|e| format!("read tp-agent stdout: {e}"))?;
        if !auth_url_sent {
            if let Some(url) = extract_auth_url(&line) {
                if let Some(cb) = on_auth_url.take() {
                    cb(url);
                }
                auth_url_sent = true;
            }
        }
    }

    // stdout 关闭(进程即将/已退出),收集退出码 + stderr。
    let status = child.wait().map_err(|e| format!("wait tp-agent login: {e}"))?;
    if !auth_url_sent {
        let mut err = String::new();
        if let Some(mut se) = child.stderr.take() {
            use std::io::Read;
            let _ = se.read_to_string(&mut err);
        }
        return Err(format!(
            "tp-agent login 未输出授权 URL(exit {status}): {}",
            err.trim()
        ));
    }
    if !status.success() {
        let mut err = String::new();
        if let Some(mut se) = child.stderr.take() {
            use std::io::Read;
            let _ = se.read_to_string(&mut err);
        }
        return Err(format!("tp-agent login failed (exit {status}): {}", err.trim()));
    }

    fetch_status()
}

#[cfg(test)]
#[path = "agent_test.rs"]
mod agent_test;
