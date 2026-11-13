//! 管理页面的状态、登录与自动接入路由，不向页面返回任何 Token

mod onboarding;
mod publication;
mod selection;

use crate::{
    Config, auth,
    config::{Account, CredentialSource, invalid},
};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::host::{self, AuthGetRequest, AuthListRequest, AuthListResult, AuthRuntimeAccount},
    call::management::{
        ManagementPage, ManagementRegistration, ManagementRequest, ManagementResource,
        ManagementResponse, ManagementRoute,
    },
    client::{TypedCall, TypedReply},
};
use serde_json::{Value, json};

pub const DEFAULT_MODEL: &str = "gpt-6-astra";

pub fn registration() -> ManagementRegistration {
    ManagementRegistration {
        routes: [
            ("GET", "api/snapshot"),
            ("POST", "api/onboarding/start"),
            ("POST", "api/onboarding/poll"),
            ("POST", "api/onboarding/import"),
            ("POST", "api/models"),
            ("POST", "api/accounts/use-host"),
            ("POST", "api/accounts/selection"),
            ("POST", "api/accounts/detach"),
            ("POST", "api/auth/start"),
            ("POST", "api/auth/poll"),
            ("POST", "api/auth/import"),
            ("POST", "api/auth/logout"),
        ]
        .into_iter()
        .map(|(method, path)| ManagementRoute {
            method: method.into(),
            path: path.into(),
            request_content_types: if method == "GET" {
                vec![]
            } else {
                vec!["application/json".into()]
            },
            response_content_types: vec!["application/json".into()],
        })
        .collect(),
        resources: ["web/index.html", "web/app.js", "web/app.css"]
            .into_iter()
            .map(|path| ManagementResource {
                path: path.into(),
                public: false,
            })
            .collect(),
        pages: vec![ManagementPage {
            id: "accounts".into(),
            title: "BPS 账号管理".into(),
            description: Some("复用宿主已有账号，无需重新登录，也可使用独立 BPS 授权".into()),
            entry: "web/index.html".into(),
            icon: None,
        }],
        callbacks: vec![],
    }
}

pub async fn handle(
    config: &Config,
    call: TypedCall<ManagementRequest>,
) -> Result<TypedReply<ManagementResponse>, PluginFault> {
    let result = route(config, &call).await;
    let (status, value) = match result {
        Ok(value) => (200, value),
        Err(error) => {
            let status = match error.code {
                ErrorCode::Conflict => 409,
                ErrorCode::PermissionDenied => 403,
                ErrorCode::Timeout => 504,
                ErrorCode::Upstream | ErrorCode::Fault => 502,
                _ => 400,
            };
            (
                status,
                json!({"error":{"code":format!("{:?}",error.code),"message":error.message}}),
            )
        }
    };
    Ok(TypedReply::new(ManagementResponse {
        status,
        content_type: "application/json".into(),
        headers: vec![],
    })
    .with_payload(serde_json::to_vec(&value).map_err(|_| invalid("无法编码管理响应"))?))
}

async fn route(config: &Config, call: &TypedCall<ManagementRequest>) -> Result<Value, PluginFault> {
    if !call.request.query.is_empty() || call.payload.len() > 64 * 1024 {
        return Err(invalid("管理请求不接受查询参数，正文不能超过 64 KiB"));
    }
    if call.request.method == "GET" && call.request.path == "api/snapshot" {
        if !call.payload.is_empty() {
            return Err(invalid("状态查询不能包含正文"));
        }
        return snapshot(config, call).await;
    }
    post(config, call).await
}

