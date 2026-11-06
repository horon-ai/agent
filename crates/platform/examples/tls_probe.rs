//! Probe the platform TLS client configuration against one URL.
//!
//! ```text
//! cargo run -p horon-agent-platform --example tls_probe -- https://models.dev/api.json
//! ```
//!
//! The two requests share one process-wide configuration, so the report shows
//! how many configurations were built (`1`) and how long the first build took.
//! A certificate or transport failure prints `error=` and exits non-zero.

use std::time::{Duration, Instant};

use horon_agent_platform::{tls_client_config, tls_config_builds};

const TIMEOUT: Duration = Duration::from_secs(20);

fn main() {
    let Some(url) = std::env::args().nth(1) else {
        eprintln!("usage: tls_probe <url>");
        std::process::exit(2);
    };

    match run(&url) {
        Ok(report) => {
            println!("status={}", report.status);
            println!("negotiated={}", report.negotiated);
            println!("config_builds={}", tls_config_builds());
            println!("config_ms={:.3}", report.config_ms);
        }
        Err(error) => {
            println!("error={}", describe(error.as_ref()));
            std::process::exit(1);
        }
    }
}

struct Report {
    status: u16,
    negotiated: &'static str,
    config_ms: f64,
}

fn run(url: &str) -> Result<Report, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let config = tls_client_config();
    let config_ms = started.elapsed().as_secs_f64() * 1000.0;

    let first_client = client(&config)?;
    let response = first_client.get(url).send()?;
    let status = response.status().as_u16();
    let negotiated = match response.version() {
        reqwest::Version::HTTP_2 => "h2",
        reqwest::Version::HTTP_11 => "http/1.1",
        reqwest::Version::HTTP_10 => "http/1.0",
        _ => "other",
    };

    // Second request through a second client: the configuration must be the
    // cached one, not a rebuild.
    let second_client = client(&tls_client_config())?;
    second_client.get(url).send()?;

    Ok(Report {
        status,
        negotiated,
        config_ms,
    })
}

/// Flatten the cause chain so a certificate rejection is visible instead of
/// the generic "error sending request".
fn describe(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn client(config: &std::sync::Arc<rustls::ClientConfig>) -> Result<reqwest::blocking::Client, reqwest::Error> {
    reqwest::blocking::Client::builder()
        .use_preconfigured_tls((**config).clone())
        .timeout(TIMEOUT)
        .build()
}
