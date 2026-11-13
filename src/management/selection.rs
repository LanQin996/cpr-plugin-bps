//! 分组与单账号独立多选取并集，保存当前成员快照，不改变宿主分组或凭据

use super::{DEFAULT_MODEL, publication};
use crate::{
    Config,
    config::{Account, CredentialSource, HostSelection, invalid},
};
use gateway_plugin_sdk::{PluginFault, call::management::ManagementRequest, client::TypedCall};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub id: String,
    pub label: String,
    pub email: Option<String>,
    pub enabled: bool,
    pub state: String,
    pub group_ids: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}
pub struct Directory {
    pub accounts: Vec<Choice>,
    pub groups: Vec<Group>,
}

#[derive(Deserialize)]
struct AccountSummary {
    id: String,
    name: String,
    email: Option<String>,
    groups: Vec<Group>,
}

pub async fn directory(call: &TypedCall<ManagementRequest>) -> Result<Directory, PluginFault> {
    let native = super::host_accounts(&call.host).await?;
    with_accounts(call, native).await
}

pub async fn with_accounts(
    call: &TypedCall<ManagementRequest>,
    native: Vec<gateway_plugin_sdk::call::host::AuthRuntimeAccount>,
) -> Result<Directory, PluginFault> {
    let mut memberships = BTreeMap::new();
    let mut account_page = 1;
    loop {
        // 使用账号管理页相同的 groups 事实，避免宿主简化投影丢失成员关系
        let data = publication::admin(
            call,
            "GET",
            &format!("/api/admin/accounts?page={account_page}&pageSize=100&provider=openai"),
            None,
        )
        .await?;
        let accounts: Vec<AccountSummary> = serde_json::from_value(data["items"].clone())
            .map_err(|_| invalid("无法读取宿主账号与分组关系"))?;
        for account in accounts {
            memberships.insert(account.id.clone(), account);
        }
        let total = data["page"]["totalPages"]
            .as_u64()
            .ok_or_else(|| invalid("宿主账号分页信息无效"))?;
        if account_page >= total {
            break;
        }
        account_page += 1;
        if account_page > 10000 {
            return Err(invalid("宿主账号列表过大"));
        }
    }
    let mut groups = vec![];
    let mut page = 1;
    loop {
        let data = publication::admin(
            call,
            "GET",
            &format!("/api/admin/account-groups/options?page={page}&pageSize=200"),
            None,
        )
        .await?;
        let items: Vec<Group> = serde_json::from_value(data["items"].clone())
            .map_err(|_| invalid("无法读取宿主分组列表"))?;
        groups.extend(items);
        let total = data["page"]["totalPages"]
            .as_u64()
            .ok_or_else(|| invalid("宿主分组分页信息无效"))?;
        if page >= total {
            break;
        }
        page += 1;
        if page > 10000 {
            return Err(invalid("宿主分组列表过大"));
        }
    }
    let accounts = native
        .into_iter()
        .map(|mut account| {
            let summary = memberships.remove(&account.account_id);
            if let Some(summary) = &summary {
                account.name = summary.name.clone();
                account.email = summary.email.clone();
            }
            Choice {
                id: account.account_id.clone(),
                label: account_label(&account),
                email: account.email,
                enabled: account.enabled,
                state: account.credential_state,
                group_ids: summary
                    .map(|row| row.groups.into_iter().map(|group| group.id).collect())
                    .unwrap_or_default(),
            }
        })
        .collect();
    Ok(Directory { accounts, groups })
}

pub(super) fn account_label(
    account: &gateway_plugin_sdk::call::host::AuthRuntimeAccount,
) -> String {
    [
        Some(account.name.as_str()),
        account.email.as_deref(),
        Some(account.account_id.as_str()),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|value| !value.is_empty())
    .unwrap_or("未命名账号")
    .to_owned()
}

pub fn resolve(
    directory: &Directory,
    selection: &HostSelection,
) -> Result<BTreeSet<String>, PluginFault> {
    let groups: BTreeSet<_> = selection.group_ids.iter().collect();
    let ids: BTreeSet<_> = selection.account_ids.iter().collect();
    if groups.len() != selection.group_ids.len()
        || ids.len() != selection.account_ids.len()
        || groups.len() > 256
        || ids.len() > 256
    {
        return Err(invalid("分组或单账号选择重复或超出数量限制"));
    }
    if groups.iter().any(|id| {
        !directory
            .groups
            .iter()
            .any(|group| &group.id == *id && group.enabled)
    }) {
        return Err(invalid("所选分组已删除或停用，请刷新选择"));
    }
    if ids.iter().any(|id| {
        !directory
            .accounts
            .iter()
            .any(|account| &account.id == *id && account.enabled)
    }) {
        return Err(invalid("所选单账号已删除、停用或不再是 OpenAI OAuth 账号"));
    }
    Ok(directory
        .accounts
        .iter()
        .filter(|account| {
            account.enabled
                && (ids.contains(&account.id)
                    || account.group_ids.iter().any(|id| groups.contains(id)))
        })
        .map(|account| account.id.clone())
        .collect())
}

