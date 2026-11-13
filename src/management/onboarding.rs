//! 可恢复的浏览器授权接入流程，自动创建占位账号并在最后发布本实例配置

use super::{DEFAULT_MODEL, publication};
use crate::{
    Config, auth,
    config::{Account, invalid},
};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        host::{
            self, AuthCredential, AuthGetRequest, AuthListRequest, AuthListResult, AuthSaveRequest,
            AuthSaveResult, CredentialFacts,
        },
        management::ManagementRequest,
    },
    client::{HostClient, SessionError, TypedCall},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use uuid::Uuid;

const JOURNAL: &str = "onboarding-active";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Setup {
    operation_id: String,
    label: String,
    models: Vec<String>,
    stage: String,
    account_id: Option<String>,
    prompt: Option<Value>,
    expires_at: i64,
}
impl Setup {
    fn draft(&self) -> String {
        format!("draft:{}", self.operation_id)
    }
    fn account_name(&self) -> String {
        format!("BPS · {} · {}", self.label, &self.operation_id[..8])
    }
    fn public(&self) -> Value {
        json!({"operationId":self.operation_id,"label":self.label,"models":self.models,"stage":self.stage,"prompt":self.prompt,"expiresAt":self.expires_at})
    }
}

async fn load(host: &HostClient) -> Result<Option<(Setup, u64)>, PluginFault> {
    auth::get(host, JOURNAL)
        .await?
        .map(|record| {
            let setup: Setup =
                serde_json::from_value(record.value).map_err(|_| invalid("账号接入记录无效"))?;
            if Uuid::parse_str(&setup.operation_id).is_err() {
                return Err(invalid("账号接入记录的标识无效"));
            }
            Ok((setup, record.version))
        })
        .transpose()
}
async fn save(host: &HostClient, setup: &Setup, version: Option<u64>) -> Result<u64, PluginFault> {
    auth::put(
        host,
        JOURNAL,
        serde_json::to_value(setup).map_err(|_| invalid("无法保存账号接入记录"))?,
        version,
    )
    .await
}

pub async fn snapshot(config: &Config, host: &HostClient) -> Result<Value, PluginFault> {
    let Some((setup, _)) = load(host).await? else {
        return Ok(Value::Null);
    };
    if setup.account_id.as_ref().is_some_and(|id| {
        config
            .accounts
            .iter()
            .any(|account| &account.account_id == id)
    }) {
        return Ok(Value::Null);
    }
    Ok(setup.public())
}

