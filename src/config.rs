//! 校验显式 BPS 账号和模型范围，并生成固定目标的原生适配器声明

use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        catalog::{AdaptedModel, ModelCatalogRegistrationV2, ModelPresentation},
        middleware::RequestFeature,
        upstream_adapter::{
            BuiltinProvider, UpstreamAdapterDeclaration, UpstreamAdapterRegistration, UpstreamPath,
            UpstreamPathPurpose, UpstreamTransport,
        },
    },
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(
        default,
        rename = "hostSelection",
        skip_serializing_if = "Option::is_none"
    )]
    pub host_selection: Option<HostSelection>,
}

#[derive(Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostSelection {
    #[serde(default)]
    pub group_ids: Vec<String>,
    #[serde(default)]
    pub account_ids: Vec<String>,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Account {
    pub account_id: String,
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "CredentialSource::is_independent")]
    pub credential_source: CredentialSource,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    #[default]
    Independent,
    Host,
}
impl CredentialSource {
    fn is_independent(&self) -> bool {
        *self == Self::Independent
    }
}

pub(crate) fn invalid(message: &'static str) -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, message)
}

impl Config {
    pub fn parse(value: serde_json::Value) -> Result<Self, PluginFault> {
        let config: Self = serde_json::from_value(value)
            .map_err(|_| invalid("Invalid BPS configuration. See config.example.json."))?;
        if config.accounts.len() > 256 {
            return Err(invalid("Configure at most 256 BPS accounts."));
        }
        if config
            .accounts
            .iter()
            .filter(|account| account.credential_source == CredentialSource::Independent)
            .count()
            > 16
        {
            return Err(invalid(
                "At most 16 independent BPS OAuth accounts are supported.",
            ));
        }
        if let Some(selection) = &config.host_selection {
            for ids in [&selection.group_ids, &selection.account_ids] {
                let mut unique = BTreeSet::new();
                if ids.len() > 256 || ids.iter().any(|id| !identifier(id) || !unique.insert(id)) {
                    return Err(invalid("Invalid host-account selection."));
                }
            }
        }
        let mut accounts = BTreeSet::new();
        for account in &config.accounts {
            if account.label.len() > 120 || account.label.chars().any(char::is_control) {
                return Err(invalid("BPS account label is invalid."));
            }
            if !identifier(&account.account_id) || !accounts.insert(account.account_id.clone()) {
                return Err(invalid(
                    "Each BPS account must have a unique, non-empty accountId.",
                ));
            }
            let mut models = BTreeSet::new();
            if account.models.is_empty()
                || account.models.len() > 16
                || account
                    .models
                    .iter()
                    .any(|model| !identifier(model) || !models.insert(model))
            {
                return Err(invalid(
                    "Each BPS account must list 1 to 16 unique authorized model IDs.",
                ));
            }
        }
        if serde_json::to_vec(&config.catalog()).map_or(true, |raw| raw.len() > 64 * 1024) {
            return Err(invalid(
                "BPS model catalog exceeds 64 KiB. Configure fewer models.",
            ));
        }
        if config.registration().adapters.len() > 16 {
            return Err(invalid(
                "At most 16 distinct BPS credential-source/model combinations are supported.",
            ));
        }
        if serde_json::to_vec(&config.registration()).map_or(true, |raw| raw.len() > 64 * 1024) {
            return Err(invalid("BPS adapter registration exceeds 64 KiB."));
        }
        Ok(config)
    }

    pub fn registration(&self) -> UpstreamAdapterRegistration {
        let mut groups: Vec<(CredentialSource, Vec<String>, Vec<String>)> = vec![];
        for account in &self.accounts {
            let mut models = account.models.clone();
            models.sort();
            if let Some(group) = groups
                .iter_mut()
                .find(|group| group.0 == account.credential_source && group.1 == models)
            {
                group.2.push(account.account_id.clone());
            } else {
                groups.push((
                    account.credential_source,
                    models,
                    vec![account.account_id.clone()],
                ));
            }
        }
        UpstreamAdapterRegistration {
            adapters: groups
                .iter()
                .enumerate()
                .map(
                    |(index, (source, models, ids))| UpstreamAdapterDeclaration {
                        id: format!("bps-{index}"),
                        provider: BuiltinProvider::OpenAi,
                        base_url: "https://bps.openai.com/".into(),
                        paths: vec![UpstreamPath {
                            path: "basispoints/api/responses".into(),
                            purpose: UpstreamPathPurpose::Inference,
                        }],
                        authentication_kinds: vec![
                            if *source == CredentialSource::Host {
                                "oauth"
                            } else {
                                "api_key"
                            }
                            .into(),
                        ],
                        transport: UpstreamTransport::WebSocket,
                        protocol: "openai".into(),
                        models: models.clone(),
                        account_ids: ids.clone(),
                    },
                )
                .collect(),
        }
    }

    pub fn catalog(&self) -> ModelCatalogRegistrationV2 {
        ModelCatalogRegistrationV2 {
            aliases: vec![],
            models: self
                .registration()
                .adapters
                .iter()
                .flat_map(|adapter| {
                    adapter.models.iter().map(move |id| AdaptedModel {
                        id: id.clone(),
                        adapter_id: adapter.id.clone(),
                        features: vec![
                            RequestFeature::Tools,
                            RequestFeature::Vision,
                            RequestFeature::Reasoning,
                            RequestFeature::JsonSchema,
                        ],
                        max_output_tokens: None,
                        presentation: ModelPresentation {
                            display_name: Some(id.clone()),
                            description: Some("Native BPS WebSocket".into()),
                            default_reasoning_effort: Some("medium".into()),
                            supported_reasoning_efforts: vec![
                                "low".into(),
                                "medium".into(),
                                "high".into(),
                                "xhigh".into(),
                            ],
                            agent_tools: true,
                            image_input: true,
                            ..ModelPresentation::default()
                        },
                    })
                })
                .collect(),
        }
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && !value.chars().any(char::is_control)
}
