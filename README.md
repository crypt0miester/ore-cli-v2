# ORE CLI

A command line interface for ORE cryptocurrency mining.

## Install

To install the CLI, use [cargo](https://doc.rust-lang.org/cargo/getting-started/installation.html):

```sh
cargo install ore-cli
```

## Build

To build the codebase from scratch, checkout the repo and use cargo to build:

```sh
cargo build --release
```

## Help

You can use the `-h` flag on any command to pull up a help menu with documentation:

```sh
ore -h
```

# Features
 * minimum difficulty argument
current configuration is 2, the higher the minimum difficulty the higher your reward.
it will depend on how many threads you use. 
 * jito fee-payer argument
the jito tipper
 * jito fee amount argument
amount to be paid for jito tips
 * jito url argument
must include `/api/v1/bundles`
 * 5 keypairs (5 transactions per bundle)
you will need to set up 5 keypairs inside a folder (add sols, stake, etc.) one of them would have higher sol to pay for jito.

## how to run (after funding 5 keypairs and building)
```sh
./target/release/ore mine --rpc <rpc_url> --folder-path <keypairs folder path> --keypair <dummy field> --priority-fee <dummy field (used for opening new accouns)> --fee-payer <path to keypair.json for jito fee payer> --jito-tip <jito tip amount> --min-difficulty 10 --jito-url <jito endpoint>
```
