//! MCP (Model Context Protocol) client implementation
//!
//! Connects to MCP servers via JSON-RPC over stdio, Streamable HTTP, or legacy
//! HTTP+SSE. Remote servers support static headers and explicit OAuth login.
//! Supports shared server pools so multiple sessions reuse the same
//! MCP server processes instead of spawning duplicates.

pub mod budget;
mod client;
mod manager;
pub mod oauth;
pub mod pool;
mod protocol;
mod remote;
pub mod schema_cache;
mod sources;
mod tool;

pub use client::{
    DEFAULT_MCP_REQUEST_TIMEOUT, McpClient, McpHandle, PROMPTS_LIST_CHANGED_NOTIFICATION,
    RESOURCES_LIST_CHANGED_NOTIFICATION, TRANSPORT_CLOSED_NOTIFICATION, request_timeout_for,
    unsupported_capability_message,
};
pub use manager::McpManager;
pub use pool::{SharedMcpPool, get_shared_pool, init_shared_pool};
pub use protocol::*;
pub use schema_cache::{McpSchemaCache, fingerprint_config};
pub use tool::{
    McpTool, create_mcp_tools, create_mcp_tools_from_cached, create_mcp_tools_from_cached_many, dispatch_name,
    dispatch_names, render_prompt_result, render_resource_body, render_resource_content, take_accept_large_output,
};
