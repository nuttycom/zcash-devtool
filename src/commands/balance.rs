use anyhow::anyhow;
use gumdrop::Options;

use zcash_client_backend::data_api::WalletRead;
use zcash_client_sqlite::WalletDb;

use crate::{
    data::{get_db_paths, get_wallet_network},
    error,
    ui::format_zec,
    MIN_CONFIRMATIONS,
};

// Options accepted for the `balance` command
#[derive(Debug, Options)]
pub(crate) struct Command {}

impl Command {
    pub(crate) fn run(self, wallet_dir: Option<String>) -> Result<(), anyhow::Error> {
        let params = get_wallet_network(wallet_dir.as_ref())?;

        let (_, db_data) = get_db_paths(wallet_dir);
        let db_data = WalletDb::for_path(db_data, params)?;
        let account_id = *db_data
            .get_account_ids()?
            .first()
            .ok_or(anyhow!("Wallet has no accounts"))?;

        let address = db_data
            .get_current_address(account_id)?
            .ok_or(error::Error::InvalidRecipient)?;

        if let Some(wallet_summary) = db_data.get_wallet_summary(MIN_CONFIRMATIONS.into())? {
            let balance = wallet_summary
                .account_balances()
                .get(&account_id)
                .ok_or_else(|| anyhow!("Missing account 0"))?;

            println!("{:#?}", wallet_summary);
            println!("{}", address.encode(&params));
            println!("     Height: {}", wallet_summary.chain_tip_height());
            if let Some(progress) = wallet_summary.scan_progress() {
                println!(
                    "     Synced: {:0.3}%",
                    (*progress.numerator() as f64) * 100f64 / (*progress.denominator() as f64)
                );
            }
            println!("    Balance: {}", format_zec(balance.total()));
            println!(
                "  Sapling Spendable: {}",
                format_zec(balance.sapling_balance().spendable_value())
            );
            println!(
                "  Orchard Spendable: {}",
                format_zec(balance.orchard_balance().spendable_value())
            );
            #[cfg(feature = "transparent-inputs")]
            println!("         Unshielded: {}", format_zec(balance.unshielded()));
        } else {
            println!("Insufficient information to build a wallet summary.");
        }

        Ok(())
    }
}
