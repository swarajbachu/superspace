use std::path::Path;

/// Regional defaults used to complete shorthand conversions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocaleDefaults {
    /// ISO 4217 currency code inferred from the user's locale.
    pub currency: String,
    /// IANA time-zone name inferred from the operating system.
    pub time_zone: String,
}

/// Read the user's local currency and time zone without network location tracking.
#[must_use]
pub fn locale_defaults() -> LocaleDefaults {
    let locale = system_locale().unwrap_or_else(|| "en_US".into());
    LocaleDefaults {
        currency: currency_for_locale(&locale).into(),
        time_zone: system_time_zone().unwrap_or_else(|| "UTC".into()),
    }
}

fn system_locale() -> Option<String> {
    #[cfg(target_os = "macos")]
    if let Ok(output) = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleLocale"])
        .output()
        && output.status.success()
    {
        let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !value.is_empty() {
            return Some(value);
        }
    }
    ["LC_ALL", "LC_MONETARY", "LANG"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
}

fn system_time_zone() -> Option<String> {
    if let Ok(value) = std::env::var("TZ")
        && !value.trim().is_empty()
    {
        return Some(value.trim().trim_start_matches(':').to_owned());
    }
    let target = std::fs::read_link("/etc/localtime").ok()?;
    zone_from_path(&target)
}

fn zone_from_path(path: &Path) -> Option<String> {
    let value = path.to_string_lossy();
    ["/usr/share/zoneinfo/", "/var/db/timezone/zoneinfo/"]
        .into_iter()
        .find_map(|prefix| value.strip_prefix(prefix).map(str::to_owned))
}

fn currency_for_locale(locale: &str) -> &'static str {
    if let Some(currency) = locale
        .split("currency=")
        .nth(1)
        .and_then(|value| value.get(..3))
    {
        return known_currency(currency).unwrap_or("USD");
    }
    let base = locale.split(['.', '@']).next().unwrap_or(locale);
    let country = base
        .rsplit(['_', '-'])
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    match country.as_str() {
        "IN" => "INR",
        "GB" => "GBP",
        "JP" => "JPY",
        "CA" => "CAD",
        "AU" => "AUD",
        "CH" | "LI" => "CHF",
        "CN" => "CNY",
        "KR" => "KRW",
        "BR" => "BRL",
        "MX" => "MXN",
        "SG" => "SGD",
        "HK" => "HKD",
        "NZ" => "NZD",
        "AE" => "AED",
        "SA" => "SAR",
        "RU" => "RUB",
        "ZA" => "ZAR",
        "AT" | "BE" | "CY" | "DE" | "EE" | "ES" | "FI" | "FR" | "GR" | "HR" | "IE" | "IT"
        | "LT" | "LU" | "LV" | "MT" | "NL" | "PT" | "SI" | "SK" => "EUR",
        _ => "USD",
    }
}

fn known_currency(value: &str) -> Option<&'static str> {
    match value.to_ascii_uppercase().as_str() {
        "USD" => Some("USD"),
        "EUR" => Some("EUR"),
        "GBP" => Some("GBP"),
        "INR" => Some("INR"),
        "JPY" => Some("JPY"),
        "CAD" => Some("CAD"),
        "AUD" => Some("AUD"),
        "CHF" => Some("CHF"),
        "CNY" => Some("CNY"),
        "KRW" => Some("KRW"),
        "BRL" => Some("BRL"),
        "MXN" => Some("MXN"),
        "SGD" => Some("SGD"),
        "HKD" => Some("HKD"),
        "NZD" => Some("NZD"),
        "AED" => Some("AED"),
        "SAR" => Some("SAR"),
        "RUB" => Some("RUB"),
        "ZAR" => Some("ZAR"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regional_currency_mapping_handles_common_locale_shapes() {
        assert_eq!(currency_for_locale("en_IN"), "INR");
        assert_eq!(currency_for_locale("de-DE.UTF-8"), "EUR");
        assert_eq!(currency_for_locale("en_US@currency=GBP"), "GBP");
        assert_eq!(currency_for_locale("unknown"), "USD");
        assert_eq!(
            zone_from_path(Path::new("/var/db/timezone/zoneinfo/Asia/Kolkata")),
            Some("Asia/Kolkata".into())
        );
    }
}