async fn snapshot(
    config: &Config,
    call: &TypedCall<ManagementRequest>,
) -> Result<Value, PluginFault> {
    let mut accounts = vec![];
    let host_accounts = host_accounts(&call.host).await?;
    for (index, account) in config.accounts.iter().enumerate() {
        let status = if account.credential_source == CredentialSource::Host {
            match host_accounts
                .iter()
                .find(|item| item.account_id == account.account_id)
            {
                Some(item) => host_status(item),
                None => {
                    json!({"configured":false,"valid":false,"status":"unavailable","message":"宿主账号已删除或认证方式发生变化"})
                }
            }
        } else {
            auth::perform(&call.host, &account.account_id, "status", None).await?
        };
        accounts.push(json!({"id":account.account_id,"label":if account.label.is_empty(){host_accounts.iter().find(|item|item.account_id==account.account_id).map(selection::account_label).unwrap_or_else(||format!("BPS 账号 {}",index+1))}else{account.label.clone()},"models":account.models,"credentialSource":account.credential_source,"auth":status}));
    }
    let directory = selection::with_accounts(call, host_accounts).await?;
    Ok(
        json!({"accounts":accounts,"hostAccounts":directory.accounts,"hostGroups":directory.groups,"hostSelection":selection::saved(config),"defaultModel":DEFAULT_MODEL,"onboarding":onboarding::snapshot(config,&call.host).await?}),
    )
}

// 在发布前读取新配置的页面快照。发布成功后，旧页面 target 已失效，不能再请求它。
async fn publish(
    config: &Config,
    next: &Config,
    call: &TypedCall<ManagementRequest>,
    mut reply: Value,
) -> Result<Value, PluginFault> {
    let snapshot = snapshot(next, call).await?;
    let changed = publication::save(config, next, call).await?;
    reply["reloadRequired"] = json!(changed);
    reply["snapshot"] = snapshot;
    Ok(reply)
}

async fn post(config: &Config, call: &TypedCall<ManagementRequest>) -> Result<Value, PluginFault> {
    if call.request.method != "POST"
        || call.request.content_type.as_deref() != Some("application/json")
    {
        return Err(invalid("管理操作需要 POST application/json"));
    }
    let body: Value =
        serde_json::from_slice(&call.payload).map_err(|_| invalid("管理请求不是有效 JSON"))?;
    if !body.is_object() {
        return Err(invalid("管理请求必须是 JSON 对象"));
    }
    match call.request.path.as_str() {
        "api/accounts/selection" => selection::apply(config, call, &body).await,
        "api/accounts/use-host" => {
            publication::require_admin(call)?;
            if config.accounts.len() >= 256 {
                return Err(invalid("最多接入 256 个 BPS 账号"));
            }
            let id = body["accountId"]
                .as_str()
                .ok_or_else(|| invalid("请选择宿主账号"))?;
            if config
                .accounts
                .iter()
                .any(|account| account.account_id == id)
            {
                return Err(PluginFault::new(
                    ErrorCode::Conflict,
                    "该账号已经接入，请刷新页面",
                ));
            }
            let account: AuthRuntimeAccount = auth::payload(
                &call.host,
                host::AUTH_GET_RUNTIME,
                AuthGetRequest {
                    account_id: id.into(),
                },
            )
            .await?;
            if account.account_id != id
                || account.provider_id != "openai"
                || account.authentication_kind != "oauth"
                || !account.enabled
            {
                return Err(invalid("请选择已启用的 OpenAI OAuth 账号"));
            }
            let models = match body.get("models") {
                Some(value) => {
                    serde_json::from_value(value.clone()).map_err(|_| invalid("模型列表无效"))?
                }
                None => vec![DEFAULT_MODEL.into()],
            };
            let mut next = config.clone();
            next.accounts.push(Account {
                account_id: id.into(),
                label: String::new(),
                models,
                credential_source: CredentialSource::Host,
            });
            if let Some(selection) = &mut next.host_selection {
                selection.account_ids.push(id.into());
            }
            let next = Config::parse(
                serde_json::to_value(next).map_err(|_| invalid("无法保存宿主账号接入"))?,
            )?;
            publish(
                config,
                &next,
                call,
                json!({"status":"connected","message":"已复用宿主账号，未修改登录凭据"}),
            )
            .await
        }
        "api/accounts/detach" => {
            let id = selected(config, &body)?;
            let mut next = config.clone();
            if let Some(selection) = &mut next.host_selection {
                if !selection.group_ids.is_empty() {
                    return Err(invalid("请在接入范围中调整分组与单账号选择"));
                }
                selection.account_ids.retain(|account| account != id);
            }
            next.accounts.retain(|account| account.account_id != id);
            publish(config, &next, call, json!({"status":"detached","message":"已取消 BPS 接入，宿主账号及登录状态保持不变"})).await
        }
        "api/onboarding/start" => onboarding::start(config, call, &body, false).await,
        "api/onboarding/import" => onboarding::start(config, call, &body, true).await,
        "api/onboarding/poll" => onboarding::poll(config, call, &body).await,
        "api/models" => {
            let selected = selected(config, &body)?;
            let mut next = config.clone();
            let account = next
                .accounts
                .iter_mut()
                .find(|account| account.account_id == selected)
                .ok_or_else(|| invalid("账号不存在"))?;
            account.models = serde_json::from_value(body["models"].clone())
                .map_err(|_| invalid("模型必须是非空字符串列表"))?;
            let next = Config::parse(
                serde_json::to_value(next).map_err(|_| invalid("无法保存模型配置"))?,
            )?;
            publish(
                config,
                &next,
                call,
                json!({"status":"saved","message":"模型已保存"}),
            )
            .await
        }
        path => {
            let id = selected(config, &body)?;
            if config.accounts.iter().any(|account| {
                account.account_id == id && account.credential_source == CredentialSource::Host
            }) {
                return Err(invalid(
                    "该账号复用宿主登录，请在宿主管理登录状态，取消 BPS 接入不会退出宿主账号",
                ));
            }
            let action = match path {
                "api/auth/start" => "login-start",
                "api/auth/poll" => "login-poll",
                "api/auth/import" => "import-oauth",
                "api/auth/logout" => "logout",
                _ => return Err(invalid("管理接口不存在")),
            };
            auth::perform(&call.host, id, action, body["oauth"].as_str()).await
        }
    }
}

