use thiserror::Error;

use crate::{Unit, UnitRegistry};

/// Successful calculator output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResultValue {
    /// Dimensionless number.
    Number(f64),
    /// Converted value with its requested display unit.
    Quantity {
        /// Numeric value.
        value: f64,
        /// Target unit.
        unit: Unit,
    },
}

/// User-facing parse and evaluation failures.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum CalculatorError {
    /// Input contains no expression.
    #[error("enter a calculation")]
    Empty,
    /// Token could not be understood at its byte offset.
    #[error("unexpected input at byte {0}")]
    Unexpected(usize),
    /// Expression ended before an expected operand or parenthesis.
    #[error("the calculation is incomplete")]
    Incomplete,
    /// A named function or constant is unavailable.
    #[error("unknown name: {0}")]
    UnknownName(String),
    /// Division or modulo by zero.
    #[error("cannot divide by zero")]
    DivisionByZero,
    /// Unit name is not registered.
    #[error("unknown unit: {0}")]
    UnknownUnit(String),
    /// Source and target have different physical dimensions.
    #[error("incompatible units")]
    IncompatibleUnits,
    /// Result is not finite.
    #[error("the result is outside the supported numeric range")]
    NonFinite,
}

/// Pure calculator with an injected unit registry.
#[derive(Debug, Clone, Default)]
pub struct Calculator {
    units: UnitRegistry,
}

impl Calculator {
    /// Evaluate arithmetic or `<expression> <unit> to <unit>` conversion.
    ///
    /// # Errors
    ///
    /// Returns [`CalculatorError`] for malformed input, invalid operations, or incompatible units.
    pub fn evaluate(&self, input: &str) -> Result<ResultValue, CalculatorError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(CalculatorError::Empty);
        }
        if let Some((left, target_name)) = split_conversion(input) {
            let (expression, source_name) = split_source_unit(left)?;
            let value = Parser::new(expression).parse()?;
            let source = self
                .units
                .resolve(source_name)
                .ok_or_else(|| CalculatorError::UnknownUnit(source_name.into()))?;
            let target_name = target_name.trim();
            let target = self
                .units
                .resolve(target_name)
                .ok_or_else(|| CalculatorError::UnknownUnit(target_name.into()))?;
            let value = self
                .units
                .convert(value, source, target)
                .ok_or(CalculatorError::IncompatibleUnits)?;
            ensure_finite(value)?;
            return Ok(ResultValue::Quantity {
                value,
                unit: target,
            });
        }
        let value = Parser::new(input).parse()?;
        ensure_finite(value)?;
        Ok(ResultValue::Number(value))
    }
}

fn split_conversion(input: &str) -> Option<(&str, &str)> {
    let lowercase = input.to_ascii_lowercase();
    for connector in [" to ", " in "] {
        if let Some(index) = lowercase.rfind(connector) {
            return Some((&input[..index], &input[index + connector.len()..]));
        }
    }
    None
}

fn split_source_unit(input: &str) -> Result<(&str, &str), CalculatorError> {
    let trimmed = input.trim_end();
    let unit_start = trimmed
        .char_indices()
        .rev()
        .take_while(|(_, character)| character.is_alphabetic())
        .map(|(index, _)| index)
        .last()
        .ok_or(CalculatorError::Incomplete)?;
    let (expression, source) = trimmed.split_at(unit_start);
    if expression.trim().is_empty() {
        return Err(CalculatorError::Incomplete);
    }
    Ok((expression.trim(), source.trim()))
}

fn ensure_finite(value: f64) -> Result<(), CalculatorError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(CalculatorError::NonFinite)
    }
}

struct Parser<'a> {
    input: &'a str,
    offset: usize,
}

impl<'a> Parser<'a> {
    const fn new(input: &'a str) -> Self {
        Self { input, offset: 0 }
    }

    fn parse(mut self) -> Result<f64, CalculatorError> {
        let result = self.expression(0)?;
        self.whitespace();
        if self.offset != self.input.len() {
            return Err(CalculatorError::Unexpected(self.offset));
        }
        Ok(result)
    }

    fn expression(&mut self, minimum_precedence: u8) -> Result<f64, CalculatorError> {
        let mut left = self.prefix()?;
        loop {
            self.whitespace();
            let Some(operator) = self.peek() else { break };
            let (precedence, right_associative) = match operator {
                '+' | '-' => (1, false),
                '*' | '/' | '%' => (2, false),
                '^' => (3, true),
                _ => break,
            };
            if precedence < minimum_precedence {
                break;
            }
            self.bump();
            let right = self.expression(if right_associative {
                precedence
            } else {
                precedence + 1
            })?;
            left = match operator {
                '+' => left + right,
                '-' => left - right,
                '*' => left * right,
                '/' | '%' if right == 0.0 => return Err(CalculatorError::DivisionByZero),
                '/' => left / right,
                '%' => left % right,
                '^' => left.powf(right),
                _ => unreachable!(),
            };
        }
        Ok(left)
    }

