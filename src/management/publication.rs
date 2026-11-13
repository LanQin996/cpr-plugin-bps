//! 以当前管理员身份更新本实例配置，保留绑定和并发版本，不访问外部管理地址

use crate::{Config, config::invalid};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        management::ManagementRequest,
        middleware::{MiddlewareHeader, http::Version},
    },
    client::{HttpBody, HttpFrame, HttpRequest, TypedCall},
};
use serde_json::{Value, json};

pub async fn save(
    previous: &Config,
    next: &Config,
    call: &TypedCall<ManagementRequest>,
) -> Result<(), PluginFault> {
    require_admin(call)?;
    let rows = admin(call, "GET", "/api/admin/plugins/instances", None).await?;
    let row = rows
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["id"] == call.context.instance_id)
        })
        .ok_or_else(|| invalid("找不到当前插件实例"))?;
    if !row["enabled"].as_bool().unwrap_or(false)
        || Config::parse(row["configuration"].clone())? != *previous
    {
        return Err(PluginFault::new(
            ErrorCode::Conflict,
            "插件配置已变化，请刷新后继续",
        ));
    }
    let request = json!({"id":call.context.instance_id,"instance":{
        "name":row["name"],"artifactSha256":row["artifactSha256"],"enabled":true,
        "configuration":next,"bindings":row["bindings"],"expectedRevision":row["revision"]
    }});
    // 自身配置发布放在操作最后，宿主会为新 revision 建立新进程和页面会话
    admin(
        call,
        "POST",
        "/api/admin/plugins/instances/update",
        Some(request),
    )
    .await?;
    Ok(())
}

pub(crate) fn require_admin(call: &TypedCall<ManagementRequest>) -> Result<(), PluginFault> {
    if !call.request.headers.iter().any(|header| {
        matches!(
            header.name.to_ascii_lowercase().as_str(),
            "authorization" | "cookie" | "x-api-key"
        )
    }) {
        return Err(PluginFault::new(
            ErrorCode::PermissionDenied,
            "当前管理请求缺少宿主认证，请重新登录宿主后再试",
        ));
    }
    Ok(())
}

pub(super) async fn admin(
    call: &TypedCall<ManagementRequest>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, PluginFault> {
    let mut headers: Vec<MiddlewareHeader> = call
        .request
        .headers
        .iter()
        .filter(|header| {
            matches!(
                header.name.to_ascii_lowercase().as_str(),
                "authorization" | "cookie" | "x-api-key" | "x-csrf-token" | "origin" | "referer"
            )
        })
        .cloned()
        .collect();
    let body = match body {
        Some(value) => {
            headers.push(MiddlewareHeader {
                name: "content-type".into(),
                value: b"application/json".to_vec(),
            });
            HttpBody::from_bytes(
                serde_json::to_vec(&value).map_err(|_| invalid("无法编码插件配置"))?,
            )
        }
        None => HttpBody::empty(),
    };
    let mut response = call
        .host
        .dispatch_http(HttpRequest {
            settings: Value::Null,
            method: method.into(),
            uri: path.into(),
            version: Version::Http11,
            headers,
            timeout_ms: Some(120_000),
            body,
        })
        .await?;
    let mut bytes = vec![];
    loop {
        match response.body.read().await {
            Ok(Some(HttpFrame::Data(chunk))) if bytes.len() + chunk.len() <= 8 * 1024 * 1024 => {
                bytes.extend(chunk)
            }
            Ok(Some(HttpFrame::Trailers(_))) => {}
            Ok(None) => break,
            result => {
                let _ = response.body.close().await;
                return Err(result.err().unwrap_or_else(|| invalid("宿主管理响应过大")));
            }
        }
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| invalid("宿主管理接口未返回 JSON"))?;
    if !(200..300).contains(&response.status) || value["code"] != 200 {
        let mut error = PluginFault::new(
            if response.status == 409 {
                ErrorCode::Conflict
            } else {
                ErrorCode::Rejected
            },
            value["message"].as_str().unwrap_or("宿主未接受配置更新"),
        );
        error.http_status = Some(response.status);
        return Err(error);
    }
    Ok(value["data"].clone())
}
