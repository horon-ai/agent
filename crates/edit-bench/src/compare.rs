//! Arm comparison: pairs `(task, rep)` rows from two result sets and reports a
//! two-sided 95% confidence interval for the mean per-pair delta.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;

use crate::results::{ResultRow, read_jsonl};

/// Two-sided 95% `t` critical values for `df` 1..=30.
const T_95: [f64; 30] = [
    12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.160, 2.145, 2.131, 2.120,
    2.110, 2.101, 2.093, 2.086, 2.080, 2.074, 2.069, 2.064, 2.060, 2.056, 2.052, 2.048, 2.045, 2.042,
];

/// Large-sample two-sided 95% `z`, used past `df` 30.
const Z_95: f64 = 1.96;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Metric {
    /// Pass rate: higher is better.
    #[value(name = "pass")]
    Pass,
    /// Number of tool calls: lower is better.
    #[value(name = "tool_calls")]
    ToolCalls,
    /// Output tokens: lower is better.
    #[value(name = "output_tokens")]
    OutputTokens,
    /// Wall-clock duration: lower is better.
    #[value(name = "duration_ms")]
    DurationMs,
}

impl Metric {
    fn value(self, row: &ResultRow) -> f64 {
        match self {
            Self::Pass => {
                if row.pass {
                    1.0
                } else {
                    0.0
                }
            }
            Self::ToolCalls => row.tool_calls as f64,
            Self::OutputTokens => row.output_tokens as f64,
            Self::DurationMs => row.duration_ms as f64,
        }
    }

