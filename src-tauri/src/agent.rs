//! tp-agent 对接层：检测 / 状态解析 / spawn CLI。
//!
//! 设计原则（见 ADR §2.6 / §2.6.1）：
//! - 客户端不内嵌 tp-agent，通过「读文件 + spawn CLI」对接（M1 零新本地接口）。
//! - 检测「装没装」= 找可执行文件；「起没起」= `tp-agent status --json`（内部 pid+kill-0）。
//! - **绝不扫端口探测**——端口会变、扫到也认不出是不是 tp-agent。

use crate::config;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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

/// 帮装路径登记文件:<app_config_dir>/agent-path。
/// 帮装成功后把最终安装路径写进来,locate 优先读它 —— 回退安装目录(unix 的
/// ~/.local/bin、Windows 的 %LOCALAPPDATA%\Programs\tp-agent)通常不在 GUI 进程
/// 的 PATH 上,不登记就会出现"刚装好却报未安装"。
fn installed_path_file() -> Option<PathBuf> {
    config::app_config_dir().map(|d| d.join("agent-path"))
}

/// 登记安装路径。尽力而为:写失败不让安装失败 —— locate 的候选目录扫描兜底。
fn record_installed_path(path: &Path) {
    let Some(file) = installed_path_file() else {
        return;
    };
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&file, path.display().to_string());
}

/// 读登记的安装路径。文件不存在 / 路径已失效(用户手动删了二进制)都返回 None,
/// 落回 PATH + 候选目录扫描。
fn read_recorded_path() -> Option<PathBuf> {
    let content = std::fs::read_to_string(installed_path_file()?).ok()?;
    let p = PathBuf::from(content.trim());
    p.is_file().then_some(p)
}

/// 检测候选 = **所有安装候选目录** + 各平台常见自装位置(Homebrew、cargo install)。
/// 闭环由构造保证:安装到哪个候选目录,这里就一定能测到。此前两套手写列表不一致,
/// Windows / macOS 回退路径装完即报"未安装"(回归测试见
/// agent_test::locate_covers_every_install_dir)。
fn locate_candidates() -> Vec<PathBuf> {
    let name = tp_agent_bin_name();
    let mut v: Vec<PathBuf> = install_dir_candidates()
        .into_iter()
        .map(|d| d.join(name))
        .collect();
    #[cfg(not(target_os = "windows"))]
    {
        v.push(PathBuf::from("/opt/homebrew/bin").join(name));
        if let Some(home) = std::env::var_os("HOME") {
            v.push(PathBuf::from(home).join(".cargo").join("bin").join(name));
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            v.push(PathBuf::from(profile).join(".cargo").join("bin").join(name));
        }
    }
    v
}

/// 在 登记路径 / PATH / 标准位置 找 tp-agent 可执行文件。
/// 检测原则（见 ADR §2.6.1）：装没装 = 找可执行文件；起没起 = `status --json`（内部 pid+kill-0）。
/// 绝不扫端口探测——端口会变、扫到也认不出是不是 tp-agent。
pub fn locate_tp_agent() -> Option<PathBuf> {
    if let Some(p) = read_recorded_path() {
        return Some(p);
    }
    if let Ok(p) = which::which("tp-agent") {
        return Some(p);
    }
    locate_candidates().into_iter().find(|p| p.is_file())
}

/// spawn `tp-agent <args>` 并等退出。未安装 / spawn 失败 / 非零退出归一成可读错误
/// (带 stderr,tp-agent 的错误提示本身是给人看的中文)。
fn run_tp_agent(args: &[&str]) -> Result<std::process::Output, String> {
    let bin = locate_tp_agent().ok_or_else(|| "tp-agent not installed".to_string())?;
    let out = Command::new(&bin)
        .args(args)
        .output()
        .map_err(|e| format!("spawn tp-agent {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "tp-agent {} exited {}: {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out)
}

/// 运行 `tp-agent status --json` 并解析。tp-agent 不存在时返回 Err。
pub fn fetch_status() -> Result<AgentStatus, String> {
    let out = run_tp_agent(&["status", "--json"])?;
    let raw = String::from_utf8_lossy(&out.stdout);
    parse_status_json(&raw).map_err(|e| format!("parse status json: {e}"))
}

/// 启动 daemon(`tp-agent start`,自身幂等:已在跑则 no-op)。返回启动后状态。
pub fn start_daemon() -> Result<AgentStatus, String> {
    run_tp_agent(&["start"])?;
    fetch_status()
}

/// 停止 daemon(`tp-agent stop`)。返回停止后状态。
pub fn stop_daemon() -> Result<AgentStatus, String> {
    run_tp_agent(&["stop"])?;
    fetch_status()
}

/// 重启 daemon。多账号场景登录新账号后,已在跑的 daemon 要重启新账号才生效
/// (tp-agent 自身提示 `tp-agent stop && tp-agent start`)。
/// stop 失败不拦 start:daemon 本来没跑时 stop 报错属正常,重启语义只关心最后起来了。
pub fn restart_daemon() -> Result<AgentStatus, String> {
    let _ = run_tp_agent(&["stop"]);
    start_daemon()
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

/// 可执行文件名(Windows 带 .exe)。
fn tp_agent_bin_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "tp-agent.exe"
    } else {
        "tp-agent"
    }
}

/// 候选安装目录(按优先级)。优先系统级公共目录(在 PATH 上、tp-agent 自身能找到),
/// 不可写时回退到用户级目录,避免一上来就要 sudo / 管理员权限失败。
/// 各候选末项一定是用户级、当前用户必可写的目录,保证总能装上。
fn install_dir_candidates() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(target_os = "windows")]
    {
        // Windows: 用户级 %LOCALAPPDATA%\Programs\tp-agent(无需管理员)。
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Programs").join("tp-agent"));
        }
        if let Some(home) = std::env::var_os("USERPROFILE") {
            dirs.push(PathBuf::from(home).join(".tp-agent").join("bin"));
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        // unix: 先试系统级 /usr/local/bin(常在 PATH),不可写回退 ~/.local/bin。
        dirs.push(PathBuf::from("/usr/local/bin"));
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(home).join(".local").join("bin"));
        }
    }
    dirs
}

