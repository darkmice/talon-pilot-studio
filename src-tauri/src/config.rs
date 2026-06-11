//! 客户端配置 —— 后端云端地址等的真相源(可被用户配置覆盖,为内网/私有部署预留)。
//!
//! 每个配置项的优先级(壳侧决议):
//!   1. 环境变量 —— 开发/CI 临时覆盖。
//!   2. 用户配置文件 config.toml(unix: ~/.config/talon-pilot-studio/,
//!      Windows: %APPDATA%\talon-pilot-studio\)——「设置页让用户填」写这里。
//!   3. 壳内置默认 —— 兜底。
//!
//! 支持的配置键:
//!   api_base            后端云端地址,注入前端 window.__TP_API_BASE__
//!   agent_download_base tp-agent 安装包下载基地址(镜像;默认 GitHub Release)
//!   update_endpoint     应用自更新 latest.json 地址(镜像;默认 tauri.conf.json 内置)
//!
//! 前端不内置任何具体地址:地址该由壳/用户配置决定,前端写死会绕过配置(用户要求)。

use std::path::PathBuf;

/// 壳内置默认云端地址(兜底)。用户可在配置文件覆盖,内网/私有部署改这里或填配置。
const DEFAULT_API_BASE: &str = "https://agents.deeplan.ai";

/// 应用配置目录:unix 用 ~/.config/talon-pilot-studio(与历史路径兼容),
/// Windows 用 %APPDATA%\talon-pilot-studio(原实现依赖 HOME,在 Windows 上整个
/// 配置功能静默失效)。config.toml 与安装路径登记等状态文件都放这里。
pub fn app_config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var_os("APPDATA")?;
        Some(PathBuf::from(appdata).join("talon-pilot-studio"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let home = std::env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join(".config")
                .join("talon-pilot-studio"),
        )
    }
}

/// 用户配置文件路径:<app_config_dir>/config.toml
fn config_path() -> Option<PathBuf> {
    Some(app_config_dir()?.join("config.toml"))
}

/// 从配置文件读指定键(极简解析,不引 toml 依赖:逐行找 `<key> = "..."`)。
fn read_configured(key: &str) -> Option<String> {
    let path = config_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    parse_config_value(&content, key)
}

/// 解析 config.toml 文本里的 `<key> = "<value>"`。抽纯函数便于单测。
/// 键名后必须是(可带空白的)`=`,所以更长键名(如 `api_base_x`)不会被 `api_base` 误中。
pub fn parse_config_value(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(rest) = rest.trim_start().strip_prefix('=') {
                let val = rest.trim().trim_matches('"').trim_matches('\'').trim();
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

/// 通用决议:env > 用户配置 > None。值统一去掉尾部 `/`。
fn resolve(env_key: &str, config_key: &str) -> Option<String> {
    if let Ok(env) = std::env::var(env_key) {
        let env = env.trim();
        if !env.is_empty() {
            return Some(env.trim_end_matches('/').to_string());
        }
    }
    read_configured(config_key).map(|v| v.trim_end_matches('/').to_string())
}

/// 决议出要注入前端的后端地址:env > 用户配置 > 内置默认。
pub fn resolve_api_base() -> String {
    resolve("TP_API_BASE", "api_base")
        .unwrap_or_else(|| DEFAULT_API_BASE.trim_end_matches('/').to_string())
}

/// tp-agent 安装包下载基地址(镜像)。None = 用 GitHub Release 默认。
/// 镜像目录结构约定:`<base>/<asset 文件名>` 直接可下(平铺存放四个平台资产)。
pub fn resolve_agent_download_base() -> Option<String> {
    resolve("TP_AGENT_DOWNLOAD_BASE", "agent_download_base")
}

/// 应用自更新 latest.json 地址(镜像)。None = 用 tauri.conf.json 内置 endpoint。
pub fn resolve_update_endpoint() -> Option<String> {
    resolve("TP_UPDATE_ENDPOINT", "update_endpoint")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_api_base_from_toml() {
        let toml = "# comment\napi_base = \"https://corp.internal:3100\"\nother = 1\n";
        assert_eq!(
            parse_config_value(toml, "api_base").as_deref(),
            Some("https://corp.internal:3100")
        );
    }

    #[test]
    fn ignores_commented_key() {
        let toml = "# api_base = \"https://nope\"\n";
        assert_eq!(parse_config_value(toml, "api_base"), None);
    }

    #[test]
    fn parses_single_quotes_and_trims() {
        let toml = "api_base = 'https://x.example/'  \n";
        assert_eq!(
            parse_config_value(toml, "api_base").as_deref(),
            Some("https://x.example/")
        );
    }

    #[test]
    fn missing_key_returns_none() {
        assert_eq!(parse_config_value("foo = 1\nbar = 2\n", "api_base"), None);
    }

    #[test]
    fn longer_key_is_not_prefix_matched() {
        // api_base_backup 不能被 api_base 误中;反向也取得到自己的值。
        let toml = "api_base_backup = \"https://backup\"\napi_base = \"https://main\"\n";
        assert_eq!(
            parse_config_value(toml, "api_base").as_deref(),
            Some("https://main")
        );
        assert_eq!(
            parse_config_value(toml, "api_base_backup").as_deref(),
            Some("https://backup")
        );
    }

    #[test]
    fn reads_each_supported_key_independently() {
        let toml = concat!(
            "api_base = \"https://a\"\n",
            "agent_download_base = \"https://mirror.example/tp-agent\"\n",
            "update_endpoint = \"https://mirror.example/studio/latest.json\"\n",
        );
        assert_eq!(
            parse_config_value(toml, "api_base").as_deref(),
            Some("https://a")
        );
        assert_eq!(
            parse_config_value(toml, "agent_download_base").as_deref(),
            Some("https://mirror.example/tp-agent")
        );
        assert_eq!(
            parse_config_value(toml, "update_endpoint").as_deref(),
            Some("https://mirror.example/studio/latest.json")
        );
    }
}
