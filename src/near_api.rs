use anyhow::anyhow;
use one_click_sdk_rs::{
    apis::{configuration::Configuration, one_click_api},
    models,
};

pub(crate) const ZEC_ASSET_ID: &str = "nep141:zec.omft.near";

/// A token from the NEAR 1Click token list, wrapping the SDK's `TokenResponse`.
pub(crate) struct SwapAsset {
    inner: models::TokenResponse,
}

impl SwapAsset {
    pub(crate) fn asset_id(&self) -> &str {
        &self.inner.asset_id
    }

    pub(crate) fn symbol(&self) -> &str {
        &self.inner.symbol
    }

    pub(crate) fn decimals(&self) -> u32 {
        self.inner.decimals as u32
    }

    /// Returns the `blockchain.symbol` identifier for display.
    pub(crate) fn display_id(&self) -> String {
        format!(
            "{}.{}",
            format!("{:?}", self.inner.blockchain).to_lowercase(),
            self.inner.symbol.to_lowercase()
        )
    }
}

/// A swap quote wrapping the SDK's `Quote` from a `QuoteResponse`.
pub(crate) struct SwapQuote {
    pub(crate) deposit_address: String,
    pub(crate) amount_in: String,
    pub(crate) amount_in_usd: String,
    pub(crate) amount_out: String,
    pub(crate) amount_out_usd: String,
    pub(crate) time_estimate: Option<f64>,
}

/// Swap execution status wrapping the SDK's `GetExecutionStatusResponse`.
pub(crate) struct SwapStatus {
    pub(crate) status: String,
}

pub(crate) struct NearClient {
    config: Configuration,
}

impl NearClient {
    pub(crate) fn new(api_key: Option<String>) -> Self {
        let mut config = Configuration::new();
        if let Some(key) = api_key {
            config.bearer_access_token = Some(key);
        }
        Self { config }
    }

    pub(crate) async fn fetch_tokens(&self) -> Result<Vec<SwapAsset>, anyhow::Error> {
        let tokens = one_click_api::get_tokens(&self.config)
            .await
            .map_err(|e| anyhow!("Failed to fetch tokens: {e}"))?;
        Ok(tokens.into_iter().map(|t| SwapAsset { inner: t }).collect())
    }

    pub(crate) async fn get_quote(
        &self,
        request: models::QuoteRequest,
    ) -> Result<SwapQuote, anyhow::Error> {
        let resp = one_click_api::get_quote(&self.config, request)
            .await
            .map_err(|e| anyhow!("Failed to get quote: {e}"))?;

        let quote = resp.quote;
        Ok(SwapQuote {
            deposit_address: quote.deposit_address.unwrap_or_default(),
            amount_in: quote.amount_in,
            amount_in_usd: quote.amount_in_usd,
            amount_out: quote.amount_out,
            amount_out_usd: quote.amount_out_usd,
            time_estimate: quote.time_estimate,
        })
    }

    pub(crate) async fn submit_deposit(
        &self,
        tx_hash: &str,
        deposit_address: &str,
    ) -> Result<(), anyhow::Error> {
        let request = models::SubmitDepositTxRequest::new(
            tx_hash.to_string(),
            deposit_address.to_string(),
        );
        one_click_api::submit_deposit_tx(&self.config, request)
            .await
            .map_err(|e| anyhow!("Failed to submit deposit: {e}"))?;
        Ok(())
    }

    pub(crate) async fn check_status(
        &self,
        deposit_address: &str,
    ) -> Result<SwapStatus, anyhow::Error> {
        let resp = one_click_api::get_execution_status(&self.config, deposit_address)
            .await
            .map_err(|e| anyhow!("Failed to check status: {e}"))?;
        Ok(SwapStatus {
            status: format!("{:?}", resp.status),
        })
    }
}

/// Find the ZEC asset in a list of swap assets.
pub(crate) fn find_zec_asset(assets: &[SwapAsset]) -> Result<&SwapAsset, anyhow::Error> {
    assets
        .iter()
        .find(|a| a.asset_id() == ZEC_ASSET_ID)
        .ok_or_else(|| anyhow!("ZEC asset not found in NEAR token list"))
}

/// Find an asset by `blockchain.symbol` query (case-insensitive).
///
/// The query format is `blockchain.symbol`, e.g. "eth.eth", "btc.btc", "sol.usdc".
pub(crate) fn find_asset<'a>(
    assets: &'a [SwapAsset],
    query: &str,
) -> Result<&'a SwapAsset, anyhow::Error> {
    let query_lower = query.to_lowercase();
    let (chain, sym) = query_lower.split_once('.').ok_or_else(|| {
        anyhow!("Asset query must be in 'blockchain.symbol' format (e.g. eth.eth, btc.btc)")
    })?;

    assets
        .iter()
        .find(|a| {
            format!("{:?}", a.inner.blockchain).to_lowercase() == chain
                && a.inner.symbol.to_lowercase() == sym
        })
        .ok_or_else(|| {
            anyhow!(
                "Asset '{query}' not found. Use 'blockchain.symbol' format (e.g. eth.eth, btc.btc, sol.usdc)"
            )
        })
}

/// Format a token amount from smallest units to a human-readable decimal string.
pub(crate) fn format_amount(amount_str: &str, decimals: u32) -> String {
    if decimals == 0 {
        return amount_str.to_string();
    }

    let amount_str = amount_str.trim();
    let is_negative = amount_str.starts_with('-');
    let digits = if is_negative {
        &amount_str[1..]
    } else {
        amount_str
    };

    let dec = decimals as usize;
    let padded = if digits.len() <= dec {
        format!("{:0>width$}", digits, width = dec + 1)
    } else {
        digits.to_string()
    };

    let split_point = padded.len() - dec;
    let integer_part = &padded[..split_point];
    let fractional_part = padded[split_point..].trim_end_matches('0');

    let prefix = if is_negative { "-" } else { "" };
    if fractional_part.is_empty() {
        format!("{prefix}{integer_part}")
    } else {
        format!("{prefix}{integer_part}.{fractional_part}")
    }
}

/// Helper to build a `QuoteRequest` with common defaults.
pub(crate) fn build_quote_request(
    origin_asset: &str,
    destination_asset: &str,
    amount: String,
    refund_to: String,
    recipient: String,
    deadline: String,
    slippage_bps: f64,
    app_fees: Option<Vec<models::AppFee>>,
) -> models::QuoteRequest {
    models::QuoteRequest {
        dry: false,
        swap_type: models::quote_request::SwapType::ExactInput,
        slippage_tolerance: slippage_bps,
        origin_asset: origin_asset.to_string(),
        deposit_type: models::quote_request::DepositType::OriginChain,
        destination_asset: destination_asset.to_string(),
        amount,
        refund_to,
        refund_type: models::quote_request::RefundType::OriginChain,
        recipient,
        recipient_type: models::quote_request::RecipientType::DestinationChain,
        deadline,
        referral: None,
        quote_waiting_time_ms: Some(10000.0),
        app_fees,
    }
}
