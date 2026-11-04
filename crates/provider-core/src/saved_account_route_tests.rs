use super::*;

#[test]
fn invalid_or_non_oauth_account_methods_are_not_accepted() {
    for method in [
        "claude-oauth@",
        "openai-oauth@work:bad",
        "openai-oauth@a@b",
        "openai-api-key@work",
        "claude-oauth@work\nlabel",
    ] {
        assert_eq!(oauth_account_label(method), None, "{method:?}");
    }
}
