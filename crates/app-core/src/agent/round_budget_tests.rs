//! Unit tests for the round-owned context budget.

use super::*;
use crate::tool::ToolOutput;

const TINY_BUDGET: usize = 400;
const TINY_THRESHOLD: usize = 360;

fn output(chars: usize) -> ToolOutput {
    ToolOutput::new("x".repeat(chars))
}

/// Characters that price at about `tokens` tokens. The estimator covers 4.87
/// ASCII characters per token rather than four, so a size stated in tokens
/// converts back with that rate.
const fn chars_in(tokens: usize) -> usize {
    (tokens * 487).div_ceil(100)
}

/// A budget on `TINY_BUDGET` seeded past the guard's overhead floor, which
/// `estimate_compaction_tokens_from_chars` only adds from half the default
/// budget up, so the seed here is exactly what the test sets.
fn budget_with_seed(seed: usize) -> RoundTokenBudget {
    let mut budget = RoundTokenBudget::with_seed(TINY_BUDGET, seed);
    budget.threshold_tokens = TINY_THRESHOLD;
    budget
}

#[test]
fn first_output_is_admitted_and_the_second_that_crosses_is_refused() {
    let mut budget = budget_with_seed(200);

    let first = budget.admit("read", output(chars_in(100)));
    assert_eq!(budget.current_tokens(), 300, "an admitted output is counted");
    assert!(!first.output.contains("CONTEXT LIMIT REACHED"), "{}", first.output);

    let second = budget.admit("read", output(chars_in(100)));
    assert!(
        second.output.contains("CONTEXT LIMIT REACHED"),
        "200 + 100 + 100 crosses the 360 ceiling and must be refused: {}",
        second.output
    );
    assert!(
        second.output.contains("accept_large_output"),
        "the refusal names the flag that would return what still fits: {}",
        second.output
    );
    assert!(
        !second.output.contains("does not apply here"),
        "room remains, so the exhausted sentence's claim would be false: {}",
        second.output
    );
    assert!(
        second.output.contains("/compact"),
        "the refusal carries the tool layer's remedy: {}",
        second.output
    );
}

#[test]
fn a_zero_token_output_at_the_ceiling_is_admitted_like_the_tool_layer() {
    // The tool layer only acts once `projected > threshold`, so an output that
    // lands exactly on the ceiling is admitted there. The round must not be one
    // token stricter. The estimator never returns zero - an empty output still
    // costs the token boundary it occupies - so the seed sits one token below
    // the ceiling and the empty output is what fills it.
    let mut budget = budget_with_seed(TINY_THRESHOLD - 1);
    let admitted = budget.admit("read", ToolOutput::new(""));
    assert_eq!(budget.current_tokens(), TINY_THRESHOLD);
    assert!(
        !admitted.output.contains("CONTEXT LIMIT REACHED"),
        "an output that lands exactly on the ceiling is admitted: {}",
        admitted.output
    );
}

/// A budget and seed large enough that the single-output cap does not mask the
/// arithmetic: 20k budget, 18k ceiling, 15k already spent.
const PARITY_BUDGET: usize = 20_000;
const PARITY_SEED: usize = 15_000;

struct FixedPayload {
    chars: usize,
}

#[async_trait::async_trait]
impl crate::tool::Tool for FixedPayload {
    fn name(&self) -> &str {
        "fixed_payload"
    }

    fn description(&self) -> &str {
        "fixed payload for the round-budget parity test"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn execute(&self, _: serde_json::Value, _: crate::tool::ToolContext) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::new("x".repeat(self.chars)))
    }
}

