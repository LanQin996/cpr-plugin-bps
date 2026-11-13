//! 将 Responses 事件映射为宿主计量事实，保留客户端 wire 与真实错误

use crate::config::invalid;
use gateway_plugin_sdk::{
    PluginFault,
    call::{
        model::{
            CanonicalEvent as Fact, ContentKind, ExecutionEvent, FinishReason, Usage, WireEvent,
            WirePayload,
        },
        upstream_adapter::{UpstreamAdapterEvent, UpstreamFailure, UpstreamFailureKind},
    },
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct Events {
    id: Option<String>,
    model: Option<String>,
    parts: BTreeMap<(u32, u32, u8), u32>,
    tools: BTreeMap<u32, (String, String)>,
    terminal: bool,
    streaming: bool,
}

impl Events {
    pub fn new(streaming: bool) -> Self {
        Self {
            streaming,
            ..Self::default()
        }
    }

    pub fn terminal(&self) -> bool {
        self.terminal
    }

    pub fn check_identity(&self, data: &Value) -> Result<(), PluginFault> {
        if matches!(data["type"].as_str(), Some("error" | "response.failed")) {
            return Ok(());
        }
        if let Some(response) = data.get("response") {
            let id = response["id"]
                .as_str()
                .filter(|id| !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control))
                .ok_or_else(|| invalid("BPS response is missing a valid ID."))?;
            if self.id.as_ref().is_some_and(|current| current != id) {
                return Err(invalid("BPS changed its response ID during generation."));
            }
            if response.get("model").is_some_and(|value| {
                !value.is_null()
                    && value.as_str().is_none_or(|model| {
                        model.is_empty() || model.len() > 256 || model.chars().any(char::is_control)
                    })
            }) {
                return Err(invalid("BPS response has an invalid model identifier."));
            }
        }
        Ok(())
    }

    pub fn convert(&mut self, data: Value) -> Result<Option<UpstreamAdapterEvent>, PluginFault> {
        if self.terminal {
            return Err(invalid("BPS sent an event after termination."));
        }
        self.check_identity(&data)?;
        let kind = data
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("BPS event is missing its type."))?
            .to_owned();
        if !data.is_object() || (kind != "error" && !kind.starts_with("response.")) {
            return Err(invalid("BPS sent an unexpected event type."));
        }
        let mut facts = vec![];
        if kind == "response.failed" || kind == "error" {
            self.terminal = true;
            let error = data
                .pointer("/response/error")
                .or_else(|| data.get("error"))
                .unwrap_or(&data)
                .clone();
            return Ok(Some(failure_event(&error, None, None, Some(data))));
        }
        if let Some(response) = data.get("response") {
            let id = response
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| invalid("BPS response is missing its ID."))?;
            if self.id.is_none() {
                self.id = Some(id.into());
                self.model = response
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                facts.push(Fact::Started {
                    id: id.into(),
                    model: self.model.clone(),
                });
            }
        }
        if self.id.is_none() {
            return Err(invalid("BPS output arrived before response creation."));
        }
        let output = number(&data, "output_index");
        let content = number(&data, "content_index");
        match kind.as_str() {
            "response.output_text.delta" | "response.refusal.delta" => {
                let index = self.part((output?, content?, 0), ContentKind::Text, &mut facts)?;
                facts.push(Fact::TextDelta {
                    index,
                    text: string(&data, "delta")?,
                });
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let summary = data
                    .get("summary_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let index = self.part(
                    (
                        output?,
                        u32::try_from(summary)
                            .map_err(|_| invalid("Invalid BPS reasoning index."))?,
                        1,
                    ),
                    ContentKind::Reasoning,
                    &mut facts,
                )?;
                facts.push(Fact::ReasoningDelta {
                    index,
                    text: string(&data, "delta")?,
                });
            }
            "response.output_item.added" => {
                let item = &data["item"];
                if matches!(
                    item["type"].as_str(),
                    Some("function_call" | "custom_tool_call")
                ) {
                    let output = output?;
                    if self.tools.contains_key(&output) {
                        return Err(invalid("BPS repeated a tool item."));
                    }
                    let id = string(item, "call_id")?;
                    let name = string(item, "name")?;
                    self.tools.insert(output, (id.clone(), name.clone()));
                    let index = self.part((output, 0, 2), ContentKind::ToolCall, &mut facts)?;
                    facts.push(Fact::ToolCallDelta {
                        index,
                        id,
                        name: Some(name),
                        arguments: String::new(),
                    });
                }
            }
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                let output = output?;
                let (id, _) = self
                    .tools
                    .get(&output)
                    .ok_or_else(|| invalid("BPS tool arguments arrived before the tool item."))?
                    .clone();
                let index = self.part((output, 0, 2), ContentKind::ToolCall, &mut facts)?;
                facts.push(Fact::ToolCallDelta {
                    index,
                    id,
                    name: None,
                    arguments: string(&data, "delta")?,
                });
            }
            "response.completed" | "response.incomplete" => {
                let response = &data["response"];
                if response["id"].as_str() != self.id.as_deref() {
                    return Err(invalid("BPS terminal event has an invalid response ID."));
                }
                if let Some(usage) = response.get("usage").filter(|value| value.is_object()) {
                    facts.push(Fact::Usage {
                        usage: parse_usage(usage)?,
                    });
                }
                let reason = if kind == "response.incomplete" {
                    match response
                        .pointer("/incomplete_details/reason")
                        .and_then(Value::as_str)
                    {
                        Some("max_output_tokens") => FinishReason::Length,
                        Some("content_filter") => FinishReason::ContentFilter,
                        _ => FinishReason::Other,
                    }
                } else if !self.tools.is_empty() {
                    FinishReason::ToolCall
                } else {
                    FinishReason::Stop
                };
                facts.push(Fact::Completed {
                    id: self.id.clone().unwrap_or_default(),
                    model: response
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| self.model.clone()),
                    reason,
                });
                self.terminal = true;
            }
            _ => {}
        }
        let wire = if self.streaming {
            Some(WireEvent {
                protocol: "openai".into(),
                payload: WirePayload::Json {
                    event: Some(kind),
                    data,
                    id: None,
                    retry: None,
                    raw_sse: None,
                },
            })
        } else if self.terminal {
            Some(WireEvent {
                protocol: "openai".into(),
                payload: WirePayload::RawJson {
                    body: serde_json::to_vec(&data["response"])
                        .map_err(|_| invalid("Could not encode the BPS response."))?,
                },
            })
        } else {
            None
        };
        Ok(Some(UpstreamAdapterEvent::new(ExecutionEvent {
            facts,
            wire,
            host: None,
        })))
    }

    fn part(
        &mut self,
        key: (u32, u32, u8),
        kind: ContentKind,
        facts: &mut Vec<Fact>,
    ) -> Result<u32, PluginFault> {
        if let Some(&index) = self.parts.get(&key) {
            return Ok(index);
        }
        if self.parts.len() >= 4096 {
            return Err(invalid("BPS response exceeds 4096 content parts."));
        }
        let index = self.parts.len() as u32;
        self.parts.insert(key, index);
        facts.push(Fact::ContentAdded { index, kind });
        Ok(index)
    }
}

