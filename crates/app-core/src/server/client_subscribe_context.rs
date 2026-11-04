use super::*;

pub(super) fn required_subscribe_working_dir(working_dir: Option<&str>) -> std::result::Result<&str, String> {
    let working_dir = working_dir
        .map(str::trim)
        .filter(|dir| !dir.is_empty())
        .ok_or_else(|| "Subscribe requires the client's working directory".to_string())?;
    if !Path::new(working_dir).is_absolute() {
        return Err("Subscribe working_dir must be an absolute path".to_string());
    }
    Ok(working_dir)
}

pub(super) fn initial_subscribe_working_dir(request: &Request) -> std::result::Result<String, String> {
    match request {
        Request::Subscribe {
            working_dir,
            continue_on_disconnect,
            ..
        } => validated_subscribe_working_dir(working_dir.as_deref(), *continue_on_disconnect).map(str::to_string),
        _ => Err("Client must Subscribe with a working_dir before sending stateful requests".to_string()),
    }
}

/// A reattachment names an existing session, not a new client working directory.
/// Resolve an omitted cwd before provisional initialization, never from the
/// daemon/bridge process cwd. Idle empty sessions may exist only in memory.
pub(super) async fn resolve_target_subscribe_working_dir(
    request: &mut Request,
    sessions: &SessionAgents,
    members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
) -> std::result::Result<(), String> {
    let Request::Subscribe {
        working_dir,
        target_session_id: Some(target),
        ..
    } = request
    else {
        return Ok(());
    };
    if working_dir.is_some() {
        return Ok(());
    }
    let live = sessions.read().await.get(target).cloned();
    let resolved = if let Some(live) = live {
        // Contention is expected during a turn, not a failed session lookup.
        let idle_cwd = match live.try_lock() {
            Ok(agent) => agent.working_dir().map(str::to_string),
            Err(_) => None,
        };
        if idle_cwd.is_some() {
            idle_cwd
        } else {
            // A generating Agent owns its mutex. The member records the same
            // session root, so attaching must not wait for the model turn.
            members
                .read()
                .await
                .get(target)
                .and_then(|member| member.working_dir.as_ref())
                .map(|path| path.to_string_lossy().into_owned())
        }
    } else {
        let session = crate::session::Session::load_startup_stub(target).map_err(|error| {
            format!("Unknown session '{target}' or session could not be loaded for attachment: {error:#}")
        })?;
        session.working_dir
    };
    *working_dir =
        Some(resolved.ok_or_else(|| format!("Unknown session '{target}' or session has no working directory"))?);
    Ok(())
}

pub(super) fn validated_subscribe_working_dir(
    working_dir: Option<&str>,
    remote_continuation: bool,
) -> std::result::Result<&str, String> {
    let working_dir = required_subscribe_working_dir(working_dir)?;
    if remote_continuation && !Path::new(working_dir).is_dir() {
        return Err(format!(
            "Remote working directory must exist and be a directory on the server: {working_dir}"
        ));
    }
    Ok(working_dir)
}

pub(super) fn new_session_system_prompt<'a>(
    provisional_session: bool,
    target_session_id: Option<&str>,
    system_prompt: Option<&'a str>,
) -> Option<&'a str> {
    if provisional_session && target_session_id.is_none() {
        system_prompt
    } else {
        None
    }
}

pub(super) fn initial_subscribe_terminal_env(request: &Request) -> Vec<(String, String)> {
    match request {
        Request::Subscribe { terminal_env, .. } => terminal_env.clone(),
        _ => Vec::new(),
    }
}

/// Start the owned provider warmup for a freshly attached session.
///
/// The snapshot is taken under the session lock and the lock is released before
/// the provider is awaited, so a request arriving mid-warmup never queues behind
/// speculative network work. Callers keep the handle so they can abort it on a
/// route change, on the first real request, on a session switch, or when the
/// connection ends.
pub(super) fn spawn_deferred_prewarm(agent: &Arc<Mutex<Agent>>) -> tokio::task::JoinHandle<()> {
    let snapshot_agent = Arc::clone(agent);
    tokio::spawn(async move {
        let (provider, tools, static_part, context) = {
            let guard = snapshot_agent.lock().await;
            guard.prewarm_snapshot().await
        };
        provider.prewarm_with_context(&tools, &static_part, &context).await;
    })
}

/// Stop the owned warmup and wait for the task to drop, reporting whether one
/// was running.
///
/// The task holds its own `Agent` clone through the provider await, and that
/// clone keeps the session writer alive. `abort()` only schedules the drop, so
/// the join is awaited: when this returns, the reference is gone. Session
/// switches settle the warmup before the handler detaches the old source, and a
/// connection settles it before the close cleanup publishes its barrier.
pub(super) async fn settle_prewarm(prewarm_task: &mut Option<tokio::task::JoinHandle<()>>) -> bool {
    let Some(handle) = prewarm_task.take() else {
        return false;
    };
    handle.abort();
    match handle.await {
        Ok(()) => {}
        Err(error) if error.is_cancelled() => {}
        Err(error) => {
            crate::logging::warn(&format!("Deferred provider warmup ended abnormally: {error}"));
        }
    }
    true
}

/// Start the owned speculative warmup for a connection that may own a fresh
/// session. A target-aware Subscribe that names a session this daemon already
/// knows adopts that Agent instead, so warming the placeholder would be
/// discarded work that can also delay the History replay.
pub(super) async fn initial_prewarm_task(
    request: &Request,
    sessions: &SessionAgents,
    agent: &Arc<Mutex<Agent>>,
) -> Option<tokio::task::JoinHandle<()>> {
    let Request::Subscribe {
        target_session_id: Some(target),
        ..
    } = request
    else {
        return Some(spawn_deferred_prewarm(agent));
    };
    if crate::session::session_exists(target) || sessions.read().await.contains_key(target) {
        return None;
    }
    Some(spawn_deferred_prewarm(agent))
}
