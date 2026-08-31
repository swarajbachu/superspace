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
    /// Function received the wrong number of arguments or an empty list.
    #[error("invalid arguments for function: {0}")]
    InvalidArguments(String),
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
            let Some(operator) = self.operator() else {
                break;
            };
            let (precedence, right_associative) = match operator {
                Operator::Add | Operator::Subtract => (1, false),
                Operator::Multiply | Operator::Divide | Operator::Modulo | Operator::Ratio => {
                    (2, false)
                }
                Operator::Power => (3, true),
            };
            if precedence < minimum_precedence {
                break;
            }
            self.consume_operator(operator);
            let right = self.expression(if right_associative {
                precedence
            } else {
                precedence + 1
            })?;
            left = match operator {
                Operator::Add => left + right,
                Operator::Subtract => left - right,
                Operator::Multiply => left * right,
                Operator::Divide | Operator::Modulo | Operator::Ratio if right == 0.0 => {
                    return Err(CalculatorError::DivisionByZero);
                }
                Operator::Divide | Operator::Ratio => left / right,
                Operator::Modulo => left % right,
                Operator::Power => left.powf(right),
            };
        }
        Ok(left)
    }

    fn prefix(&mut self) -> Result<f64, CalculatorError> {
        self.whitespace();
        let mut value = match self.peek() {
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
        }?;
        while self.peek() == Some('%') {
            self.bump();
            value /= 100.0;
        }
        Ok(value)
    }

    fn number(&mut self) -> Result<f64, CalculatorError> {
        let start = self.offset;
        if self.input[self.offset..].starts_with("0x")
            || self.input[self.offset..].starts_with("0X")
        {
            return self.radix_number(start, 16, 2);
        }
        if self.input[self.offset..].starts_with("0b")
            || self.input[self.offset..].starts_with("0B")
        {
            return self.radix_number(start, 2, 2);
        }
        if self.input[self.offset..].starts_with("0o")
            || self.input[self.offset..].starts_with("0O")
        {
            return self.radix_number(start, 8, 2);
        }
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

    fn radix_number(
        &mut self,
        start: usize,
        radix: u32,
        prefix_bytes: usize,
    ) -> Result<f64, CalculatorError> {
        self.offset += prefix_bytes;
        let digits_start = self.offset;
        while self
            .peek()
            .is_some_and(|character| character.is_digit(radix) || character == '_')
        {
            self.bump();
        }
        if self.offset == digits_start {
            return Err(CalculatorError::Unexpected(start));
        }
        let digits = self.input[digits_start..self.offset].replace('_', "");
        u64::from_str_radix(&digits, radix)
            .map_err(|_| CalculatorError::Unexpected(start))
            .and_then(exact_integer_as_f64)
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
        let mut arguments = Vec::new();
        self.whitespace();
        if self.peek() != Some(')') {
            loop {
                arguments.push(self.expression(0)?);
                self.whitespace();
                if self.peek() != Some(',') {
                    break;
                }
                self.bump();
            }
        }
        if self.bump() != Some(')') {
            return Err(CalculatorError::Incomplete);
        }
        let unary = || {
            (arguments.len() == 1)
                .then(|| arguments[0])
                .ok_or_else(|| CalculatorError::InvalidArguments(name.clone()))
        };
        match name.as_str() {
            "sqrt" => Ok(unary()?.sqrt()),
            "cbrt" => Ok(unary()?.cbrt()),
            "abs" => Ok(unary()?.abs()),
            "sin" => Ok(unary()?.sin()),
            "cos" => Ok(unary()?.cos()),
            "tan" => Ok(unary()?.tan()),
            "ln" => Ok(unary()?.ln()),
            "log" | "log10" => Ok(unary()?.log10()),
            "log2" => Ok(unary()?.log2()),
            "exp" => Ok(unary()?.exp()),
            "floor" => Ok(unary()?.floor()),
            "ceil" => Ok(unary()?.ceil()),
            "round" => Ok(unary()?.round()),
            "sum" if !arguments.is_empty() => Ok(arguments.iter().sum()),
            "avg" | "average" if !arguments.is_empty() => {
                let count = u32::try_from(arguments.len())
                    .map_err(|_| CalculatorError::InvalidArguments(name.clone()))?;
                Ok(arguments.iter().sum::<f64>() / f64::from(count))
            }
            "min" if !arguments.is_empty() => Ok(arguments
                .into_iter()
                .reduce(f64::min)
                .expect("non-empty arguments")),
            "max" if !arguments.is_empty() => Ok(arguments
                .into_iter()
                .reduce(f64::max)
                .expect("non-empty arguments")),
            "sum" | "avg" | "average" | "min" | "max" => {
                Err(CalculatorError::InvalidArguments(name))
            }
            _ => Err(CalculatorError::UnknownName(name)),
        }
    }

    fn operator(&self) -> Option<Operator> {
        match self.peek()? {
            '+' => Some(Operator::Add),
            '-' => Some(Operator::Subtract),
            '*' => Some(Operator::Multiply),
            '/' => Some(Operator::Divide),
            '%' => Some(Operator::Modulo),
            ':' => Some(Operator::Ratio),
            '^' => Some(Operator::Power),
            'o' | 'O'
                if self.input[self.offset..]
                    .get(..2)
                    .is_some_and(|word| word.eq_ignore_ascii_case("of"))
                    && self.input[self.offset + 2..]
                        .chars()
                        .next()
                        .is_none_or(char::is_whitespace) =>
            {
                Some(Operator::Multiply)
            }
            _ => None,
        }
    }

    fn consume_operator(&mut self, operator: Operator) {
        if operator == Operator::Multiply
            && self.input[self.offset..]
                .get(..2)
                .is_some_and(|word| word.eq_ignore_ascii_case("of"))
        {
            self.offset += 2;
        } else {
            self.bump();
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

#[allow(clippy::cast_precision_loss)]
fn exact_integer_as_f64(value: u64) -> Result<f64, CalculatorError> {
    if value <= 9_007_199_254_740_992 {
        Ok(value as f64)
    } else {
        Err(CalculatorError::NonFinite)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operator {
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Ratio,
    Power,
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

    #[test]
    fn percentages_ratios_and_base_literals() {
        close(number("20% of 50"), 10.0);
        close(number("16:9"), 16.0 / 9.0);
        close(number("0xff + 0b10 + 0o10"), 265.0);
        close(number("12.5%"), 0.125);
    }

    #[test]
    fn list_functions_validate_arity() {
        close(number("sum(1, 2, 3, 4)"), 10.0);
        close(number("avg(2, 4, 9)"), 5.0);
        close(number("min(3, -2, 8) + max(3, -2, 8)"), 6.0);
        assert!(matches!(
            Calculator::default().evaluate("avg()"),
            Err(CalculatorError::InvalidArguments(_))
        ));
        assert!(matches!(
            Calculator::default().evaluate("sqrt(1, 2)"),
            Err(CalculatorError::InvalidArguments(_))
        ));
    }
}
