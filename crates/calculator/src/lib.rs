//! Pure inline calculator and unit conversion engine.

mod expression;
mod units;

pub use expression::{Calculator, CalculatorError, ResultValue};
pub use units::{Dimension, Unit, UnitRegistry};
