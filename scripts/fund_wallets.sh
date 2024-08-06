#!/bin/bash

cd keypairs

# Keypair used for sending SOL if balance is below threshold
SENDER_KEYPAIR=".json"

# Threshold balance in lamports (0.01 SOL)
THRESHOLD_BALANCE=0.001

# Loop through all keypairx.json files in the directory
for keypair in *.json; do
    # Extract the public key from the keypair file
    pubkey=$(solana-keygen pubkey $keypair)

    # Check the balance of the wallet
    balance=$(solana balance $pubkey --url https:// | awk '{print $1}')

    echo "Checking balance for $pubkey: $balance SOL $keypair"

    # # # Use awk to compare float values directly
    # if awk 'BEGIN {exit !('$balance' < '$THRESHOLD_BALANCE')}' ; then
    #     echo "Balance of $pubkey is below $THRESHOLD_BALANCE SOL, sending SOL..."

    #     # Command to send SOL from SENDER_KEYPAIR to the current wallet
    #     # Adjust the amount as needed. This example sends 0.01 SOL.
    #     transaction=$(solana transfer -k $SENDER_KEYPAIR $pubkey 0.1 --url https:// --allow-unfunded-recipient --with-compute-unit-price 100000)

    #    echo "Transaction Signature: $transaction
    # fi
done