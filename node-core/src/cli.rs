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

    #[arg(long = "make-client-token")]
    pub make_client_token: bool,

    #[arg(long = "token-user-id")]
    pub token_user_id: Option<u64>,

    #[arg(long = "token-device-id")]
    pub token_device_id: Option<String>,

    #[arg(long = "token-game-id")]
    pub token_game_id: Option<u64>,

    #[arg(long = "token-ttl-sec", default_value_t = 120)]
    pub token_ttl_sec: u64,

    #[arg(long = "token-nonce")]
    pub token_nonce: Option<String>,

    #[arg(long = "token-intent-id")]
    pub token_intent_id: Option<String>,

    #[arg(long = "token-target-addr")]
    pub token_target_addr: Option<String>,
}
