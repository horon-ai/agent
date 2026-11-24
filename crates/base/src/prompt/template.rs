//! Bounded system prompt templates for the one-shot CLI.
//!
//! A template is plain text with `{{name}}` placeholders. Rendering is a single
//! left-to-right pass over the template, so a value is never scanned again.
//! There is no file inclusion, environment access, command execution or
//! recursion: only the variables the caller passes in are available.
use anyhow::{Result, bail};

/// Largest template accepted, in bytes.
pub const MAX_TEMPLATE_BYTES: usize = 16 * 1024;

/// Variables a template may use, in the order they are documented.
pub const VARIABLES: [&str; 3] = ["cwd", "date", "model"];

/// Render `template`, replacing each `{{name}}` with its value in `values`.
pub fn render(template: &str, values: &[(&str, &str)]) -> Result<String> {
    if template.len() > MAX_TEMPLATE_BYTES {
        bail!(
            "system template is {} bytes; the limit is {MAX_TEMPLATE_BYTES}",
            template.len()
        );
    }
    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            bail!("system template has an unclosed '{{{{'");
        };
        let name = after[..end].trim();
        let Some((_, value)) = values.iter().find(|(key, _)| *key == name) else {
            bail!(
                "system template uses unknown variable '{name}'; supported: {}",
                VARIABLES.join(", ")
            );
        };
        output.push_str(value);
        rest = &after[end + 2..];
    }
    output.push_str(rest);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALUES: [(&str, &str); 3] = [("cwd", "/work"), ("date", "2026-11-22"), ("model", "m1")];

    #[test]
    fn each_variable_is_expanded_exactly_once() {
        let text = render("in {{cwd}} on {{ date }} with {{model}}", &VALUES).unwrap();
        assert_eq!(text, "in /work on 2026-11-22 with m1");
    }

    #[test]
    fn a_value_that_looks_like_a_placeholder_is_not_expanded_again() {
        let text = render("{{cwd}}", &[("cwd", "{{date}}")]).unwrap();
        assert_eq!(text, "{{date}}");
    }

    #[test]
    fn unknown_variables_unclosed_braces_and_oversized_templates_are_refused() {
        assert!(
            render("{{secret}}", &VALUES)
                .unwrap_err()
                .to_string()
                .contains("unknown variable 'secret'")
        );
        assert!(render("a {{cwd", &VALUES).is_err());
        assert!(render(&"x".repeat(MAX_TEMPLATE_BYTES + 1), &VALUES).is_err());
    }
}
