use super::*;
use crate::server::reload_recovery::ReloadContext;

pub(super) fn resolve_selfdev_reload_repo_dir(working_dir: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    resolve_selfdev_reload_repo_dir_from(build::get_repo_dir(), working_dir)
}

pub(super) fn resolve_selfdev_reload_repo_dir_from(
    primary: Option<std::path::PathBuf>,
    working_dir: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    primary.or_else(|| working_dir.and_then(build::find_repo_in_ancestors))
}

impl SelfDevTool {
    pub(super) async fn do_reload(
        &self,
        context: Option<String>,
        session_id: &str,
        execution_mode: ToolExecutionMode,
        working_dir: Option<&std::path::Path>,
    ) -> Result<ToolOutput> {
        let repo_dir = resolve_selfdev_reload_repo_dir(working_dir)
            .ok_or_else(|| anyhow::anyhow!("Could not find horon-agent repository directory"))?;

        let target_binary = build::find_dev_binary(&repo_dir).unwrap_or_else(|| build::release_binary_path(&repo_dir));
        // In a test session the rest of this method fakes the build source,
        // hash and published state, so the real on-disk binary check is both
        // inconsistent and environment-dependent (CI builds a release binary;
        // a local debug/selfdev checkout may not have target/release/horon-agent).
        // Skipping it lets the reload-signal/ack contract be exercised
        // deterministically regardless of which profile was built locally.
        if !SelfDevTool::is_test_session() && !target_binary.exists() {
            return Ok(ToolOutput::new(
                format!(
                    "No binary found at {}.\n\
                     Run 'horon-agent self-dev --build' first, or build with 'scripts/dev/cargo.sh build --profile selfdev -p horon-agent --bin horon-agent' and then try reload again.",
                    target_binary.display()
                )
                .to_string(),
            ));
        }

        let source = if SelfDevTool::is_test_session() {
            build::SourceState {
                repo_scope: "test-repo-scope".to_string(),
                worktree_scope: "test-worktree-scope".to_string(),
                short_hash: "test-reload-hash".to_string(),
                full_hash: "test-reload-hash-full".to_string(),
                dirty: true,
                fingerprint: "test-reload-fingerprint".to_string(),
                version_label: "test-reload-hash".to_string(),
                changed_paths: 0,
            }
        } else {
            build::current_source_state(&repo_dir)?
        };
        let hash = source.version_label.clone();
        let version_before = horon_agent_build_meta::version().to_string();
        let published = if SelfDevTool::is_test_session() {
            None
        } else {
            Some(build::publish_local_current_build_for_source(&repo_dir, &source)?)
        };
        let previous_shared_server_version = if SelfDevTool::is_test_session() {
            None
        } else {
            let published_build = published.as_ref().ok_or_else(|| {
                anyhow::anyhow!("published build metadata was missing after publish_local_current_build_for_source")
            })?;
            build::smoke_test_server_binary(&published_build.versioned_path)?;
            build::read_shared_server_version()?
        };

        // Update manifest - track what we're testing
        let mut manifest = build::BuildManifest::load()?;
        manifest.canary = Some(hash.clone());
        manifest.canary_status = Some(build::CanaryStatus::Testing);
        manifest.set_pending_activation(build::PendingActivation {
            session_id: session_id.to_string(),
            new_version: hash.clone(),
            previous_current_version: published
                .as_ref()
                .and_then(|published| published.previous_current_version.clone()),
            previous_shared_server_version,
            source_fingerprint: Some(source.fingerprint.clone()),
            requested_at: chrono::Utc::now(),
        })?;
        manifest.save()?;

        if !SelfDevTool::is_test_session()
            && let Err(error) = build::update_shared_server_symlink(&hash)
        {
            let _ = build::rollback_pending_activation_for_session(session_id);
            return Err(error);
        }

        // Save reload context for continuation after restart
        let reload_ctx = ReloadContext {
            task_context: context,
            version_before,
            version_after: hash.clone(),
            session_id: session_id.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        crate::logging::info(&format!(
            "Saving reload context to {:?}",
            ReloadContext::path_for_session(session_id)
        ));
        if let Err(e) = reload_ctx.save() {
            crate::logging::error(&format!("Failed to save reload context: {}", e));
            let _ = build::rollback_pending_activation_for_session(session_id);
            return Err(e);
        }
        crate::logging::info("Reload context saved successfully");

        // Signal the server via in-process channel (replaces filesystem-based rebuild-signal)
        let request_id = server::send_reload_signal(hash.clone(), Some(session_id.to_string()), true);
        crate::logging::info(&format!(
            "selfdev reload: request={} session_id={} hash={} execution_mode={:?}",
            request_id, session_id, hash, execution_mode
        ));

        let timeout = std::time::Duration::from_secs(SelfDevTool::reload_timeout_secs());
        let ack_wait_started = std::time::Instant::now();
        let ack = server::wait_for_reload_ack(&request_id, timeout)
            .await
            .map_err(|error| {
                let _ = build::rollback_pending_activation_for_session(session_id);
                anyhow::anyhow!(
                    "Timed out waiting for the server to begin reload after {}s: {}. The reload signal may not have been picked up; check that the connected server is running a build with unified self-dev reload support and try restarting the shared server.",
                    timeout.as_secs(),
                    error
                )
            })?;

        crate::logging::info(&format!(
            "selfdev reload: acked request={} hash={} after {}ms state={}",
            ack.request_id,
            ack.hash,
            ack_wait_started.elapsed().as_millis(),
            server::reload_state_summary(std::time::Duration::from_secs(60))
        ));

        match execution_mode {
            ToolExecutionMode::Direct => {
                if SelfDevTool::is_test_session() {
                    return Ok(ToolOutput::new(format!(
                        "Reload acknowledged for build {}. Server is restarting now.",
                        ack.hash
                    )));
                }
                match server::await_reload_handoff(&server::socket_path(), timeout).await {
                    server::ReloadWaitStatus::Ready => {
                        let _ = build::complete_pending_activation_for_session(session_id);
                        Ok(ToolOutput::new(format!(
                            "Reload completed successfully for build {}. Server reported ready.",
                            ack.hash
                        )))
                    }
                    server::ReloadWaitStatus::Failed(detail) => {
                        let _ = build::rollback_pending_activation_for_session(session_id);
                        Err(anyhow::anyhow!(
                            "Reload was acknowledged for build {}, but the replacement server failed before becoming ready on {}: {}; recent_state={}",
                            ack.hash,
                            server::socket_path().display(),
                            detail.unwrap_or_else(|| "unknown reload failure".to_string()),
                            server::reload_state_summary(std::time::Duration::from_secs(60))
                        ))
                    }
                    server::ReloadWaitStatus::Idle | server::ReloadWaitStatus::Waiting { .. } => {
                        let _ = build::rollback_pending_activation_for_session(session_id);
                        Err(anyhow::anyhow!(
                            "Reload was acknowledged for build {}, but readiness could not be confirmed within {}s.",
                            ack.hash,
                            timeout.as_secs()
                        ))
                    }
                }
            }
            ToolExecutionMode::AgentTurn => {
                // In normal agent turns the reload will intentionally terminate this
                // process shortly after the server acknowledges the request. Return a
                // tool result immediately so the harness can persist/deliver the tool
                // output before the process exits. Previously this branch waited to be
                // interrupted by shutdown; depending on timing, the process could exit
                // before the in-flight tool result reached the client, producing
                // "Tool output missing" even though reload succeeded.
                Ok(ToolOutput::new(format!(
                    "Reload initiated for build {}. Process restarting...",
                    ack.hash
                )))
            }
        }
    }
}
