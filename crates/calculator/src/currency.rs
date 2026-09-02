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
        let (amount, from, to) = match fields.as_slice() {
            [value, separator, to]
                if matches!(separator.to_ascii_lowercase().as_str(), "to" | "in") =>
            {
                let (amount, from) = parse_compact_amount(value)?;
                (amount, from, *to)
            }
            [first, second, separator, to]
                if matches!(separator.to_ascii_lowercase().as_str(), "to" | "in") =>
            {
                let amount_first = parse_amount(first);
                match amount_first {
                    Some(amount) => (amount, *second, *to),
                    None => (parse_amount(second)?, *first, *to),
                }
            }
            _ => return None,
        };
        Some(Self {
            amount,
            from: parse_asset(from)?,
            to: parse_asset(to)?,
        })
    }
}

fn parse_amount(value: &str) -> Option<Decimal> {
    Decimal::from_str(&value.replace(',', "")).ok()
}

fn parse_asset(value: &str) -> Option<AssetCode> {
    let code = match value.trim().to_ascii_lowercase().as_str() {
        "$" | "usd" | "dollar" | "dollars" => "USD",
        "€" | "eur" | "euro" | "euros" => "EUR",
        "£" | "gbp" | "pound" | "pounds" => "GBP",
        "¥" | "jpy" | "yen" => "JPY",
        "₹" | "inr" | "rupee" | "rupees" => "INR",
        "₩" | "krw" | "won" => "KRW",
        "₽" | "rub" | "ruble" | "rubles" => "RUB",
        "₿" | "btc" | "bitcoin" => "BTC",
        _ => value,
    };
    AssetCode::parse(code).ok()
}

fn parse_compact_amount(value: &str) -> Option<(Decimal, &str)> {
    const SYMBOLS: &[char] = &['$', '€', '£', '¥', '₹', '₩', '₽', '₿'];
    if let Some(symbol) = value.chars().next().filter(|value| SYMBOLS.contains(value)) {
        return Some((
            parse_amount(&value[symbol.len_utf8()..])?,
            &value[..symbol.len_utf8()],
        ));
    }
    if let Some(symbol) = value
        .chars()
        .next_back()
        .filter(|value| SYMBOLS.contains(value))
    {
        let split = value.len() - symbol.len_utf8();
        return Some((parse_amount(&value[..split])?, &value[split..]));
    }
    let split = value
        .char_indices()
        .find_map(|(index, character)| character.is_ascii_digit().then_some(index))?;
    if split > 0 {
        return Some((parse_amount(&value[split..])?, &value[..split]));
    }
    let split = value
        .char_indices()
        .find_map(|(index, character)| character.is_ascii_alphabetic().then_some(index))?;
    if split == 0 {
        return None;
    }
    Some((parse_amount(&value[..split])?, &value[split..]))
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

        let symbol = CurrencyQuery::parse("$5 in inr").expect("symbol currency query");
        assert_eq!(symbol.amount, Decimal::from(5));
        assert_eq!(symbol.from.as_str(), "USD");
        assert_eq!(symbol.to.as_str(), "INR");

        let named = CurrencyQuery::parse("10 dollars to rupees").expect("named currency query");
        assert_eq!(named.from.as_str(), "USD");
        assert_eq!(named.to.as_str(), "INR");

        assert_eq!(CurrencyQuery::parse("2 + 2"), None);
        assert_eq!(CurrencyQuery::parse("USD to EUR"), None);
        assert_eq!(CurrencyQuery::parse("USD ten in INR"), None);
    }
}