pub async fn apply(
    config: &Config,
    call: &TypedCall<ManagementRequest>,
    body: &Value,
) -> Result<Value, PluginFault> {
    let selection: HostSelection =
        serde_json::from_value(body.clone()).map_err(|_| invalid("分组和单账号选择格式无效"))?;
    let directory = directory(call).await?;
    let effective = resolve(&directory, &selection)?;
    let mut next = config.clone();
    next.accounts.retain(|account| {
        account.credential_source == CredentialSource::Independent
            || effective.contains(&account.account_id)
    });
    for id in &effective {
        match next
            .accounts
            .iter()
            .find(|account| &account.account_id == id)
        {
            Some(account) if account.credential_source != CredentialSource::Host => {
                return Err(invalid("所选账号已配置独立 BPS 凭据，请先调整其接入方式"));
            }
            Some(_) => {}
            None => next.accounts.push(Account {
                account_id: id.clone(),
                models: vec![DEFAULT_MODEL.into()],
                label: String::new(),
                credential_source: CredentialSource::Host,
            }),
        }
    }
    next.host_selection = Some(selection);
    let next = Config::parse(serde_json::to_value(next).map_err(|_| invalid("无法保存接入范围"))?)?;
    super::publish(config, &next, call,
        json!({"status":"connected","effectiveAccountIds":effective,"message":"已按分组与单账号的并集更新 BPS 接入"}),
    ).await
}

pub fn saved(config: &Config) -> HostSelection {
    config
        .host_selection
        .clone()
        .unwrap_or_else(|| HostSelection {
            group_ids: vec![],
            account_ids: config
                .accounts
                .iter()
                .filter(|account| account.credential_source == CredentialSource::Host)
                .map(|account| account.account_id.clone())
                .collect(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn groups_and_individual_accounts_form_a_deduplicated_union() {
        let directory = Directory {
            groups: vec![
                Group {
                    id: "a".into(),
                    name: "A".into(),
                    enabled: true,
                },
                Group {
                    id: "b".into(),
                    name: "B".into(),
                    enabled: true,
                },
            ],
            accounts: vec![
                Choice {
                    id: "a1".into(),
                    label: "A1".into(),
                    email: None,
                    enabled: true,
                    state: "ready".into(),
                    group_ids: vec!["a".into()],
                },
                Choice {
                    id: "ab".into(),
                    label: "AB".into(),
                    email: None,
                    enabled: true,
                    state: "ready".into(),
                    group_ids: vec!["a".into(), "b".into()],
                },
                Choice {
                    id: "b1".into(),
                    label: "B1".into(),
                    email: None,
                    enabled: true,
                    state: "ready".into(),
                    group_ids: vec!["b".into()],
                },
                Choice {
                    id: "b2".into(),
                    label: "B2".into(),
                    email: None,
                    enabled: true,
                    state: "ready".into(),
                    group_ids: vec!["b".into()],
                },
                Choice {
                    id: "ungrouped".into(),
                    label: "Ungrouped".into(),
                    email: None,
                    enabled: true,
                    state: "ready".into(),
                    group_ids: vec![],
                },
                Choice {
                    id: "disabled".into(),
                    label: "Disabled".into(),
                    email: None,
                    enabled: false,
                    state: "ready".into(),
                    group_ids: vec!["a".into()],
                },
            ],
        };
        let select = |groups: Vec<&str>, ids: Vec<&str>| HostSelection {
            group_ids: groups.into_iter().map(str::to_owned).collect(),
            account_ids: ids.into_iter().map(str::to_owned).collect(),
        };
        assert_eq!(
            resolve(&directory, &select(vec!["a"], vec!["b1", "ab"])).unwrap(),
            BTreeSet::from(["a1".into(), "ab".into(), "b1".into()])
        );
        assert_eq!(
            resolve(&directory, &select(vec!["a", "b"], vec!["b1"]))
                .unwrap()
                .len(),
            4
        );
        assert_eq!(
            resolve(&directory, &select(vec![], vec!["b1"])).unwrap(),
            BTreeSet::from(["b1".into()])
        );
        assert!(
            resolve(&directory, &select(vec![], vec![]))
                .unwrap()
                .is_empty()
        );
        assert!(resolve(&directory, &select(vec!["missing"], vec![])).is_err());
        assert!(resolve(&directory, &select(vec![], vec!["disabled"])).is_err());
        assert_eq!(
            resolve(&directory, &select(vec![], vec!["ungrouped"])).unwrap(),
            BTreeSet::from(["ungrouped".into()])
        );
        assert_eq!(
            resolve(&directory, &select(vec!["a"], vec!["ungrouped"])).unwrap(),
            BTreeSet::from(["a1".into(), "ab".into(), "ungrouped".into()])
        );
    }
}
