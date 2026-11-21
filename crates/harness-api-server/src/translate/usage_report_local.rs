use super::{StoredUsage, Sum, enumerate, read_canonical, read_legacy, sessions_root};
use horon_agent_time::Timestamp;
use serde::Serialize;
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(super) struct Group {
    date: Option<String>,
    provider: Option<String>,
    model: Option<String>,
}

impl Group {
    pub(super) fn new(date: Option<&str>, provider: Option<&str>, model: Option<&str>) -> Self {
        Self {
            date: date.and_then(|date| match date.parse::<Timestamp>() {
                Ok(date) => Some(date.calendar_utc().date()),
                Err(_) => None,
            }),
            provider: provider.filter(|value| !value.is_empty()).map(str::to_owned),
            model: model.map(str::to_owned),
        }
    }
}

pub(super) struct Groups {
    from: Option<String>,
    through: Option<String>,
    rows: BTreeMap<Group, Sum>,
}

impl Groups {
    pub(super) fn record(&mut self, group: Group, usage: &StoredUsage) {
        if self
            .from
            .as_ref()
            .is_some_and(|from| group.date.as_ref().is_none_or(|date| date < from))
            || self
                .through
                .as_ref()
                .is_some_and(|through| group.date.as_ref().is_none_or(|date| date > through))
        {
            return;
        }
        self.rows.entry(group).or_default().record(usage);
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
    let date = |value: &str| -> anyhow::Result<String> {
        let day = Timestamp::parse_naive_date(value)?.calendar_utc().date();
        anyhow::ensure!(
            day == value && day.len() == 10,
            "expected a UTC date in YYYY-MM-DD format"
        );
        Ok(day)
    };
    let from = from.map(date).transpose()?;
    let through = through.map(date).transpose()?;
    anyhow::ensure!(
        from.as_ref()
            .zip(through.as_ref())
            .is_none_or(|(from, through)| from <= through),
        "--from must be on or before --through"
    );
    let mut groups = Groups {
        from,
        through,
        rows: BTreeMap::new(),
    };
    let root = sessions_root(home);
    for id in enumerate(Some(home), None, &[]) {
        let canonical = root.join(&id).join("events.jsonl");
        if canonical.is_file() {
            read_canonical(&canonical, Some(&mut groups));
        } else {
            read_legacy(&root.join(format!("{id}.json")), Some(&mut groups));
        }
    }
    let rows = groups
        .rows
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
