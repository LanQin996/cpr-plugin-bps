//! 规范化文本和 HTTPS 图片，保留加密状态并拒绝不可表达的内容

use super::{error, text};
use gateway_plugin_sdk::PluginFault;
use serde_json::{Value, json};

pub fn normalize(source: &mut Value) -> Result<(), PluginFault> {
    let Some(input) = source["input"].as_array_mut() else {
        return Ok(());
    };
    for item in input {
        for field in ["content", "output"] {
            if field == "output"
                && !matches!(
                    text(&item["type"]),
                    "function_call_output" | "custom_tool_call_output"
                )
            {
                continue;
            }
            let Some(parts) = item.get_mut(field).and_then(Value::as_array_mut) else {
                continue;
            };
            for part in parts {
                if let Some(value) = part.as_str() {
                    *part = json!({"type":"input_text","text":value});
                    continue;
                }
                let kind = text(&part["type"]);
                if !matches!(kind, "image" | "image_url" | "input_image") {
                    continue;
                }
                let mut url = part["image_url"].clone();
                let mut detail = part.get("detail").cloned();
                if url.is_object() {
                    if let Some(value) = url.get("detail") {
                        detail = Some(value.clone());
                    }
                    url = url["url"].clone();
                }
                if kind == "image" {
                    if part["source"]["type"] == "url" {
                        url = part["source"]["url"].clone();
                    } else {
                        return Err(error(
                            "BPS image input requires an HTTPS URL; inline base64 is unsupported.",
                        ));
                    }
                }
                let mut result = json!({"type":"input_image","image_url":url});
                if let Some(value) = detail {
                    result["detail"] = value;
                }
                if let Some(value) = part.get("file_id") {
                    result["file_id"] = value.clone();
                }
                validate(&json!([result.clone()]), false)?;
                *part = result;
            }
        }
    }
    Ok(())
}

pub fn validate(value: &Value, agent: bool) -> Result<(), PluginFault> {
    if agent && !value.is_array() {
        return Err(error("BPS agent_message content must be an array."));
    }
    if value.is_null() || value.is_string() {
        return Ok(());
    }
    let parts = value
        .as_array()
        .ok_or_else(|| error("BPS content must be text or an array."))?;
    for part in parts {
        match text(&part["type"]) {
            "input_text" | "output_text" | "text" | "refusal" => {}
            "encrypted_content"
                if agent
                    && part["encrypted_content"]
                        .as_str()
                        .is_some_and(|v| !v.is_empty()) => {}
            "input_image" => {
                let raw = text(&part["image_url"]);
                let url = url::Url::parse(raw)
                    .map_err(|_| error("BPS images require an absolute HTTPS URL."))?;
                if raw.trim() != raw
                    || url.scheme() != "https"
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || !text(&part["file_id"]).is_empty()
                    || part.get("detail").is_some_and(|v| {
                        !v.is_null() && !matches!(text(v), "auto" | "low" | "high")
                    })
                {
                    return Err(error(
                        "BPS images require HTTPS URLs without credentials or file IDs and detail auto, low or high.",
                    ));
                }
            }
            _ => {
                return Err(error(
                    "BPS supports text, encrypted agent state and HTTPS image content only.",
                ));
            }
        }
    }
    Ok(())
}
