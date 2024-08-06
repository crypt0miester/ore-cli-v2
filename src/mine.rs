use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use colored::*;
use drillx::{
    equix::{self},
    Hash, Solution,
};
use futures::future::join_all;
use ore_api::{
    consts::{BUS_ADDRESSES, BUS_COUNT, EPOCH_DURATION, TOKEN_DECIMALS_V1},
    state::{Bus, Config, Proof},
};
use ore_utils::AccountDeserialize;
use solana_client::client_error::Result;
use solana_program::pubkey::Pubkey;
use solana_rpc_client::spinner;
use solana_sdk::signer::Signer;

use crate::{
    args::MineArgs,
    utils::{amount_u64_to_string, get_clock, get_config, get_proof_with_authority, proof_pubkey},
    Miner,
};

impl Miner {
    pub async fn mine(&self, args: MineArgs) {
        // Register, if needed.
        let signers = self.multi_signers();
        let fee_payer = self.fee_payer();
        self.open_all().await;

        // Check num threads
        self.check_num_cores(args.threads);

        // Start mining loop
        loop {
            let mut proofs = Vec::new();
            let mut solutions = Vec::new();
            let mut sol_balances = Vec::new();
            let client = self.rpc_client.clone();

            println!("Mining for multi valid hash...\n");
            let start = std::time::Instant::now();

            for signer in &signers {
                // Fetch proof
                let proof = get_proof_with_authority(&client, signer.pubkey()).await;
                println!(
                    "\nStake balance for {}: {} ORE",
                    signer.pubkey(),
                    amount_u64_to_string(proof.balance)
                );
                proofs.push(proof.clone());

                let sol_balance = client
                    .get_balance(&signer.pubkey())
                    .await
                    .unwrap_or_else(|_| 0);
                let sol_balance_normal =
                    (sol_balance as f64) / (10f64.powf(TOKEN_DECIMALS_V1 as f64));
                sol_balances.push(sol_balance_normal);

                // Run drillx
                let config = get_config(&client).await;
                let min_difficulty = if args.min_difficulty == 0 {
                    config.min_difficulty as u32
                } else {
                    args.min_difficulty
                };
                let solution = Self::find_hash_par(
                    proof,
                    0, // We'll handle cutoff time later
                    args.threads,
                    min_difficulty,
                )
                .await;
                solutions.push(solution);

            }
            println!("Sol Balances: {:?} SOL", sol_balances);
            println!("fee payer address: {}", fee_payer.pubkey());

            let duration = start.elapsed();
            println!("\nHash generation took {:?}", duration);

            // Calc cutoff time and wait if necessary
            let cutoff_time = self.get_cutoff(proofs.last().unwrap().clone(), args.buffer_time).await;
            let elapsed = start.elapsed().as_secs();
            let progress_bar = Arc::new(spinner::new_progress_bar());
            if elapsed < cutoff_time {
                let wait_time = cutoff_time - elapsed;
                println!("Waiting for {} seconds before submitting...", wait_time);
                
                let wait_start = Instant::now();
                while wait_start.elapsed().as_secs() < wait_time {
                    // You can add a small sleep here to prevent busy-waiting
                    tokio::time::sleep(Duration::from_millis(1000)).await;
                    
                    // Update the progress bar with the remaining time
                    let remaining = wait_time - wait_start.elapsed().as_secs();
                    progress_bar.set_message(format!("Time remaining: {} seconds", remaining));
                }
            }

            // Submit mine tx
            progress_bar.finish_with_message(format!(
                "\n\nSubmitting hash...",
            ));
            let highest_bus_pubkey = self.find_highest_reward_bus().await;

            let mut all_ixs = Vec::new();
            for (signer, solution) in signers.iter().zip(solutions.iter()) {
                
                all_ixs.push(ore_api::instruction::auth(proof_pubkey(signer.pubkey())));
                
                all_ixs.push(ore_api::instruction::mine(
                    signer.pubkey(),
                    signer.pubkey(),
                    highest_bus_pubkey,
                    *solution,
                ));
            }
            let jito_url = args.jito_url.clone();
            match self
                .send_and_confirm_bundle(all_ixs.as_slice(), false, args.jito_tip, jito_url)
                .await
            {
                Ok(_sig) => {
                    println!("\n\n");
                }
                Err(_err) => {
                    println!("Failed to send, let's try again.\n\n");
                }
            }
        }
    }

