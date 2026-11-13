//! 组合 BPS 上游适配器和模型目录，不访问宿主内部实现

mod adapter;
pub mod auth;
mod config;
pub mod events;
pub mod protocol;

pub use config::Config;
use gateway_plugin_sdk::{
    Manifest, ManifestError,
    client::{AuthorError, ComposedPlugin, PluginBuilder, TypedReply, methods},
};

pub const PLUGIN_ID: &str = "local.bps";
pub const MAX_EVENT_BYTES: usize = (16 * 1024 * 1024) - 1024;

pub fn manifest() -> Result<Manifest, ManifestError> {
    Manifest::from_author_slice(include_bytes!("../plugin.json"))
}

pub fn plugin(config: Config) -> Result<ComposedPlugin, AuthorError> {
    let registration = config.registration();
    let auth_config = config.clone();
    let auth_lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    let replay = std::sync::Arc::new(std::sync::Mutex::new(protocol::ReplayCache::default()));
    PluginBuilder::from_json(include_bytes!("../plugin.json"))?
        .model_catalog(config.catalog())?
        .command_line(auth::registration(), move |call| {
            let config = auth_config.clone();
            let lock = auth_lock.clone();
            async move {
                let _guard = lock.lock().await;
                auth::command(&config, call).await
            }
        })?
        .on(methods::UPSTREAM_ADAPTER_REGISTER, move |_| {
            let registration = registration.clone();
            async move { Ok(TypedReply::new(registration)) }
        })?
        .on(methods::UPSTREAM_ADAPTER_EXECUTE, move |call| {
            let config = config.clone();
            let replay = replay.clone();
            async move { adapter::execute(&config, replay, call).await }
        })?
        .build()
}
