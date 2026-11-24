use std::io::Write;

use clap::CommandFactory;

use crate::cli::args::{Args, CompletionShell};

const BASH: &str = r#"_horon_complete() {
    local context=horon token next words i item
    local current=${COMP_WORDS[COMP_CWORD]}
    for ((i=1; i<COMP_CWORD; i++)); do
        token=${COMP_WORDS[i]}
        next=$(_horon_subcommands "$context")
        case " $next " in *" $token "*) context="$context/$token" ;; esac
    done
    words=$(_horon_choices "$context")
    COMPREPLY=()
    for item in $words; do
        [[ $item == "$current"* ]] && COMPREPLY+=("$item")
    done
}
complete -F _horon_complete horon horon-agent hrn
"#;

const ZSH: &str = r#"_horon_complete() {
    local context=horon token next choices i
    for ((i=2; i<CURRENT; i++)); do
        token=${words[i]}
        next=$(_horon_subcommands "$context")
        case " $next " in *" $token "*) context="$context/$token" ;; esac
    done
    choices=$(_horon_choices "$context")
    compadd -- ${(s: :)choices}
}
compdef _horon_complete horon horon-agent hrn
"#;

pub(in crate::cli) fn write(shell: CompletionShell, mut out: impl Write) -> anyhow::Result<()> {
    let mut command = Args::command();
    command.build();
    let mut context = String::from("horon");
    match shell {
        CompletionShell::Bash | CompletionShell::Zsh => {
            for (function, subcommands_only) in [("_horon_subcommands", true), ("_horon_choices", false)] {
                writeln!(out, "{function}() {{\n    case \"$1\" in")?;
                walk(&command, &mut context, &mut |command, context| {
                    write!(out, "        '{context}') printf '%s\\n' '")?;
                    let mut first = true;
                    write_words(out_subcommands(command), &mut first, &mut out)?;
                    if !subcommands_only {
                        for value in out_possible_values(command) {
                            write_space_word(value.get_name(), &mut first, &mut out)?;
                        }
                        for opt in command.get_arguments().filter(|arg| !arg.is_hide_set()) {
                            if let Some(short) = opt.get_short() {
                                if !first {
                                    out.write_all(b" ")?;
                                }
                                first = false;
                                write!(out, "-{short}")?;
                            }
                            if let Some(long) = opt.get_long() {
                                if !first {
                                    out.write_all(b" ")?;
                                }
                                first = false;
                                write!(out, "--{long}")?;
                            }
                        }
                    }
                    writeln!(out, "' ;;")
                })?;
                writeln!(out, "    esac\n}}")?;
            }
            out.write_all(
                if matches!(shell, CompletionShell::Bash) {
                    BASH
                } else {
                    ZSH
                }
                .as_bytes(),
            )?;
        }
        CompletionShell::Fish => write_fish(&command, &mut context, &mut out)?,
    }
    Ok(())
}

fn write_fish(command: &clap::Command, context: &mut String, out: &mut impl Write) -> std::io::Result<()> {
    writeln!(
        out,
        "function __horon_at\n    set -l context horon\n    set -l tokens (commandline -opc)\n    for token in $tokens[2..-1]\n        switch \"$context/$token\""
    )?;
    walk(command, context, &mut |_, context| {
        if context != "horon" {
            writeln!(
                out,
                "            case '{context}'\n                set context \"$context/$token\""
            )?;
        }
        Ok(())
    })?;
    writeln!(out, "        end\n    end\n    test \"$context\" = \"$argv[1]\"\nend")?;
    walk(command, context, &mut |command, context| {
        let mut first = true;
        for name in out_subcommands(command) {
            if first {
                write!(out, "complete -c horon -n '__horon_at {context}' -f -a '")?;
            }
            write_space_word(name, &mut first, out)?;
        }
        for value in out_possible_values(command) {
            if first {
                write!(out, "complete -c horon -n '__horon_at {context}' -f -a '")?;
            }
            write_space_word(value.get_name(), &mut first, out)?;
        }
        if !first {
            writeln!(out, "'")?;
        }
        for arg in command.get_arguments().filter(|arg| !arg.is_hide_set()) {
            if let Some(short) = arg.get_short() {
                writeln!(out, "complete -c horon -n '__horon_at {context}' -s {short}")?;
            }
            if let Some(long) = arg.get_long() {
                writeln!(out, "complete -c horon -n '__horon_at {context}' -l {long}")?;
            }
        }
        Ok(())
    })?;
    writeln!(out, "complete -c horon-agent -w horon\ncomplete -c hrn -w horon")
}

fn walk(
    command: &clap::Command,
    context: &mut String,
    f: &mut impl FnMut(&clap::Command, &str) -> std::io::Result<()>,
) -> std::io::Result<()> {
    f(command, context)?;
    let prefix_len = context.len();
    for subcommand in command.get_subcommands().filter(|subcommand| !subcommand.is_hide_set()) {
        context.push('/');
        context.push_str(subcommand.get_name());
        walk(subcommand, context, f)?;
        context.truncate(prefix_len);
    }
    Ok(())
}

fn out_subcommands(command: &clap::Command) -> impl Iterator<Item = &str> {
    command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
        .map(clap::Command::get_name)
}

fn out_possible_values(command: &clap::Command) -> impl Iterator<Item = clap::builder::PossibleValue> + '_ {
    command
        .get_positionals()
        .filter(|arg| !arg.is_hide_set())
        .flat_map(|arg| arg.get_value_parser().possible_values().into_iter().flatten())
        .filter(|value| !value.is_hide_set())
}

fn write_space_word(word: &str, first: &mut bool, out: &mut impl Write) -> std::io::Result<()> {
    if !*first {
        out.write_all(b" ")?;
    }
    *first = false;
    out.write_all(word.as_bytes())
}

fn write_words<'a>(
    words: impl Iterator<Item = &'a str>,
    first: &mut bool,
    out: &mut impl Write,
) -> std::io::Result<()> {
    for word in words {
        write_space_word(word, first, out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn scripts_offer_run_options_and_shells() -> anyhow::Result<()> {
        for shell in [CompletionShell::Bash, CompletionShell::Zsh, CompletionShell::Fish] {
            let mut out = Vec::new();
            write(shell, &mut out)?;
            let script = String::from_utf8(out)?;
            assert!(script.contains("horon/run"));
            assert!(script.contains(if matches!(shell, CompletionShell::Fish) {
                "-l ndjson"
            } else {
                "--ndjson"
            }));
            assert!(script.contains("bash zsh fish"));
            assert!(!script.contains("temporary-server"));
        }
        Ok(())
    }

    #[test]
    fn unknown_shell_lists_valid_values() {
        let error = Args::try_parse_from(["horon", "completions", "unsupported"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
        let message = error.to_string();
        assert!(message.contains("bash, zsh, fish"));
        assert!(message.contains("--help"));
    }
}