async fn host_accounts(
    host: &gateway_plugin_sdk::client::HostClient,
) -> Result<Vec<AuthRuntimeAccount>, PluginFault> {
    let mut accounts = vec![];
    let mut cursor = None;
    loop {
        let page: AuthListResult = auth::payload(
            host,
            host::AUTH_LIST,
            AuthListRequest {
                provider_id: Some("openai".into()),
                cursor: cursor.clone(),
                limit: 200,
            },
        )
        .await?;
        accounts.extend(page.accounts.into_iter().filter(|account| {
            account.provider_id == "openai" && account.authentication_kind == "oauth"
        }));
        match page.next_cursor {
            Some(next) if cursor.as_ref() != Some(&next) => cursor = Some(next),
            Some(_) => return Err(invalid("宿主账号分页游标未前进")),
            None => break,
        }
    }
    Ok(accounts)
}

fn host_status(account: &AuthRuntimeAccount) -> Value {
    let valid = account.enabled && account.credential_state == "ready";
    json!({"configured":true,"valid":valid,"status":if valid{"host_managed"}else{"unavailable"},"expiresAt":account.access_token_expires_at_ms.map(|time|time/1000),"message":if valid{"使用宿主 OAuth，刷新由宿主管理，BPS 权限尚未验证"}else{"宿主账号未启用或暂不可用，请在宿主账号页检查"}})
}

fn selected<'a>(config: &Config, body: &'a Value) -> Result<&'a str, PluginFault> {
    let id = body["accountId"]
        .as_str()
        .ok_or_else(|| invalid("请选择 BPS 账号"))?;
    if !config
        .accounts
        .iter()
        .any(|account| account.account_id == id)
    {
        return Err(invalid("该账号不属于当前 BPS 配置"));
    }
    Ok(id)
}
