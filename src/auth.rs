//! 独立 BPS 设备授权、OAuth 导入和私有状态 CAS，不读取 Codex 凭据

use crate::{Config, config::invalid};
use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        host::{
            self, HttpRequest, StateGetRequest, StateGetResult, StatePutRequest, StatePutResult,
            StateRecord,
        },
        management::{
            CommandDescriptor, CommandInvocation, CommandParameter, CommandParameterType,
            CommandRegistration, CommandResult, CommandValue,
        },
    },
    client::{HostClient, SessionError, TypedCall, TypedReply},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const CLIENT_ID: &str = "app_fnr0pYvVwwFDocDumLG3H2Bp";
const AUTH_ORIGIN: &str = "https://auth.openai.com";

pub struct Credentials {
    pub access_token: String,
    pub account_id: String,
    pub expires_at: Option<i64>,
}

pub fn now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

pub fn parse_credentials(doc: &Value) -> Result<Credentials, PluginFault> {
    if doc.get("tokens").is_some() && doc.get("access_token").is_some() {
        return Err(invalid("Ambiguous BPS OAuth document."));
    }
    if doc
        .get("token_type")
        .is_some_and(|v| !v.as_str().is_some_and(|s| s.eq_ignore_ascii_case("bearer")))
    {
        return Err(invalid("BPS OAuth token_type must be Bearer."));
    }
    let tokens = doc.get("tokens").unwrap_or(doc);
    let access_token = tokens["access_token"]
        .as_str()
        .filter(|v| {
            !v.is_empty()
                && v.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
        })
        .ok_or_else(|| {
            invalid("BPS requires independent ChatGPT OAuth credentials, not an API key.")
        })?
        .to_owned();
    let claims = jwt(&access_token);
    let mut expires_at = claims["exp"].as_i64();
    let declared = if let Some(expiry) = doc.get("expires_at").filter(|v| !v.is_null()) {
        Some(
            expiry
                .as_i64()
                .filter(|v| *v > 0)
                .or_else(|| {
                    expiry
                        .as_str()
                        .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
                        .map(|t| t.unix_timestamp())
                })
                .ok_or_else(|| invalid("BPS expires_at must be Unix seconds or RFC3339."))?,
        )
    } else if let Some(duration) = doc["expires_in"].as_i64().filter(|v| *v > 0) {
        if duration > 31_536_000 {
            return Err(invalid("BPS expires_in exceeds one year."));
        }
        if let Some(obtained) = doc["obtained_at"].as_str() {
            Some(
                OffsetDateTime::parse(obtained, &Rfc3339)
                    .map_err(|_| invalid("BPS obtained_at must be RFC3339."))?
                    .unix_timestamp()
                    .saturating_add(duration),
            )
        } else if expires_at.is_none() {
            return Err(invalid(
                "Opaque BPS tokens require obtained_at or expires_at.",
            ));
        } else {
            None
        }
    } else {
        None
    };
    if let Some(declared) = declared {
        expires_at = Some(expires_at.map_or(declared, |jwt| jwt.min(declared)));
    }
    if expires_at.is_some_and(|expiry| expiry <= now()) {
        return Err(PluginFault::new(
            ErrorCode::Rejected,
            "BPS access token expired. Authorize the independent BPS account again.",
        ));
    }
    let account_id = tokens["account_id"]
        .as_str()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            claims
                .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
                .and_then(Value::as_str)
                .unwrap_or("")
        })
        .to_owned();
    if account_id.chars().any(char::is_control) {
        return Err(invalid("Invalid BPS account identifier."));
    }
    Ok(Credentials {
        access_token,
        account_id,
        expires_at,
    })
}

fn jwt(token: &str) -> Value {
    token
        .split('.')
        .nth(1)
        .and_then(|part| {
            URL_SAFE_NO_PAD
                .decode(part)
                .or_else(|_| URL_SAFE.decode(part))
                .ok()
        })
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or(Value::Null)
}

