mod protocol;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "rushway", version, about = "Rust forwarding service compatible with the GoWay command-line shape")]
struct Args {
    /// Optional JSON configuration file.
    #[arg(short, long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        .init();

    let args = Args::parse();

    if let Some(path) = args.config {
        let content = tokio::fs::read_to_string(&path).await?;
        let _: serde_json::Value = serde_json::from_str(&content)?;
        tracing::info!(config = %path.display(), "configuration loaded");
    }

    tracing::warn!("RushWay v0.0.1 bootstrap is not yet protocol-compatible with GoWay v1.8.4");
    tracing::warn!("The transport, MUX, TLS/QUIC and forwarding implementation must be completed before release");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_config_argument() {
        let args = Args::parse_from(["rushway", "--config", "config.json"]);
        assert_eq!(args.config.unwrap().to_string_lossy(), "config.json");
    }
}
