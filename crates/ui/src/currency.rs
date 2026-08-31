use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use superspace_calculator::{CurrencyQuery, ExchangeRates};

const FRESH_FOR_MS: i64 = 15 * 60 * 1_000;
static CACHE_WRITE: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug)]
pub(crate) struct Conversion {
    pub(crate) query: CurrencyQuery,
    pub(crate) value: Decimal,
    pub(crate) observed_at_ms: i64,
    pub(crate) cached: bool,
}

impl Conversion {
    pub(crate) fn display_value(&self) -> String {
        format_decimal(self.value)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CachedRate {
    rate: String,
    observed_at_ms: i64,
}

#[derive(Default, Deserialize, Serialize)]
struct RateCache {
    rates: HashMap<String, CachedRate>,
}

#[derive(Deserialize)]
struct CoinbaseResponse {
    data: CoinbaseRates,
}

#[derive(Deserialize)]
struct CoinbaseRates {
    rates: HashMap<String, String>,
}

/// Resolve a current conversion, falling back to the most recent durable rate offline.
pub(crate) fn convert(query: CurrencyQuery, data_root: &Path) -> Result<Conversion, String> {
    let cache_path = data_root.join("currency-rates.json");
    let cache = read_cache(&cache_path);
    let key = format!("{}:{}", query.from, query.to);
    let now = now_millis();
    if let Some(rate) = cache.rates.get(&key)
        && now.saturating_sub(rate.observed_at_ms) <= FRESH_FOR_MS
    {
        return apply_rate(query, rate, true);
    }

    match fetch_rate(&query) {
        Ok(rate) => {
            let rate = CachedRate {
                rate,
                observed_at_ms: now,
            };
            let conversion = apply_rate(query, &rate, false)?;
            save_rate(data_root, &cache_path, key, rate);
            Ok(conversion)
        }
        Err(network_error) => cache
            .rates
            .get(&key)
            .map_or_else(|| Err(network_error), |rate| apply_rate(query, rate, true)),
    }
}

fn save_rate(data_root: &Path, cache_path: &Path, key: String, rate: CachedRate) {
    let Ok(_guard) = CACHE_WRITE.lock() else {
        return;
    };
    let mut cache = read_cache(cache_path);
    cache.rates.insert(key, rate);
    let Ok(encoded) = serde_json::to_vec_pretty(&cache) else {
        return;
    };
    if fs::create_dir_all(data_root).is_err() {
        return;
    }
    let temporary = cache_path.with_extension(format!("{}.tmp", std::process::id()));
    if fs::write(&temporary, encoded).is_ok() {
        let _ = fs::rename(temporary, cache_path);
    }
}

fn fetch_rate(query: &CurrencyQuery) -> Result<String, String> {
    if query.from == query.to {
        return Ok("1".into());
    }
    let url = format!(
        "https://api.coinbase.com/v2/exchange-rates?currency={}",
        query.from
    );
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .build()
        .new_agent();
    let mut response = agent
        .get(&url)
        .header("Accept", "application/json")
        .call()
        .map_err(|error| format!("live exchange rates are unavailable: {error}"))?;
    let response: CoinbaseResponse = response
        .body_mut()
        .read_json()
        .map_err(|error| format!("exchange-rate response was invalid: {error}"))?;
    response
        .data
        .rates
        .get(query.to.as_str())
        .cloned()
        .ok_or_else(|| format!("{} is not available from the rate provider", query.to))
}

fn apply_rate(
    query: CurrencyQuery,
    cached_rate: &CachedRate,
    cached: bool,
) -> Result<Conversion, String> {
    let mut rates = ExchangeRates::new(query.from.clone(), cached_rate.observed_at_ms);
    rates
        .set_rate(query.to.clone(), &cached_rate.rate)
        .map_err(|error| error.to_string())?;
    let value = rates
        .convert(query.amount, &query.from, &query.to)
        .map_err(|error| error.to_string())?;
    Ok(Conversion {
        query,
        value,
        observed_at_ms: cached_rate.observed_at_ms,
        cached,
    })
}

fn read_cache(path: &Path) -> RateCache {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

fn format_decimal(value: Decimal) -> String {
    value.round_dp(8).normalize().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use superspace_calculator::CurrencyQuery;

    #[test]
    fn applies_cached_decimal_rates_without_float_rounding() {
        let query = CurrencyQuery::parse("12.50 USD to EUR").expect("query");
        let result = apply_rate(
            query,
            &CachedRate {
                rate: "0.9234".into(),
                observed_at_ms: 42,
            },
            true,
        )
        .expect("conversion");
        assert_eq!(result.display_value(), "11.5425");
        assert!(result.cached);
    }
}
