//! 原生 BPS WebSocket 出站、控制帧过滤、取消和唯一终态回收

use crate::{
    Config, MAX_EVENT_BYTES, auth,
    config::invalid,
    events::{Events, failure_event},
    protocol::{self, Prepared, ReplayCache},
};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        model::facts::ResponseObservation,
        upstream_adapter::{
            BuiltinProvider, UpstreamAdapterEvent, UpstreamAdapterRequest,
            UpstreamWebSocketRequest, WebSocketMessageKind,
        },
    },
    client::{
        Empty, PullResponseFuture, PullResponseStream, ResponseStream, TypedCall, TypedReply,
        UpstreamWebSocket, UpstreamWebSocketUpgrade,
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

pub async fn execute(
    config: &Config,
    replay: Arc<Mutex<ReplayCache>>,
    call: TypedCall<UpstreamAdapterRequest>,
) -> Result<TypedReply<Empty>, PluginFault> {
    let declaration = config
        .registration()
        .adapters
        .into_iter()
        .find(|adapter| adapter.id == call.request.adapter_id)
        .ok_or_else(|| invalid("Unknown BPS adapter."))?;
    let account = config
        .accounts
        .iter()
        .find(|account| account.account_id == call.request.account_id)
        .ok_or_else(|| invalid("Unknown BPS adapter."))?;
    if !declaration.account_ids.contains(&call.request.account_id)
        || call.request.provider != BuiltinProvider::OpenAi
        || call.request.protocol != "openai"
        || !account.models.contains(&call.request.upstream_model)
    {
        return Err(invalid(
            "BPS account, provider or model does not match the adapter configuration.",
        ));
    }
    if call.request.continuation.is_some() {
        return Err(invalid(
            "BPS requires expanded history instead of native continuation.",
        ));
    }
    let (credentials, version) =
        if account.credential_source == crate::config::CredentialSource::Host {
            auth::host_credentials(
                &call.host,
                &account.account_id,
                call.request.credential_revision,
            )
            .await?
        } else {
            auth::credentials(&call.host, &account.account_id).await?
        };
    let session = call
        .request
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("session_id"))
        .and_then(|(_, value)| std::str::from_utf8(value).ok())
        .unwrap_or("");
    // 状态删除后版本可能从头计数，另绑定 Token 摘要避免跨重新登录复用历史
    let token_identity = Sha256::digest(credentials.access_token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let scope = json!([
        call.request.client_key_id,
        account.account_id,
        call.request.credential_revision,
        version,
        token_identity,
        session
    ])
    .to_string();
    let mut prepared =
        protocol::prepare(&call.payload, &call.request.upstream_model, &scope, replay)?;
    let streaming = prepared.streaming;
    let request_id = Uuid::new_v4().to_string();
    prepared.body["basispoints_request_id"] = json!(request_id);
    prepared.body["basispoints_resume_token"] =
        json!(format!("resume_{}", Uuid::new_v4().simple()));
    let profile = client_profile();
    let mut headers = vec![
        (
            "authorization".into(),
            format!("Bearer {}", credentials.access_token).into_bytes(),
        ),
        (
            "chatgpt-account-id".into(),
            credentials.account_id.as_bytes().to_vec(),
        ),
        (
            "x-openai-account-id".into(),
            credentials.account_id.into_bytes(),
        ),
        ("x-basispoints-auth-mode".into(), b"chatgpt".to_vec()),
        ("origin".into(), b"https://bps.openai.com".to_vec()),
        ("user-agent".into(), b"Mozilla/5.0".to_vec()),
        ("accept".into(), b"*/*".to_vec()),
    ];
    // BPS 接受 Authorization 认证，不声明宿主受管握手禁止的 Sec-WebSocket-* 头
    headers.extend(
        profile
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key.clone(), value.as_str().unwrap().as_bytes().to_vec())),
    );
    let upgrade = call
        .host
        .upstream_websocket(UpstreamWebSocketRequest {
            path: "basispoints/api/responses".into(),
            headers,
            query: vec![
                ("bps_client_info".into(), profile.to_string()),
                ("bps_auth_mode".into(), "chatgpt".into()),
                ("bps_ws_affinity".into(), request_id),
                (
                    "bps_control_frames".into(),
                    "upstream_sent,heartbeat".into(),
                ),
            ],
        })
        .await?;
    let (connection, http_version, headers) = match upgrade {
        UpstreamWebSocketUpgrade::Connected {
            connection,
            http_version,
            headers,
        } => (connection, http_version, headers),
        UpstreamWebSocketUpgrade::Rejected {
            status,
            http_version,
            headers,
            body,
            ..
        } => {
            let doc: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let retry = header(&headers, "retry-after")
                .and_then(|v| v.parse::<u64>().ok())
                .map(|v| v.saturating_mul(1000).min(86_400_000));
            let mut event =
                failure_event(doc.get("error").unwrap_or(&doc), Some(status), retry, None);
            event.observation = Some(observation(http_version, status, &headers));
            return Ok(TypedReply::new(Empty {})
                .with_stream(ResponseStream::from_chunks(vec![encode(event)?])));
        }
    };
    let body = serde_json::to_vec(&prepared.body)
        .map_err(|_| invalid("Could not encode BPS response.create."))?;
    if let Err(error) = connection.send(WebSocketMessageKind::Text, body).await {
        let _ = connection.close().await;
        return Err(error);
    }
    // 初始回复之后才等待输出，长推理不占用初始 RPC 的建立预算
    Ok(
        TypedReply::new(Empty {}).with_stream(ResponseStream::pull(Box::new(NativeStream {
            connection,
            prepared,
            events: Events::new(streaming),
            pending: VecDeque::new(),
            observation: Some(observation(http_version, 101, &headers)),
            ended: false,
        }))),
    )
}

