//! Source-shape checks for the generated-runtime boundary. The pure test build cannot compile
//! `glue.rs`, so these make its security and latency properties executable anyway.

const GLUE: &str = include_str!("../src/glue.rs");

fn code_only(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let (mut string, mut line_comment) = (false, false);
    while let Some(ch) = chars.next() {
        if line_comment {
            if ch == '\n' {
                line_comment = false;
                out.push(ch);
            } else {
                out.push(' ');
            }
        } else if string {
            match ch {
                '\\' => {
                    out.push(' ');
                    if chars.next().is_some() {
                        out.push(' ');
                    }
                }
                '"' => {
                    string = false;
                    out.push(' ');
                }
                '\n' => out.push('\n'),
                _ => out.push(' '),
            }
        } else if ch == '/' && chars.peek() == Some(&'/') {
            out.push(' ');
            out.push(' ');
            chars.next();
            line_comment = true;
        } else if ch == '"' {
            string = true;
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
}

fn method(source: &str, name: &str, next: &str) -> String {
    let implementations = source.find("impl EvmAssetsModule for").unwrap_or(0);
    let start = implementations
        + source[implementations..]
            .find(&format!("fn {name}("))
            .unwrap_or_else(|| panic!("missing {name}"));
    let rest = &source[start..];
    let end = rest.find(&format!("fn {next}(")).unwrap_or(rest.len());
    rest[..end].to_string()
}

#[test]
fn money_never_leaves_from_the_assets_module() {
    let code = code_only(GLUE);
    for forbidden in [
        "tx_sender_module",
        "keystore_module",
        "send_raw_transaction",
        "request_approval",
        "get_transaction_count",
    ] {
        assert!(
            !code.contains(forbidden),
            "assets glue contains forbidden money-moving capability: {forbidden}"
        );
    }
}

#[test]
fn outbound_requests_are_explicitly_bounded() {
    let code = code_only(GLUE);
    for unbounded in [
        ".config_status(",
        ".init_defaults(",
        ".list_chain_configs(",
        ".call(",
    ] {
        assert!(
            !code.contains(unbounded),
            "unbounded outbound call: {unbounded}"
        );
    }
    assert!(code.matches("_with_timeout(").count() >= 4);
}

#[test]
fn no_dependency_call_can_hide_under_a_lock() {
    let code = code_only(GLUE);
    for lock in [".lock()", ".read()", ".write()"] {
        assert!(!code.contains(lock), "glue acquired {lock}");
    }
}

#[test]
fn a_transfer_builds_one_call_and_checks_at_most_one_contract() {
    let body = method(GLUE, "build_transfer", "decorate_history");
    assert_eq!(body.matches("self.token_balance(").count(), 1);
    assert_eq!(body.matches("transfer::call(").count(), 1);
    assert!(body.contains("\"calls\":[call]"));
}

#[test]
fn refusals_are_relayed_as_json_objects() {
    let start = GLUE.find("fn err(").expect("missing err");
    let end = start
        + GLUE[start..]
            .find("fn ok_value(")
            .expect("missing ok_value");
    let body = &GLUE[start..end];
    assert!(body.contains("serde_json::from_str"));
    assert!(body.contains("value.to_string()"));
}

#[test]
fn only_the_declared_fact_providers_are_called() {
    let code = code_only(GLUE);
    let mut clients = Vec::new();
    for tail in code.split("modules().").skip(1) {
        clients.push(
            tail.chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                .collect::<String>(),
        );
    }
    clients.sort();
    clients.dedup();
    assert_eq!(clients, ["eth_rpc_module"]);
}

#[test]
fn the_caller_owns_token_membership() {
    for gone in ["token_list", "EvmAssetsModuleEvents"] {
        assert!(!GLUE.contains(gone), "assets glue still contains {gone}");
    }
}

#[test]
fn every_fact_read_retries_unsettled_dependency_initialization() {
    for (name, next) in [
        ("list_assets", "get_balances"),
        ("get_balances", "resolve_asset"),
        ("resolve_asset", "build_transfer"),
        ("build_transfer", "decorate_history"),
        ("decorate_history", "logos_module_install"),
    ] {
        let body = method(GLUE, name, next);
        assert!(
            body.contains("self.ensure_eth_rpc(&budget)"),
            "{name} does not retry dependency initialization"
        );
    }

    let startup = method(GLUE, "on_context_ready", "list_assets");
    assert!(startup.contains("self.ensure_eth_rpc(&budget)"));
}