/// 把临时目录里的 tp-agent 二进制装到首个可写候选目录,返回最终路径。
/// 逐个候选试:创建目录 + copy,成功即登记路径并返回;全失败汇总错误(含手动安装提示)。
fn install_binary(bin_tmp: &Path) -> Result<PathBuf, String> {
    let name = tp_agent_bin_name();
    let mut errs = Vec::new();
    for dir in install_dir_candidates() {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            errs.push(format!("{}: 创建目录失败 {e}", dir.display()));
            continue;
        }
        let dest = dir.join(name);
        match std::fs::copy(bin_tmp, &dest) {
            Ok(_) => {
                record_installed_path(&dest);
                return Ok(dest);
            }
            Err(e) => errs.push(format!("{}: 写入失败 {e}", dest.display())),
        }
    }
    Err(format!(
        "无法安装 tp-agent(已尝试 {} 个目录均失败):\n{}\n可手动下载并放到 PATH 目录。",
        errs.len(),
        errs.join("\n")
    ))
}

/// 从 tar.gz 字节里取出 tp-agent 二进制到 bin_tmp(macOS / Linux)。
#[cfg(not(target_os = "windows"))]
fn extract_tp_agent(bytes: &[u8], bin_tmp: &std::path::Path) -> Result<(), String> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut ar = tar::Archive::new(gz);
    for entry in ar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        if path.file_name().and_then(|n| n.to_str()) == Some("tp-agent") {
            entry.unpack(bin_tmp).map_err(|e| e.to_string())?;
            return Ok(());
        }
    }
    Err("tp-agent binary not found in release asset".to_string())
}

/// 从 zip 字节里取出 tp-agent.exe 到 bin_tmp(Windows)。
#[cfg(target_os = "windows")]
fn extract_tp_agent(bytes: &[u8], bin_tmp: &std::path::Path) -> Result<(), String> {
    use std::io::{Read, Write};
    let reader = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| format!("open zip: {e}"))?;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
        let fname = std::path::Path::new(f.name())
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string());
        if fname.as_deref() == Some("tp-agent.exe") || fname.as_deref() == Some("tp-agent") {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
            let mut out = std::fs::File::create(bin_tmp).map_err(|e| e.to_string())?;
            out.write_all(&buf).map_err(|e| e.to_string())?;
            return Ok(());
        }
    }
    Err("tp-agent binary not found in release asset".to_string())
}

/// 下载超时:连接 15s;整个请求(含读 body)上限 10 分钟 —— 此前无超时,断网/挂起
/// 会把调用方永久吊死。10 分钟对慢网下载几 MB 的二进制足够宽裕。
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TOTAL_TIMEOUT: Duration = Duration::from_secs(600);

/// 下载 url 全部字节,按 chunk 回报进度 `(已下载, 总长)`(服务器没报 Content-Length
/// 时总长为 None)。带连接/总超时;预分配按 64MiB 封顶,防异常 Content-Length 撑爆内存。
fn download(url: &str, mut on_progress: impl FnMut(u64, Option<u64>)) -> Result<Vec<u8>, String> {
    use std::io::Read;

    let client = reqwest::blocking::Client::builder()
        .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
        .timeout(DOWNLOAD_TOTAL_TIMEOUT)
        .build()
        .map_err(|e| format!("init http client: {e}"))?;
    let mut resp = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("下载 {url} 失败: {e}"))?;

    let total = resp.content_length();
    let mut bytes = Vec::with_capacity(total.unwrap_or(0).min(64 * 1024 * 1024) as usize);
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = resp
            .read(&mut buf)
            .map_err(|e| format!("下载 {url} 中断: {e}"))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        on_progress(bytes.len() as u64, total);
    }
    Ok(bytes)
}

