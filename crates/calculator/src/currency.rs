use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use rust_decimal::Decimal;
use thiserror::Error;

/// Validated uppercase fiat or crypto ticker.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AssetCode(String);

impl AssetCode {
    /// Parse a 2–12 character ASCII ticker such as `USD`, `EUR`, or `BTC`.
    ///
    /// # Errors
    ///
    /// Returns [`ExchangeError::InvalidCode`] for punctuation or invalid length.
    pub fn parse(value: &str) -> Result<Self, ExchangeError> {
        let value = value.trim().to_ascii_uppercase();
        if (2..=12).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            Ok(Self(value))
        } else {
            Err(ExchangeError::InvalidCode)
        }
    }

    /// Canonical uppercase ticker.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A parsed inline conversion such as `100 USD to EUR`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CurrencyQuery {
    /// Decimal amount to convert.
    pub amount: Decimal,
    /// Source asset.
    pub from: AssetCode,
    /// Destination asset.
    pub to: AssetCode,
}

impl CurrencyQuery {
    /// Parse common conversion phrasing without guessing bare arithmetic.
    ///
    /// Accepts both `10 USD in INR` and `USD 10 in INR`, matching the two
    /// word orders people naturally use in launchers.
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        let fields = input.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 4 || !matches!(fields[2].to_ascii_lowercase().as_str(), "to" | "in") {
            return None;
        }
        let amount_first = Decimal::from_str(&fields[0].replace(',', ""));
        let (amount, from) = match amount_first {
            Ok(amount) => (amount, fields[1]),
            Err(_) => (
                Decimal::from_str(&fields[1].replace(',', "")).ok()?,
                fields[0],
            ),
        };
        Some(Self {
            amount,
            from: AssetCode::parse(from).ok()?,
            to: AssetCode::parse(fields[3]).ok()?,
        })
    }
}

impl fmt::Display for AssetCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Timestamped decimal exchange-rate snapshot with an arbitrary base asset.
#[derive(Clone, Debug)]
pub struct ExchangeRates {
    base: AssetCode,
    observed_at: i64,
    quotes: HashMap<AssetCode, Decimal>,
}

impl ExchangeRates {
    /// Create a rate snapshot. Each quote is units of that asset per one base asset.
    #[must_use]
    pub fn new(base: AssetCode, observed_at: i64) -> Self {
        let mut quotes = HashMap::new();
        quotes.insert(base.clone(), Decimal::ONE);
        Self {
            base,
            observed_at,
            quotes,
        }
    }

    /// Insert a positive decimal quote without binary floating-point rounding.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid decimal text or non-positive rates.
    pub fn set_rate(
        &mut self,
        asset: AssetCode,
        units_per_base: &str,
    ) -> Result<(), ExchangeError> {
        let rate = Decimal::from_str(units_per_base).map_err(|_| ExchangeError::InvalidRate)?;
        if rate <= Decimal::ZERO {
            return Err(ExchangeError::InvalidRate);
        }
        self.quotes.insert(asset, rate);
        Ok(())
    }

    /// Convert between any two assets present in the same snapshot.
    ///
    /// # Errors
    ///
    /// Returns missing-rate or decimal-overflow failures.
    pub fn convert(
        &self,
        amount: Decimal,
        from: &AssetCode,
        to: &AssetCode,
    ) -> Result<Decimal, ExchangeError> {
        let from_rate = self.quotes.get(from).ok_or(ExchangeError::MissingRate)?;
        let to_rate = self.quotes.get(to).ok_or(ExchangeError::MissingRate)?;
        amount
            .checked_div(*from_rate)
            .and_then(|base_amount| base_amount.checked_mul(*to_rate))
            .ok_or(ExchangeError::Overflow)
    }

    /// Snapshot base asset.
    #[must_use]
    pub fn base(&self) -> &AssetCode {
        &self.base
    }

    /// Unix milliseconds supplied by the rate provider.
    #[must_use]
    pub const fn observed_at(&self) -> i64 {
        self.observed_at
    }

    /// Whether this snapshot is older than the caller's accepted age.
    #[must_use]
    pub fn is_stale(&self, now_millis: i64, maximum_age_millis: i64) -> bool {
        now_millis.saturating_sub(self.observed_at) > maximum_age_millis.max(0)
    }
}

/// Exchange-rate validation and arithmetic failures.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ExchangeError {
    /// Asset ticker has an invalid shape.
    #[error("currency or crypto code is invalid")]
    InvalidCode,
    /// Quote is not a positive decimal.
    #[error("exchange rate is invalid")]
    InvalidRate,
    /// Requested asset is absent from the snapshot.
    #[error("exchange rate is unavailable")]
    MissingRate,
    /// Fixed-precision result exceeded supported range.
    #[error("currency conversion exceeds supported range")]
    Overflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_fiat_and_crypto_through_decimal_base_rates() {
        let usd = AssetCode::parse("usd").expect("USD");
        let eur = AssetCode::parse("EUR").expect("EUR");
        let btc = AssetCode::parse("btc").expect("BTC");
        let mut rates = ExchangeRates::new(usd.clone(), 1_000);
        rates.set_rate(eur.clone(), "0.92").expect("EUR rate");
        rates.set_rate(btc.clone(), "0.00001").expect("BTC rate");
        assert_eq!(
            rates
                .convert(Decimal::new(100, 0), &usd, &eur)
                .expect("USD to EUR"),
            Decimal::new(92, 0)
        );
        assert_eq!(
            rates
                .convert(Decimal::new(5, 1), &btc, &usd)
                .expect("BTC to USD"),
            Decimal::new(50_000, 0)
        );
        assert!(!rates.is_stale(1_500, 1_000));
        assert!(rates.is_stale(2_001, 1_000));
    }

    #[test]
    fn rejects_invalid_codes_rates_and_missing_assets() {
        assert!(AssetCode::parse("$").is_err());
        let usd = AssetCode::parse("USD").expect("USD");
        let mut rates = ExchangeRates::new(usd.clone(), 0);
        assert!(
            rates
                .set_rate(AssetCode::parse("EUR").expect("EUR"), "0")
                .is_err()
        );
        assert!(
            rates
                .convert(Decimal::ONE, &usd, &AssetCode::parse("JPY").expect("JPY"))
                .is_err()
        );
    }

    #[test]
    fn parses_currency_queries_without_claiming_other_input() {
        let query = CurrencyQuery::parse("1,250.50 usd to EUR").expect("currency query");
        assert_eq!(query.amount, Decimal::new(125_050, 2));
        assert_eq!(query.from.as_str(), "USD");
        assert_eq!(query.to.as_str(), "EUR");

        let natural_order = CurrencyQuery::parse("usd 10 in inr").expect("currency query");
        assert_eq!(natural_order.amount, Decimal::TEN);
        assert_eq!(natural_order.from.as_str(), "USD");
        assert_eq!(natural_order.to.as_str(), "INR");

        assert_eq!(CurrencyQuery::parse("2 + 2"), None);
        assert_eq!(CurrencyQuery::parse("USD to EUR"), None);
        assert_eq!(CurrencyQuery::parse("USD ten in INR"), None);
    }
}
