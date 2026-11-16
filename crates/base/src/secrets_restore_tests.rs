//! `restore` and `restore_tool_input` put back what the placeholders stand
//! for and leave everything else exactly as it came in.

use super::*;
use serde_json::json;

const OPENAI_KEY: &str = "sk-proj-abcdefghijklmnopqrstuvwxyz012345";
const GITHUB_KEY: &str = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123";

/// The restore rule, written out independently of the production code: every
/// placeholder, longest first and then by name, replaced one after another.
fn reference_restore(text: &str, vault: &SecretVault) -> String {
    let mut entries: Vec<(&String, &String)> = vault.reverse.iter().collect();
    entries.sort_by(|left, right| right.0.len().cmp(&left.0.len()).then_with(|| left.0.cmp(right.0)));
    let mut out = text.to_string();
    for (placeholder, secret) in entries {
        out = out.replace(placeholder.as_str(), secret);
    }
    out
}

fn reference_restore_value(value: &Value, vault: &SecretVault) -> Value {
    match value {
        Value::String(text) => Value::String(reference_restore(text, vault)),
        Value::Array(entries) => Value::Array(
            entries
                .iter()
                .map(|entry| reference_restore_value(entry, vault))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, entry)| (key.clone(), reference_restore_value(entry, vault)))
                .collect(),
        ),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
}

/// A vault holding exactly these placeholder → value pairs.
fn vault_of(pairs: &[(&str, &str)]) -> SecretVault {
    let mut vault = SecretVault::new();
    for (placeholder, secret) in pairs {
        vault.forward.insert((*secret).to_string(), (*placeholder).to_string());
        vault.reverse.insert((*placeholder).to_string(), (*secret).to_string());
    }
    vault
}

#[test]
fn restore_matches_the_rule_for_every_text_shape() {
    let mut vault = SecretVault::new();
    let openai = vault.placeholder_for(OPENAI_KEY, "openai_api_key");
    let github = vault.placeholder_for(GITHUB_KEY, "github");
    let texts = [
        String::new(),
        "ls -la src".to_string(),
        "가나다 🙂 no placeholder here".to_string(),
        openai.clone(),
        format!("export OPENAI_API_KEY={openai}"),
        format!("{openai}{github}"),
        format!("{github} then {openai} then {github} again"),
        format!("한글 {openai} 🙂 {github} 끝"),
        "«SECRET:unknown:0000000000» is not in the vault".to_string(),
        "«SECRET: an unfinished marker".to_string(),
        format!("{} and a stray « before {github}", &openai[..openai.len() - 2]),
    ];

    for text in &texts {
        assert_eq!(restore(text, &vault), reference_restore(text, &vault), "text: {text}");
    }
    assert_eq!(restore(&openai, &vault), OPENAI_KEY);
    assert_eq!(
        restore(&format!("{github} then {openai}"), &vault),
        format!("{GITHUB_KEY} then {OPENAI_KEY}")
    );
}

#[test]
fn an_empty_vault_returns_the_text_unchanged() {
    let vault = SecretVault::new();
    let text = "curl -H 'Authorization: «SECRET:github:abcdef»'";

    assert_eq!(restore(text, &vault), text);
}

#[test]
fn a_longer_placeholder_wins_over_its_prefix() {
    let vault = vault_of(&[("«SECRET:x", "short"), ("«SECRET:xy", "long")]);

    assert_eq!(restore("a «SECRET:xy b", &vault), "a long b");
    assert_eq!(restore("«SECRET:x «SECRET:xy", &vault), "short long");
    for text in ["a «SECRET:xy b", "«SECRET:x «SECRET:xy", "«SECRET:xyx«SECRET:x"] {
        assert_eq!(restore(text, &vault), reference_restore(text, &vault), "text: {text}");
    }
}

#[test]
fn a_restored_value_that_holds_a_placeholder_is_restored_too() {
    let vault = vault_of(&[("«SECRET:outer:1»", "A«SECRET:in»B"), ("«SECRET:in»", "Z")]);

    assert_eq!(restore("«SECRET:outer:1»", &vault), "AZB");
    assert_eq!(
        restore("x «SECRET:outer:1» y «SECRET:in»", &vault),
        reference_restore("x «SECRET:outer:1» y «SECRET:in»", &vault)
    );
}

#[test]
fn tool_input_restores_nested_strings_and_keeps_the_rest() {
    let session_id = "secrets-restore-test-nested";
    let _registration = register_session_vault(session_id);
    let (openai, github) = session_vault_with(session_id, |vault| {
        (
            vault.placeholder_for(OPENAI_KEY, "openai_api_key"),
            vault.placeholder_for(GITHUB_KEY, "github"),
        )
    })
    .expect("session vault");
    let vault = session_vault_with(session_id, |vault| vault.clone()).expect("session vault");

    let mut input = json!({
        "command": format!("curl -H 'Authorization: Bearer {openai}' https://example.test"),
        "plain": "no placeholder in this string",
        "timeout": 30,
        "background": false,
        "nothing": null,
        "env": { "GITHUB_TOKEN": github.clone(), "LANG": "ko_KR.UTF-8" },
        "args": ["--token", github.clone(), 7, [format!("{openai}:{github}")], { "deep": openai.clone() }],
    });
    // Object keys are not restored, only values.
    input[github.as_str()] = json!("a key that looks like a placeholder");
    let expected = reference_restore_value(&input, &vault);

    let restored = restore_tool_input(input, session_id);

    assert_eq!(restored, expected);
    assert_eq!(
        restored["command"],
        format!("curl -H 'Authorization: Bearer {OPENAI_KEY}' https://example.test")
    );
    assert_eq!(restored["plain"], "no placeholder in this string");
    assert_eq!(restored["timeout"], 30);
    assert_eq!(restored["env"]["GITHUB_TOKEN"], GITHUB_KEY);
    assert_eq!(restored["args"][1], GITHUB_KEY);
    assert_eq!(restored["args"][3][0], format!("{OPENAI_KEY}:{GITHUB_KEY}"));
    assert_eq!(restored["args"][4]["deep"], OPENAI_KEY);
    assert_eq!(restored[github.as_str()], "a key that looks like a placeholder");
}

#[test]
fn tool_input_without_a_placeholder_comes_back_equal() {
    let session_id = "secrets-restore-test-plain";
    let _registration = register_session_vault(session_id);
    session_vault_with(session_id, |vault| vault.placeholder_for(OPENAI_KEY, "openai_api_key")).expect("session vault");
    let input = json!({
        "file_path": "src/main.rs",
        "content": "x".repeat(10_000),
        "nested": [{ "text": "가나다" }, 1, null],
    });

    assert_eq!(restore_tool_input(input.clone(), session_id), input);
}

#[test]
fn tool_input_of_a_session_without_a_vault_comes_back_equal() {
    let input = json!({ "command": "echo «SECRET:github:abcdef»", "count": 2 });

    assert_eq!(
        restore_tool_input(input.clone(), "secrets-restore-test-no-vault"),
        input
    );
}