/// 帮装 tp-agent —— 跨平台(macOS arm64/x64 + Linux x64 + Windows x64)。
///
/// 流程:解析平台 asset → 下载(进度经 on_progress 回报)→ 解包(tar.gz / zip)→
/// 平台后处理(unix chmod;macOS 额外 ad-hoc codesign 绕 Gatekeeper)→ 装到首个
/// 可写目录(系统级优先,不可写回退用户级)+ 登记安装路径。
/// 不支持的平台/架构返回带具体平台信息的清晰错误。
///
/// 下载源:用户配置的镜像 agent_download_base 优先(大陆直连 GitHub 不稳,
/// 内网部署也用它),没配则用 GitHub Release 默认。
pub fn install_tp_agent(on_progress: impl FnMut(u64, Option<u64>)) -> Result<PathBuf, String> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    // 仅四个预编译组合有 release 资产;其余(如 macOS 之外的 arm64、32 位)无包。
    let asset = release_asset_name(os, arch)
        .map_err(|_| format!("暂无 {os}-{arch} 的 tp-agent 预编译包,请手动安装 tp-agent。"))?;
    let url = match config::resolve_agent_download_base() {
        Some(base) => format!("{base}/{asset}"),
        None => release_download_url("darkmice/talon-pilot-client", None, asset),
    };

    let tmp = std::env::temp_dir().join("tp-agent-install");
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let bytes = download(&url, on_progress)?;

    let bin_tmp = tmp.join(tp_agent_bin_name());
    extract_tp_agent(&bytes, &bin_tmp)?;

    // unix: chmod 755(可执行位)。Windows 无此概念。
    #[cfg(not(target_os = "windows"))]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin_tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }

    // macOS: 去隔离属性 + ad-hoc 重签,否则下载来的二进制被 Gatekeeper 杀。
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("xattr").args(["-c"]).arg(&bin_tmp).status();
        let sign = Command::new("codesign")
            .args(["-s", "-", "--force"])
            .arg(&bin_tmp)
            .status()
            .map_err(|e| format!("codesign: {e}"))?;
        if !sign.success() {
            return Err("codesign ad-hoc 签名失败".to_string());
        }
    }

    install_binary(&bin_tmp)
}

/// 用 api_key 驱动 tp-agent 完成 login + self-enroll（spawn CLI，复用现成逻辑）。
/// M1 用 `--key` 路线：WebView 内 OAuth 授权拿到 api_key 后交给这里。
/// 账号以客户端登录为准——`tp-agent login` 会把该账号设为 active（多账号场景见 ADR §3 M1.5）。
pub fn login_with_key(api_key: &str) -> Result<AgentStatus, String> {
    run_tp_agent(&["login", "--key", api_key])?;
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
///
/// 去重语义(用户 2026-06-09 拍板):客户端登录账户 Y == 在云端 OAuth 登录 Y(同一后端),
/// "登录哪个账户"由 WebView 里的实际授权决定。所以这里带 `--force` **绕过 tp-agent
/// 「本机有任意 enabled 账号就短路」的本地短路**(否则本机已有别的账号时根本走不到
/// OAuth、登录 Y 失败)。**不重复注册由云端 self-enroll 的机器幂等保证**:self-enroll
/// 按 (tenant_id, machine_id) 查已有 edge node,同机器同账户复用同一节点(返回 200、
/// 不新建),machine_id 在本机持久化稳定。即:授权登 Y → self-enroll 自动按机器去重。
/// `on_child_spawned`:子进程起来后回调一次,传 pid —— 让调用方(lib.rs)记下 pid,
/// 以便用户关授权窗 / 点取消时 kill 掉它(否则 tp-agent pair-poll 会一直轮询到自身
/// 超时,前端永久 loading)。kill 后本函数的 `child.wait()` 立即返回,流程干净 unwind。
pub fn login_with_browser<F, P>(
    api_base_url: Option<&str>,
    web_base_url: Option<&str>,
    on_auth_url: F,
    on_child_spawned: P,
) -> Result<AgentStatus, String>
where
    F: FnOnce(String),
    P: FnOnce(u32),
{
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let bin = locate_tp_agent().ok_or_else(|| "tp-agent not installed".to_string())?;
    let mut cmd = Command::new(&bin);
    cmd.args(["login", "--force", "--suppress-browser", "--print-auth-url"]);
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

    // 把 pid 交给调用方登记(用于取消)。
    on_child_spawned(child.id());

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
    let status = child
        .wait()
        .map_err(|e| format!("wait tp-agent login: {e}"))?;
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
        return Err(format!(
            "tp-agent login failed (exit {status}): {}",
            err.trim()
        ));
    }

    fetch_status()
}

#[cfg(test)]
#[path = "agent_test.rs"]
mod agent_test;