    /// Whether a smaller value is a better result. Deltas are sign-flipped for
    /// these metrics, so a positive delta always means "candidate is better".
    fn lower_is_better(self) -> bool {
        !matches!(self, Self::Pass)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::ToolCalls => "tool_calls",
            Self::OutputTokens => "output_tokens",
            Self::DurationMs => "duration_ms",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Up,
    Down,
    Flat,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Flat => "flat",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Comparison {
    pub metric: Metric,
    /// Number of paired `(task, rep)` observations.
    pub pairs: usize,
    /// Rows present on one side only.
    pub unpaired: usize,
    /// Mean per-pair delta (positive = candidate better).
    pub mean: f64,
    /// Half-width of the two-sided 95% interval.
    pub half_width: f64,
    pub verdict: Verdict,
    /// Set when fewer than two pairs exist, which is never a verdict.
    pub insufficient: bool,
}

impl Comparison {
    /// The one-line reason printed next to `flat` when there is no data.
    pub fn note(&self) -> Option<&'static str> {
        self.insufficient.then_some("insufficient pairs")
    }
}

/// Compare `baseline` against `candidate` on `metric`.
///
/// Each argument may be a directory holding `results.jsonl` (the
/// `<out>/<arm>` shape `run` writes) or the JSONL file itself.
pub fn compare(baseline: &Path, candidate: &Path, metric: Metric) -> Result<Comparison> {
    let baseline_rows = read_results(baseline)?;
    let candidate_rows = read_results(candidate)?;
    let key = |row: &ResultRow| (row.task.clone(), row.rep);
    let baseline_by_key: BTreeMap<(String, u32), &ResultRow> =
        baseline_rows.iter().map(|row| (key(row), row)).collect();
    let candidate_by_key: BTreeMap<(String, u32), &ResultRow> =
        candidate_rows.iter().map(|row| (key(row), row)).collect();

    let mut deltas = Vec::new();
    let mut unpaired = 0usize;
    for (pair, candidate_row) in &candidate_by_key {
        match baseline_by_key.get(pair) {
            Some(baseline_row) => {
                let mut delta = metric.value(candidate_row) - metric.value(baseline_row);
                if metric.lower_is_better() {
                    delta = -delta;
                }
                deltas.push(delta);
            }
            None => unpaired += 1,
        }
    }
    unpaired += baseline_by_key
        .keys()
        .filter(|pair| !candidate_by_key.contains_key(*pair))
        .count();

    let pairs = deltas.len();
    if pairs < 2 {
        let mean = if pairs == 1 { deltas[0] } else { 0.0 };
        return Ok(Comparison {
            metric,
            pairs,
            unpaired,
            mean,
            half_width: 0.0,
            verdict: Verdict::Flat,
            insufficient: true,
        });
    }

    let mean = deltas.iter().sum::<f64>() / pairs as f64;
    let variance = deltas.iter().map(|delta| (delta - mean).powi(2)).sum::<f64>() / (pairs - 1) as f64;
    let standard_deviation = variance.sqrt();
    let half_width = t_value(pairs - 1) * standard_deviation / (pairs as f64).sqrt();
    let verdict = if mean - half_width > 0.0 {
        Verdict::Up
    } else if mean + half_width < 0.0 {
        Verdict::Down
    } else {
        Verdict::Flat
    };

    Ok(Comparison {
        metric,
        pairs,
        unpaired,
        mean,
        half_width,
        verdict,
        insufficient: false,
    })
}

/// Render a comparison as the human-readable table `compare` prints.
pub fn render(comparison: &Comparison, baseline: &Path, candidate: &Path) -> String {
    let mut out = String::new();
    out.push_str(&format!("baseline:  {}\n", baseline.display()));
    out.push_str(&format!("candidate: {}\n\n", candidate.display()));
    out.push_str(&format!(
        "metric: {} ({})\n",
        comparison.metric.as_str(),
        if comparison.metric.lower_is_better() {
            "lower is better"
        } else {
            "higher is better"
        }
    ));
    out.push_str("pairs  unpaired  mean delta  half width  verdict\n");
    let note = comparison.note().map(|note| format!("  ({note})")).unwrap_or_default();
    out.push_str(&format!(
        "{:>5}  {:>8}  {:>+10.3}  {:>10.3}  {}{}\n",
        comparison.pairs,
        comparison.unpaired,
        comparison.mean,
        comparison.half_width,
        comparison.verdict.as_str(),
        note,
    ));
    out
}

fn t_value(degrees_of_freedom: usize) -> f64 {
    if degrees_of_freedom == 0 {
        return T_95[0];
    }
    T_95.get(degrees_of_freedom - 1).copied().unwrap_or(Z_95)
}

fn read_results(path: &Path) -> Result<Vec<ResultRow>> {
    let file = resolve_results_path(path)?;
    let text = fs::read_to_string(&file).with_context(|| format!("cannot read {}", file.display()))?;
    read_jsonl(&text, &file.display().to_string())
}

/// Resolve `--out`-shaped directories: prefer `<dir>/<arm>/results.jsonl`.
pub(crate) fn resolve_results_path(path: &Path) -> Result<PathBuf> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if !path.is_dir() {
        bail!("no such results path: {}", path.display());
    }
    let direct = path.join("results.jsonl");
    if direct.is_file() {
        return Ok(direct);
    }
    let arms: Vec<PathBuf> = fs::read_dir(path)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|entry| entry.join("results.jsonl").is_file())
        .collect();
    match arms.len() {
        1 => Ok(arms[0].join("results.jsonl")),
        _ => bail!(
            "{} holds no results.jsonl and {} arm directories; pass the arm directory itself",
            path.display(),
            arms.len()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(task: &str, rep: u32, pass: bool, tool_calls: u64, output_tokens: u64) -> String {
        serde_json::json!({
            "task": task,
            "rep": rep,
            "arm": "a",
            "pass": pass,
            "tool_calls": tool_calls,
            "tool_counts": {},
            "input_tokens": 10,
            "output_tokens": output_tokens,
            "duration_ms": 1000,
        })
        .to_string()
    }

    fn write_results(dir: &Path, lines: &[String]) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("results.jsonl"), lines.join("\n") + "\n").unwrap();
    }

    #[test]
    fn unanimous_improvement_is_up() {
        let temp = tempfile::tempdir().unwrap();
        let baseline = temp.path().join("base");
        let candidate = temp.path().join("cand");
        write_results(
            &baseline,
            &[
                row("t1", 0, false, 0, 0),
                row("t2", 0, false, 0, 0),
                row("t3", 0, false, 0, 0),
                row("t4", 0, false, 0, 0),
                row("t5", 0, false, 0, 0),
            ],
        );
        write_results(
            &candidate,
            &[
                row("t1", 0, true, 0, 0),
                row("t2", 0, true, 0, 0),
                row("t3", 0, true, 0, 0),
                row("t4", 0, true, 0, 0),
                row("t5", 0, true, 0, 0),
            ],
        );
        let comparison = compare(&baseline, &candidate, Metric::Pass).unwrap();
        assert_eq!(comparison.pairs, 5);
        assert_eq!(comparison.unpaired, 0);
        assert_eq!(comparison.verdict, Verdict::Up);
        assert!((comparison.mean - 1.0).abs() < 1e-9);
        assert_eq!(comparison.half_width, 0.0);
    }

    #[test]
    fn unanimous_regression_is_down() {
        let temp = tempfile::tempdir().unwrap();
        let baseline = temp.path().join("base");
        let candidate = temp.path().join("cand");
        write_results(
            &baseline,
            &(0..5).map(|i| row(&format!("t{i}"), 0, true, 0, 0)).collect::<Vec<_>>(),
        );
        write_results(
            &candidate,
            &(0..5)
                .map(|i| row(&format!("t{i}"), 0, false, 0, 0))
                .collect::<Vec<_>>(),
        );
        let comparison = compare(&baseline, &candidate, Metric::Pass).unwrap();
        assert_eq!(comparison.verdict, Verdict::Down);
    }

    #[test]
    fn mixed_results_are_flat() {
        let temp = tempfile::tempdir().unwrap();
        let baseline = temp.path().join("base");
        let candidate = temp.path().join("cand");
        write_results(
            &baseline,
            &(0..5)
                .map(|i| row(&format!("t{i}"), 0, false, 0, 0))
                .collect::<Vec<_>>(),
        );
        let passes = [true, false, true, false, true];
        write_results(
            &candidate,
            &(0..5)
                .map(|i| row(&format!("t{i}"), 0, passes[i], 0, 0))
                .collect::<Vec<_>>(),
        );
        let comparison = compare(&baseline, &candidate, Metric::Pass).unwrap();
        assert_eq!(comparison.verdict, Verdict::Flat);
        assert!((comparison.mean - 0.6).abs() < 1e-9);
    }

    #[test]
    fn lower_is_better_metrics_are_negated() {
        let temp = tempfile::tempdir().unwrap();
        let baseline = temp.path().join("base");
        let candidate = temp.path().join("cand");
        write_results(
            &baseline,
            &(0..4)
                .map(|i| row(&format!("t{i}"), 0, true, 10, 500))
                .collect::<Vec<_>>(),
        );
        write_results(
            &candidate,
            &(0..4)
                .map(|i| row(&format!("t{i}"), 0, true, 4, 200))
                .collect::<Vec<_>>(),
        );
        let tool_calls = compare(&baseline, &candidate, Metric::ToolCalls).unwrap();
        assert_eq!(tool_calls.verdict, Verdict::Up);
        assert!((tool_calls.mean - 6.0).abs() < 1e-9);

        let output_tokens = compare(&baseline, &candidate, Metric::OutputTokens).unwrap();
        assert_eq!(output_tokens.verdict, Verdict::Up);
        assert!((output_tokens.mean - 300.0).abs() < 1e-9);
    }

    #[test]
    fn unmatched_and_single_pairs_are_flat() {
        let temp = tempfile::tempdir().unwrap();
        let baseline = temp.path().join("base");
        let candidate = temp.path().join("cand");
        write_results(&baseline, &[row("t1", 0, false, 0, 0), row("t2", 0, false, 0, 0)]);
        write_results(&candidate, &[row("t1", 0, true, 0, 0)]);
        let comparison = compare(&baseline, &candidate, Metric::Pass).unwrap();
        assert_eq!(comparison.pairs, 1);
        assert_eq!(comparison.unpaired, 1);
        assert_eq!(comparison.verdict, Verdict::Flat);
        assert!(comparison.insufficient);
        assert_eq!(comparison.note(), Some("insufficient pairs"));
    }

    #[test]
    fn t_table_matches_published_values() {
        assert!((t_value(1) - 12.706).abs() < 1e-9);
        assert!((t_value(30) - 2.042).abs() < 1e-9);
        assert!((t_value(31) - Z_95).abs() < 1e-9);
        assert!((t_value(1000) - Z_95).abs() < 1e-9);
    }

    #[test]
    fn render_includes_the_table_row() {
        let comparison = Comparison {
            metric: Metric::ToolCalls,
            pairs: 5,
            unpaired: 0,
            mean: -1.5,
            half_width: 0.25,
            verdict: Verdict::Down,
            insufficient: false,
        };
        let text = render(&comparison, Path::new("base"), Path::new("cand"));
        assert!(text.contains("metric: tool_calls (lower is better)"));
        assert!(text.contains("pairs  unpaired  mean delta  half width  verdict"));
        assert!(text.contains("-1.500"));
        assert!(text.contains("down"));
    }

    #[test]
    fn resolve_results_path_finds_the_single_arm() {
        let temp = tempfile::tempdir().unwrap();
        write_results(&temp.path().join("base"), &[row("t1", 0, true, 0, 0)]);
        let resolved = resolve_results_path(temp.path()).unwrap();
        assert_eq!(resolved, temp.path().join("base/results.jsonl"));
        assert_eq!(
            resolve_results_path(&resolved).unwrap(),
            temp.path().join("base/results.jsonl")
        );
    }
}
