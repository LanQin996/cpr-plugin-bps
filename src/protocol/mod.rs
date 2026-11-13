//! BPS 原生协议转换，移植自 bps-local 的纯协议部分

mod content;
mod output;
mod request;
mod structured;
mod tools;
pub use request::{Prepared, prepare};
pub use tools::ReplayCache;

use gateway_plugin_sdk::PluginFault;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
fn fingerprint(value: &Value) -> String {
    let raw = serde_json::to_vec(value).unwrap_or_default();
    let digest = Sha256::digest(raw);
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn message(role: &str, content: &str) -> Value {
    json!({"type":"message","role":role,"content":[{"type":"input_text","text":content}]})
}
fn error(message: &'static str) -> PluginFault {
    crate::config::invalid(message)
}
fn is_tool(value: &Value) -> bool {
    matches!(text(&value["type"]), "function_call" | "custom_tool_call")
}

pub fn normalize_effort(effort: &str) -> Result<&'static str, PluginFault> {
    match effort.trim().to_ascii_lowercase().as_str() {
        "" | "medium" => Ok("medium"),
        "low" | "none" | "minimal" => Ok("low"),
        "high" => Ok("high"),
        "xhigh" | "x-high" | "extra-high" | "extra_high" | "max" | "ultra" => Ok("xhigh"),
        _ => Err(error("Unsupported BPS reasoning effort.")),
    }
}
