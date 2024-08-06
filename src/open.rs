use solana_sdk::{signature::Signer, compute_budget::ComputeBudgetInstruction};

use crate::{send_and_confirm::ComputeBudget, utils::proof_pubkey, Miner};

impl Miner {
    pub async fn open_all(&self) {
        let signers = self.multi_signers();
        let client = self.rpc_client.clone();
    
        for signer in signers {
            // Return early if miner is already registered
            let proof_address = proof_pubkey(signer.pubkey());
            println!("{}", signer.pubkey());
    
            if client.get_account(&proof_address).await.is_err() {
                // Sign and send transaction.
                println!("Generating proof account... for {}", signer.pubkey());
                // let cu_limit_ix = ComputeBudgetInstruction::set_compute_unit_limit(CU_LIMIT_REGISTER + 1000);
                let cu_price_ix = ComputeBudgetInstruction::set_compute_unit_price(self.priority_fee);
                let ix = ore_api::instruction::open(signer.pubkey(), signer.pubkey(), signer.pubkey());
    
                self.send_and_confirm_with_key(&mut [cu_price_ix, ix], false, &signer)
                .await
                .ok();
            }
        }
    }
}
