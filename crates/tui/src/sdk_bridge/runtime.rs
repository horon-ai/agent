//! What the harness reports about the session it serves, and the reasoning
//! effort the client asks it to store.
//!
//! Kept beside the worker loop rather than inside it for the same reason as the
//! session flows: each request here is one call in, one answer out, and the
//! answer is a structure the client draws instead of a line it prints.

use horon_agent_sdk::{Error, HistoryMessage, HoronAgentClient, RuntimeCommand, RuntimeInfo, UsageReport};

use super::Incoming;
use crate::statusline::{UsageSnapshot, UsageTotals};

/// Run one harness command and shape its answer for the transcript.
///
/// `/context` keeps its measured panel. A command that repeats its payload as
/// JSON is drawn from that payload, so the terminal shows labels and values
/// instead of a serialization of the answer it already has; one that reported
/// in prose keeps the message it wrote.
pub(super) fn command(
    client: &HoronAgentClient,
    session_id: &str,
    command: RuntimeCommand,
    arguments: Vec<String>,
) -> Result<Option<Incoming>, Error> {
    let output = client.runtime_command(session_id, command, arguments)?;
    if let Some(report) = super::context_report(command, &output.data) {
        return Ok(Some(Incoming::Context(Box::new(report))));
    }
    Ok(Some(
        match crate::report::Report::from_answer(command, &output.data, &output.message) {
            Some(report) => Incoming::Report(Box::new(report)),
            None => Incoming::Info(output.message),
        },
    ))
}

pub(super) fn stats(client: &HoronAgentClient, session_id: &str) -> Result<Incoming, Error> {
    let report = client.usage_report(Some(session_id))?;
    Ok(Incoming::Info(format!("Session token usage\n{}", usage_lines(&report))))
}

/// The token and cost lines `/stats` and `/session` both print.
fn usage_lines(report: &UsageReport) -> String {
    let totals = &report.totals;
    let count = |value: Option<u64>| value.map_or_else(|| "unknown".to_owned(), grouped);
    let cost = report
        .estimated_cost
        .map_or_else(|| "unknown".to_owned(), |value| value.to_string());
    format!(
        "Input: {}\nOutput: {}\nCache read: {}\nCache write: {}\nRequests: {}\nEstimated cost: {cost}",
        count(totals.input),
        count(totals.output),
        count(totals.cache_read_input),
        count(totals.cache_creation_input),
        grouped(totals.requests)
    )
}

/// `value` with a comma between every three digits, as `1,336,303`.
fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `/session`: who the session is, what runs it, and what it has cost.
///
/// The runtime report is required, because it is what proves the session is
/// still served. The stored record, the history and the usage are each read
/// on their own, and a read the daemon refuses leaves that section saying so
/// rather than failing the whole answer.
pub(super) fn session_info(client: &HoronAgentClient, session_id: &str) -> Result<Incoming, Error> {
    let info = client.get_runtime_info(session_id)?;
    let known = |value: Option<&str>| value.filter(|value| !value.is_empty()).unwrap_or("unknown").to_owned();
    let mut out = format!("ID: {session_id}\n");
    match client.list_sessions() {
        Ok(sessions) => match sessions.iter().find(|session| session.session_id == session_id) {
            Some(session) => out.push_str(&format!(
                "Title: {}\nFolder: {}\nStatus: {}\n",
                known(session.title.as_deref()),
                known(session.working_dir.as_deref()),
                session.status
            )),
            None => out.push_str("Title: not in the session list\n"),
        },
        Err(error) => out.push_str(&format!("Title: could not list the sessions ({error})\n")),
    }
    out.push_str(&format!("Server: {}\n", info.server));
    out.push_str(&format!(
        "\nProvider\nName: {}\nModel: {}\n",
        known(info.provider.as_deref()),
        known(info.model.as_deref())
    ));
    if let Some(serving) = info
        .serving_model
        .as_deref()
        .filter(|serving| Some(*serving) != info.model.as_deref())
    {
        out.push_str(&format!("Served by: {serving}\n"));
    }
    out.push_str(&format!(
        "Effort: {}\nCredential: {}\n",
        known(effort(info.reasoning_effort.clone()).as_deref()),
        known(info.auth_method.as_deref())
    ));
    match client.get_history(session_id) {
        Ok(history) => {
            let count = |matches: &dyn Fn(&HistoryMessage) -> bool| {
                grouped(u64::try_from(history.iter().filter(|message| matches(message)).count()).unwrap_or(u64::MAX))
            };
            out.push_str(&format!(
                "\nMessages\nUser: {}\nAssistant: {}\nTool calls: {}\nTotal: {}\n",
                count(&|message| message.role == "user"),
                count(&|message| message.role == "assistant"),
                count(&|message| message.tool.is_some()),
                count(&|_| true)
            ));
        }
        Err(error) => out.push_str(&format!("\nMessages\ncould not read the history ({error})\n")),
    }
    match client.usage_report(Some(session_id)) {
        Ok(report) => out.push_str(&format!("\nTokens\n{}", usage_lines(&report))),
        Err(error) => out.push_str(&format!("\nTokens\ncould not read the usage ({error})")),
    }
    Ok(Incoming::SessionInfo(out))
}

/// The effort a report names, trimmed, with a blank one read as unknown.
///
/// A provider that does not expose an effort answers with an empty string
/// rather than naming a level, and drawing that would leave an empty part in
/// the status line.
pub(super) fn effort(reported: Option<String>) -> Option<String> {
    reported
        .map(|effort| effort.trim().to_string())
        .filter(|effort| !effort.is_empty())
}

