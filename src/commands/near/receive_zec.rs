use anyhow::anyhow;
use clap::Args;
use rand::rngs::OsRng;
use rust_decimal::Decimal;
use uuid::Uuid;

use zcash_client_backend::data_api::Account;
use zcash_client_sqlite::{util::SystemClock, WalletDb};
use zcash_keys::keys::UnifiedAddressRequest;

use crate::{
    commands::select_account,
    config::WalletConfig,
    data::get_db_paths,
    near_api::{self, format_amount, find_asset, find_zec_asset, NearClient},
};

#[derive(Debug, Args)]
pub(crate) struct Command {
    /// The UUID of the wallet account to receive ZEC into
    account_id: Option<Uuid>,

    /// The source asset to swap from (e.g. "eth.eth", "btc.btc", "sol.usdc")
    #[arg(long)]
    asset: String,

    /// The amount of the source token to send (as decimal string, e.g. "0.1")
    #[arg(long)]
    amount: String,

    /// Refund address on the source chain
    #[arg(long)]
    refund_address: String,

    /// Slippage tolerance in percent (default 1.0)
    #[arg(long, default_value = "1.0")]
    slippage: f64,

    /// Disable the affiliate fee
    #[arg(long)]
    no_affiliate_fee: bool,
}

impl Command {
    pub(crate) async fn run(self, wallet_dir: Option<String>) -> Result<(), anyhow::Error> {
        let config = WalletConfig::read(wallet_dir.as_ref())?;
        let params = config.network();

        // Set up the NEAR API client.
        let api_key = config.near_api_key().map(|s| s.to_string());
        let near = NearClient::new(api_key);

        // Open wallet and get the receiving address.
        let (_, db_data) = get_db_paths(wallet_dir.as_ref());
        let db_data = WalletDb::for_path(db_data, params, SystemClock, OsRng)?;
        let account = select_account(&db_data, self.account_id)?;
        let (ua, _) = account
            .uivk()
            .default_address(UnifiedAddressRequest::AllAvailableKeys)?;
        let recipient_address = ua.encode(&params);

        // Fetch available tokens and resolve assets.
        println!("Fetching available swap assets...");
        let tokens = near.fetch_tokens().await?;
        let zec_asset = find_zec_asset(&tokens)?;
        let source_asset = find_asset(&tokens, &self.asset)?;

        // Convert the human-readable amount to smallest units.
        let amount_decimal: Decimal = self
            .amount
            .parse()
            .map_err(|_| anyhow!("Invalid amount '{}'", self.amount))?;
        let multiplier = Decimal::from(10u64.pow(source_asset.decimals()));
        let amount_smallest = (amount_decimal * multiplier)
            .to_string()
            .split('.')
            .next()
            .unwrap()
            .to_string();

        // Compute deadline (2 hours from now).
        let deadline = chrono::Utc::now() + chrono::Duration::hours(2);
        let deadline_str = deadline.to_rfc3339();

        // Convert slippage from percent to basis points.
        let slippage_bps = self.slippage * 100.0;

        // Request a quote.
        let quote_request = near_api::build_quote_request(
            source_asset.asset_id(),
            zec_asset.asset_id(),
            amount_smallest,
            self.refund_address.clone(),
            recipient_address.clone(),
            deadline_str.clone(),
            slippage_bps,
            None,
        );

        println!("Requesting swap quote...");
        let quote = near.get_quote(quote_request).await?;

        // Display results.
        println!();
        println!("=== Swap Quote ===");
        println!(
            "  Send:    {} {} ({})",
            format_amount(&quote.amount_in, source_asset.decimals()),
            source_asset.symbol(),
            source_asset.display_id(),
        );
        if !quote.amount_in_usd.is_empty() {
            println!("           (~${} USD)", quote.amount_in_usd);
        }
        println!(
            "  Receive: {} ZEC ({} zatoshis)",
            format_amount(&quote.amount_out, zec_asset.decimals()),
            quote.amount_out,
        );
        if !quote.amount_out_usd.is_empty() {
            println!("           (~${} USD)", quote.amount_out_usd);
        }
        if let Some(estimate) = quote.time_estimate {
            println!("  Estimated time: {estimate} seconds");
        }
        println!();
        println!("=== Deposit Instructions ===");
        println!("  Deposit address: {}", quote.deposit_address);
        println!(
            "  Send {} {} to the deposit address above.",
            format_amount(&quote.amount_in, source_asset.decimals()),
            source_asset.symbol(),
        );
        println!("  Deadline: {deadline_str}");
        println!(
            "  ZEC will be received at: {recipient_address}"
        );

        Ok(())
    }
}
