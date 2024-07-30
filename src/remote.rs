use std::{borrow::Cow, fmt, path::Path};

use anyhow::anyhow;
use tonic::transport::{Channel, ClientTlsConfig};

use tracing::info;
use zcash_client_backend::{
    proto::service::compact_tx_streamer_client::CompactTxStreamerClient, tor,
};
use zcash_protocol::consensus::Network;

use crate::data::get_tor_dir;

const ECC_TESTNET: &[Server<'_>] = &[Server::fixed("lightwalletd.testnet.electriccoin.co", 9067)];

const YWALLET_MAINNET: &[Server<'_>] = &[
    Server::fixed("lwd1.zcash-infra.com", 9067),
    Server::fixed("lwd2.zcash-infra.com", 9067),
    Server::fixed("lwd3.zcash-infra.com", 9067),
    Server::fixed("lwd4.zcash-infra.com", 9067),
    Server::fixed("lwd5.zcash-infra.com", 9067),
    Server::fixed("lwd6.zcash-infra.com", 9067),
    Server::fixed("lwd7.zcash-infra.com", 9067),
    Server::fixed("lwd8.zcash-infra.com", 9067),
];

const ZEC_ROCKS_MAINNET: &[Server<'_>] = &[
    Server::fixed("zec.rocks", 443),
    Server::fixed("ap.zec.rocks", 443),
    Server::fixed("eu.zec.rocks", 443),
    Server::fixed("na.zec.rocks", 443),
    Server::fixed("sa.zec.rocks", 443),
];
const ZEC_ROCKS_TESTNET: &[Server<'_>] = &[Server::fixed("testnet.zec.rocks", 443)];

#[derive(Debug)]
pub(crate) enum ServerOperator {
    Ecc,
    YWallet,
    ZecRocks,
}

impl ServerOperator {
    fn servers(&self, network: Network) -> &[Server<'_>] {
        match (self, network) {
            (ServerOperator::Ecc, Network::MainNetwork) => &[],
            (ServerOperator::Ecc, Network::TestNetwork) => ECC_TESTNET,
            (ServerOperator::YWallet, Network::MainNetwork) => YWALLET_MAINNET,
            (ServerOperator::YWallet, Network::TestNetwork) => &[],
            (ServerOperator::ZecRocks, Network::MainNetwork) => ZEC_ROCKS_MAINNET,
            (ServerOperator::ZecRocks, Network::TestNetwork) => ZEC_ROCKS_TESTNET,
        }
    }
}

#[derive(Debug)]
pub(crate) enum Servers {
    Hosted(ServerOperator),
    Custom(Vec<Server<'static>>),
}

impl Servers {
    pub(crate) fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "ecc" => Ok(Self::Hosted(ServerOperator::Ecc)),
            "ywallet" => Ok(Self::Hosted(ServerOperator::YWallet)),
            "zecrocks" => Ok(Self::Hosted(ServerOperator::ZecRocks)),
            _ => s
                .split(',')
                .map(|sub| {
                    sub.rsplit_once(':').and_then(|(host, port_str)| {
                        port_str
                            .parse()
                            .ok()
                            .map(|port| Server::custom(host.into(), port))
                    })
                })
                .collect::<Option<_>>()
                .map(Self::Custom)
                .ok_or(anyhow!("'{}' must be one of ['ecc', 'ywallet', 'zecrocks'], or a comma-separated list of host:port", s)),
        }
    }

    pub(crate) fn pick(&self, network: Network) -> anyhow::Result<&Server<'_>> {
        // For now just use the first server in the list.
        match self {
            Servers::Hosted(server_operator) => server_operator
                .servers(network)
                .first()
                .ok_or(anyhow!("{:?} doesn't serve {:?}", server_operator, network)),
            Servers::Custom(servers) => Ok(servers.first().expect("not empty")),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Server<'a> {
    host: Cow<'a, str>,
    port: u16,
}

impl<'a> fmt::Display for Server<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

impl Server<'static> {
    const fn fixed(host: &'static str, port: u16) -> Self {
        Self {
            host: Cow::Borrowed(host),
            port,
        }
    }
}

impl<'a> Server<'a> {
    fn custom(host: String, port: u16) -> Self {
        Self {
            host: Cow::Owned(host),
            port,
        }
    }

    fn use_tls(&self) -> bool {
        // Assume that localhost will never have a cert, and require remotes to have one.
        !matches!(self.host.as_ref(), "localhost" | "127.0.0.1" | "::1")
    }

    fn endpoint(&self) -> String {
        format!(
            "{}://{}:{}",
            if self.use_tls() { "https" } else { "http" },
            self.host,
            self.port
        )
    }
}

pub(crate) async fn connect_to_lightwalletd(
    server: &Server<'_>,
) -> Result<CompactTxStreamerClient<Channel>, anyhow::Error> {
    info!("Connecting to {}", server);

    let channel = Channel::from_shared(server.endpoint())?;

    let channel = if server.use_tls() {
        let tls = ClientTlsConfig::new()
            .domain_name(server.host.to_string())
            .with_webpki_roots();
        channel.tls_config(tls)?
    } else {
        channel
    };

    Ok(CompactTxStreamerClient::new(channel.connect().await?))
}

pub(crate) async fn tor_client<P: AsRef<Path>>(
    wallet_dir: Option<P>,
) -> anyhow::Result<tor::Client> {
    let tor_dir = get_tor_dir(wallet_dir);

    // Ensure Tor directory exists.
    tokio::fs::create_dir_all(&tor_dir).await?;

    Ok(tor::Client::create(&tor_dir).await?)
}
