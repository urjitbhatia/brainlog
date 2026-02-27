use tracing::{debug, info, warn};

use crate::config::Config;
use crate::storage::models::EnrichmentStatus;
use crate::storage::Database;

use super::Message;

pub async fn enrich_service(
    config: &Config,
    service_id: &str,
    command: &[String],
    working_dir: &str,
    tags: &[String],
    user_desc: Option<&str>,
    has_user_name: bool,
) {
    let client = match super::create_client(&config.llm) {
        Some(c) => c,
        None => {
            // No LLM configured, mark as skipped
            match Database::open(&config.db_path()) {
                Ok(db) => {
                    if let Err(e) = db.update_service_enrichment(
                        service_id,
                        None,
                        None,
                        &EnrichmentStatus::Skipped,
                    ) {
                        warn!("Failed to mark enrichment as skipped for service {service_id}: {e}");
                    }
                }
                Err(e) => {
                    warn!("Failed to open database for enrichment (service {service_id}): {e}");
                }
            }
            return;
        }
    };

    // Gather context
    let mut context = format!(
        "Command: {}\nWorking directory: {}\n",
        command.join(" "),
        working_dir
    );

    if !tags.is_empty() {
        context.push_str(&format!("Tags: {}\n", tags.join(", ")));
    }

    if let Some(desc) = user_desc {
        context.push_str(&format!("User description: {}\n", desc));
    }

    // Try to read project files for additional context
    for pattern in &config.enrichment.project_file_patterns {
        let path = std::path::Path::new(working_dir).join(pattern);
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    let preview = if content.len() > config.enrichment.max_file_preview_bytes {
                        &content[..config.enrichment.max_file_preview_bytes]
                    } else {
                        &content
                    };
                    context.push_str(&format!("\n{}:\n{}\n", pattern, preview));
                }
                Err(e) => {
                    debug!("Could not read project file {}: {e}", path.display());
                }
            }
        }
    }

    let messages = vec![
        Message {
            role: "system".to_string(),
            content: "You are a service metadata generator. Given information about a command being run, generate a concise name and description. Respond in exactly this format:\nNAME: <short name, 2-4 words>\nDESCRIPTION: <one sentence description>".to_string(),
        },
        Message {
            role: "user".to_string(),
            content: context,
        },
    ];

    match client.complete(messages).await {
        Ok(response) => {
            let mut name = None;
            let mut description = None;

            for line in response.lines() {
                if let Some(n) = line.strip_prefix("NAME:") {
                    name = Some(n.trim().to_string());
                } else if let Some(d) = line.strip_prefix("DESCRIPTION:") {
                    description = Some(d.trim().to_string());
                }
            }

            // If the user provided a name, do not overwrite it with the LLM-generated one
            let effective_name = if has_user_name { None } else { name.as_deref() };

            match Database::open(&config.db_path()) {
                Ok(db) => {
                    if let Err(e) = db.update_service_enrichment(
                        service_id,
                        effective_name,
                        description.as_deref(),
                        &EnrichmentStatus::Completed,
                    ) {
                        warn!(
                            "Failed to store enrichment result for service {service_id}: {e}"
                        );
                    } else {
                        info!("Enriched service {}: name={:?}", service_id, name);
                    }
                }
                Err(e) => {
                    warn!("Failed to open database for enrichment (service {service_id}): {e}");
                }
            }
        }
        Err(e) => {
            warn!("LLM enrichment failed: {}", e);
            match Database::open(&config.db_path()) {
                Ok(db) => {
                    if let Err(e) = db.update_service_enrichment(
                        service_id,
                        None,
                        None,
                        &EnrichmentStatus::Failed,
                    ) {
                        warn!(
                            "Failed to mark enrichment as failed for service {service_id}: {e}"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to open database for enrichment (service {service_id}): {e}"
                    );
                }
            }
        }
    }
}
