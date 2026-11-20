//! Read a TypeScript union member for the wire-schema parity checks.

/// The object literal for one variant, including the fields of a named
/// interface when the TypeScript member intersects one.
pub(super) fn sdk_variant_block(sdk: &str, tag: &str, variant: &str) -> Option<String> {
    let needle = format!("{tag}: \"{variant}\"");
    let hit = sdk.find(&needle)?;
    let open = sdk[..hit].rfind('{')?;
    let mut depth = 0usize;
    for (offset, ch) in sdk[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let close = open + offset + 1;
                    let mut block = sdk[open..close].to_owned();
                    if let Some((name, _)) = sdk[close..]
                        .trim_start()
                        .strip_prefix("& ")
                        .and_then(|rest| rest.split_once(')'))
                        && let Some((_, declaration)) = sdk.split_once(&format!("interface {} {{", name.trim()))
                        && let Some((fields, _)) = declaration.split_once("\n}")
                    {
                        block.push_str(fields);
                    }
                    return Some(block);
                }
            }
            _ => {}
        }
    }
    None
}