fn header<'a>(headers: &'a [(String, Vec<u8>)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| std::str::from_utf8(value).ok())
}
fn observation(version: String, status: u16, headers: &[(String, Vec<u8>)]) -> ResponseObservation {
    let mut value = ResponseObservation::new("websocket");
    value.http_version = Some(version);
    value.status_code = Some(status);
    value.request_id = header(headers, "x-request-id").map(str::to_owned);
    value
}

pub fn client_profile() -> Value {
    let platform = if cfg!(target_os = "macos") {
        "Mac"
    } else {
        "PC"
    };
    json!({
        "X-Openai-Internal-Basispoints-Client-Product":"basispoints-excel-plugin",
        "X-Openai-Internal-Basispoints-Client-Platform":"excel",
        "X-Openai-Internal-Basispoints-Client-Agent-Profile":"excel",
        "X-Openai-Internal-Basispoints-Client-Editor":"excel",
        "X-Openai-Internal-Basispoints-Client-Host":"office",
        "X-Openai-Internal-Basispoints-Client-Runtime":"desktop",
        "X-Openai-Internal-Basispoints-Client-Platform-Class":platform,
        "X-Openai-Internal-Basispoints-Office-Host":"Excel",
        "X-Openai-Internal-Basispoints-Office-Platform":platform,
        "X-Openai-Internal-Basispoints-Browser-Name":"unknown"
    })
}

struct NativeStream {
    connection: UpstreamWebSocket,
    prepared: Prepared,
    events: Events,
    pending: VecDeque<Value>,
    observation: Option<ResponseObservation>,
    ended: bool,
}
impl NativeStream {
    async fn pull(&mut self) -> Result<Option<Vec<u8>>, PluginFault> {
        if self.ended {
            return Ok(None);
        }
        loop {
            if let Some(data) = self.pending.pop_front() {
                if let Some(mut event) = self.events.convert(data)? {
                    event.observation = self.observation.take();
                    if self.events.terminal() {
                        self.connection.close().await?;
                        self.ended = true;
                    }
                    return Ok(Some(encode(event)?));
                }
            } else {
                let message = tokio::time::timeout(
                    Duration::from_secs(120),
                    self.connection.read(),
                )
                .await
                .map_err(|_| {
                    PluginFault::new(
                        ErrorCode::Timeout,
                        "BPS WebSocket was idle for 120 seconds. The request was not replayed.",
                    )
                })??;
                let (kind,raw) = message.ok_or_else(||invalid("BPS WebSocket closed without a terminal event. The request was not replayed."))?;
                if kind != WebSocketMessageKind::Text || raw.len() > MAX_EVENT_BYTES {
                    return Err(invalid(
                        "BPS WebSocket event is invalid or exceeds its size limit.",
                    ));
                }
                let data: Value = serde_json::from_slice(&raw)
                    .map_err(|_| invalid("BPS sent invalid WebSocket JSON."))?;
                // 先检查响应身份，再释放由终态重建的客户端工具事件
                self.events.check_identity(&data)?;
                self.pending.extend(self.prepared.transform(data)?);
            }
        }
    }
}
impl PullResponseStream for NativeStream {
    fn next(&mut self) -> PullResponseFuture<'_> {
        Box::pin(async move {
            match self.pull().await {
                Ok(value) => value.map(Ok),
                Err(error) => {
                    self.ended = true;
                    let _ = self.connection.close().await;
                    Some(Err(error))
                }
            }
        })
    }
}
fn encode(event: UpstreamAdapterEvent) -> Result<Vec<u8>, PluginFault> {
    event
        .encode()
        .map_err(|_| invalid("BPS event exceeds the host event budget."))
}
