#![allow(deprecated)]
use std::{num::NonZeroUsize, str::FromStr};

use anyhow::anyhow;
use clap::Args;
use rand::rngs::OsRng;
use secrecy::ExposeSecret;
use uuid::Uuid;

use zcash_address::ZcashAddress;
use zcash_client_backend::{
    data_api::{
        wallet::{
            create_proposed_transactions, input_selection::GreedyInputSelector, propose_transfer,
            ConfirmationsPolicy, SpendingKeys,
        },
        Account, WalletRead,
    },
    fees::{standard::MultiOutputChangeStrategy, DustOutputPolicy, SplitPolicy, StandardFeeRule},
    proto::service,
    wallet::OvkPolicy,
};
use zcash_client_sqlite::{util::SystemClock, WalletDb};
use zcash_keys::keys::{UnifiedAddressRequest, UnifiedSpendingKey};
use zcash_proofs::prover::LocalTxProver;
use zcash_protocol::{value::Zatoshis, ShieldedProtocol};
use zip321::{Payment, TransactionRequest};

use crate::{
    commands::select_account,
    config::WalletConfig,
    data::get_db_paths,
    error,
    near_api::{self, format_amount, find_asset, find_zec_asset, NearClient},
    remote::ConnectionArgs,
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// The UUID of the account to send funds from
    account_id: Option<Uuid>,

    /// age identity file to decrypt the mnemonic phrase with
    #[arg(short, long)]
    identity: String,

    /// The recipient's address on the destination chain
    #[arg(long)]
    address: String,

    /// The destination asset (e.g. "eth.eth", "btc.btc", "sol.usdc")
    #[arg(long)]
    asset: String,

    /// The amount of ZEC to spend, in zatoshis
    #[arg(long)]
    value: u64,

    /// Slippage tolerance in percent (default 1.0)
    #[arg(long, default_value = "1.0")]
    slippage: f64,

    /// Disable the affiliate fee
    #[arg(long)]
    no_affiliate_fee: bool,

    #[command(flatten)]
    connection: ConnectionArgs,

    /// Note management: the number of notes to maintain in the wallet
    #[arg(long)]
    #[arg(default_value_t = 4)]
    target_note_count: usize,

    /// Note management: the minimum allowed value for split change amounts
    #[arg(long)]
    #[arg(default_value_t = 10000000)]
    min_split_output_value: u64,
}

