use std::path::PathBuf;

use anyhow::{anyhow, Context as _};
use bip0039::{English, Mnemonic};
use clap::Args;
use tracing_subscriber::filter::targets;
use zcash_client_backend::proto::service;
use zcash_protocol::consensus;
use zewif::ZewifWallet;
use zewif_zcashd::{migrate::migrate_to_zewif, BDBDump, ZcashdDump, ZcashdParser};
use zip32::fingerprint::SeedFingerprint;

use crate::remote::{tor_client, Servers};

//
// Options accepted for the `init-wallet-dat` command
#[derive(Debug, Args)]
pub(crate) struct Command {
    /// Flag indicating whether lenient parsing is allowed.
    ///
    /// Defaults to `false`. If set to `true`, parse errors will be reported to `stderr` but will
    /// not halt execution.
    #[arg(long, default_value = "false")]
    lenient: bool,

    /// The seed fingerprint to use to identify the desired wallet from a multi-wallet `wallet.dat`
    /// file.
    #[arg(long)]
    seed_fingerprint: Option<String>,

    /// The server to initialize with (default is \"ecc\")
    #[arg(short, long)]
    #[arg(default_value = "ecc", value_parser = Servers::parse)]
    server: Servers,

    /// Disable connections via TOR
    #[arg(long)]
    disable_tor: bool,

    /// The path to the zcashd `wallet.dat` file.
    path: PathBuf,
}

impl Command {
    pub(crate) async fn run(self, wallet_dir: Option<String>) -> Result<(), anyhow::Error> {
        let db_dump = BDBDump::from_file(&self.path).context("Parsing BerkeleyDB file")?;
        let zcashd_dump =
            ZcashdDump::from_bdb_dump(&db_dump, !self.lenient).context("Parsing Zcashd dump")?;
        let (zcashd_wallet, unparsed_keys) = ZcashdParser::parse_dump(&zcashd_dump, !self.lenient)
            .context("Parsing Zcashd wallet")?;

        let zewif = migrate_to_zewif(&zcashd_wallet).context("Migrating to Zewif")?;

        let matches_seed = |target_seed_fp: &SeedFingerprint, w: &ZewifWallet| {
            w.seed_material().iter().any(|s| match s {
                zewif::SeedMaterial::PreBIP39Seed(seed_bytes) => {
                    SeedFingerprint::from_seed(seed_bytes.as_slice()).as_ref()
                        == Some(target_seed_fp)
                }
                zewif::SeedMaterial::Bip39Mnemonic(phrase) => {
                    Mnemonic::<English>::from_phrase(phrase).map_or(false, |m| {
                        SeedFingerprint::from_seed(&m.to_seed("")).as_ref() == Some(target_seed_fp)
                    })
                }
            })
        };

        let target_seed_fp = self
            .seed_fingerprint
            .map(|seedfp| {
                Ok::<_, anyhow::Error>(SeedFingerprint::from_bytes(<[u8; 32]>::try_from(
                    &hex::decode(seedfp)?[..],
                )?))
            })
            .transpose()?;

        let wallet = if zewif.wallets().len() > 1 {
            if let Some(seedfp) = target_seed_fp {
                zewif
                    .wallets()
                    .values()
                    .find(|w| matches_seed(&seedfp, w))
                    .ok_or(anyhow!("No wallet found matching seed fingerprint."))
            } else {
                Err(anyhow!(
                    "At present, only importing a single wallet is supported."
                ))
            }
        } else {
            zewif
                .wallets()
                .values()
                .next()
                .filter(|w| {
                    target_seed_fp
                        .iter()
                        .all(|seed_fp| matches_seed(seed_fp, w))
                })
                .ok_or(anyhow!("No wallets found."))
        }?;

        let network = wallet.network();
        let params = match network {
            zewif::Network::Main => consensus::Network::MainNetwork,
            zewif::Network::Test => consensus::Network::TestNetwork,
            zewif::Network::Regtest => {
                return Err(anyhow!("Import of regtest wallets is not supported."));
            }
        };

        let server = self.server.pick(params)?;
        let mut client = if self.disable_tor {
            server.connect_direct().await?
        } else {
            server.connect(|| tor_client(wallet_dir.as_ref())).await?
        };

        // Get the current chain height (for the wallet's birthday and/or recover-until height).
        let chain_tip: u32 = client
            .get_latest_block(service::ChainSpec::default())
            .await?
            .into_inner()
            .height
            .try_into()
            .expect("block heights must fit into u32");


        Ok(())
    }
}
