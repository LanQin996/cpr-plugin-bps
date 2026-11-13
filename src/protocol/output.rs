//! 校验权威终态后释放工具和结构化文本，过滤 BPS 控制帧与续传游标

use super::{Prepared, error, is_tool, text};
use gateway_plugin_sdk::PluginFault;
use serde_json::{Value, json};

impl Prepared {
    pub fn transform(&mut self, mut data: Value) -> Result<Vec<Value>, PluginFault> {
        let object = data
            .as_object_mut()
            .ok_or_else(|| error("Invalid BPS WebSocket JSON event."))?;
        object.remove("basispoints_replay_cursor");
        let kind = text(&data["type"]).to_owned();
        if kind.is_empty()
            || kind.len() > 128
            || !kind
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'.' || c == b'_')
        {
            return Err(error("Invalid BPS WebSocket event type."));
        }
        match kind.as_str() {
            "basispoints.response.resume_token"
            | "basispoints.response.metadata"
            | "basispoints.response.upstream_sent"
            | "basispoints.response.heartbeat" => return Ok(vec![]),
            "basispoints.response.server_draining" => {
                return Err(error(
                    "BPS server is draining. Automatic replay is disabled.",
                ));
            }
            _ => {}
        }
        if kind != "error" && !kind.starts_with("response.") {
            return Err(error("Unexpected BPS WebSocket event type."));
        }
        if kind.starts_with("response.function_call_arguments.")
            || kind.starts_with("response.custom_tool_call_input.")
        {
            return Ok(vec![]);
        }
        if kind.starts_with("response.output_item.") && is_tool(&data["item"]) {
            if kind == "response.output_item.done" {
                self.pending_tools.insert((
                    text(&data["item"]["call_id"]).into(),
                    text(&data["item"]["id"]).into(),
                ));
                if self.pending_tools.len() > 1024 {
                    return Err(error("BPS response exceeds 1024 tool items."));
                }
            }
            return Ok(vec![]);
        }
        if self.structured.is_some()
            && (kind.starts_with("response.output_text.")
                || kind.starts_with("response.refusal.")
                || kind.starts_with("response.content_part.")
                || (kind.starts_with("response.output_item.") && data["item"]["type"] == "message"))
        {
            return Ok(vec![]);
        }
        let mut events = vec![];
        if let Some(response) = data.get_mut("response") {
            if !response.is_object() {
                return Err(error("BPS response must be an object."));
            }
            if let Some(format) = &self.structured {
                response["text"] = json!({"format":format.format});
            }
            if kind == "response.completed" {
                if let Some(format) = &self.structured {
                    format.validate(response)?;
                }
                let output = response["output"]
                    .as_array()
                    .ok_or_else(|| error("BPS completed response is missing output."))?;
                let mut remaining = self.pending_tools.clone();
                let mut translated = vec![];
                let mut cache_writes = vec![];
                // 全部工具先验证，避免终态中后一个工具非法却提交前一个工具
                for item in output {
                    if is_tool(item) {
                        remaining
                            .remove(&(text(&item["call_id"]).into(), text(&item["id"]).into()));
                        let (client, native) = self.tools.translate(item)?;
                        cache_writes.push((native, client.clone()));
                        translated.push(client);
                    } else {
                        translated.push(item.clone());
                    }
                }
                if !remaining.is_empty() {
                    return Err(error(
                        "BPS completed response omitted an original tool item.",
                    ));
                }
                {
                    let mut cache = self
                        .replay
                        .lock()
                        .map_err(|_| error("BPS replay cache is unavailable."))?;
                    for (native, client) in cache_writes {
                        cache.put(&self.scope, native, &client);
                    }
                }
                for (index, item) in translated.iter().enumerate() {
                    if is_tool(item) {
                        tool_events(item, index, &mut events);
                    } else if self.structured.is_some() && item["type"] == "message" {
                        message_events(item, index, &mut events)?;
                    }
                }
                response["output"] = json!(translated);
                response["parallel_tool_calls"] = json!(false);
            } else if let Some(output) = response["output"].as_array() {
                response["output"] = json!(
                    output
                        .iter()
                        .filter(|item| !is_tool(item)
                            && (self.structured.is_none() || item["type"] != "message"))
                        .collect::<Vec<_>>()
                );
            }
            response["reasoning"] = json!({"effort":self.effort});
        } else if kind == "response.completed" {
            return Err(error("BPS completion is missing its terminal response."));
        }
        events.push(data);
        for event in &mut events {
            event["sequence_number"] = json!(self.sequence);
            self.sequence += 1;
        }
        Ok(events)
    }
}

fn tool_events(item: &Value, index: usize, events: &mut Vec<Value>) {
    let (field, prefix) = if item["type"] == "custom_tool_call" {
        ("input", "response.custom_tool_call_input")
    } else {
        ("arguments", "response.function_call_arguments")
    };
    let mut added = item.clone();
    added[field] = json!("");
    added["status"] = json!("in_progress");
    events.push(json!({"type":"response.output_item.added","output_index":index,"item":added}));
    events.push(json!({"type":format!("{prefix}.delta"),"output_index":index,"item_id":item["id"],"delta":item[field]}));
    let mut done =
        json!({"type":format!("{prefix}.done"),"output_index":index,"item_id":item["id"]});
    done[field] = item[field].clone();
    events.push(done);
    events.push(json!({"type":"response.output_item.done","output_index":index,"item":item}));
}

fn message_events(item: &Value, index: usize, events: &mut Vec<Value>) -> Result<(), PluginFault> {
    let mut added = item.clone();
    added["content"] = json!([]);
    added["status"] = json!("in_progress");
    events.push(json!({"type":"response.output_item.added","output_index":index,"item":added}));
    for (i, part) in item["content"]
        .as_array()
        .ok_or_else(|| error("Invalid BPS structured message."))?
        .iter()
        .enumerate()
    {
        let (field, prefix) = if part["type"] == "refusal" {
            ("refusal", "response.refusal")
        } else {
            ("text", "response.output_text")
        };
        let mut empty = part.clone();
        empty[field] = json!("");
        let mut parts = vec![
            json!({"type":"response.content_part.added","part":empty}),
            json!({"type":format!("{prefix}.delta"),"delta":part[field]}),
            json!({"type":format!("{prefix}.done")}),
            json!({"type":"response.content_part.done","part":part}),
        ];
        parts[2][field] = part[field].clone();
        for event in &mut parts {
            event["output_index"] = json!(index);
            event["item_id"] = item["id"].clone();
            event["content_index"] = json!(i);
        }
        events.extend(parts);
    }
    events.push(json!({"type":"response.output_item.done","output_index":index,"item":item}));
    Ok(())
}