    fn prefix(&mut self) -> Result<f64, CalculatorError> {
        self.whitespace();
        match self.peek() {
            Some('+') => {
                self.bump();
                self.prefix()
            }
            Some('-') => {
                self.bump();
                Ok(-self.prefix()?)
            }
            Some('(') => {
                self.bump();
                let value = self.expression(0)?;
                self.whitespace();
                if self.bump() != Some(')') {
                    return Err(CalculatorError::Incomplete);
                }
                Ok(value)
            }
            Some(character) if character.is_ascii_digit() || character == '.' => self.number(),
            Some(character) if character.is_alphabetic() => self.name(),
            Some(_) => Err(CalculatorError::Unexpected(self.offset)),
            None => Err(CalculatorError::Incomplete),
        }
    }

    fn number(&mut self) -> Result<f64, CalculatorError> {
        let start = self.offset;
        let mut exponent_allowed = true;
        while let Some(character) = self.peek() {
            if character.is_ascii_digit() || character == '.' || character == '_' {
                self.bump();
            } else if exponent_allowed && matches!(character, 'e' | 'E') {
                exponent_allowed = false;
                self.bump();
                if matches!(self.peek(), Some('+' | '-')) {
                    self.bump();
                }
            } else {
                break;
            }
        }
        self.input[start..self.offset]
            .replace('_', "")
            .parse()
            .map_err(|_| CalculatorError::Unexpected(start))
    }

    fn name(&mut self) -> Result<f64, CalculatorError> {
        let start = self.offset;
        while self.peek().is_some_and(char::is_alphabetic) {
            self.bump();
        }
        let name = self.input[start..self.offset].to_ascii_lowercase();
        self.whitespace();
        if self.peek() != Some('(') {
            return match name.as_str() {
                "pi" => Ok(std::f64::consts::PI),
                "tau" => Ok(std::f64::consts::TAU),
                "e" => Ok(std::f64::consts::E),
                "phi" => Ok(1.618_033_988_749_895),
                _ => Err(CalculatorError::UnknownName(name)),
            };
        }
        self.bump();
        let argument = self.expression(0)?;
        self.whitespace();
        if self.bump() != Some(')') {
            return Err(CalculatorError::Incomplete);
        }
        match name.as_str() {
            "sqrt" => Ok(argument.sqrt()),
            "cbrt" => Ok(argument.cbrt()),
            "abs" => Ok(argument.abs()),
            "sin" => Ok(argument.sin()),
            "cos" => Ok(argument.cos()),
            "tan" => Ok(argument.tan()),
            "ln" => Ok(argument.ln()),
            "log" | "log10" => Ok(argument.log10()),
            "log2" => Ok(argument.log2()),
            "exp" => Ok(argument.exp()),
            "floor" => Ok(argument.floor()),
            "ceil" => Ok(argument.ceil()),
            "round" => Ok(argument.round()),
            _ => Err(CalculatorError::UnknownName(name)),
        }
    }

    fn whitespace(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    fn peek(&self) -> Option<char> {
        self.input[self.offset..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.offset += character.len_utf8();
        Some(character)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number(input: &str) -> f64 {
        let ResultValue::Number(value) =
            Calculator::default().evaluate(input).expect("calculation")
        else {
            panic!("expected number")
        };
        value
    }

    fn close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 0.000_000_001,
            "{actual} != {expected}"
        );
    }

    #[test]
    fn precedence_parentheses_and_right_associative_power() {
        close(number("2 + 3 * 4"), 14.0);
        close(number("(2 + 3) * 4"), 20.0);
        close(number("2 ^ 3 ^ 2"), 512.0);
    }

    #[test]
    fn constants_functions_and_scientific_notation() {
        assert!((number("sin(pi / 2)") - 1.0).abs() < 0.000_001);
        close(number("sqrt(81) + 5e2"), 509.0);
        close(number("1_000 / 4"), 250.0);
    }

    #[test]
    fn converts_expression_between_units() {
        let ResultValue::Quantity { value, unit } = Calculator::default()
            .evaluate("(2 + 3) km to miles")
            .expect("conversion")
        else {
            panic!("expected quantity")
        };
        assert_eq!(unit.symbol, "mi");
        assert!((value - 3.106_855).abs() < 0.000_001);
    }

    #[test]
    fn rejects_division_by_zero_and_incompatible_units() {
        assert_eq!(
            Calculator::default().evaluate("1 / 0"),
            Err(CalculatorError::DivisionByZero)
        );
        assert_eq!(
            Calculator::default().evaluate("10 kg to m"),
            Err(CalculatorError::IncompatibleUnits)
        );
    }
}
