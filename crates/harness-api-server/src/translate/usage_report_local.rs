use super::{LocalSession, StoredUsage, Sum, enumerate_local_sessions, read_canonical, read_legacy, sessions_root};
use horon_agent_time::Timestamp;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::rc::Rc;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(super) struct Group {
    date: Option<String>,
    provider: Option<String>,
    model: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct GroupDraft {
    pub(super) day: Option<i64>,
    pub(super) provider: Option<Rc<str>>,
    pub(super) model: Option<Rc<str>>,
}

pub(super) fn parse_day_index(date: Option<&str>) -> Option<i64> {
    match date {
        Some(raw) => match raw.parse::<Timestamp>() {
            Ok(ts) => Some(ts.timestamp().div_euclid(86_400)),
            Err(_) => None,
        },
        None => None,
    }
}

pub(super) struct Groups {
    from_day: Option<i64>,
    through_day: Option<i64>,
    rows: HashMap<GroupDraft, Sum>,
}

impl Groups {
    pub(super) fn record_draft(&mut self, draft: GroupDraft, usage: &StoredUsage) {
        if self.from_day.is_some_and(|from| draft.day.is_none_or(|day| day < from))
            || self
                .through_day
                .is_some_and(|through| draft.day.is_none_or(|day| day > through))
        {
            return;
        }
        self.rows.entry(draft).or_default().record(usage);
    }
}

#[derive(Serialize)]
pub struct LocalUsage {
    pub rows: Vec<UsageRow>,
    pub estimated_cost: Option<f64>,
}

#[derive(Serialize)]
pub struct UsageRow {
    #[serde(flatten)]
    group: Group,
    input: Option<u64>,
    output: Option<u64>,
    cache_read_input: Option<u64>,
    cache_creation_input: Option<u64>,
    requests: u64,
}

impl std::fmt::Display for LocalUsage {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            out,
            "Date (UTC)\tProvider\tModel\tInput\tOutput\tCache read\tCache write\tRequests"
        )?;
        let count = |value: Option<u64>| value.map_or_else(|| "unknown".to_owned(), |value| value.to_string());
        for row in &self.rows {
            writeln!(
                out,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                row.group.date.as_deref().unwrap_or("unknown"),
                row.group.provider.as_deref().unwrap_or("unknown"),
                row.group.model.as_deref().unwrap_or("unknown"),
                count(row.input),
                count(row.output),
                count(row.cache_read_input),
                count(row.cache_creation_input),
                row.requests
            )?;
        }
        writeln!(out, "Estimated cost: unknown")
    }
}

/// Read a local token report without a daemon connection or provider call.
/// Bounds are inclusive UTC dates. Without bounds, undated usage is included.
pub fn local_usage(home: &Path, from: Option<&str>, through: Option<&str>) -> anyhow::Result<LocalUsage> {
    let parse_day = |value: &str| -> anyhow::Result<i64> {
        let ts = Timestamp::parse_naive_date(value)?;
        let day = ts.calendar_utc().date();
        anyhow::ensure!(
            day == value && day.len() == 10,
            "expected a UTC date in YYYY-MM-DD format"
        );
        Ok(ts.timestamp().div_euclid(86_400))
    };
    let from_day = from.map(parse_day).transpose()?;
    let through_day = through.map(parse_day).transpose()?;
    anyhow::ensure!(
        from_day.zip(through_day).is_none_or(|(from, through)| from <= through),
        "--from must be on or before --through"
    );
    let mut groups = Groups {
        from_day,
        through_day,
        rows: HashMap::new(),
    };
    let root = sessions_root(home);
    for session in enumerate_local_sessions(&root) {
        match session {
            LocalSession::Canonical(path) => {
                read_canonical(&path, Some(&mut groups));
            }
            LocalSession::Legacy(path) => {
                read_legacy(&path, Some(&mut groups));
            }
        }
    }
    let mut day_cache: HashMap<i64, Option<String>> = HashMap::new();
    let mut ordered: BTreeMap<Group, Sum> = BTreeMap::new();
    for (draft, sum) in groups.rows {
        let date = draft.day.and_then(|day| {
            day_cache
                .entry(day)
                .or_insert_with(|| {
                    day.checked_mul(86_400)
                        .and_then(|secs| Timestamp::from_timestamp(secs, 0))
                        .map(|ts| ts.calendar_utc().date())
                })
                .clone()
        });
        let group = Group {
            date,
            provider: draft.provider.map(|value| value.to_string()),
            model: draft.model.map(|value| value.to_string()),
        };
        ordered.entry(group).or_default().merge(&sum);
    }
    let rows = ordered
        .into_iter()
        .map(|(group, sum)| UsageRow {
            group,
            input: sum.input,
            output: sum.output,
            cache_read_input: sum.cache_read,
            cache_creation_input: sum.cache_creation,
            requests: sum.requests,
        })
        .collect();
    Ok(LocalUsage {
        rows,
        estimated_cost: None,
    })
}
