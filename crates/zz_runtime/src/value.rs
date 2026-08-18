//! Runtime values for the Phase 0 tree-walker.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Unit,
}

impl Value {
    /// Promote a numeric value to float (used for mixed arithmetic).
    pub fn to_float(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            Value::Unit => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(i) => write!(f, "{i}"),
            // Always show a decimal point so floats are distinguishable.
            Value::Float(x) => {
                if x.is_finite() && x.fract() == 0.0 {
                    write!(f, "{x:.1}")
                } else {
                    write!(f, "{x}")
                }
            }
            Value::Unit => write!(f, ""),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_displays_plain() {
        assert_eq!(Value::Int(42).to_string(), "42");
    }

    #[test]
    fn float_always_shows_decimal() {
        assert_eq!(Value::Float(3.0).to_string(), "3.0");
        assert_eq!(Value::Float(3.5).to_string(), "3.5");
    }

    #[test]
    fn unit_displays_empty() {
        assert_eq!(Value::Unit.to_string(), "");
    }
}
