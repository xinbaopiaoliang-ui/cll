use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "xaccel-node")]
#[command(about = "Linux game acceleration node daemon")]
pub struct Cli {
    #[arg(long)]
    pub config: Option<PathBuf>,

    #[arg(long = "check-config")]
    pub check_config: Option<PathBuf>,

    #[arg(long)]
    pub version: bool,
}
