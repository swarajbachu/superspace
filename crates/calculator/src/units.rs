use std::collections::HashMap;

/// Physical dimension used to prevent invalid conversions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dimension {
    /// Distance.
    Length,
    /// Mass.
    Mass,
    /// Duration.
    Time,
    /// Digital information.
    Data,
    /// Plane angle.
    Angle,
}

/// Linear unit represented relative to its dimension's base unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Unit {
    /// Canonical short symbol.
    pub symbol: &'static str,
    /// Physical dimension.
    pub dimension: Dimension,
    /// Multiply by this value to reach the base unit.
    pub to_base: f64,
}

/// Case-insensitive unit aliases and conversion policy.
#[derive(Debug, Clone)]
pub struct UnitRegistry {
    aliases: HashMap<&'static str, Unit>,
}

impl Default for UnitRegistry {
    fn default() -> Self {
        let mut registry = Self {
            aliases: HashMap::new(),
        };
        registry.add(
            Dimension::Length,
            "m",
            1.0,
            &["meter", "meters", "metre", "metres"],
        );
        registry.add(
            Dimension::Length,
            "km",
            1_000.0,
            &["kilometer", "kilometers"],
        );
        registry.add(
            Dimension::Length,
            "cm",
            0.01,
            &["centimeter", "centimeters"],
        );
        registry.add(
            Dimension::Length,
            "mm",
            0.001,
            &["millimeter", "millimeters"],
        );
        registry.add(Dimension::Length, "in", 0.0254, &["inch", "inches"]);
        registry.add(Dimension::Length, "ft", 0.3048, &["foot", "feet"]);
        registry.add(Dimension::Length, "yd", 0.9144, &["yard", "yards"]);
        registry.add(Dimension::Length, "mi", 1_609.344, &["mile", "miles"]);

        registry.add(Dimension::Mass, "kg", 1.0, &["kilogram", "kilograms"]);
        registry.add(Dimension::Mass, "g", 0.001, &["gram", "grams"]);
        registry.add(
            Dimension::Mass,
            "mg",
            0.000_001,
            &["milligram", "milligrams"],
        );
        registry.add(
            Dimension::Mass,
            "lb",
            0.453_592_37,
            &["pound", "pounds", "lbs"],
        );
        registry.add(
            Dimension::Mass,
            "oz",
            0.028_349_523_125,
            &["ounce", "ounces"],
        );

        registry.add(Dimension::Time, "s", 1.0, &["sec", "second", "seconds"]);
        registry.add(Dimension::Time, "min", 60.0, &["minute", "minutes"]);
        registry.add(Dimension::Time, "h", 3_600.0, &["hr", "hour", "hours"]);
        registry.add(Dimension::Time, "day", 86_400.0, &["days"]);
        registry.add(Dimension::Time, "week", 604_800.0, &["weeks"]);

        registry.add(Dimension::Data, "B", 1.0, &["byte", "bytes"]);
        registry.add(Dimension::Data, "KB", 1_000.0, &["kilobyte", "kilobytes"]);
        registry.add(
            Dimension::Data,
            "MB",
            1_000_000.0,
            &["megabyte", "megabytes"],
        );
        registry.add(
            Dimension::Data,
            "GB",
            1_000_000_000.0,
            &["gigabyte", "gigabytes"],
        );
        registry.add(Dimension::Data, "KiB", 1_024.0, &["kibibyte", "kibibytes"]);
        registry.add(
            Dimension::Data,
            "MiB",
            1_048_576.0,
            &["mebibyte", "mebibytes"],
        );
        registry.add(
            Dimension::Data,
            "GiB",
            1_073_741_824.0,
            &["gibibyte", "gibibytes"],
        );

        registry.add(Dimension::Angle, "rad", 1.0, &["radian", "radians"]);
        registry.add(
            Dimension::Angle,
            "deg",
            std::f64::consts::PI / 180.0,
            &["degree", "degrees"],
        );
        registry
    }
}

impl UnitRegistry {
    fn add(
        &mut self,
        dimension: Dimension,
        symbol: &'static str,
        to_base: f64,
        aliases: &[&'static str],
    ) {
        let unit = Unit {
            symbol,
            dimension,
            to_base,
        };
        self.aliases.insert(symbol, unit);
        for alias in aliases {
            self.aliases.insert(alias, unit);
        }
    }

    /// Resolve a symbol or noun without case sensitivity.
    #[must_use]
    pub fn resolve(&self, value: &str) -> Option<Unit> {
        self.aliases.get(value).copied().or_else(|| {
            self.aliases
                .iter()
                .find_map(|(alias, unit)| alias.eq_ignore_ascii_case(value).then_some(*unit))
        })
    }

    /// Convert a linear value between compatible units.
    #[must_use]
    pub fn convert(&self, value: f64, source: Unit, target: Unit) -> Option<f64> {
        (source.dimension == target.dimension).then_some(value * source.to_base / target.to_base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_across_aliases() {
        let units = UnitRegistry::default();
        let km = units.resolve("kilometers").expect("km");
        let miles = units.resolve("mi").expect("miles");
        let result = units.convert(5.0, km, miles).expect("compatible");
        assert!((result - 3.106_855).abs() < 0.000_001);
    }

    #[test]
    fn rejects_incompatible_dimensions() {
        let units = UnitRegistry::default();
        assert!(
            units
                .convert(
                    1.0,
                    units.resolve("kg").expect("kg"),
                    units.resolve("m").expect("m")
                )
                .is_none()
        );
    }
}