fn tool_context() -> crate::tool::ToolContext {
    crate::tool::ToolContext {
        session_id: "round-budget-parity".into(),
        message_id: "msg-parity".into(),
        tool_call_id: "call-parity".into(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    }
}

/// Registry on the parity budget with a fixed payload tool, so both layers can
/// be driven at the same measured context size.
async fn parity_registry(chars: usize) -> Registry {
    parity_registry_at(chars, PARITY_SEED).await
}

/// Same registry with an explicit observed count, so a path can be driven with
/// the round's room untouched (`used = 0`).
async fn parity_registry_at(chars: usize, used: usize) -> Registry {
    let registry = Registry::empty();
    {
        let compaction = registry.compaction();
        let mut manager = compaction.write().await;
        manager.set_budget(PARITY_BUDGET);
        manager.update_observed_input_tokens(used as u64);
    }
    registry
        .register("fixed_payload".to_string(), Arc::new(FixedPayload { chars }))
        .await;
    registry
}

/// Head and tail of a verdict, so a QA dump shows both the payload decision
/// and the notice the model reads without printing thousands of filler chars.
fn qa_excerpt(text: &str) -> String {
    let head: String = text.chars().take(40).collect();
    let tail: String = text.chars().rev().take(240).collect::<String>().chars().rev().collect();
    format!("{head:?} ... {tail:?}")
}

/// The same tool call must get the same verdict whether it runs alone or shares
/// a round. The tool layer is driven through the real `Registry::execute` path,
/// so its `accept_large_output` handling is the reference and not a copy of it.
#[tokio::test]
async fn an_opted_in_output_gets_the_same_verdict_alone_and_in_a_group() {
    // 30k chars is ~6.2k tokens: too much for the 3k that remains, so the tool
    // layer has to truncate it when the caller opts in.
    let registry = parity_registry(30_000).await;

    // The reference verdict: the caller opted in, so the tool layer returns the
    // prefix that fits instead of withholding the payload.
    let alone = registry
        .execute(
            "fixed_payload",
            serde_json::json!({"accept_large_output": true}),
            tool_context(),
        )
        .await
        .expect("the tool runs");
    assert!(
        alone.output.contains("OUTPUT TRUNCATED"),
        "the tool layer must truncate an opted-in output: {}",
        qa_excerpt(&alone.output)
    );

    // Alone in a round: content, not a refusal.
    let mut solo_round = RoundTokenBudget::with_seed(PARITY_BUDGET, PARITY_SEED);
    let solo = solo_round.admit("fixed_payload", alone.clone());
    assert!(
        solo.output.starts_with('x'),
        "alone, the opted-in payload survives: {}",
        qa_excerpt(&solo.output)
    );
    assert!(
        !solo.output.contains("CONTEXT LIMIT REACHED"),
        "alone, the round must not refuse what the tool layer truncated: {}",
        qa_excerpt(&solo.output)
    );

    // Sharing a round with another 1k-token output: still content, because the
    // caller asked to pay and room remains.
    let mut shared_round = RoundTokenBudget::with_seed(PARITY_BUDGET, PARITY_SEED);
    let filler = shared_round.admit("read", ToolOutput::new("y".repeat(chars_in(1_000))));
    assert!(
        !filler.output.contains("CONTEXT LIMIT REACHED"),
        "the filler fits: {}",
        qa_excerpt(&filler.output)
    );
    let shared = shared_round.admit("fixed_payload", alone.clone());
    assert!(
        shared.output.starts_with('x'),
        "in a group, the same opted-in call must also return content: {}",
        qa_excerpt(&shared.output)
    );
    assert!(
        !shared.output.contains("CONTEXT LIMIT REACHED"),
        "in a group, the round must not refuse what it would truncate alone: {}",
        qa_excerpt(&shared.output)
    );
    assert!(
        shared.output.contains("OUTPUT TRUNCATED"),
        "the truncated result keeps the tool layer's notice: {}",
        qa_excerpt(&shared.output)
    );
    assert_eq!(
        shared.output.matches("OUTPUT TRUNCATED").count(),
        1,
        "the round replaces the tool layer's notice instead of stacking a second: {}",
        qa_excerpt(&shared.output)
    );

    // The parity dump: the same call, alone and in a group, and what the model
    // reads in each case.
    std::println!(
        "QA defect A budget={PARITY_BUDGET} threshold={} seed={PARITY_SEED} payload_tokens={}",
        Registry::context_guard_threshold_tokens(PARITY_BUDGET),
        crate::util::estimate_tokens(&alone.output)
    );
    std::println!("QA tool layer, call alone  : {}", qa_excerpt(&alone.output));
    std::println!("QA round, call alone       : {}", qa_excerpt(&solo.output));
    std::println!("QA round, call in a group  : {}", qa_excerpt(&shared.output));
}

/// The cumulative case this counter exists for, in both directions: an output
/// that the tool layer admitted on its own no longer fits once the round has
/// spent part of the window. Without the flag the round withholds it and says
/// the flag exists; with the flag it returns the prefix that fits. The two
/// outcomes differ only by the flag, never by whether the call had company.
#[tokio::test]
async fn the_cumulative_case_honors_the_flag_the_same_way_the_tool_layer_does() {
    // 10k chars is ~2.05k tokens: it fits the 3k that remains, so the tool layer
    // admits it whole when the call runs alone.
    let registry = parity_registry(10_000).await;

    // Alone, the tool layer admits this payload whole: it fits the ceiling at
    // the count the tool layer can see.
    let plain = registry
        .execute("fixed_payload", serde_json::json!({}), tool_context())
        .await
        .expect("the tool runs");
    assert!(
        plain.output.starts_with('x') && !plain.output.contains("OUTPUT WITHHELD"),
        "the tool layer admits this payload on its own: {}",
        qa_excerpt(&plain.output)
    );

    // Same call with the flag, same size: the tool layer admits it too.
    let opted_in = registry
        .execute(
            "fixed_payload",
            serde_json::json!({"accept_large_output": true}),
            tool_context(),
        )
        .await
        .expect("the tool runs");
    assert!(
        plain.output.len() == opted_in.output.len(),
        "the flag does not change a call that already fits"
    );

    let plain_len = plain.output.len();

    // In a round, after a 1k-token sibling, the same output no longer fits.
    let mut without_flag = RoundTokenBudget::with_seed(PARITY_BUDGET, PARITY_SEED);
    without_flag.admit("read", ToolOutput::new("y".repeat(chars_in(1_000))));
    let withheld = without_flag.admit("fixed_payload", plain);

    let mut with_flag = RoundTokenBudget::with_seed(PARITY_BUDGET, PARITY_SEED);
    with_flag.admit("read", ToolOutput::new("y".repeat(chars_in(1_000))));
    let truncated = with_flag.admit("fixed_payload", opted_in);

    std::println!("QA round, no flag          : {}", qa_excerpt(&withheld.output));
    std::println!("QA round, flag passed      : {}", qa_excerpt(&truncated.output));

    assert!(
        withheld.output.contains("CONTEXT LIMIT REACHED"),
        "without the flag the round withholds the payload: {}",
        qa_excerpt(&withheld.output)
    );
    assert!(
        !withheld.output.contains(&"x".repeat(64)),
        "nothing of the withheld payload is spent: {}",
        qa_excerpt(&withheld.output)
    );
    assert!(
        withheld.output.contains("accept_large_output"),
        "the round names the flag that would return what still fits: {}",
        qa_excerpt(&withheld.output)
    );
    assert!(
        !withheld.output.contains("does not apply here"),
        "room remains, so the flag does apply: {}",
        qa_excerpt(&withheld.output)
    );
    assert!(
        withheld.output.contains("/compact"),
        "the tool layer's remedy stays: {}",
        qa_excerpt(&withheld.output)
    );

    assert!(
        truncated.output.starts_with('x'),
        "with the flag the round returns the payload it can afford: {}",
        qa_excerpt(&truncated.output)
    );
    assert!(
        !truncated.output.contains("CONTEXT LIMIT REACHED"),
        "an opted-in caller is not refused while room remains: {}",
        qa_excerpt(&truncated.output)
    );
    assert!(
        truncated.output.contains("OUTPUT TRUNCATED"),
        "the truncation notice names the flag the caller passed: {}",
        qa_excerpt(&truncated.output)
    );
    assert!(
        truncated.output.len() < plain_len,
        "the truncated result is shorter than the whole payload: {}",
        truncated.output.len()
    );
}

#[test]
fn a_seed_at_or_past_the_threshold_refuses_every_output() {
    for seed in [TINY_THRESHOLD, TINY_THRESHOLD + 10_000] {
        let mut budget = budget_with_seed(seed);
        for _ in 0..3 {
            let refused = budget.admit("read", output(4));
            assert!(
                refused.output.contains("CONTEXT LIMIT REACHED"),
                "seed {seed} is already spent, so every output is refused: {}",
                refused.output
            );
        }
    }
}

#[test]
fn the_counter_survives_a_group_boundary() {
    let mut budget = budget_with_seed(350);

    let first = budget.admit("bash", output(chars_in(10)));
    assert!(!first.output.contains("CONTEXT LIMIT REACHED"), "{}", first.output);
    assert_eq!(
        budget.current_tokens(),
        360,
        "the first group lands exactly on the ceiling"
    );

    let second = budget.admit("bash", output(4));
    assert!(
        second.output.contains("CONTEXT LIMIT REACHED"),
        "the second group starts from the first group's total, not from the seed: {}",
        second.output
    );
}

#[test]
fn a_lone_output_under_the_threshold_is_untouched() {
    let mut budget = budget_with_seed(100);
    let admitted = budget.admit("read", output(chars_in(10)));
    assert_eq!(admitted.output, "x".repeat(chars_in(10)));
    assert_eq!(budget.current_tokens(), 110);
}

#[test]
fn a_zero_budget_disables_the_guard() {
    let mut budget = RoundTokenBudget::with_seed(0, 1_000_000);
    let admitted = budget.admit("read", output(4_000));
    assert_eq!(admitted.output, "x".repeat(4_000));
}

/// An output the tool layer already refused is a verdict, not content. It
/// carries no payload, and its sentence is what the model must act on, so the
/// round must pass it through instead of truncating it or restating it at the
/// round's total.
#[tokio::test]
async fn an_opted_in_call_refused_by_the_tool_layer_reaches_the_model_intact() {
    let registry = Registry::empty();
    {
        let compaction = registry.compaction();
        let mut manager = compaction.write().await;
        manager.set_budget(10_000);
        manager.update_observed_input_tokens(9_990);
    }
    registry
        .register("fixed_payload".to_string(), Arc::new(FixedPayload { chars: 400_000 }))
        .await;

    let refused = registry
        .execute(
            "fixed_payload",
            serde_json::json!({"accept_large_output": true}),
            tool_context(),
        )
        .await
        .expect("the tool runs");
    std::println!(
        "QA tool layer refusal (marked as content: {}):\n---\n{}\n---",
        Registry::accept_large_output_was_passed(&refused),
        refused.output
    );

    let mut round = RoundTokenBudget::with_seed(10_000, 9_990);
    let admitted = round.admit("fixed_payload", refused.clone());
    std::println!("QA model receives:\n---\n{}\n---", admitted.output);

    assert_eq!(
        admitted.output, refused.output,
        "the refusal must arrive intact, not truncated or restated"
    );
    assert!(
        admitted.output.contains("does not apply here"),
        "the actionable sentence survives: {}",
        qa_excerpt(&admitted.output)
    );
    assert_eq!(
        round.current_tokens(),
        9_990 + crate::util::estimate_tokens(&refused.output),
        "the refusal costs what it says it costs"
    );
}

/// Todo 35: an SDK-answered result is an output this round applies, so it takes
/// the same decision a tool result does - including the protocol ceiling, which
/// used to be the only bound on this path.
#[test]
fn an_sdk_result_is_capped_and_admitted_like_a_tool_result() {
    let mut round = budget_with_seed(200);

    // Under the round's ceiling: admitted whole and counted.
    let small = round.admit_sdk_result(
        "test-session",
        "remote_job",
        &serde_json::json!({}),
        "s".repeat(chars_in(100)),
        false,
    );
    assert_eq!(small, "s".repeat(chars_in(100)));
    assert_eq!(round.current_tokens(), 300, "the applied SDK result is counted");

    // Over the round's ceiling: withheld with the tool layer's sentence, and
    // the withheld payload is not spent.
    let withheld = round.admit_sdk_result(
        "test-session",
        "remote_job",
        &serde_json::json!({}),
        "x".repeat(chars_in(100)),
        false,
    );
    std::println!("QA sdk result through a round near its ceiling:\n{withheld}");
    assert!(
        withheld.contains("CONTEXT LIMIT REACHED"),
        "the second SDK result crosses the ceiling and must be withheld: {withheld}"
    );
    assert!(
        !withheld.contains(&"x".repeat(64)),
        "nothing of the withheld SDK payload is spent: {withheld}"
    );
    assert_eq!(
        round.current_tokens(),
        300 + crate::util::estimate_tokens(&withheld),
        "the refusal costs what it says it costs"
    );
}

/// The flag has to be read from the call on this path: no tool layer ran to mark
/// the output, so without it the round would withhold a payload and advise a
/// retry with a flag the same call already carried.
#[test]
fn an_sdk_caller_that_passed_accept_large_output_gets_what_fits() {
    // Seeded at the ceiling's edge so the payload does not fit whole and room
    // still remains: the branch the flag exists for.
    let mut round = budget_with_seed(300);
    let opted_in = serde_json::json!({ "accept_large_output": true });

    let truncated = round.admit_sdk_result("test-session", "remote_job", &opted_in, "x".repeat(400), false);
    std::println!("QA opted-in sdk result:\n{truncated}");
    assert!(
        truncated.starts_with('x'),
        "an opted-in SDK result returns the prefix that fits: {truncated}"
    );
    assert!(
        truncated.contains("OUTPUT TRUNCATED"),
        "it carries the tool layer's truncation notice: {truncated}"
    );
    assert!(
        !truncated.contains("CONTEXT LIMIT REACHED"),
        "room remains, so the opt-in is honored instead of refused: {truncated}"
    );

    // The withheld path is the contrast: same round, same payload, no flag.
    let mut without_flag = budget_with_seed(300);
    let withheld = without_flag.admit_sdk_result(
        "test-session",
        "remote_job",
        &serde_json::json!({}),
        "x".repeat(400),
        false,
    );
    assert!(
        withheld.contains("CONTEXT LIMIT REACHED") && !withheld.starts_with('x'),
        "without the flag the same SDK payload is withheld: {withheld}"
    );

    // The string spelling the tool layer also honors, so the two paths cannot
    // disagree about what the flag accepts.
    let mut string_flag = budget_with_seed(300);
    let accepted = string_flag.admit_sdk_result(
        "test-session",
        "remote_job",
        &serde_json::json!({ "accept_large_output": "true" }),
        "x".repeat(400),
        false,
    );
    assert_eq!(accepted, truncated, "both spellings mean the same thing");
}

/// The protocol ceiling still applies before the round's own rule: an SDK result
/// past 512K characters is truncated there first, so a round with a huge budget
/// never stores the whole payload.
#[test]
fn an_sdk_result_past_the_protocol_ceiling_is_capped_first() {
    let mut round = RoundTokenBudget::with_seed(0, 0);
    let huge = "y".repeat(crate::agent::tools::MAX_TOOL_OUTPUT_BYTES_FOR_HISTORY + 10);

    let capped = round.admit_sdk_result("", "remote_job", &serde_json::json!({}), huge, false);
    std::println!("QA protocol ceiling: kept {} chars", capped.chars().count());
    assert!(
        capped.chars().count() < crate::agent::tools::MAX_TOOL_OUTPUT_BYTES_FOR_HISTORY + 1_000,
        "the protocol ceiling still bounds the payload: {} chars",
        capped.chars().count()
    );
    assert!(capped.contains("Tool output truncated by horon-agent"));
}

/// An SDK error result is a verdict, not content: the provider's failure
/// sentence is what the model must read to act, so the round must not replace it
/// with a context refusal. It is still counted, so the round's total stays
/// honest - the same trade `admit` makes for a refusal the tool layer already
/// wrote.
#[test]
fn an_sdk_error_result_is_admitted_intact_and_still_counted() {
    let mut round = budget_with_seed(TINY_THRESHOLD);
    let error = "Error: the remote job was cancelled by its owner";

    let admitted = round.admit_sdk_result(
        "test-session",
        "remote_job",
        &serde_json::json!({}),
        error.to_string(),
        true,
    );
    std::println!("QA sdk error through a spent round:\n{admitted}");
    assert_eq!(admitted, error, "the failure sentence reaches the model whole");
    assert_eq!(
        round.current_tokens(),
        TINY_THRESHOLD + crate::util::estimate_tokens(error),
        "the error still costs what it says it costs"
    );
}

/// The exact truncation the defect describes: a refusal marked as content, in a
/// round just under its ceiling. The mark used to send it down the truncation
/// branch, which chopped the sentence to whatever the remaining headroom was.
#[test]
fn a_refusal_marked_as_content_is_still_admitted_intact() {
    let refusal = Registry::exhausted_context_refusal(400_000 / 4, 9_990, 10_000);
    let mut marked = ToolOutput::new(refusal.clone());
    marked.metadata = Some(serde_json::json!({
        crate::tool::ACCEPT_LARGE_OUTPUT_METADATA_KEY: true
    }));

    let mut round = RoundTokenBudget::with_seed(10_000, 8_990);
    let admitted = round.admit("fixed_payload", marked);
    std::println!(
        "QA marked refusal through a round near its ceiling:\n---\n{}\n---",
        admitted.output
    );

    assert_eq!(
        admitted.output, refusal,
        "a verdict is not truncated by the round's content rule"
    );
}

/// Todo 40: the single-output ceiling is not a rule for rounds that are near
/// their ceiling.
///
/// A payload that fits the round whole but is itself larger than
/// `Registry::single_output_max_tokens` is bounded on every admit path, exactly
/// as `Registry::guard_context_overflow` bounds it for a tool result. Before
/// this, the `projected <= threshold` branch admitted it untouched, so the same
/// content was withheld from a tool caller and stored whole from an SDK client.
#[tokio::test]
async fn a_single_output_past_the_ceiling_is_bounded_on_every_admit_path() {
    // 20k budget: an 18k threshold and a 6k single-output ceiling. A 40k-char
    // payload is ~10k tokens - comfortably inside the round, twice the ceiling -
    // so only the single-output rule can bound it.
    const OVERSIZED_CHARS: usize = 40_000;
    let threshold = Registry::context_guard_threshold_tokens(PARITY_BUDGET);
    let single_max = Registry::single_output_max_tokens(PARITY_BUDGET);
    let payload_tokens = crate::util::estimate_tokens(&"x".repeat(OVERSIZED_CHARS));
    assert!(
        payload_tokens <= threshold,
        "the round has room for this payload whole: {payload_tokens} <= {threshold}"
    );
    assert!(
        payload_tokens > single_max,
        "and the payload is past the single-output ceiling: {payload_tokens} > {single_max}"
    );

    // (a) the tool layer, on a registry that has spent nothing.
    let registry = parity_registry_at(OVERSIZED_CHARS, 0).await;
    let via_tool = registry
        .execute("fixed_payload", serde_json::json!({}), tool_context())
        .await
        .expect("the tool runs");

    // (b) the same content through `admit`, on a round with all its room left.
    let mut round = RoundTokenBudget::with_seed(PARITY_BUDGET, 0);
    let via_round = round.admit("fixed_payload", ToolOutput::new("x".repeat(OVERSIZED_CHARS)));

    // (c) the same content as an SDK-answered result.
    let mut sdk_round = RoundTokenBudget::with_seed(PARITY_BUDGET, 0);
    let via_sdk = sdk_round.admit_sdk_result(
        "test-session",
        "fixed_payload",
        &serde_json::json!({}),
        "x".repeat(OVERSIZED_CHARS),
        false,
    );

    std::println!(
        "QA single-output ceiling budget={PARITY_BUDGET} threshold={threshold} \
         single_max={single_max} payload_tokens={payload_tokens}"
    );
    std::println!("QA tool layer, no flag : {}", qa_excerpt(&via_tool.output));
    std::println!("QA admit, room left    : {}", qa_excerpt(&via_round.output));
    std::println!("QA admit_sdk_result    : {}", qa_excerpt(&via_sdk));

    for (label, text) in [
        ("the tool layer", via_tool.output.as_str()),
        ("admit", via_round.output.as_str()),
        ("admit_sdk_result", via_sdk.as_str()),
    ] {
        assert!(
            !text.starts_with('x'),
            "{label} must not admit a payload past the single-output ceiling whole: {}",
            qa_excerpt(text)
        );
        assert!(
            text.contains("accept_large_output"),
            "{label} names the flag that returns what fits: {}",
            qa_excerpt(text)
        );
        assert!(
            text.contains("Nothing was added to the context"),
            "{label} states that none of the payload was spent: {}",
            qa_excerpt(text)
        );
    }
    assert_eq!(
        round.current_tokens(),
        crate::util::estimate_tokens(&via_round.output),
        "the refusal costs what it says it costs, and the payload costs nothing"
    );
    assert_eq!(
        sdk_round.current_tokens(),
        crate::util::estimate_tokens(&via_sdk),
        "the SDK path counts the refusal the same way"
    );

    // Not merely the same kind of verdict: the same sentence, so a caller cannot
    // tell whether a tool, the round, or an SDK client produced it.
    assert_eq!(
        via_round.output, via_tool.output,
        "the round refuses with the tool layer's own oversized sentence"
    );
    assert_eq!(
        via_sdk, via_tool.output,
        "an SDK result gets the same sentence as the identical tool result"
    );
    assert_eq!(
        via_tool.verdict,
        Some(ToolVerdict::Withheld),
        "the tool layer says the payload was withheld, not that the tool never ran"
    );
    assert_eq!(
        via_round.verdict,
        Some(ToolVerdict::Withheld),
        "the round says the same"
    );

    // With the opt-in all three return the prefix that fits, and the two admit
    // paths return exactly what the tool layer returns: same arithmetic, same
    // notice, so a caller cannot tell which layer bounded it.
    let registry = parity_registry_at(OVERSIZED_CHARS, 0).await;
    let opted_in = serde_json::json!({ "accept_large_output": true });
    let tool_opted_in = registry
        .execute("fixed_payload", opted_in.clone(), tool_context())
        .await
        .expect("the tool runs");
    let mut marked = ToolOutput::new("x".repeat(OVERSIZED_CHARS));
    marked.metadata = Some(serde_json::json!({
        crate::tool::ACCEPT_LARGE_OUTPUT_METADATA_KEY: true
    }));
    let mut opted_round = RoundTokenBudget::with_seed(PARITY_BUDGET, 0);
    let round_opted_in = opted_round.admit("fixed_payload", marked);
    let mut sdk_opted_round = RoundTokenBudget::with_seed(PARITY_BUDGET, 0);
    let sdk_opted_in = sdk_opted_round.admit_sdk_result(
        "test-session",
        "fixed_payload",
        &opted_in,
        "x".repeat(OVERSIZED_CHARS),
        false,
    );

    std::println!("QA tool layer, opted in: {}", qa_excerpt(&tool_opted_in.output));
    std::println!("QA admit, opted in     : {}", qa_excerpt(&round_opted_in.output));
    std::println!("QA admit_sdk, opted in : {}", qa_excerpt(&sdk_opted_in));

    assert!(
        tool_opted_in.output.contains("OUTPUT TRUNCATED"),
        "the tool layer truncates an opted-in output past the ceiling: {}",
        qa_excerpt(&tool_opted_in.output)
    );
    assert_eq!(
        round_opted_in.output, tool_opted_in.output,
        "the round truncates the same payload identically"
    );
    assert_eq!(
        sdk_opted_in, tool_opted_in.output,
        "an SDK result gets the same truncated prefix and notice"
    );
    assert_eq!(
        tool_opted_in.verdict,
        Some(ToolVerdict::Truncated),
        "an opted-in cut is reported as a cut"
    );
    assert_eq!(round_opted_in.verdict, Some(ToolVerdict::Truncated), "by both layers");
}

/// The single-output rule must not touch an ordinary result: every path admits
/// a payload under both ceilings whole, byte for byte.
#[tokio::test]
async fn a_normal_sized_output_is_admitted_untouched_on_every_path() {
    // 10k chars is ~2.5k tokens: inside the 6k single-output ceiling and inside
    // the 18k round.
    const NORMAL_CHARS: usize = 10_000;
    let expected = "x".repeat(NORMAL_CHARS);

    let registry = parity_registry_at(NORMAL_CHARS, 0).await;
    let via_tool = registry
        .execute("fixed_payload", serde_json::json!({}), tool_context())
        .await
        .expect("the tool runs");
    assert_eq!(via_tool.output, expected, "the tool layer admits it whole");

    let mut round = RoundTokenBudget::with_seed(PARITY_BUDGET, 0);
    let via_round = round.admit("fixed_payload", ToolOutput::new(expected.clone()));
    assert_eq!(via_round.output, expected, "admit admits it whole");
    assert_eq!(
        via_tool.verdict, None,
        "a whole result needs no statement from the tool layer"
    );
    assert_eq!(via_round.verdict, None, "nor from the round");
    assert_eq!(
        round.current_tokens(),
        crate::util::estimate_tokens(&expected),
        "and counts exactly its size"
    );

    let mut sdk_round = RoundTokenBudget::with_seed(PARITY_BUDGET, 0);
    let via_sdk = sdk_round.admit_sdk_result(
        "test-session",
        "fixed_payload",
        &serde_json::json!({}),
        expected.clone(),
        false,
    );
    assert_eq!(via_sdk, expected, "an SDK result of normal size is admitted whole");
    assert_eq!(
        sdk_round.current_tokens(),
        crate::util::estimate_tokens(&expected),
        "and is counted once"
    );
}
