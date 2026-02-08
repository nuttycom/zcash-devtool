use clap::Subcommand;

pub(crate) mod pay;
pub(crate) mod receive_zec;

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Pay an address in a NEAR-supported currency by sending shielded ZEC
    Pay(pay::Command),
    /// Receive shielded ZEC by swapping from another currency
    ReceiveZec(receive_zec::Command),
}
