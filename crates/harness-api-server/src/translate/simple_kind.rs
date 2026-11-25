//! What a queued request is waiting for, so the daemon reply that answers it
//! can be matched to the API request id it belongs to.

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SimpleKind {
    RuntimeCommand,
    /// Awaiting `package_control_result`.
    PackageControl,
    /// Awaiting `done` for `configure_a2ui`.
    ConfigureA2ui,
    Detach,
    Tools,
    ToolControl,
    Ping,
    History,
    Ok,
    ContinueRun,
    /// Awaiting `done`: the daemon acks a delete before it touches the store,
    /// and acknowledging a delete that later failed would report a transcript
    /// as gone while it is still there. Only `done` answers this one.
    Delete,
    /// Awaiting `model_changed`, which carries its own error field.
    Model,
    /// Awaiting `reasoning_effort_changed`.
    ReasoningEffort,
    /// Awaiting `compacted_history`.
    Compact,
    /// Awaiting the catalog reply that answers `list_models`.
    Models,
    RuntimeInfo,
    /// Awaiting `background_tool_result`.
    BackgroundTool,
    /// Awaiting `soft_interrupts_cancelled`.
    SoftInterruptCancel,
    /// Awaiting `cancel_turn_result`.
    CancelTurnResult,
    /// Awaiting `tool_call_cancel_result`.
    ToolCallCancelResult,
    /// Awaiting `background_work_list`.
    BackgroundWorkList,
    /// Awaiting `runtime_state`.
    RuntimeState,
    /// Awaiting `effective_settings`.
    EffectiveSettings,
    /// Awaiting `todo_edit_result` for one revision-checked status change.
    TodoEdit,
    /// Awaiting `control_receipt` from `apply_control` (daemon Error means invalid request).
    ApplyControl,
    /// Awaiting `control_receipt` from `get_control_receipt`.
    ControlReceipt,
    /// Awaiting `snapshot_result`.
    Snapshot,
    Credential {
        provider: String,
        configured: bool,
    },
}

impl SimpleKind {
    pub(super) fn error_is_reply_only(&self) -> bool {
        matches!(
            self,
            Self::Tools | Self::ToolControl | Self::ContinueRun | Self::ApplyControl | Self::ControlReceipt
        )
    }
}
