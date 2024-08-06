mod args;
mod balance;
mod benchmark;
mod busses;
mod claim;
mod close;
mod config;
mod cu_limits;
#[cfg(feature = "admin")]
mod initialize;
mod mine;
mod open;
mod rewards;
mod send_and_confirm;
mod send_and_confirm_bundle;
mod stake;
mod upgrade;
mod utils;
mod jito_tip;
use std::sync::Arc;

use args::*;
use clap::{command, Parser, Subcommand};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    signature::{read_keypair_file, Keypair},
};

struct Miner {
    pub folder_path: Option<String>,
    pub keypair_filepath: Option<String>,
    pub fee_payer_file_path: Option<String>,
    pub priority_fee: u64,
    pub rpc_client: Arc<RpcClient>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    #[command(about = "Fetch an account balance")]
    Balance(BalanceArgs),

    #[command(about = "Benchmark your hashpower")]
    Benchmark(BenchmarkArgs),

    #[command(about = "Fetch the bus account balances")]
    Busses(BussesArgs),

    #[command(about = "Claim your mining rewards")]
    Claim(ClaimArgs),

    #[command(about = "Close your account to recover rent")]
    Close(CloseArgs),

    #[command(about = "Fetch the program config")]
    Config(ConfigArgs),

    #[command(about = "Start mining")]
    Mine(MineArgs),

    #[command(about = "Fetch the current reward rate for each difficulty level")]
    Rewards(RewardsArgs),

    #[command(about = "Stake to earn a rewards multiplier")]
    Stake(StakeArgs),

    #[command(about = "Upgrade your ORE tokens from v1 to v2")]
    Upgrade(UpgradeArgs),

    #[cfg(feature = "admin")]
    #[command(about = "Initialize the program")]
    Initialize(InitializeArgs),
}

#[derive(Parser, Debug)]
#[command(about, version)]
struct Args {
    #[arg(
        long,
        value_name = "NETWORK_URL",
        help = "Network address of your RPC provider",
        global = true
    )]
    rpc: Option<String>,

    #[clap(
        global = true,
        short = 'C',
        long = "config",
        id = "PATH",
        help = "Filepath to config file."
    )]
    config_file: Option<String>,

    #[arg(
        long,
        value_name = "KEYPAIR_FILEPATH",
        help = "Filepath to keypair to use",
        global = true
    )]
    keypair: Option<String>,

    #[arg(
        long,
        value_name = "MICROLAMPORTS",
        help = "Number of microlamports to pay as priority fee per transaction",
        default_value = "0",
        global = true
    )]
    priority_fee: u64,

    #[arg(
        long,
        value_name = "FOLDER_PATH",
        help = "folder path for to keypairs",
        global = true
    )]
    folder_path: Option<String>,

    #[arg(
        long,
        value_name = "FEE_PAYER_FILE_PATH",
        help = "fee payer file path for jito",
        global = true
    )]
    fee_payer: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Load the config file from custom path, the default path, or use default config values
    let cli_config = if let Some(config_file) = &args.config_file {
        solana_cli_config::Config::load(config_file).unwrap_or_else(|_| {
            eprintln!("error: Could not find config file `{}`", config_file);
            std::process::exit(1);
        })
    } else if let Some(config_file) = &*solana_cli_config::CONFIG_FILE {
        solana_cli_config::Config::load(config_file).unwrap_or_default()
    } else {
        solana_cli_config::Config::default()
    };

    // Initialize miner.
    let cluster = args.rpc.unwrap_or(cli_config.json_rpc_url);
    let default_keypair = args.keypair.unwrap_or(cli_config.keypair_path);
    let rpc_client = RpcClient::new_with_commitment(cluster, CommitmentConfig::confirmed());
    let folder_path = args.folder_path;
    let miner = Arc::new(Miner::new(
        Arc::new(rpc_client),
        args.priority_fee,
        Some(default_keypair),
        folder_path,
        args.fee_payer
    ));

    // Execute user command.
    match args.command {
        Commands::Balance(args) => {
            miner.balance(args).await;
        }
        Commands::Benchmark(args) => {
            miner.benchmark(args).await;
        }
        Commands::Busses(_) => {
            miner.busses().await;
        }
        Commands::Claim(args) => {
            miner.claim(args).await;
        }
        Commands::Close(_) => {
            miner.close().await;
        }
        Commands::Config(_) => {
            miner.config().await;
        }
        Commands::Mine(args) => {
            miner.mine(args).await;
        }
        Commands::Rewards(_) => {
            miner.rewards().await;
        }
        Commands::Stake(args) => {
            miner.stake(args).await;
        }
        Commands::Upgrade(args) => {
            miner.upgrade(args).await;
        }
        #[cfg(feature = "admin")]
        Commands::Initialize(_) => {
            miner.initialize().await;
        }
    }
}

impl Miner {
    pub fn new(
        rpc_client: Arc<RpcClient>,
        priority_fee: u64,
        keypair_filepath: Option<String>,
        folder_path: Option<String>,
        fee_payer: Option<String>,
    ) -> Self {
        Self {
            rpc_client,
            keypair_filepath,
            priority_fee,
            folder_path,
            fee_payer_file_path: fee_payer
        }
    }

    pub fn signer(&self) -> Keypair {
        match self.keypair_filepath.clone() {
            Some(filepath) => read_keypair_file(filepath.clone())
                .expect(format!("No keypair found at {}", filepath).as_str()),
            None => panic!("No keypair provided"),
        }
    }
    pub fn fee_payer(&self) -> Keypair {
        let fee_payer = self.fee_payer_file_path.clone().unwrap();
        read_keypair_file(fee_payer).unwrap()
    }
    
    pub fn multi_signers(&self) -> Vec<Keypair> {
        let mut keypairs = Vec::new();

        match self.folder_path.clone() {
            Some(folder_path) => {
                self.traverse_folder(&folder_path, &mut keypairs);
            }
            None => panic!("No keypair provided"),
        }

        keypairs
    }

    fn traverse_folder(&self, folder_path: &String, keypairs: &mut Vec<Keypair>) {
        let entries = std::fs::read_dir(folder_path).unwrap();

        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            if !path.is_file() {
                self.traverse_folder(&path.to_str().unwrap().to_string(), keypairs);
            } else if path.is_file() {
                let file_name = path.file_name().unwrap().to_str().unwrap();
                if file_name != "payer.json" {
                    if let Ok(keypair) = read_keypair_file(&path) {
                        keypairs.push(keypair);
                    } else {
                        eprintln!("Error reading keypair file: {}", file_name);
                    }
                }
            }
        }
    }
}
