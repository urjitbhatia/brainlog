use anyhow::Result;
use rmcp::ServiceExt;

use crate::config::Config;
use crate::mcp::BrainlogMcp;

pub async fn handle_mcp() -> Result<()> {
    let config = Config::load()?;
    let server = BrainlogMcp::new(config.db_path());
    let service = server
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}
