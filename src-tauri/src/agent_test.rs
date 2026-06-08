use super::{parse_status_json, AgentStatus};

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
