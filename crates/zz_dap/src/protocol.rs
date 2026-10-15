//! Minimal Debug Adapter Protocol types (subset handshake).
//!
//! Only the messages `zz-dap` implements are modeled. Unknown commands
//! get a `success: false` response instead of a disconnect, so newer
//! editors degrade instead of dying.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct Request {
    pub seq: u64,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    pub typ: String,
    pub command: String,
    pub arguments: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub seq: u64,
    #[serde(rename = "type")]
    pub typ: &'static str,
    pub request_seq: u64,
    pub success: bool,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

impl Response {
    pub fn ok(seq: u64, request_seq: u64, command: &str, body: Option<Value>) -> Self {
        Self {
            seq,
            typ: "response",
            request_seq,
            success: true,
            command: command.to_string(),
            message: None,
            body,
        }
    }

    pub fn err(seq: u64, request_seq: u64, command: &str, message: String) -> Self {
        Self {
            seq,
            typ: "response",
            request_seq,
            success: false,
            command: command.to_string(),
            message: Some(message),
            body: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Event {
    pub seq: u64,
    #[serde(rename = "type")]
    pub typ: &'static str,
    pub event: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct LaunchArgs {
    pub program: String,
}

#[derive(Debug, Deserialize)]
pub struct SetBreakpointsArgs {
    pub source: SourceRef,
    #[serde(default)]
    pub breakpoints: Vec<SourceBreakpoint>,
}

#[derive(Debug, Deserialize)]
pub struct SourceRef {
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SourceBreakpoint {
    pub line: u32,
}

#[derive(Debug, Deserialize)]
pub struct StackTraceArgs {
    #[serde(rename = "threadId")]
    pub thread_id: u64,
}

#[derive(Debug, Deserialize)]
pub struct ScopesArgs {
    #[serde(rename = "frameId")]
    pub frame_id: u64,
}

#[derive(Debug, Deserialize)]
pub struct VariablesArgs {
    #[serde(rename = "variablesReference")]
    pub variables_reference: u64,
    #[serde(default)]
    pub start: Option<u64>,
    #[serde(default)]
    pub count: Option<u64>,
}
