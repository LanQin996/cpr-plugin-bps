//! 以白名单编码 BPS response.create，保留模型、工具目录和完整历史

use super::{
    content, error, fingerprint, message, normalize_effort,
    structured::Structured,
    text,
    tools::{SharedReplay, Tools},
};
use gateway_plugin_sdk::PluginFault;
use serde_json::{Value, json};

pub struct Prepared {
    pub body: Value,
    pub streaming: bool,
    pub effort: String,
    pub(crate) tools: Tools,
    pub(crate) structured: Option<Structured>,
    pub(crate) scope: String,
    pub(crate) replay: SharedReplay,
    pub(crate) sequence: u64,
    pub(crate) pending_tools: std::collections::BTreeSet<(String, String)>,
}

pub fn prepare(
    raw: &[u8],
    model: &str,
    identity_scope: &str,
    replay: SharedReplay,
) -> Result<Prepared, PluginFault> {
    let mut source: Value = serde_json::from_slice(raw)
        .map_err(|_| error("BPS requires a valid Responses JSON object."))?;
    if !source.is_object() || model.is_empty() {
        return Err(error("BPS requires a model and a Responses JSON object."));
    }
    if source
        .get("previous_response_id")
        .is_some_and(|id| !id.is_null() && id != "")
    {
        return Err(error(
            "BPS requires expanded history instead of previous_response_id.",
        ));
    }
    if source
        .get("background")
        .is_some_and(|v| !v.is_null() && v != false)
        || source
            .get("generate")
            .is_some_and(|v| !v.is_null() && v != true)
    {
        return Err(error(
            "BPS does not support background generation or connection prewarming.",
        ));
    }
    let streaming = match source.get("stream") {
        None | Some(Value::Bool(true)) => true,
        Some(Value::Bool(false)) => false,
        _ => return Err(error("Responses stream must be a boolean.")),
    };
    content::normalize(&mut source)?;
    let input = match &source["input"] {
        Value::String(value) => vec![message("user", value)],
        Value::Array(input) => input.clone(),
        _ => return Err(error("BPS input must be text or a Responses item array.")),
    };
    let explicit = source
        .pointer("/reasoning/effort")
        .or_else(|| source.get("reasoning_effort"))
        .map(text)
        .unwrap_or("");
    if source
        .pointer("/reasoning/mode")
        .is_some_and(|value| value != "standard")
    {
        return Err(error("BPS supports standard reasoning mode only."));
    }
    let mut effort = explicit.to_owned();
    for item in &input {
        if item["type"] == "configuration_update" {
            let object = item
                .as_object()
                .ok_or_else(|| error("Invalid history configuration."))?;
            let reasoning = item["reasoning"]
                .as_object()
                .ok_or_else(|| error("History configuration requires reasoning."))?;
            if object.keys().any(|key| key != "type" && key != "reasoning")
                || reasoning.keys().any(|key| key != "effort" && key != "mode")
                || reasoning.get("mode").is_some_and(|mode| mode != "standard")
                || text(&item["reasoning"]["effort"]).is_empty()
            {
                return Err(error("Unsupported history reasoning configuration."));
            }
            let updated = normalize_effort(text(&item["reasoning"]["effort"]))?;
            if explicit.is_empty() {
                effort = updated.into();
            }
        }
    }
    let effort = normalize_effort(&effort)?.to_owned();
    let structured = Structured::new(&source["text"])?;
    if source
        .get("tool_choice")
        .is_some_and(|value| !value.is_null() && value != "auto" && value != "none")
    {
        return Err(error("BPS supports tool_choice auto or none only."));
    }
    let mut tools = Tools::default();
    if source["tool_choice"] != "none" {
        tools.merge(&source["tools"])?;
        for item in &input {
            if item["type"] == "additional_tools" {
                tools.merge(&item["tools"])?;
            }
        }
    }
    let conversation = if !text(&source["prompt_cache_key"]).is_empty() {
        source["prompt_cache_key"].clone()
    } else {
        input.first().cloned().unwrap_or(Value::Null)
    };
    let scope = fingerprint(&json!([identity_scope, conversation]));
    let history = tools.history(&input, &scope, &replay)?;
    let mut output = vec![];
    if !text(&source["instructions"]).is_empty() {
        output.push(message("developer", text(&source["instructions"])));
    }
    let mut prompt = if tools.catalog.is_empty() {
        "This request comes from an external Responses client. Return assistant text. Do not call Excel, Office, workbook or connector tools.".to_owned()
    } else {
        let mut prompt = include_str!("tool_protocol.txt").to_owned();
        for tool in tools.catalog.values() {
            prompt.push_str("\nClient tool contract: ");
            prompt.push_str(&tool.entry.to_string());
        }
        prompt
    };
    if !tools.unsupported.is_empty() {
        prompt.push_str(&format!("\nHosted tools unavailable through BPS: {}. These declarations were omitted. Never claim to have used them.", tools.unsupported.iter().cloned().collect::<Vec<_>>().join(", ")));
    }
    if let Some(format) = &structured {
        prompt.push('\n');
        prompt.push_str(&format.instructions());
    }
    output.push(message("developer", &prompt));
    output.extend(history);
    let mut turn_end = usize::from(!input.is_empty());
    let mut iteration = 1;
    for (i, item) in input.iter().enumerate().rev() {
        if item["role"] == "user" {
            turn_end = i + 1;
            break;
        }
        if text(&item["type"]).ends_with("_call_output") {
            iteration += 1;
        }
    }
    let mut body = json!({
        "type":"response.create", "model":model, "model_selection":"explicit", "stream":true,"store":false,
        "input":output,"reasoning_effort":effort,
        "context_management": [{"type":"compaction","compact_threshold":200000}],
        "metadata":{"task_id":fingerprint(&json!([scope,conversation])),"turn_id":fingerprint(&json!([scope,&input[..turn_end]])),"agent_iteration":iteration.to_string()}
    });
    if let Some(management) = source.get("context_management").filter(|v| v.is_array()) {
        body["context_management"] = management.clone();
    }
    if !text(&source["prompt_cache_key"]).is_empty() {
        body["prompt_cache_key"] = json!(format!(
            "bps-{}",
            fingerprint(&json!([scope, source["prompt_cache_key"]]))
        ));
    }
    Ok(Prepared {
        body,
        streaming,
        effort,
        tools,
        structured,
        scope,
        replay,
        sequence: 0,
        pending_tools: Default::default(),
    })
}
