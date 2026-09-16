//! Ask-then-initialize state for the two fact providers this module composes.
//!
//! A failed call is not evidence that a dependency has no configuration. Only an explicit
//! `unconfigured` answer licenses `init_defaults`; unreadable and early-startup replies must
//! be retried by a later assets call.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    Initialize,
    Settled,
    AskAgain,
}

pub fn next_step(status_json: &str) -> Next {
    let Ok(value) = serde_json::from_str::<Value>(status_json) else {
        return Next::AskAgain;
    };
    match value.get("state").and_then(Value::as_str) {
        Some("unconfigured") => Next::Initialize,
        Some("configured") => Next::Settled,
        _ => Next::AskAgain,
    }
}

/// `applied: false` is still success: another consumer initialized the dependency first.
pub fn reply_ok(raw: &str) -> bool {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| value.get("ok").and_then(Value::as_bool))
        == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_explicit_unconfigured_licenses_initialization() {
        assert_eq!(
            next_step(&json!({"ok":true,"state":"unconfigured"}).to_string()),
            Next::Initialize
        );
        assert_eq!(
            next_step(&json!({"ok":true,"state":"configured"}).to_string()),
            Next::Settled
        );
    }

    #[test]
    fn startup_failures_are_retried() {
        for raw in [
            "",
            "not json",
            "[]",
            "{}",
            r#"{"ok":false,"state":"unready"}"#,
            r#"{"ok":false,"error":"unauthorized"}"#,
            r#"{"state":"unknown"}"#,
        ] {
            assert_eq!(next_step(raw), Next::AskAgain, "{raw}");
        }
    }

    #[test]
    fn another_initializer_winning_is_settled() {
        assert!(reply_ok(
            &json!({"ok":true,"applied":false,"state":"configured"}).to_string()
        ));
        assert!(reply_ok(&json!({"ok":true,"applied":true}).to_string()));
        assert!(!reply_ok(
            &json!({"ok":false,"error":"unready"}).to_string()
        ));
        assert!(!reply_ok("not json"));
    }
}
