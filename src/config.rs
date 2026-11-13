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
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub accounts: Vec<Account>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Account {
    pub account_id: String,
    pub models: Vec<String>,
}

pub(crate) fn invalid(message: &'static str) -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, message)
}

impl Config {
    pub fn parse(value: serde_json::Value) -> Result<Self, PluginFault> {
        let config: Self = serde_json::from_value(value)
            .map_err(|_| invalid("Invalid BPS configuration. See config.example.json."))?;
        if config.accounts.is_empty() || config.accounts.len() > 16 {
            return Err(invalid("Configure between 1 and 16 BPS accounts."));
        }
        let mut accounts = BTreeSet::new();
        for account in &config.accounts {
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
        Ok(config)
    }

    pub fn registration(&self) -> UpstreamAdapterRegistration {
        UpstreamAdapterRegistration {
            adapters: self
                .accounts
                .iter()
                .enumerate()
                .map(|(index, account)| UpstreamAdapterDeclaration {
                    id: format!("bps-{index}"),
                    provider: BuiltinProvider::OpenAi,
                    base_url: "https://bps.openai.com/".into(),
                    paths: vec![UpstreamPath {
                        path: "basispoints/api/responses".into(),
                        purpose: UpstreamPathPurpose::Inference,
                    }],
                    authentication_kinds: vec!["api_key".into()],
                    transport: UpstreamTransport::WebSocket,
                    protocol: "openai".into(),
                    models: account.models.clone(),
                    account_ids: vec![account.account_id.clone()],
                })
                .collect(),
        }
    }

    pub fn catalog(&self) -> ModelCatalogRegistrationV2 {
        ModelCatalogRegistrationV2 {
            aliases: vec![],
            models: self
                .accounts
                .iter()
                .enumerate()
                .flat_map(|(index, account)| {
                    account.models.iter().map(move |id| AdaptedModel {
                        id: id.clone(),
                        adapter_id: format!("bps-{index}"),
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
