use clap::Parser;
use codesync::{
    config::AppConfig,
    error::{CodeSyncError, Result},
    git_backend::Git2Backend,
    http,
    sync::sync_once,
};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "codesync",
    version,
    about = "Synchronize two HTTPS Git remotes without invoking git or Python"
)]
struct Args {
    #[arg(long, value_name = "PATH", default_value = "config.json")]
    config: PathBuf,

    #[arg(long)]
    once: bool,

    #[arg(long, value_name = "LEVEL", default_value = "info")]
    log_level: String,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    init_logging(&args.log_level)?;

    let config = AppConfig::from_path(&args.config)?;
    if args.once {
        let mut backend = Git2Backend::default();
        let result = sync_once(&config, &mut backend, "manual")?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    http::serve(config)
}

fn init_logging(level: &str) -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_new(level)
        .map_err(|_| CodeSyncError::Config(format!("invalid log level: {level}")))?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .try_init()
        .map_err(|error| CodeSyncError::Config(format!("failed to initialize logging: {error}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_config_path_to_config_json() {
        let args = Args::parse_from(["codesync"]);

        assert_eq!(args.config, PathBuf::from("config.json"));
        assert!(!args.once);
        assert_eq!(args.log_level, "info");
    }

    #[test]
    fn parses_once_and_log_level_flags() {
        let args = Args::parse_from(["codesync", "--once", "--log-level", "debug"]);

        assert!(args.once);
        assert_eq!(args.log_level, "debug");
    }
}
