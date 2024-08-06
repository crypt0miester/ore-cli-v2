use crate::{jito_tip::JITO_COUNT, Miner};
use colored::Colorize;
use rand::Rng;
use serde_json::json;
use solana_client::{
    client_error::{ClientError, ClientErrorKind, Result as ClientResult},
    nonblocking::rpc_client::RpcClient,
    rpc_client::SerializableTransaction,
    rpc_config::RpcSendTransactionConfig,
    rpc_request::{RpcError, RpcRequest, RpcResponseErrorData},
    rpc_response::RpcSimulateTransactionResult,
};
use solana_program::instruction::Instruction;
use solana_rpc_client::spinner;
use solana_sdk::{
    commitment_config::{CommitmentConfig, CommitmentLevel},
    compute_budget::ComputeBudgetInstruction,
    hash::Hash,
    message::{v0, Message, VersionedMessage},
    pubkey::Pubkey,
    signature::{Keypair, Signature, Signer},
    system_instruction,
    transaction::{Transaction, VersionedTransaction},
};
use solana_transaction_status::{TransactionConfirmationStatus, UiTransactionEncoding};
use std::{
    io::{stdout, Write},
    time::Duration,
};

const RPC_RETRIES: usize = 1;
const GATEWAY_RETRIES: usize = 4;
const CONFIRM_RETRIES: usize = 4;

