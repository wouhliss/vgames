//! vgames-api entry point. Owner: Agent 1 (Backend & DB).

use std::process::ExitCode;

use clap::Parser;
use vgames_api::{Config, server::Role};

#[derive(Parser, Debug)]
#[command(
    name = "vgames-api",
    version,
    about = "vgames server: REST API, realtime gateway and job runner"
)]
struct Cli {
    /// Which parts of the service to run.
    #[arg(long, value_enum, default_value = "all")]
    role: Role,
    /// Apply database migrations (with DATABASE_MIGRATION_URL) and exit.
    #[arg(long)]
    migrate: bool,
    /// Validate the configuration and exit.
    #[arg(long)]
    check_config: bool,
}

fn main() -> ExitCode {
    // A missing .env is fine; a malformed one is reported by the config check below.
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    if cli.check_config {
        println!("configuration OK");
        return ExitCode::SUCCESS;
    }
    if let Err(e) = vgames_api::telemetry::init(&config.log_filter, config.log_format) {
        eprintln!("{e}");
        return ExitCode::from(1);
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("failed to start the async runtime: {e}");
            return ExitCode::from(1);
        }
    };
    let result = runtime.block_on(async {
        if cli.migrate {
            vgames_api::db::migrate(&config).await?;
            tracing::info!("migrations applied");
            return Ok(());
        }
        vgames_api::server::run(config, cli.role).await
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "fatal");
            eprintln!("fatal: {e:#}");
            ExitCode::from(1)
        }
    }
}