impl Command {
    pub(crate) async fn run(self, wallet_dir: Option<String>) -> Result<(), anyhow::Error> {
        let mut config = WalletConfig::read(wallet_dir.as_ref())?;
        let params = config.network();

        // Set up the NEAR API client.
        let api_key = config.near_api_key().map(|s| s.to_string());
        let near = NearClient::new(api_key);

        // Fetch available tokens and resolve assets.
        println!("Fetching available swap assets...");
        let tokens = near.fetch_tokens().await?;
        let zec_asset = find_zec_asset(&tokens)?;
        let dest_asset = find_asset(&tokens, &self.asset)?;

        // Open wallet and get the refund address.
        let (_, db_data) = get_db_paths(wallet_dir.as_ref());
        let db_data = WalletDb::for_path(db_data, params, SystemClock, OsRng)?;
        let account = select_account(&db_data, self.account_id)?;
        let (ua, _) = account
            .uivk()
            .default_address(UnifiedAddressRequest::AllAvailableKeys)?;
        let refund_address = ua.encode(&params);

        // Compute deadline (2 hours from now).
        let deadline = chrono::Utc::now() + chrono::Duration::hours(2);
        let deadline_str = deadline.to_rfc3339();

        // Convert slippage from percent to basis points.
        let slippage_bps = self.slippage * 100.0;

        // Request a quote.
        let quote_request = near_api::build_quote_request(
            zec_asset.asset_id(),
            dest_asset.asset_id(),
            self.value.to_string(),
            refund_address.clone(),
            self.address.clone(),
            deadline_str,
            slippage_bps,
            None,
        );

        println!("Requesting swap quote...");
        let quote = near.get_quote(quote_request).await?;

        // Display quote details.
        println!();
        println!("=== Swap Quote ===");
        println!(
            "  Send:    {} ZEC ({} zatoshis)",
            format_amount(&quote.amount_in, zec_asset.decimals()),
            quote.amount_in,
        );
        if !quote.amount_in_usd.is_empty() {
            println!("           (~${} USD)", quote.amount_in_usd);
        }
        println!(
            "  Receive: {} {} ({})",
            format_amount(&quote.amount_out, dest_asset.decimals()),
            dest_asset.symbol(),
            dest_asset.display_id(),
        );
        if !quote.amount_out_usd.is_empty() {
            println!("           (~${} USD)", quote.amount_out_usd);
        }
        if let Some(estimate) = quote.time_estimate {
            println!("  Estimated time: {estimate} seconds");
        }
        println!("  Deposit address: {}", quote.deposit_address);
        println!();

        // Ask for confirmation.
        print!("Proceed with swap? [y/n]: ");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut buffer = String::new();
        std::io::stdin().read_line(&mut buffer)?;
        if buffer.trim() != "y" {
            println!("Swap cancelled.");
            return Ok(());
        }

        // Decrypt the mnemonic to access the seed.
        let identities =
            age::IdentityFile::from_file(self.identity.clone())?.into_identities()?;
        let seed = config
            .decrypt_seed(identities.iter().map(|i| i.as_ref() as _))?
            .ok_or(anyhow!("Seed must be present to enable sending"))?;

        let derivation = account
            .source()
            .key_derivation()
            .ok_or(anyhow!("Cannot spend from view-only accounts"))?;

        let usk = UnifiedSpendingKey::from_seed(
            &params,
            seed.expose_secret(),
            derivation.account_index(),
        )
        .map_err(error::Error::from)?;

        // Need a mutable db_data for proposing and creating transactions.
        let (_, db_data_path) = get_db_paths(wallet_dir.as_ref());
        let mut db_data = WalletDb::for_path(db_data_path, params, SystemClock, OsRng)?;

        // Build a ZEC payment to the deposit address.
        let deposit_zcash_address = ZcashAddress::from_str(&quote.deposit_address)
            .map_err(|_| anyhow!("Deposit address '{}' is not a valid Zcash address", quote.deposit_address))?;

        let zatoshis =
            Zatoshis::from_u64(self.value).map_err(|_| error::Error::InvalidAmount)?;

        let payment = Payment::new(
            deposit_zcash_address,
            Some(zatoshis),
            None,
            None,
            None,
            vec![],
        )
        .ok_or_else(|| anyhow!("Failed to construct payment"))?;
        let request = TransactionRequest::new(vec![payment]).map_err(error::Error::from)?;

        // Connect to lightwalletd.
        let mut client = self
            .connection
            .connect(params, wallet_dir.as_ref())
            .await?;

        // Create the transaction.
        println!("Creating transaction...");
        let prover = LocalTxProver::bundled();
        let change_strategy = MultiOutputChangeStrategy::new(
            StandardFeeRule::Zip317,
            None,
            ShieldedProtocol::Orchard,
            DustOutputPolicy::default(),
            SplitPolicy::with_min_output_value(
                NonZeroUsize::new(self.target_note_count)
                    .ok_or(anyhow!("target note count must be nonzero"))?,
                Zatoshis::from_u64(self.min_split_output_value)?,
            ),
        );
        let input_selector = GreedyInputSelector::new();

        let proposal = propose_transfer(
            &mut db_data,
            &params,
            account.id(),
            &input_selector,
            &change_strategy,
            request,
            ConfirmationsPolicy::default(),
        )
        .map_err(error::Error::from)?;

        let txids = create_proposed_transactions(
            &mut db_data,
            &params,
            &prover,
            &prover,
            &SpendingKeys::from_unified_spending_key(usk),
            OvkPolicy::Sender,
            &proposal,
        )
        .map_err(error::Error::from)?;

        if txids.len() > 1 {
            return Err(anyhow!(
                "Multi-transaction proposals are not yet supported."
            ));
        }

        let txid = *txids.first();

        // Send the transaction.
        println!("Sending transaction...");
        let (txid, raw_tx) = db_data
            .get_transaction(txid)?
            .map(|tx| {
                let mut raw_tx = service::RawTransaction::default();
                tx.write(&mut raw_tx.data).unwrap();
                (tx.txid(), raw_tx)
            })
            .ok_or(anyhow!("Transaction not found for id {:?}", txid))?;
        let response = client.send_transaction(raw_tx).await?.into_inner();

        if response.error_code != 0 {
            return Err(error::Error::SendFailed {
                code: response.error_code,
                reason: response.error_message,
            }
            .into());
        }

        println!("Transaction sent: {txid}");

        // Submit the deposit to speed up processing.
        println!("Submitting deposit notification...");
        match near
            .submit_deposit(&txid.to_string(), &quote.deposit_address)
            .await
        {
            Ok(()) => println!("Deposit submitted successfully."),
            Err(e) => println!("Warning: failed to submit deposit notification: {e}"),
        }

        // Check status.
        match near.check_status(&quote.deposit_address).await {
            Ok(status) => {
                println!("Swap status: {}", status.status);
            }
            Err(e) => {
                println!("Warning: could not check swap status: {e}");
            }
        }

        println!(
            "\nYou can check the swap status later with the deposit address: {}",
            quote.deposit_address
        );

        Ok(())
    }
}
