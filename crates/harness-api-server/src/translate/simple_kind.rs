//! What a queued request is waiting for, so the daemon reply that answers it
//! can be matched to the API request id it belongs to.

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SimpleKind {
    RuntimeCommand,
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
    /// Awaiting `control_receipt`.
    ControlReceipt,
    /// Awaiting `snapshot_result`.
    Snapshot,
    Credential {
        provider: String,
        configured: bool,
    },
}