    pub async fn get_bus(&self, id: usize) -> Result<Bus> {
        let client = self.rpc_client.clone();
        let data = client.get_account_data(&BUS_ADDRESSES[id]).await?;
        Ok(*Bus::try_from_bytes(&data).unwrap())
    }

    async fn find_highest_reward_bus(&self) -> Pubkey {
        let bus_futures: Vec<_> = (0..BUS_COUNT).map(|bus_id| self.get_bus(bus_id)).collect();

        let buses: Vec<_> = join_all(bus_futures)
            .await
            .into_iter()
            .filter_map(Result::ok)
            .collect();

        let highest_bus = buses.into_iter().max_by_key(|bus| bus.rewards).unwrap();
        let id = highest_bus.id;
        BUS_ADDRESSES[id as usize]
    }

    async fn find_hash_par(
        proof: Proof,
        cutoff_time: u64,
        threads: u64,
        min_difficulty: u32,
    ) -> Solution {
        // Dispatch job to each thread
        let progress_bar = Arc::new(spinner::new_progress_bar());
        progress_bar.set_message("Mining...");
        let handles: Vec<_> = (0..threads)
            .map(|i| {
                std::thread::spawn({
                    let proof = proof.clone();
                    let progress_bar = progress_bar.clone();
                    let mut memory = equix::SolverMemory::new();
                    move || {
                        let timer = Instant::now();
                        let mut nonce = u64::MAX.saturating_div(threads).saturating_mul(i);
                        let mut best_nonce = nonce;
                        let mut best_difficulty = 0;
                        let mut best_hash = Hash::default();
                        loop {
                            // Create hash
                            if let Ok(hx) = drillx::hash_with_memory(
                                &mut memory,
                                &proof.challenge,
                                &nonce.to_le_bytes(),
                            ) {
                                let difficulty = hx.difficulty();
                                if difficulty.gt(&best_difficulty) {
                                    best_nonce = nonce;
                                    best_difficulty = difficulty;
                                    best_hash = hx;
                                }
                            }

                            // Exit if time has elapsed
                            if nonce % 100 == 0 {
                                if timer.elapsed().as_secs().ge(&cutoff_time) {
                                    if best_difficulty.gt(&min_difficulty) {
                                        // Mine until min difficulty has been met
                                        break;
                                    }
                                } else if i == 0 {
                                    progress_bar.set_message(format!(
                                        "Mining... ({} sec remaining)",
                                        cutoff_time.saturating_sub(timer.elapsed().as_secs()),
                                    ));
                                }
                            }

                            // Increment nonce
                            nonce += 1;
                        }

                        // Return the best nonce
                        (best_nonce, best_difficulty, best_hash)
                    }
                })
            })
            .collect();

        // Join handles and return best nonce
        let mut best_nonce = 0;
        let mut best_difficulty = 0;
        let mut best_hash = Hash::default();
        for h in handles {
            if let Ok((nonce, difficulty, hash)) = h.join() {
                if difficulty > best_difficulty {
                    best_difficulty = difficulty;
                    best_nonce = nonce;
                    best_hash = hash;
                }
            }
        }

        // Update log
        progress_bar.finish_with_message(format!(
            "Best hash: {} (difficulty: {})",
            bs58::encode(best_hash.h).into_string(),
            best_difficulty
        ));

        Solution::new(best_hash.d, best_nonce.to_le_bytes())
    }

    pub fn check_num_cores(&self, threads: u64) {
        // Check num threads
        let num_cores = num_cpus::get() as u64;
        if threads.gt(&num_cores) {
            println!(
                "{} Number of threads ({}) exceeds available cores ({})",
                "WARNING".bold().yellow(),
                threads,
                num_cores
            );
        }
    }

    // async fn should_reset(&self, config: Config) -> bool {
    //     let clock = get_clock(&self.rpc_client).await;
    //     config
    //         .last_reset_at
    //         .saturating_add(EPOCH_DURATION)
    //         .saturating_sub(5) // Buffer
    //         .le(&clock.unix_timestamp)
    // }

    async fn get_cutoff(&self, proof: Proof, buffer_time: u64) -> u64 {
        let clock = get_clock(&self.rpc_client).await;
        proof
            .last_hash_at
            .saturating_add(60)
            .saturating_sub(buffer_time as i64)
            .saturating_sub(clock.unix_timestamp)
            .max(0) as u64
    }
}

// // TODO Pick a better strategy (avoid draining bus)
// fn find_bus() -> Pubkey {
//     let i = rand::thread_rng().gen_range(0..BUS_COUNT);
//     BUS_ADDRESSES[i]
// }
