use super::*;
use crate::{terminal_eprintln as eprintln, terminal_println as println};
use std::io::{BufRead, Write};

pub(super) async fn run_repl(agent: &mut Agent) -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    run_repl_with_io(agent, stdin.lock(), stdout.lock()).await
}

pub(super) async fn run_repl_with_io<R: BufRead, W: Write>(
    agent: &mut Agent,
    mut reader: R,
    mut writer: W,
) -> Result<()> {
    println!("horon-agent - Coding Agent");
    println!("Type your message, or 'quit' to exit.");

    // Show available skills
    let skills = agent.current_skills_snapshot();
    let skill_list = skills.list();
    if !skill_list.is_empty() {
        println!(
            "Available skills: {}",
            skill_list
                .iter()
                .map(|s| format!("/{}", s.name))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!();

    loop {
        write!(writer, "> ")?;
        writer.flush()?;

        let mut input = String::new();
        let bytes_read = reader.read_line(&mut input)?;
        if bytes_read == 0 {
            writeln!(writer)?;
            break;
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        if input == "quit" || input == "exit" {
            break;
        }

        if input == "clear" {
            agent.clear();
            println!("Conversation cleared.");
            continue;
        }

        // Check for skill invocation. Resolve against the registry (not
        // the bare tokenizer) so a `SKILL.md` `name:` field containing
        // spaces, e.g. "My Custom Skill", can still be matched: the
        // bare parse always stops at the first whitespace.
        if let Some(invocation) = skills.resolve_invocation(input) {
            if let Some(skill) = skills.get(invocation.name) {
                println!("Activating skill: {}", skill.name);
                println!("{}\n", skill.description);
                agent.active_skill = Some(invocation.name.to_string());
                if let Some(prompt) = invocation.prompt {
                    agent.arm_effort_keywords(prompt);
                    if let Err(e) = agent.run_once(prompt).await {
                        eprintln!("\nError: {}\n", e);
                    }
                    println!();
                }
                continue;
            } else {
                println!("Unknown skill: /{}", invocation.name);
                println!(
                    "Available: {}",
                    skills
                        .list()
                        .iter()
                        .map(|s| format!("/{}", s.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                continue;
            }
        }

        agent.arm_effort_keywords(input);
        if let Err(e) = agent.run_once(input).await {
            eprintln!("\nError: {}\n", e);
        }

        println!();
    }

    // Extract memories from session before exiting
    agent.extract_session_memories().await;

    Ok(())
}

impl Agent {
    /// Explicit one-shot turn failure when the final provider turn did not reach
    /// a clean completion (either truncated at the output token limit or stopped
    /// by a runtime/loop guardrail).
    pub fn one_shot_incomplete_failure(&self) -> Option<String> {
        if self.turn_state.outcome() == Some(TurnOutcome::Aborted) {
            return None;
        }
        if let Some(notice) = &self.last_guardrail_notice {
            return Some(notice.clone());
        }
        if self.turn_state.outcome() == Some(TurnOutcome::Length) {
            return Some(
                "Provider stopped early after hitting the output token limit (stop_reason: length).".to_string(),
            );
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::run_repl_with_io;
    use crate::agent::Agent;
    use crate::message::{Message, StreamEvent, ToolDefinition};
    use crate::provider::{EventStream, Provider};
    use crate::tool::Registry;
    use async_trait::async_trait;
    use futures::stream;
    use std::io::Cursor;
    use std::sync::Arc;

    struct DummyProvider;

    #[async_trait]
    impl Provider for DummyProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> anyhow::Result<EventStream> {
            Ok(Box::pin(stream::empty::<anyhow::Result<StreamEvent>>()))
        }

        fn name(&self) -> &'static str {
            "dummy"
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(Self)
        }
    }

    #[tokio::test]
    async fn repl_exits_cleanly_on_immediate_and_post_whitespace_eof() {
        let provider: Arc<dyn Provider> = Arc::new(DummyProvider);
        let registry = Registry::new(provider.clone()).await;
        let mut agent = Agent::for_memory_only_run(provider, registry);

        let mut out_empty = Vec::new();
        run_repl_with_io(&mut agent, Cursor::new(Vec::<u8>::new()), &mut out_empty)
            .await
            .expect("empty stdin EOF must exit cleanly");
        assert_eq!(String::from_utf8_lossy(&out_empty), "> \n");

        let mut out_blank = Vec::new();
        run_repl_with_io(&mut agent, Cursor::new(b"\n   \n".to_vec()), &mut out_blank)
            .await
            .expect("blank lines followed by EOF must exit cleanly");
        assert_eq!(String::from_utf8_lossy(&out_blank), "> > > \n");
    }
}