fn key(prefix: &str, account: &str) -> String {
    let digest = Sha256::digest(account.as_bytes());
    format!(
        "{prefix}-{}",
        digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

pub async fn credentials(
    host: &HostClient,
    account: &str,
) -> Result<(Credentials, u64), PluginFault> {
    let record = get(host, &key("credentials", account))
        .await?
        .ok_or_else(|| {
            PluginFault::new(
                ErrorCode::Rejected,
                "BPS OAuth is not configured. Use login-start/login-poll or import-oauth.",
            )
        })?;
    Ok((parse_credentials(&record.value)?, record.version))
}

pub(crate) async fn payload<I: Serialize, O: DeserializeOwned>(
    host: &HostClient,
    method: &str,
    value: I,
) -> Result<O, PluginFault> {
    let reply = host
        .call(
            method,
            json!({}),
            serde_json::to_vec(&value)
                .map_err(|_| invalid("Could not encode host account request."))?,
        )
        .await
        .map_err(SessionError::into_plugin_fault)?;
    if reply.result != json!({}) {
        return Err(invalid("Invalid host account response."));
    }
    serde_json::from_slice(&reply.payload).map_err(|_| invalid("Invalid host account payload."))
}

pub async fn host_credentials(
    host: &HostClient,
    account_id: &str,
    expected_revision: u64,
) -> Result<(Credentials, u64), PluginFault> {
    let credential: host::AuthCredential = payload(
        host,
        host::AUTH_GET,
        host::AuthGetRequest {
            account_id: account_id.into(),
        },
    )
    .await?;
    if credential.account_id != account_id
        || credential.provider_id != "openai"
        || credential.facts.authentication_kind != "oauth"
    {
        return Err(invalid(
            "BPS host-account mode requires an OpenAI OAuth account.",
        ));
    }
    if credential.credential_revision != expected_revision {
        return Err(PluginFault::new(
            ErrorCode::Conflict,
            "Host OAuth credentials changed during this attempt. No BPS request was sent.",
        ));
    }
    let mut document = Value::Object(credential.facts.material);
    // 只使用所选账号的宿主凭据，不复制到插件私有状态，不调用独立登录或刷新
    if let Some(expiry) = credential.facts.access_token_expires_at_ms {
        document["expires_at"] = json!(expiry / 1000);
    }
    if let Some(id) = credential.facts.upstream_account_id {
        document["account_id"] = json!(id);
    }
    Ok((
        parse_credentials(&document)?,
        credential.credential_revision,
    ))
}

pub(crate) async fn metadata<I: Serialize, O: DeserializeOwned>(
    host: &HostClient,
    method: &str,
    value: I,
) -> Result<O, PluginFault> {
    let reply = host
        .call(
            method,
            serde_json::to_value(value).map_err(|_| invalid("Could not encode OAuth state."))?,
            vec![],
        )
        .await
        .map_err(SessionError::into_plugin_fault)?;
    if !reply.payload.is_empty() {
        return Err(invalid("Unexpected OAuth state payload."));
    }
    serde_json::from_value(reply.result).map_err(|_| invalid("Invalid OAuth state response."))
}
pub(crate) async fn get(host: &HostClient, key: &str) -> Result<Option<StateRecord>, PluginFault> {
    let result: StateGetResult = metadata(
        host,
        host::STATE_GET,
        StateGetRequest {
            namespace: "oauth".into(),
            key: key.into(),
        },
    )
    .await?;
    Ok(result.record)
}
pub(crate) async fn put(
    host: &HostClient,
    key: &str,
    value: Value,
    expected_version: Option<u64>,
) -> Result<u64, PluginFault> {
    let result: StatePutResult = metadata(
        host,
        host::STATE_PUT,
        StatePutRequest {
            namespace: "oauth".into(),
            key: key.into(),
            value,
            expected_version,
        },
    )
    .await?;
    Ok(result.version)
}

pub fn registration() -> CommandRegistration {
    CommandRegistration {
        commands: [
            ("login-start", "申请 BPS 设备授权码"),
            ("login-poll", "查询一次授权结果并保存 OAuth"),
            ("import-oauth", "导入独立 BPS OAuth JSON"),
            ("status", "检查本地 OAuth 元数据，不验证生成权限"),
            ("logout", "删除该 BPS 账号的 OAuth 和待授权状态"),
        ]
        .into_iter()
        .map(|(name, description)| {
            let mut parameters = vec![CommandParameter {
                name: "account".into(),
                description: "配置中的宿主账号 ID；仅有一个账号时可省略".into(),
                value_type: CommandParameterType::String,
                required: false,
                sensitive: false,
                default: None,
            }];
            if name == "import-oauth" {
                parameters.push(CommandParameter {
                    name: "oauth".into(),
                    description: "独立 BPS OAuth 文件的 JSON 内容".into(),
                    value_type: CommandParameterType::String,
                    required: true,
                    sensitive: true,
                    default: None,
                });
            }
            CommandDescriptor {
                name: name.into(),
                description: description.into(),
                parameters,
            }
        })
        .collect(),
    }
}

pub async fn command(
    config: &Config,
    call: TypedCall<CommandInvocation>,
) -> Result<TypedReply<CommandResult>, PluginFault> {
    let selected = match call.request.arguments.get("account") {
        Some(CommandValue::String(value)) => value.as_str(),
        None if config.accounts.len() == 1 => &config.accounts[0].account_id,
        _ => return Err(invalid("Select a configured BPS account with --account.")),
    };
    if !config
        .accounts
        .iter()
        .any(|account| account.account_id == selected)
    {
        return Err(invalid("Unknown configured BPS account."));
    }
    if config.accounts.iter().any(|account| {
        account.account_id == selected
            && account.credential_source == crate::config::CredentialSource::Host
    }) {
        if call.request.name == "status" {
            return reply("当前 BPS 接入复用宿主 OAuth，凭据刷新由宿主管理");
        }
        return Err(invalid(
            "This BPS account uses host OAuth. Manage login in the host or detach BPS from the management page.",
        ));
    }
    let oauth = match call.request.arguments.get("oauth") {
        Some(CommandValue::String(raw)) => Some(raw.as_str()),
        _ => None,
    };
    let result = perform(&call.host, selected, &call.request.name, oauth).await?;
    reply(result["message"].as_str().unwrap_or("操作已完成"))
}

pub async fn perform(
    host: &HostClient,
    selected: &str,
    action: &str,
    oauth: Option<&str>,
) -> Result<Value, PluginFault> {
    let credential_key = key("credentials", selected);
    let pending_key = key("pending", selected);
    let result = match action {
        "status" => match get(host, &credential_key).await? {
            None => {
                json!({"configured":false,"valid":false,"status":"signed_out","message":"尚未配置 BPS OAuth"})
            }
            Some(record) => match parse_credentials(&record.value) {
                Ok(credentials) => {
                    json!({"configured":true,"valid":true,"status":"authorized","expiresAt":credentials.expires_at,"message":"OAuth 已保存，真实生成权限尚未验证"})
                }
                Err(_) => {
                    json!({"configured":true,"valid":false,"status":"expired","message":"OAuth 不可用或已过期，请重新授权"})
                }
            },
        },
        "logout" => {
            for key in [&credential_key, &pending_key] {
                if let Some(record) = get(host, key).await? {
                    let _: host::StateDeleteResult = metadata(
                        host,
                        host::STATE_DELETE,
                        host::StateDeleteRequest {
                            namespace: "oauth".into(),
                            key: key.clone(),
                            expected_version: record.version,
                        },
                    )
                    .await?;
                }
            }
            json!({"status":"signed_out","message":"已退出 BPS 登录"})
        }
        "import-oauth" => {
            let raw = oauth.ok_or_else(|| invalid("Provide BPS OAuth JSON."))?;
            if raw.len() > 48 * 1024 {
                return Err(invalid("BPS OAuth document exceeds 48 KiB."));
            }
            let value: Value =
                serde_json::from_str(raw).map_err(|_| invalid("Invalid BPS OAuth JSON."))?;
            parse_credentials(&value)?;
            let previous = get(host, &credential_key).await?;
            put(host, &credential_key, value, previous.map(|r| r.version)).await?;
            json!({"status":"authorized","message":"已导入 BPS OAuth，真实生成权限尚未验证"})
        }
        "login-start" => {
            let previous = get(host, &pending_key).await?;
            let credentials = get(host, &credential_key).await?;
            let (status, start) = post(
                host,
                "/api/accounts/deviceauth/usercode",
                json!({"client_id":CLIENT_ID}),
                false,
            )
            .await?;
            if status != 200 {
                return Err(oauth_error(status));
            }
            let id = start["device_auth_id"]
                .as_str()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| invalid("OAuth response is missing device_auth_id."))?;
            let code = start["user_code"]
                .as_str()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| invalid("OAuth response is missing user_code."))?;
            let interval = start
                .get("interval")
                .map(|v| {
                    v.as_u64()
                        .or_else(|| v.as_str().and_then(|v| v.parse().ok()))
                        .filter(|v| (1..=300).contains(v))
                        .ok_or_else(|| invalid("Invalid OAuth polling interval."))
                })
                .transpose()?
                .unwrap_or(5);
            put(host,&pending_key,json!({"device_auth_id":id,"user_code":code,"interval":interval,"expires_at":now()+900,"next_poll_at":now()+interval as i64,"credential_version":credentials.map(|r|r.version)}),previous.map(|r|r.version)).await?;
            let url =
                format!("{AUTH_ORIGIN}/api/accounts/deviceauth/authorize?client_id={CLIENT_ID}");
            json!({"status":"pending","authorizationUrl":url,"userCode":code,"interval":interval,"expiresAt":now()+900,
                "message":format!("打开 {url}\n输入设备码：{code}\n浏览器批准后执行 login-poll，至少间隔 {interval} 秒，15 分钟内有效")})
        }
        "login-poll" => {
            let record = get(host, &pending_key)
                .await?
                .ok_or_else(|| invalid("Start BPS device authorization with login-start."))?;
            let pending = &record.value;
            if pending["expires_at"].as_i64().is_none_or(|v| v <= now()) {
                return Err(invalid(
                    "BPS device authorization expired. Run login-start again.",
                ));
            }
            let interval = pending["interval"]
                .as_i64()
                .filter(|v| (1..=300).contains(v))
                .ok_or_else(|| invalid("Invalid stored OAuth polling interval."))?;
            if pending["next_poll_at"].as_i64().is_some_and(|v| v > now()) {
                return Ok(
                    json!({"status":"pending","retryAfterSeconds":pending["next_poll_at"].as_i64().unwrap_or(now())-now(),"message":"等待浏览器授权"}),
                );
            }
            // 先 CAS 占用本次轮询，避免并发调用重复兑换同一授权码
            let mut reserved = pending.clone();
            reserved["next_poll_at"] = json!(now() + interval.max(30));
            let reserved_version = put(host, &pending_key, reserved, Some(record.version)).await?;
            let (status,grant) = post(host,"/api/accounts/deviceauth/token",json!({"client_id":CLIENT_ID,"device_auth_id":pending["device_auth_id"],"user_code":pending["user_code"]}),false).await?;
            let error = grant["error"]
                .as_str()
                .or_else(|| grant["error"]["code"].as_str())
                .unwrap_or("");
            if (status == 403 && error == "deviceauth_authorization_pending")
                || matches!(error, "authorization_pending" | "slow_down")
            {
                let mut next = pending.clone();
                let delay = if error == "slow_down" {
                    (interval + 5).min(300)
                } else {
                    interval
                };
                next["interval"] = json!(delay);
                next["next_poll_at"] = json!(now() + delay);
                put(host, &pending_key, next, Some(reserved_version)).await?;
                return Ok(
                    json!({"status":"pending","retryAfterSeconds":delay,"message":format!("等待浏览器授权，请至少 {delay} 秒后再次执行 login-poll")}),
                );
            }
            if status != 200 {
                return Err(oauth_error(status));
            }
            let verifier = grant["code_verifier"]
                .as_str()
                .filter(|v| (43..=128).contains(&v.len()))
                .ok_or_else(|| invalid("Invalid OAuth PKCE verifier."))?;
            if URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
                != grant["code_challenge"].as_str().unwrap_or("")
            {
                return Err(invalid("BPS OAuth PKCE challenge mismatch."));
            }
            let code = grant["authorization_code"]
                .as_str()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| invalid("Missing BPS OAuth authorization code."))?;
            let (status,mut tokens) = post(host,"/oauth/token?unified=true",json!({"grant_type":"authorization_code","code":code,"client_id":CLIENT_ID,"redirect_uri":"https://auth.openai.com/basispoints/deviceauth/callback","code_verifier":verifier}),true).await?;
            if status != 200 {
                return Err(oauth_error(status));
            }
            if !tokens["token_type"]
                .as_str()
                .is_some_and(|v| v.eq_ignore_ascii_case("bearer"))
                || !tokens["expires_in"]
                    .as_i64()
                    .is_some_and(|v| (1..=31_536_000).contains(&v))
            {
                return Err(invalid("Invalid BPS OAuth token response."));
            }
            tokens["obtained_at"] = json!(
                OffsetDateTime::now_utc()
                    .format(&Rfc3339)
                    .map_err(|_| invalid("Could not format OAuth acquisition time."))?
            );
            tokens["client_id"] = json!(CLIENT_ID);
            parse_credentials(&tokens)?;
            put(
                host,
                &credential_key,
                tokens,
                pending["credential_version"].as_u64(),
            )
            .await?;
            let _: host::StateDeleteResult = metadata(
                host,
                host::STATE_DELETE,
                host::StateDeleteRequest {
                    namespace: "oauth".into(),
                    key: pending_key,
                    expected_version: reserved_version,
                },
            )
            .await?;
            json!({"status":"authorized","message":"BPS OAuth 已保存，真实生成权限尚未验证"})
        }
        _ => return Err(invalid("Unknown BPS command.")),
    };
    Ok(result)
}