/// The model and the effort of a session, for a client that has just moved to
/// it.
///
/// A report the daemon refuses leaves both unknown rather than failing a move
/// that already happened: the identity event that follows an attach or a create
/// carries the same two fields, and the status line fills in from it.
pub(super) fn identity(client: &HoronAgentClient, session_id: &str) -> (Option<String>, Option<String>) {
    let Ok(info) = client.get_runtime_info(session_id) else {
        return (None, None);
    };
    (info.model, effort(info.reasoning_effort))
}

/// The spending the runtime reported for a session, for the status line.
///
/// A harness without the `usage_report` capability is not asked: the request
/// would come back as a structured `unsupported` error, and a segment nobody
/// can fill would print a warning on every session change instead of drawing
/// nothing. A daemon that does answer but names no figure leaves the matching
/// field unknown, so the segment hides rather than printing a zero.
pub(super) fn usage(client: &HoronAgentClient, session_id: &str) -> Option<UsageSnapshot> {
    if !client.supports("usage_report") {
        return None;
    }
    let Ok(report) = client.usage_report(Some(session_id)) else {
        // A report this client cannot read says nothing about the session; the
        // status line keeps whatever it knew rather than clearing a value the
        // runtime did report a moment ago.
        return None;
    };
    let mut snapshot = snapshot(&report);
    // A freshly created session never saw a model change, which is what
    // carries the window into the report, so the share would stay hidden. The
    // session's own runtime info names the window the serving route has.
    if snapshot.context_window.is_none()
        && let Ok(info) = client.runtime_command(session_id, RuntimeCommand::Info, Vec::new())
    {
        snapshot.context_window = info.data["context_window"].as_u64().filter(|window| *window > 0);
    }
    Some(snapshot)
}

/// The snapshot one report carries, narrowed to what the status line draws.
///
/// `estimated_cost` is `None` on every build without a price table, and the
/// segment then hides: the report never guesses a price.
pub(super) fn snapshot(report: &UsageReport) -> UsageSnapshot {
    UsageSnapshot {
        totals: known(&report.totals),
        context_window: report.context_window,
        cost: report.estimated_cost,
    }
}

/// The token sums that were actually recorded.
///
/// `None` when the runtime recorded no metric at all, which is what a session
/// whose first request has not finished sends: an all-`None` struct would be
/// indistinguishable from "nothing spent" once a segment formatted it.
fn known(totals: &horon_agent_sdk::api::UsageTotals) -> Option<UsageTotals> {
    let sums = UsageTotals {
        input: totals.input,
        output: totals.output,
        cache_read: totals.cache_read_input,
        cache_creation: totals.cache_creation_input,
    };
    sums.is_known().then_some(sums)
}

/// The report the client draws, for the session the worker follows.
pub(super) fn read(client: &HoronAgentClient, session_id: &str) -> Result<Option<Incoming>, Error> {
    let info = client.get_runtime_info(session_id)?;
    Ok(reported(info, session_id, None))
}

/// Store one level and report the state the harness answers with.
///
/// The answer travels with the level that was asked for, so the client can say
/// the level was set only after the harness took it: a refusal never reaches
/// this function's `Ok`.
pub(super) fn set(client: &HoronAgentClient, session_id: &str, level: &str) -> Result<Option<Incoming>, Error> {
    client.set_reasoning_effort(session_id, level)?;
    // The level is stored once this request succeeds. A report the daemon then
    // refuses costs the fresh provider and model, not the confirmation: the
    // client would otherwise report a refusal for a change the harness took.
    let Ok(info) = client.get_runtime_info(session_id) else {
        return Ok(confirmed(level));
    };
    Ok(reported(info, session_id, Some(level)))
}

/// The answer for a level the harness stored, with nothing else known.
fn confirmed(level: &str) -> Option<Incoming> {
    Some(Incoming::RuntimeInfo {
        provider: None,
        model: None,
        effort: Some(level.to_string()),
        level: Some(level.to_string()),
    })
}

/// Turn one runtime report into the answer the client draws.
///
/// A report that names another session is dropped: the daemon rewrites a typed
/// prefix, and an answer that describes a conversation the worker has left
/// would put one session's model beside another's transcript.
pub(super) fn reported(info: RuntimeInfo, followed: &str, level: Option<&str>) -> Option<Incoming> {
    if info.session_id != followed {
        return None;
    }
    let effort = effort(info.reasoning_effort).or_else(|| level.map(str::to_string));
    Some(Incoming::RuntimeInfo {
        provider: info.provider,
        model: info.model,
        effort,
        level: level.map(str::to_string),
    })
}

/// The models a `refresh_model_list` reply lists, in its order.
///
/// The daemon answers the refresh with the catalog it rebuilt as
/// `data.models`; a reply without one (an older daemon) lists none.
pub(super) fn refreshed_models(data: &serde_json::Value) -> Vec<String> {
    data["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod refresh_tests {
    #[test]
    fn a_refresh_reply_lists_its_models_and_an_older_one_none() {
        let data = serde_json::json!({"refreshed": true, "models": ["a", 7, "group/deepseek-v4.1-flash"]});
        assert_eq!(super::refreshed_models(&data), ["a", "group/deepseek-v4.1-flash"]);
        assert!(super::refreshed_models(&serde_json::json!({"refreshed": true})).is_empty());
    }
}

#[cfg(test)]
mod grouped_tests {
    #[test]
    fn large_numbers_get_a_comma_every_three_digits() {
        assert_eq!(super::grouped(7), "7");
        assert_eq!(super::grouped(1_000), "1,000");
        assert_eq!(super::grouped(213_303_421), "213,303,421");
    }
}
