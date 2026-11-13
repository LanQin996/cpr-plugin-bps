//! 接受宿主握手并启动公开 SDK 管理的插件会话

use cpr_plugin_bps::{Config, PLUGIN_ID, manifest, plugin};
use gateway_plugin_sdk::client::{PluginSession, SessionConfig, SessionError};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session = PluginSession::accept(
        tokio::io::stdin(),
        tokio::io::stdout(),
        SessionConfig::default(),
    )
    .await?;
    let manifest = manifest()?;
    if session.handshake().plugin_id != PLUGIN_ID
        || session.handshake().contributes != manifest.contributes
    {
        return Err(SessionError::Handshake.into());
    }
    let config = Config::parse(session.handshake().configuration.clone())
        .map_err(|fault| std::io::Error::other(fault.message))?;
    session.run(plugin(config)?).await?;
    Ok(())
}