pub(crate) async fn copy_credentials(
    host: &HostClient,
    from: &str,
    to: &str,
) -> Result<(), PluginFault> {
    let source = get(host, &key("credentials", from))
        .await?
        .ok_or_else(|| invalid("BPS OAuth is not available."))?;
    parse_credentials(&source.value)?;
    let previous = get(host, &key("credentials", to)).await?;
    put(
        host,
        &key("credentials", to),
        source.value,
        previous.map(|record| record.version),
    )
    .await?;
    Ok(())
}

fn reply(stdout: &str) -> Result<TypedReply<CommandResult>, PluginFault> {
    Ok(TypedReply::new(CommandResult {
        stdout: format!("{stdout}\n"),
        stderr: String::new(),
        exit_code: 0,
        accounts: vec![],
    }))
}
fn oauth_error(status: u16) -> PluginFault {
    PluginFault::new(
        ErrorCode::Upstream,
        format!("BPS OAuth request failed with HTTP {status}."),
    )
}

async fn post(
    host: &HostClient,
    path: &str,
    value: Value,
    form: bool,
) -> Result<(u16, Value), PluginFault> {
    let body = if form {
        let object = value
            .as_object()
            .ok_or_else(|| invalid("Invalid OAuth form."))?;
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(
                object
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str().unwrap_or(""))),
            )
            .finish()
            .into_bytes()
    } else {
        serde_json::to_vec(&value).map_err(|_| invalid("Could not encode OAuth request."))?
    };
    let request = HttpRequest {
        method: "POST".into(),
        url: format!("{AUTH_ORIGIN}{path}"),
        headers: vec![
            (
                "content-type".into(),
                if form {
                    b"application/x-www-form-urlencoded".to_vec()
                } else {
                    b"application/json".to_vec()
                },
            ),
            ("accept".into(), b"application/json".to_vec()),
        ],
    };
    let response = host.http(request, body).await?;
    let status = response.status;
    let raw = response.body.collect(48 * 1024).await?;
    Ok((
        status,
        serde_json::from_slice(&raw).map_err(|_| invalid("Invalid BPS OAuth JSON response."))?,
    ))
}
