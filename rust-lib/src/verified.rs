use serde_json::Value;

#[derive(Debug)]
pub struct Answer {
    pub value: Value,
    pub route: Option<String>,
}

pub fn unwrap_answer(reply: &str) -> Result<Answer, String> {
    let value: Value = serde_json::from_str(reply).map_err(|e| e.to_string())?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(value.to_string());
    }
    Ok(Answer {
        value: value
            .get("result")
            .or_else(|| value.get("hash"))
            .cloned()
            .unwrap_or(Value::Null),
        route: value
            .get("route")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn rank(route: &str) -> u8 {
    match route {
        "verified" => 3,
        "proxied" => 2,
        "direct" => 1,
        _ => 0,
    }
}

pub fn fold_route<'a>(routes: impl IntoIterator<Item = Option<&'a str>>) -> String {
    routes
        .into_iter()
        .map(|route| route.unwrap_or("unknown"))
        .min_by_key(|route| rank(route))
        .filter(|route| rank(route) > 0)
        .unwrap_or("unknown")
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn structured_refusals_survive() {
        let refusal =
            r#"{"ok":false,"code":"verified_blocked","verifiedProxy":{"state":"wrong_chain"}}"#;
        assert_eq!(
            serde_json::from_str::<Value>(&unwrap_answer(refusal).unwrap_err()).unwrap()["code"],
            "verified_blocked"
        );
    }
    #[test]
    fn the_weakest_route_wins() {
        assert_eq!(fold_route([Some("verified"), Some("direct")]), "direct");
        assert_eq!(fold_route([Some("verified"), None]), "unknown");
    }
}
