use anyhow::Result;
use std::path::Path;

use crate::cli::ListArgs;
use crate::config::Config;
use crate::storage::logfile::log_sizes;
use crate::storage::Database;

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}K", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}M", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub async fn handle_list(args: ListArgs) -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(&config.db_path())?;

    let services = if let Some(ref name) = args.name {
        db.search_services(Some(name), None, &[], None, None, 100)?
    } else {
        db.list_services()?
    };

    if services.is_empty() {
        println!("No services found.");
        return Ok(());
    }

    if !args.verbose {
        println!(
            "{:<8}  {:<20}  {:<12}  {}",
            "ID", "NAME", "STATUS", "COMMAND"
        );
    }

    for service in &services {
        let name_display = service.name.as_deref().unwrap_or(&service.id[..8]);
        let desc_display = service
            .description
            .as_deref()
            .unwrap_or("(no description)");

        if args.verbose {
            println!("ID:          {}", service.id);
            println!("Name:        {}", name_display);
            println!("Description: {}", desc_display);
            println!("Executable:  {}", service.executable);
            println!("Command:     {}", service.command_line.join(" "));
            println!("Working Dir: {}", service.working_dir);
            println!("Created:     {}", service.created_at);
            println!("Updated:     {}", service.updated_at);

            let tags = db.get_tags(&service.id)?;
            if !tags.is_empty() {
                let tag_strs: Vec<String> =
                    tags.iter().map(|t| format!("{}:{}", t.key, t.value)).collect();
                println!("Tags:        {}", tag_strs.join(", "));
            }

            if let Some(run) = db.get_latest_run(&service.id)? {
                println!(
                    "Latest Run:  {} ({})",
                    run.id[..8].to_string(),
                    run.status.as_str()
                );
                println!("Started At:  {}", run.started_at);
                if let Some(exit_code) = run.exit_code {
                    println!("Exit Code:   {}", exit_code);
                }
                let (stdout_sz, stderr_sz, stdin_sz, combined_sz) =
                    log_sizes(Path::new(&run.log_dir));
                println!(
                    "Log Sizes:   stdout={} stderr={} stdin={} combined={}",
                    format_bytes(stdout_sz),
                    format_bytes(stderr_sz),
                    format_bytes(stdin_sz),
                    format_bytes(combined_sz),
                );
            }
            println!("---");
        } else {
            let status = if let Some(run) = db.get_latest_run(&service.id)? {
                run.status.as_str().to_string()
            } else {
                "no runs".to_string()
            };
            println!(
                "{:<8}  {:<20}  {:<12}  {}",
                &service.id[..8],
                name_display,
                status,
                service.command_line.join(" ")
            );
        }
    }

    Ok(())
}
