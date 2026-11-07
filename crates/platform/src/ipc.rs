//! Local IPC behind one API: a Unix socket on Unix, a named pipe on Windows.
//!
//! A listener and a stream are types, not functions, so this module re-exports
//! the adapter selected for this build instead of dispatching through
//! `Current`. Both adapters expose the same names and call shapes, so a caller
//! writes one code path and gets the platform's native mechanism.

#[cfg(target_os = "macos")]
pub use horon_agent_platform_macos::ipc::{
    Addr, InstanceLock, Listener, LockAttempt, ReadHalf, Stream, SyncStream, WriteHalf, acquire_bootstrap_lock,
    connect_probe_is_safe, daemon_lock_file_is_used, is_socket_path, lock_instance_file, mark_listener_close_on_exec,
    remove_socket, restrict_socket_to_owner, signal_ready_fd, socket_is_owner_only, socket_nodes_can_be_stale,
    stream_pair, sync_stream_shutdown_handle,
};

#[cfg(all(unix, not(target_os = "macos")))]
pub use horon_agent_platform_linux::ipc::{
    Addr, InstanceLock, Listener, LockAttempt, ReadHalf, Stream, SyncStream, WriteHalf, acquire_bootstrap_lock,
    connect_probe_is_safe, daemon_lock_file_is_used, is_socket_path, lock_instance_file, mark_listener_close_on_exec,
    remove_socket, restrict_socket_to_owner, signal_ready_fd, socket_is_owner_only, socket_nodes_can_be_stale,
    stream_pair, sync_stream_shutdown_handle,
};

#[cfg(windows)]
pub use horon_agent_platform_windows::ipc::{
    Addr, InstanceLock, Listener, LockAttempt, ReadHalf, SplitReadRef, SplitWriteRef, Stream, SyncStream, WriteHalf,
    acquire_bootstrap_lock, connect_probe_is_safe, daemon_lock_file_is_used, is_socket_path, lock_instance_file,
    mark_listener_close_on_exec, remove_socket, restrict_socket_to_owner, signal_ready_fd, socket_is_owner_only,
    socket_nodes_can_be_stale, stream_pair, sync_stream_shutdown_handle,
};
