//! 客户端配置 —— 后端云端地址的真相源(可被用户配置覆盖,为内网/私有部署预留)。
//!
//! 地址优先级(壳侧决议,注入给前端 window.__TP_API_BASE__):
//!   1. 环境变量 TP_API_BASE —— 开发/CI 临时覆盖。
//!   2. 用户配置文件 ~/.config/talon-pilot-studio/config.toml 的 `api_base` ——
//!      后续「设置页让用户填云端地址(内网/私有部署)」写这里,前端零改动。
//!   3. 壳内置默认 DEFAULT_API_BASE —— 兜底。
//!
//! 前端不内置任何具体地址:地址该由壳/用户配置决定,前端写死会绕过配置(用户要求)。

use std::path::PathBuf;

/// 壳内置默认云端地址(兜底)。用户可在配置文件覆盖,内网/私有部署改这里或填配置。
const DEFAULT_API_BASE: &str = "https://agents.deeplan.ai";

/// 用户配置文件路径:~/.config/talon-pilot-studio/config.toml
fn config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("talon-pilot-studio")
            .join("config.toml"),
    )
}

/// 从配置文件读 `api_base`(极简解析,不引 toml 依赖:逐行找 `api_base = "..."`)。
fn read_configured_api_base() -> Option<String> {
    let path = config_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    parse_api_base(&content)
}

/// 解析 config.toml 文本里的 `api_base = "<url>"`。抽纯函数便于单测。
pub fn parse_api_base(content: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("api_base") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let val = rest.trim().trim_matches('"').trim_matches('\'').trim();
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

/// 决议出要注入前端的后端地址:env > 用户配置 > 内置默认。
pub fn resolve_api_base() -> String {
    if let Ok(env) = std::env::var("TP_API_BASE") {
        let env = env.trim();
        if !env.is_empty() {
            return env.trim_end_matches('/').to_string();
        }
    }
    if let Some(cfg) = read_configured_api_base() {
        return cfg.trim_end_matches('/').to_string();
    }
    DEFAULT_API_BASE.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_api_base_from_toml() {
        let toml = "# comment\napi_base = \"https://corp.internal:3100\"\nother = 1\n";
        assert_eq!(
            parse_api_base(toml).as_deref(),
            Some("https://corp.internal:3100")
        );
    }

    #[test]
    fn ignores_commented_api_base() {
        let toml = "# api_base = \"https://nope\"\n";
        assert_eq!(parse_api_base(toml), None);
    }

    #[test]
    fn parses_single_quotes_and_trims() {
        let toml = "api_base = 'https://x.example/'  \n";
        assert_eq!(parse_api_base(toml).as_deref(), Some("https://x.example/"));
    }

    #[test]
    fn missing_api_base_returns_none() {
        assert_eq!(parse_api_base("foo = 1\nbar = 2\n"), None);
    }
}
