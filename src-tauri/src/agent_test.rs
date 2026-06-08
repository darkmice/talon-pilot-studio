use super::{
    extract_auth_url, parse_status_json, release_asset_name, release_download_url, AgentStatus,
};

#[test]
fn parses_status_json_running_enrolled() {
    let raw = r#"{"running":true,"pid":4321,"lifecycle":"running","enrolled":true,
        "active_tenant_id":"ten-1","accounts_bound":1,"accounts_enabled":1,"last_error":null}"#;
    let st: AgentStatus = parse_status_json(raw).expect("parse");
    assert!(st.running);
    assert!(st.enrolled);
    assert_eq!(st.pid, Some(4321));
    assert_eq!(st.active_tenant_id.as_deref(), Some("ten-1"));
}

#[test]
fn parses_status_json_not_running() {
    let raw = r#"{"running":false,"pid":null,"lifecycle":"stopped","enrolled":false,
        "active_tenant_id":null,"accounts_bound":0,"accounts_enabled":0,"last_error":null}"#;
    let st: AgentStatus = parse_status_json(raw).expect("parse");
    assert!(!st.running);
    assert!(!st.enrolled);
    assert_eq!(st.pid, None);
    assert_eq!(st.accounts_bound, 0);
}

#[test]
fn parse_status_json_rejects_garbage() {
    assert!(parse_status_json("not json").is_err());
}

#[test]
fn macos_arm64_asset_name() {
    assert_eq!(
        release_asset_name("macos", "aarch64").unwrap(),
        "tp-agent-macos-arm64.tar.gz"
    );
}

#[test]
fn unsupported_platform_asset_errs() {
    assert!(release_asset_name("freebsd", "riscv").is_err());
}

#[test]
fn latest_download_url() {
    let url = release_download_url(
        "darkmice/talon-pilot-client",
        None,
        "tp-agent-macos-arm64.tar.gz",
    );
    assert_eq!(
        url,
        "https://github.com/darkmice/talon-pilot-client/releases/latest/download/tp-agent-macos-arm64.tar.gz"
    );
}

#[test]
fn versioned_download_url_strips_v_prefix() {
    let url = release_download_url("r/x", Some("v0.1.2"), "a.tar.gz");
    assert_eq!(url, "https://github.com/r/x/releases/download/v0.1.2/a.tar.gz");
}

#[test]
fn extract_auth_url_picks_prefixed_line() {
    assert_eq!(
        extract_auth_url("TP_AUTH_URL=https://app.example/agent-auth?user_code=AB12"),
        Some("https://app.example/agent-auth?user_code=AB12".to_string())
    );
}

#[test]
fn extract_auth_url_trims_trailing_whitespace() {
    assert_eq!(
        extract_auth_url("TP_AUTH_URL=https://x/y\r\n"),
        Some("https://x/y".to_string())
    );
}

#[test]
fn extract_auth_url_ignores_other_lines() {
    assert_eq!(extract_auth_url("打开浏览器完成 Talon Pilot 登录授权 ..."), None);
    assert_eq!(extract_auth_url("pair-init ok"), None);
    assert_eq!(extract_auth_url(""), None);
}
