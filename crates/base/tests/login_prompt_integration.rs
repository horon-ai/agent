//! Exercise the real OpenAI login prompt in a child process, without completing
//! OAuth, opening a browser, or touching saved credentials.
use std::process::{Command, Stdio};

/// Plain-text presenter: the headless build has no terminal crate, so the
/// browserless login surfaces the URL as ordinary text instead of a QR code.
struct PlainPresenter;

impl horon_agent_base::auth::login_ui::LoginPresenter for PlainPresenter {
    fn qr_section(&self, data: &str, heading: &str, indent: &str, browser_suppressed: bool) -> Option<String> {
        if !browser_suppressed {
            return None;
        }
        Some(format!("{heading}\n\n{indent}{data}"))
    }

    fn read_secret_line(&self) -> anyhow::Result<String> {
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        Ok(input.trim().to_string())
    }
}

#[test]
fn openai_browserless_login_prints_url() {
    if std::env::var_os("HORON_AGENT_LOGIN_TEST_CHILD").is_some() {
        horon_agent_base::auth::login_ui::install(Box::new(PlainPresenter));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime
            .block_on(horon_agent_base::auth::oauth::login_openai(true))
            .unwrap_err();
        assert!(error.to_string().contains("No callback URL entered"));
        return;
    }

    let home = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "openai_browserless_login_prints_url", "--nocapture"])
        .env("HORON_AGENT_LOGIN_TEST_CHILD", "1")
        .env("HORON_AGENT_HOME", home.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("https://auth.openai.com/"), "{stderr}");
    assert!(
        stderr.contains("Scan this QR on another device if this machine has no browser:"),
        "{stderr}"
    );
    assert!(stderr.contains("Paste the full callback URL"), "{stderr}");
    assert!(!stderr.contains("Waiting up to"), "{stderr}");
    assert!(!stderr.contains('█') && !stderr.contains('▀') && !stderr.contains('▄'));
}