use base64::Engine;
use bincode::serialize;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct RequestPayload {
    method: String,
    params: serde_json::Value,
    id: u64,
    jsonrpc: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct ResponseData {
    jsonrpc: String,
    result: String,
    id: u64,
}

fn serialize_and_encode_multi<T>(
    inputs: &[T],
    encoding: UiTransactionEncoding,
) -> ClientResult<Vec<String>>
where
    T: serde::ser::Serialize,
{
    let mut encoded_inputs = Vec::new();

    for input in inputs {
        let serialized = serialize(input)
            .map_err(|e| ClientErrorKind::Custom(format!("Serialization failed: {e}")))?;
        let encoded = match encoding {
            UiTransactionEncoding::Base58 => bs58::encode(serialized).into_string(),
            UiTransactionEncoding::Base64 => base64::prelude::BASE64_STANDARD.encode(serialized),
            _ => {
                return Err(ClientErrorKind::Custom(format!(
                    "unsupported encoding: {encoding}. Supported encodings: base58, base64"
                ))
                .into())
            }
        };
        encoded_inputs.push(encoded);
    }

    Ok(encoded_inputs)
}
fn serialize_and_encode<T>(input: &T, encoding: UiTransactionEncoding) -> ClientResult<String>
where
    T: serde::ser::Serialize,
{
    let serialized = serialize(input)
        .map_err(|e| ClientErrorKind::Custom(format!("Serialization failed: {e}")))?;
    let encoded = match encoding {
        UiTransactionEncoding::Base58 => bs58::encode(serialized).into_string(),
        UiTransactionEncoding::Base64 => base64::prelude::BASE64_STANDARD.encode(serialized),
        _ => {
            return Err(ClientErrorKind::Custom(format!(
                "unsupported encoding: {encoding}. Supported encodings: base58, base64"
            ))
            .into())
        }
    };
    Ok(encoded)
}

async fn send_transaction_with_config_bundle(
    transactions: Vec<impl SerializableTransaction>,
    jito_url: String
) -> ClientResult<Signature> {
    let encoding = UiTransactionEncoding::Base58;
    let serialized_encoded = serialize_and_encode_multi(transactions.as_slice(), encoding)?;

    let payload = RequestPayload {
        method: "sendBundle".to_string(),
        params: json!([serialized_encoded]),
        id: 1,
        jsonrpc: "2.0".to_string(),
    };

    let client = reqwest::Client::new();

    let signature_base58_str = match client
        .post(jito_url)
        .json(&payload)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<ResponseData>()
        .await
    {
        Ok(response) => response.result,
        Err(_) => {
            return Err(ClientError {
                request: None,
                kind: ClientErrorKind::Custom("Failed to send jito transaction".into()),
            })
        }
    };

    println!("Jito Bundle: {}", signature_base58_str);
    let first_txn = transactions.first().unwrap();
    Ok(*first_txn.get_signature())
}

async fn send_transaction_with_config(
    client: &RpcClient,
    transaction: &impl SerializableTransaction,
    config: RpcSendTransactionConfig,
) -> ClientResult<Signature> {
    let encoding = if let Some(encoding) = config.encoding {
        encoding
    } else {
        UiTransactionEncoding::Base64
    };
    let preflight_commitment = CommitmentConfig {
        commitment: config.preflight_commitment.unwrap_or_default(),
    };
    // let preflight_commitment = client.maybe_map_commitment(preflight_commitment).await?;
    let config = RpcSendTransactionConfig {
        encoding: Some(encoding),
        preflight_commitment: Some(preflight_commitment.commitment),
        ..config
    };
    let serialized_encoded = serialize_and_encode(transaction, encoding)?;
    let signature_base58_str: String = match client
        .send(
            RpcRequest::SendTransaction,
            json!([serialized_encoded, config]),
        )
        .await
    {
        Ok(signature_base58_str) => signature_base58_str,
        Err(err) => {
            if let ClientErrorKind::RpcError(RpcError::RpcResponseError {
                code,
                message,
                data,
            }) = &err.kind
            {
                println!("{} {}", code, message);
                if let RpcResponseErrorData::SendTransactionPreflightFailure(
                    RpcSimulateTransactionResult {
                        logs: Some(logs), ..
                    },
                ) = data
                {
                    for (i, log) in logs.iter().enumerate() {
                        println!("{:>3}: {}", i + 1, log);
                    }
                    println!("");
                }
            }
            return Err(err);
        }
    };

    let signature = signature_base58_str
        .parse::<Signature>()
        .map_err(|err| Into::<ClientError>::into(RpcError::ParseError(err.to_string())))?;
    if signature != *transaction.get_signature() {
        Err(RpcError::RpcRequestError(format!(
            "RPC node returned mismatched signature {:?}, expected {:?}",
            signature,
            transaction.get_signature()
        ))
        .into())
    } else {
        Ok(*transaction.get_signature())
    }
}

impl Miner {
    pub async fn send_and_confirm_with_key(
        &self,
        ixs: &[Instruction],
        skip_confirm: bool,
        signer: &Keypair,
    ) -> ClientResult<Signature> {
        let mut stdout = stdout();
        println!("for {}", signer.pubkey());
        let client = self.rpc_client.clone();

        // Build tx
        let (mut hash, mut slot, mut send_cfg, mut tx) =
            generate_transaction(&client, ixs, signer).await;

        // Submit tx
        let mut sigs = vec![];
        let mut attempts = 0;
        let mut sleep_duration = Duration::from_millis(10000);
        loop {
            println!("Attempt: {:?}", attempts);
            match send_transaction_with_config(&client, &tx, send_cfg).await {
                Ok(sig) => {
                    sigs.push(sig);
                    println!("{:?}", sig);

                    // Confirm tx
                    if skip_confirm {
                        return Ok(sig);
                    }
                    for _ in 0..CONFIRM_RETRIES {
                        std::thread::sleep(sleep_duration);
                        match client.get_signature_statuses(&sigs).await {
                            Ok(signature_statuses) => {
                                println!("Confirms: {:?}", signature_statuses.value);
                                for signature_status in signature_statuses.value {
                                    if let Some(signature_status) = signature_status.as_ref() {
                                        if signature_status.confirmation_status.is_some() {
                                            let current_commitment = signature_status
                                                .confirmation_status
                                                .as_ref()
                                                .unwrap();
                                            match current_commitment {
                                                TransactionConfirmationStatus::Processed => {
                                                    sleep_duration = Duration::from_millis(1000)
                                                }
                                                TransactionConfirmationStatus::Confirmed
                                                | TransactionConfirmationStatus::Finalized => {
                                                    println!("Transaction landed!");
                                                    return Ok(sig);
                                                }
                                            }
                                        } else {
                                            println!("No status");
                                        }
                                    }
                                }
                            }

                            // Handle confirmation errors
                            Err(err) => {
                                println!("Error: {:?}", err);
                            }
                        }
                    }
                    println!("Transaction did not land");
                }

                // Handle submit errors
                Err(err) => {
                    println!("Error {:?}", err);
                }
            }
            stdout.flush().ok();

            // Retry
            std::thread::sleep(Duration::from_millis(2000));
            (hash, slot) = client
                .get_latest_blockhash_with_commitment(CommitmentConfig::confirmed())
                .await
                .unwrap();
            send_cfg = RpcSendTransactionConfig {
                skip_preflight: true,
                preflight_commitment: Some(CommitmentLevel::Confirmed),
                encoding: Some(UiTransactionEncoding::Base64),
                max_retries: Some(RPC_RETRIES),
                min_context_slot: Some(slot),
            };
            tx.sign(&[&signer], hash);
            attempts += 1;
            if attempts > GATEWAY_RETRIES {
                return Err(ClientError {
                    request: None,
                    kind: ClientErrorKind::Custom("Max retries".into()),
                });
            }
        }
    }

    pub async fn send_and_confirm_bundle(
        &self,
        ixs: &[Instruction],
        skip_confirm: bool,
        jito_tip_amount: u64,
        jito_url: String
    ) -> ClientResult<Signature> {
        let progress_bar = spinner::new_progress_bar();
        let signers = self.multi_signers();
        let fee_payer = self.fee_payer();
        let client = self.rpc_client.clone();
        // Build tx
        let (hash, _slot) = client
            .get_latest_blockhash_with_commitment(CommitmentConfig::finalized())
            .await
            .unwrap();
        let final_ixs = ixs.to_vec();
        let mut txs: Vec<VersionedTransaction> = vec![];
        let num_ixs_per_tx: usize = 2; // Number of instructions per transaction

        let mut current_idx = 0;

        while current_idx < final_ixs.len() {
            let mut current_ixs: Vec<Instruction> = vec![];

            for _ in 0..num_ixs_per_tx {
                if current_idx >= final_ixs.len() {
                    break;
                }
                current_ixs.push(final_ixs[current_idx].clone());
                current_idx += 1;
            }

            let cu_limit_ix = ComputeBudgetInstruction::set_compute_unit_limit(500_000);
            current_ixs.push(cu_limit_ix);

            // Add Jito instruction to the last transaction
            let mut fee_payer_signers: Vec<&Keypair> = vec![&fee_payer];
            let mut add_jito_tip = false;

            for ixn in current_ixs.iter() {
                if ixn
                    .accounts
                    .iter()
                    .any(|account| account.pubkey == fee_payer.pubkey())
                {
                    add_jito_tip = true;
                    break;
                }
            }

            if add_jito_tip {
                let jito_key = self.find_jito_tip_account().await;
                let jito_tip_ix =
                    system_instruction::transfer(&fee_payer.pubkey(), &jito_key, jito_tip_amount);
                current_ixs.push(jito_tip_ix);
                fee_payer_signers = vec![];
            }

            let message_v0 =
                v0::Message::try_compile(&fee_payer.pubkey(), current_ixs.as_slice(), &[], hash)
                    .unwrap();
            let message_v0 = VersionedMessage::V0(message_v0);

            fee_payer_signers.extend(
                signers
                    .iter()
                    .filter(|keypair| {
                        current_ixs.iter().any(|ix| {
                            ix.accounts
                                .iter()
                                .any(|acc| acc.is_signer && acc.pubkey == keypair.pubkey())
                        })
                    })
                    .map(|keypair| keypair),
            );

            let tx =
                VersionedTransaction::try_new(message_v0, fee_payer_signers.as_slice()).unwrap();
            txs.push(tx);
            current_ixs.clear();
        }

        // Submit tx
        let mut sigs = vec![];
        let mut sleep_duration = Duration::from_millis(2000);
        let mut attempts = 0;
        loop {
            let jito_url_clone = jito_url.clone();
            match send_transaction_with_config_bundle(txs.clone(), jito_url_clone).await {
                Ok(sig) => {
                    sigs.push(sig);
                    println!("{:?}", sig);

                    // Confirm tx
                    if skip_confirm {
                        return Ok(sig);
                    }
                    for _ in 0..CONFIRM_RETRIES {
                        std::thread::sleep(sleep_duration);
                        match client.get_signature_statuses(&sigs).await {
                            Ok(signature_statuses) => {
                                progress_bar.set_message(format!(
                                    "Confirms: {:?}",
                                    signature_statuses.value
                                ));
                                for signature_status in signature_statuses.value {
                                    if let Some(signature_status) = signature_status.as_ref() {
                                        if signature_status.confirmation_status.is_some() {
                                            let current_commitment = signature_status
                                                .confirmation_status
                                                .as_ref()
                                                .unwrap();
                                            match current_commitment {
                                                TransactionConfirmationStatus::Processed => {
                                                    sleep_duration = Duration::from_millis(1000)
                                                }
                                                TransactionConfirmationStatus::Confirmed
                                                | TransactionConfirmationStatus::Finalized => {
                                                    progress_bar.finish_with_message(format!(
                                                        "Transaction landed"
                                                    ));
                                                    return Ok(sig);
                                                }
                                            }
                                        } else {
                                            println!("No status");
                                        }
                                    }
                                }
                            }

                            // Handle confirmation errors
                            Err(err) => {
                                progress_bar.set_message(format!(
                                    "{}: {}",
                                    "ERROR".bold().red(),
                                    err.kind().to_string()
                                ));
                            }
                        }
                    }
                }

                // Handle submit errors
                Err(err) => {
                    progress_bar.set_message(format!(
                        "{}: {}",
                        "ERROR".bold().red(),
                        err.kind().to_string()
                    ));
                }
            }
            // Retry
            std::thread::sleep(Duration::from_millis(300));
            attempts += 1;
            if attempts > GATEWAY_RETRIES {
                progress_bar.finish_with_message(format!("{}: Max retries", "ERROR".bold().red()));
                return Err(ClientError {
                    request: None,
                    kind: ClientErrorKind::Custom("Max retries".into()),
                });
            }
        }
    }

    pub async fn find_jito_tip_account(&self) -> Pubkey {
        let mut rng = rand::thread_rng();
        let jito_id = rng.gen_range(0..JITO_COUNT);
        self.get_jito_tip_account(jito_id)
    }
}

async fn generate_transaction(
    client: &RpcClient,
    ixs: &[Instruction],
    signer: &Keypair,
) -> (Hash, u64, RpcSendTransactionConfig, Transaction) {
    let (hash, slot) = client
        .get_latest_blockhash_with_commitment(CommitmentConfig::confirmed())
        .await
        .unwrap();

    let send_cfg = RpcSendTransactionConfig {
        skip_preflight: true,
        preflight_commitment: Some(CommitmentLevel::Processed),
        encoding: Some(UiTransactionEncoding::Base64),
        max_retries: None,
        min_context_slot: Some(slot),
    };
    let message = Message::new(ixs, Some(&signer.pubkey()));

    let mut tx = Transaction::new(&[&signer], message, hash);

    tx.sign(&[&signer], hash);
    (hash, slot, send_cfg, tx)
}
