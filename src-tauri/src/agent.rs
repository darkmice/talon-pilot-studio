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

#[cfg(test)]
#[path = "agent_test.rs"]
mod agent_test;
