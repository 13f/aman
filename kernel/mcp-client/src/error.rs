// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Error types for MCP client operations.

use thiserror::Error;

/// Errors that can occur during MCP client operations.
#[derive(Debug, Error)]
pub enum McpError {
    /// Failed to establish connection to the MCP server.
    #[error("MCP connection failed for server '{server}': {detail}")]
    ConnectionFailed { server: String, detail: String },

    /// Transport-level error during communication.
    #[error("MCP transport error on server '{server}': {detail}")]
    TransportError { server: String, detail: String },

    /// A tool call to the MCP server failed.
    #[error("MCP tool call failed for '{tool}' on server '{server}': {detail}")]
    ToolCallFailed {
        tool: String,
        server: String,
        detail: String,
    },

    /// The requested MCP server is not found in the manager.
    #[error("MCP server '{server}' not found")]
    ServerNotFound { server: String },

    /// The requested tool was not found on the MCP server.
    #[error("MCP tool '{tool}' not found on server '{server}'")]
    ToolNotFound { tool: String, server: String },

    /// A server with this name is already connected.
    #[error("MCP server '{server}' is already connected")]
    AlreadyConnected { server: String },

    /// JSON file I/O or parse error.
    #[error("MCP config file error: {0}")]
    ConfigFileError(String),

    /// Generic error.
    #[error("{0}")]
    Other(String),
}

/// Convenience result type for MCP operations.
pub type McpResult<T> = Result<T, McpError>;