fn number(value: &Value, key: &str) -> Result<u32, PluginFault> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid("BPS event is missing a valid content index."))
}

fn string(value: &Value, key: &str) -> Result<String, PluginFault> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| invalid("BPS event is missing a required string."))
}

fn parse_usage(value: &Value) -> Result<Usage, PluginFault> {
    let token = |path: &str| match value.pointer(path) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|n| *n <= i64::MAX as u64)
            .map(Some)
            .ok_or_else(|| invalid("BPS returned an invalid token count.")),
    };
    Ok(Usage {
        input_tokens: token("/input_tokens")?,
        output_tokens: token("/output_tokens")?,
        cached_tokens: token("/input_tokens_details/cached_tokens")?,
        reasoning_tokens: token("/output_tokens_details/reasoning_tokens")?,
        total_tokens: token("/total_tokens")?,
        ..Usage::default()
    })
}

/// 原样携带失败 wire，不将 WebSocket 拒绝或流内失败降为未发送
pub fn failure_event(
    error: &Value,
    status: Option<u16>,
    retry_after_ms: Option<u64>,
    wire: Option<Value>,
) -> UpstreamAdapterEvent {
    let code = error.get("code").and_then(Value::as_str).map(str::to_owned);
    let error_type = error.get("type").and_then(Value::as_str).map(str::to_owned);
    let status = status
        .or_else(|| {
            error
                .get("status")
                .and_then(Value::as_u64)
                .and_then(|s| u16::try_from(s).ok())
        })
        .filter(|s| (400..=599).contains(s));
    let kind = match (status, code.as_deref()) {
        (_, Some("insufficient_quota" | "quota_exceeded")) => UpstreamFailureKind::QuotaExhausted,
        (Some(401), _) => UpstreamFailureKind::Unauthorized,
        (Some(403), _) => UpstreamFailureKind::PermissionDenied,
        (Some(429), _) | (_, Some("rate_limit_exceeded")) => UpstreamFailureKind::RateLimited,
        (Some(408 | 504), _) | (_, Some("basispoints_ws_timeout")) => UpstreamFailureKind::Timeout,
        (Some(400 | 422), _) => UpstreamFailureKind::InvalidRequest,
        (Some(500..=599), _) => UpstreamFailureKind::Unavailable,
        _ => UpstreamFailureKind::Protocol,
    };
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("BPS bridge failed.")
        .to_owned();
    let wire = wire.unwrap_or_else(|| json!({"type":"response.failed","response":{"status":"failed","output":[],"error":{"code":code,"message":message,"type":error_type}}}));
    let event = ExecutionEvent::wire(WireEvent {
        protocol: "openai".into(),
        payload: WirePayload::Json {
            event: Some(wire["type"].as_str().unwrap_or("response.failed").into()),
            data: wire,
            id: None,
            retry: None,
            raw_sse: None,
        },
    });
    let mut event = UpstreamAdapterEvent::new(event);
    event.failure = Some(UpstreamFailure {
        kind,
        error_type,
        status,
        retry_after_ms,
        message,
        code,
    });
    event
}