pub async fn start(
    config: &Config,
    call: &TypedCall<ManagementRequest>,
    body: &Value,
    import: bool,
) -> Result<Value, PluginFault> {
    publication::require_admin(call)?;
    if config.accounts.len() >= 256
        || config
            .accounts
            .iter()
            .filter(|account| {
                account.credential_source == crate::config::CredentialSource::Independent
            })
            .count()
            >= 16
    {
        return Err(invalid("最多接入 16 个 BPS 账号"));
    }
    if import {
        let raw = body["oauth"]
            .as_str()
            .filter(|raw| raw.len() <= 48 * 1024)
            .ok_or_else(|| invalid("请选择不超过 48 KiB 的独立 BPS OAuth 文件"))?;
        let value: Value =
            serde_json::from_str(raw).map_err(|_| invalid("OAuth 文件不是有效 JSON"))?;
        auth::parse_credentials(&value)?;
    }
    let operation = body["operationId"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .filter(|id| id.get_version_num() == 4)
        .ok_or_else(|| invalid("账号接入操作缺少有效标识"))?
        .to_string();
    let previous = load(&call.host).await?;
    if let Some((setup, version)) = &previous {
        let published = setup.account_id.as_ref().is_some_and(|id| {
            config
                .accounts
                .iter()
                .any(|account| &account.account_id == id)
        });
        if !published && (setup.stage != "pending" || setup.expires_at > auth::now()) {
            if setup.operation_id == operation {
                if setup.stage == "creating" || setup.stage == "ready" {
                    return finish(config, call, setup.clone(), *version).await;
                }
                if import && setup.stage == "pending" {
                    auth::perform(
                        &call.host,
                        &setup.draft(),
                        "import-oauth",
                        body["oauth"].as_str(),
                    )
                    .await?;
                    return finish(config, call, setup.clone(), *version).await;
                }
                if setup.stage == "pending" && setup.prompt.is_none() {
                    let mut retry = setup.clone();
                    retry.prompt =
                        Some(auth::perform(&call.host, &retry.draft(), "login-start", None).await?);
                    save(&call.host, &retry, Some(*version)).await?;
                    return Ok(retry.public());
                }
                return Ok(setup.public());
            }
            return Err(PluginFault::new(
                ErrorCode::Conflict,
                "已有账号正在接入，请先完成或等待授权过期",
            ));
        }
        auth::perform(&call.host, &setup.draft(), "logout", None).await?;
    }
    let label = body["label"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("BPS 账号")
        .trim()
        .to_owned();
    let models: Vec<String> = match body.get("models") {
        Some(value) => {
            serde_json::from_value(value.clone()).map_err(|_| invalid("模型列表无效"))?
        }
        None => vec![DEFAULT_MODEL.into()],
    };
    Config::parse(json!({"accounts":[{"accountId":"pending","label":label,"models":models}]}))?;
    let mut setup = Setup {
        operation_id: operation,
        label,
        models,
        stage: "pending".into(),
        account_id: None,
        prompt: None,
        expires_at: auth::now() + 900,
    };
    let version = save(&call.host, &setup, previous.map(|(_, version)| version)).await?;
    if import {
        auth::perform(
            &call.host,
            &setup.draft(),
            "import-oauth",
            body["oauth"].as_str(),
        )
        .await?;
        return finish(config, call, setup, version).await;
    }
    let prompt = auth::perform(&call.host, &setup.draft(), "login-start", None).await?;
    setup.prompt = Some(prompt.clone());
    save(&call.host, &setup, Some(version)).await?;
    Ok(
        json!({"operationId":setup.operation_id,"label":setup.label,"models":setup.models,"stage":"pending","prompt":prompt,"expiresAt":setup.expires_at}),
    )
}

pub async fn poll(
    config: &Config,
    call: &TypedCall<ManagementRequest>,
    body: &Value,
) -> Result<Value, PluginFault> {
    publication::require_admin(call)?;
    let (setup, version) = load(&call.host)
        .await?
        .ok_or_else(|| invalid("请先发起账号授权"))?;
    if body["operationId"].as_str() != Some(&setup.operation_id) {
        return Err(PluginFault::new(
            ErrorCode::Conflict,
            "授权操作已变化，请刷新后继续",
        ));
    }
    if setup.account_id.as_ref().is_some_and(|id| {
        config
            .accounts
            .iter()
            .any(|account| &account.account_id == id)
    }) {
        return Ok(
            json!({"status":"authorized","accountId":setup.account_id,"message":"账号已接入"}),
        );
    }
    if setup.stage == "ready" || setup.stage == "creating" {
        return finish(config, call, setup, version).await;
    }
    let status = auth::perform(&call.host, &setup.draft(), "status", None).await?;
    if status["valid"] != true {
        let result = auth::perform(&call.host, &setup.draft(), "login-poll", None).await?;
        if result["status"] != "authorized" {
            return Ok(result);
        }
    }
    finish(config, call, setup, version).await
}

async fn finish(
    config: &Config,
    call: &TypedCall<ManagementRequest>,
    mut setup: Setup,
    mut version: u64,
) -> Result<Value, PluginFault> {
    if config.accounts.len() >= 256
        || config
            .accounts
            .iter()
            .filter(|account| {
                account.credential_source == crate::config::CredentialSource::Independent
            })
            .count()
            >= 16
    {
        return Err(invalid("最多接入 16 个 BPS 账号"));
    }
    if setup.account_id.is_none() {
        if setup.stage == "creating" {
            setup.account_id = recover_account(&call.host, &setup).await?;
            if setup.account_id.is_none() {
                return Err(PluginFault::new(
                    ErrorCode::Conflict,
                    "宿主账号创建结果尚未确认，请稍后再检查，避免重复创建",
                ));
            }
        } else {
            setup.stage = "creating".into();
            version = save(&call.host, &setup, Some(version)).await?;
            let material = json!({"schema_version":1,"installation_id":setup.operation_id,"api_key":"bps-plugin-placeholder","base_url":"https://bps-plugin.invalid/","protocol":"responses","transport":"http"});
            let result: AuthSaveResult = payload(
                &call.host,
                host::AUTH_SAVE,
                AuthSaveRequest::Create {
                    provider_id: "openai".into(),
                    facts: CredentialFacts {
                        name: setup.account_name(),
                        authentication_kind: "api_key".into(),
                        material: material
                            .as_object()
                            .cloned()
                            .ok_or_else(|| invalid("无法构造宿主账号"))?,
                        email: None,
                        upstream_user_id: None,
                        upstream_account_id: None,
                        plan_type: None,
                        has_refresh_token: false,
                        access_token_expires_at_ms: None,
                        next_refresh_at_ms: None,
                    },
                },
            )
            .await?;
            setup.account_id = Some(result.account_id);
        }
        version = save(&call.host, &setup, Some(version)).await?;
    }
    let account_id = setup
        .account_id
        .as_deref()
        .ok_or_else(|| invalid("宿主账号创建结果缺少 ID"))?
        .to_owned();
    if setup.stage != "ready" {
        auth::copy_credentials(&call.host, &setup.draft(), &account_id).await?;
        setup.stage = "ready".into();
        setup.prompt = None;
        save(&call.host, &setup, Some(version)).await?;
        auth::perform(&call.host, &setup.draft(), "logout", None).await?;
    }
    let mut next = config.clone();
    next.accounts.push(Account {
        account_id: account_id.clone(),
        label: setup.label,
        models: setup.models,
        credential_source: crate::config::CredentialSource::Independent,
    });
    let next = Config::parse(serde_json::to_value(next).map_err(|_| invalid("无法构造账号配置"))?)?;
    publication::save(config, &next, call).await?;
    Ok(
        json!({"status":"authorized","accountId":account_id,"message":"账号已授权并接入，宿主正在刷新插件页面"}),
    )
}

async fn payload<I: Serialize, O: DeserializeOwned>(
    host: &HostClient,
    method: &str,
    value: I,
) -> Result<O, PluginFault> {
    let reply = host
        .call(
            method,
            json!({}),
            serde_json::to_vec(&value).map_err(|_| invalid("无法编码宿主账号请求"))?,
        )
        .await
        .map_err(SessionError::into_plugin_fault)?;
    if reply.result != json!({}) {
        return Err(invalid("宿主账号接口返回格式无效"));
    }
    serde_json::from_slice(&reply.payload).map_err(|_| invalid("无法读取宿主账号结果"))
}

async fn recover_account(host: &HostClient, setup: &Setup) -> Result<Option<String>, PluginFault> {
    let mut cursor = None;
    loop {
        let page: AuthListResult = payload(
            host,
            host::AUTH_LIST,
            AuthListRequest {
                provider_id: Some("openai".into()),
                cursor,
                limit: 200,
            },
        )
        .await?;
        for account in page
            .accounts
            .into_iter()
            .filter(|account| account.name == setup.account_name())
        {
            // 只读取可恢复操作明确引用的账号，不凭同名接管管理员账号
            let credential: AuthCredential = payload(
                host,
                host::AUTH_GET,
                AuthGetRequest {
                    account_id: account.account_id.clone(),
                },
            )
            .await?;
            let material = &credential.facts.material;
            if credential.provider_id == "openai"
                && credential.facts.authentication_kind == "api_key"
                && material.get("installation_id") == Some(&json!(setup.operation_id))
                && material.get("base_url") == Some(&json!("https://bps-plugin.invalid/"))
                && material.get("api_key") == Some(&json!("bps-plugin-placeholder"))
            {
                return Ok(Some(account.account_id));
            }
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok(None),
        }
    }
}
