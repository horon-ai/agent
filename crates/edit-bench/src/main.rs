//! `edit-bench` CLI.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use horon_agent_edit_bench::compare::{Metric, Verdict, compare, render};
use horon_agent_edit_bench::generate::{GenerateOptions, generate};
use horon_agent_edit_bench::run::{RunOptions, run};

#[derive(Parser, Debug)]
#[command(
    name = "edit-bench",
    version,
    about = "Edit benchmark: generate single-line-bug fixtures, run arms, and compare results"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Mutate one line of real .rs files into task fixtures
    Generate {
        /// Directory holding the .rs files to mutate
        #[arg(long)]
        source: PathBuf,

        /// Directory the task fixtures are written to
        #[arg(long)]
        out: PathBuf,

        /// Seed for file, mutation, and line selection
        #[arg(long, default_value_t = 1)]
        seed: u64,

        /// Number of tasks to generate
        #[arg(long, default_value_t = 10)]
        count: usize,
    },

    /// Run an arm against every fixture and append results.jsonl rows
    Run {
        /// Fixtures directory written by `generate`
        #[arg(long)]
        fixtures: PathBuf,

        /// Arm name; also the results subdirectory name
        #[arg(long)]
        arm: String,

        /// config.toml staged as the run's HORON_AGENT_HOME config
        #[arg(long)]
        config: PathBuf,

        /// Model override passed to `horon-agent --model`
        #[arg(long)]
        model: Option<String>,

        /// Repetitions per task
        #[arg(long, default_value_t = 1)]
        repeat: usize,

        /// Results directory; rows go to <out>/<arm>/results.jsonl
        #[arg(long)]
        out: PathBuf,
    },

    /// Compare two arms and report whether the candidate is up, down, or flat
    Compare {
        /// Baseline results directory (or its results.jsonl)
        baseline: PathBuf,

        /// Candidate results directory (or its results.jsonl)
        candidate: PathBuf,

        #[arg(long, value_enum, default_value_t = Metric::Pass)]
        metric: Metric,

        /// Exit 1 when the candidate is a significant regression
        #[arg(long)]
        fail_on_down: bool,
    },
}

fn main() -> ExitCode {
    match run_cli() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("edit-bench: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_cli() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        Command::Generate {
            source,
            out,
            seed,
            count,
        } => {
            let tasks = generate(&GenerateOptions {
                source,
                out,
                seed,
                count,
            })?;
            for task in &tasks {
                println!(
                    "{}: {} near line {} ({})",
                    task.id, task.metadata.file, task.metadata.line, task.metadata.mutation
                );
            }
            println!("{} task(s) written", tasks.len());
            Ok(ExitCode::SUCCESS)
        }
        Command::Run {
            fixtures,
            arm,
            config,
            model,
            repeat,
            out,
        } => {
            run(&RunOptions {
                fixtures,
                arm,
                config,
                model,
                repeat,
                out,
            })?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Compare {
            baseline,
            candidate,
            metric,
            fail_on_down,
        } => {
            let comparison = compare(&baseline, &candidate, metric)?;
            print!("{}", render(&comparison, &baseline, &candidate));
            if fail_on_down && comparison.verdict == Verdict::Down {
                return Ok(ExitCode::FAILURE);
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}
