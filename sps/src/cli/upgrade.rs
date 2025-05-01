use std::sync::Arc;

use clap::Args;
use sps_common::cache::Cache;
use sps_common::config::Config;
use sps_common::error::Result;
use sps_core::installed;

use crate::cli::runner::{self, CommandType, PipelineFlags};

#[derive(Args, Debug)]
pub struct UpgradeArgs {
    #[arg()]
    pub names: Vec<String>,

    #[arg(long, conflicts_with = "names")]
    pub all: bool,

    #[arg(long)]
    pub build_from_source: bool,
}

impl UpgradeArgs {
    pub async fn run(&self, config: &Config, cache: Arc<Cache>) -> Result<()> {
        let targets = if self.all {
            println!("Checking all installed packages for upgrades...");
            // Get all installed package names
            let installed = installed::get_installed_packages(config).await?;
            installed.into_iter().map(|p| p.name).collect()
        } else {
            println!("Checking specified packages for upgrades: {:?}", self.names);
            self.names.clone()
        };

        if targets.is_empty() && !self.all {
            println!("No packages specified to upgrade.");
            return Ok(());
        } else if targets.is_empty() && self.all {
            println!("No packages installed to upgrade.");
            return Ok(());
        }

        let flags = PipelineFlags {
            // Populate flags from args
            build_from_source: self.build_from_source,
            // Upgrade should respect original install options ideally,
            // but for now let's default them. This could be enhanced later
            // by reading install receipts.
            include_optional: false,
            skip_recommended: false,
            // ... add other common flags if needed ...
        };

        runner::run_pipeline(
            &targets,
            CommandType::Upgrade { all: self.all },
            config,
            cache,
            &flags,
        )
        .await
    }
}
