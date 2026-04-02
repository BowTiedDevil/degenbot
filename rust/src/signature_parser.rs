//! Function signature parser with robust error handling.
//!
//! Parses signatures like `"transfer(address,uint256) returns (uint256)"`.

use crate::abi_types::{AbiType, AbiTypeError};
use std::iter::Peekable;
use std::str::Chars;

/// Errors that can occur during signature parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Unexpected character at position.
    UnexpectedChar {
        pos: usize,
        expected: String,
        found: char,
    },
    /// Unexpected end of input.
    UnexpectedEnd { pos: usize, expected: String },
    /// Invalid identifier.
    InvalidIdentifier { pos: usize, msg: String },
    /// Invalid ABI type.
    InvalidType { pos: usize, err: AbiTypeError },
    /// Empty function name.
    EmptyFunctionName,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnexpectedChar {
                pos,
                expected,
                found,
            } => {
                write!(f, "at position {pos}: expected {expected}, found '{found}'")
            }
            Self::UnexpectedEnd { pos, expected } => {
                write!(
                    f,
                    "at position {pos}: unexpected end of input, expected {expected}"
                )
            }
            Self::InvalidIdentifier { pos, msg } => {
                write!(f, "at position {pos}: {msg}")
            }
            Self::InvalidType { pos, err } => {
                write!(f, "at position {pos}: invalid type - {err}")
            }
            Self::EmptyFunctionName => {
                write!(f, "function name cannot be empty")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Parser state for function signatures.
struct SignatureParser<'a> {
    /// Original input string (kept for potential error message improvements).
    #[allow(dead_code)]
    input: &'a str,
    chars: Peekable<Chars<'a>>,
    pos: usize,
}

impl<'a> SignatureParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            chars: input.chars().peekable(),
            pos: 0,
        }
    }

    /// Parse a complete function signature.
    fn parse(mut self) -> Result<ParsedSignature, ParseError> {
        // Skip leading whitespace
        self.skip_whitespace();

        // Parse function name
        let name = self.parse_identifier()?;
        if name.is_empty() {
            return Err(ParseError::EmptyFunctionName);
        }

        // Parse input parameter list
        self.skip_whitespace();
        self.expect_char('(')?;
        let inputs = self.parse_type_list()?;
        self.expect_char(')')?;

        // Parse optional returns clause
        let outputs = self.parse_returns_clause()?;

        // Skip trailing whitespace
        self.skip_whitespace();

        // Ensure we've consumed all input
        if self.chars.peek().is_some() {
            let pos = self.pos;
            let found = self.chars.peek().copied().unwrap_or('\0');
            return Err(ParseError::UnexpectedChar {
                pos,
                expected: "end of input".to_string(),
                found,
            });
        }

        Ok(ParsedSignature {
            name,
            inputs,
            outputs,
        })
    }

    /// Parse the returns clause if present.
    fn parse_returns_clause(&mut self) -> Result<Vec<AbiType>, ParseError> {
        self.skip_whitespace();

        // Check for 'returns' keyword
        if !self.peek_keyword("returns") {
            return Ok(Vec::new());
        }

        // Consume 'returns'
        self.consume_keyword("returns")?;
        self.skip_whitespace();

        // Expect opening paren
        self.expect_char('(')?;

        // Parse output types
        let outputs = self.parse_type_list()?;

        // Expect closing paren
        self.expect_char(')')?;

        Ok(outputs)
    }

    /// Parse a comma-separated list of types.
    fn parse_type_list(&mut self) -> Result<Vec<AbiType>, ParseError> {
        self.skip_whitespace();

        // Empty list
        if self.peek_char(')') {
            return Ok(Vec::new());
        }

        let mut types = Vec::new();

        loop {
            self.skip_whitespace();

            // Parse type string (collect until comma or closing paren)
            let type_start = self.pos;
            let type_str = self.collect_type_string();

            if type_str.is_empty() {
                return Err(ParseError::UnexpectedChar {
                    pos: self.pos,
                    expected: "type name".to_string(),
                    found: self.chars.peek().copied().unwrap_or('\0'),
                });
            }

            // Parse the ABI type
            let abi_type = AbiType::parse(&type_str).map_err(|e| ParseError::InvalidType {
                pos: type_start,
                err: e,
            })?;
            types.push(abi_type);

            self.skip_whitespace();

            // Check for comma or end of list
            if self.peek_char(',') {
                self.consume_char(',');
            } else if self.peek_char(')') {
                break;
            } else {
                return Err(ParseError::UnexpectedChar {
                    pos: self.pos,
                    expected: "',' or ')'".to_string(),
                    found: self.chars.peek().copied().unwrap_or('\0'),
                });
            }
        }

        Ok(types)
    }

    /// Collect a type string (handles nested brackets for arrays).
    #[allow(clippy::unnested_or_patterns)]
    fn collect_type_string(&mut self) -> String {
        let mut depth = 0;
        let mut result = String::new();

        loop {
            match self.chars.peek() {
                None | Some(')') | Some(',') if depth == 0 => break,
                Some('[') => {
                    depth += 1;
                    // Safe: we just peeked '[' so consume will succeed
                    #[allow(clippy::expect_used)]
                    result.push(self.consume_char('[').expect("peeked '['"));
                }
                Some(']') => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    // Safe: we just peeked ']' so consume will succeed
                    #[allow(clippy::expect_used)]
                    result.push(self.consume_char(']').expect("peeked ']'"));
                }
                Some(&c) => {
                    if c.is_whitespace() && depth == 0 {
                        break;
                    }
                    result.push(c);
                    self.advance();
                }
                None => break,
            }
        }

        result
    }

    /// Parse an identifier (function name).
    fn parse_identifier(&mut self) -> Result<String, ParseError> {
        let mut result = String::new();

        // First char must be alphabetic or underscore
        match self.chars.peek() {
            Some(&c) if c.is_alphabetic() || c == '_' => {
                result.push(c);
                self.advance();
            }
            Some(&c) => {
                return Err(ParseError::UnexpectedChar {
                    pos: self.pos,
                    expected: "letter or '_'".to_string(),
                    found: c,
                });
            }
            None => {
                return Err(ParseError::UnexpectedEnd {
                    pos: self.pos,
                    expected: "function name".to_string(),
                });
            }
        }

        // Rest can be alphanumeric or underscore
        while let Some(&c) = self.chars.peek() {
            if c.is_alphanumeric() || c == '_' {
                result.push(c);
                self.advance();
            } else {
                break;
            }
        }

        Ok(result)
    }

    // ========== Helper methods ==========

    fn skip_whitespace(&mut self) {
        while let Some(&c) = self.chars.peek() {
            if c.is_whitespace() {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn peek_char(&mut self, expected: char) -> bool {
        self.chars.peek() == Some(&expected)
    }

    fn expect_char(&mut self, expected: char) -> Result<(), ParseError> {
        match self.chars.peek() {
            Some(&c) if c == expected => {
                self.advance();
                Ok(())
            }
            Some(&c) => Err(ParseError::UnexpectedChar {
                pos: self.pos,
                expected: format!("'{expected}'"),
                found: c,
            }),
            None => Err(ParseError::UnexpectedEnd {
                pos: self.pos,
                expected: format!("'{expected}'"),
            }),
        }
    }

    fn consume_char(&mut self, expected: char) -> Option<char> {
        if self.peek_char(expected) {
            self.advance();
            Some(expected)
        } else {
            None
        }
    }

    fn advance(&mut self) -> Option<char> {
        self.chars.next().inspect(|&c| {
            self.pos += c.len_utf8();
        })
    }

    fn peek_keyword(&self, keyword: &str) -> bool {
        let remaining: String = self.chars.clone().take(keyword.len()).collect();
        remaining.eq_ignore_ascii_case(keyword)
    }

    fn consume_keyword(&mut self, keyword: &str) -> Result<(), ParseError> {
        for expected in keyword.chars() {
            match self.chars.peek() {
                Some(&c) if c.eq_ignore_ascii_case(&expected) => {
                    self.advance();
                }
                Some(&c) => {
                    return Err(ParseError::UnexpectedChar {
                        pos: self.pos,
                        expected: format!("'{expected}'"),
                        found: c,
                    });
                }
                None => {
                    return Err(ParseError::UnexpectedEnd {
                        pos: self.pos,
                        expected: format!("'{expected}'"),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Parsed function signature result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSignature {
    pub name: String,
    pub inputs: Vec<AbiType>,
    pub outputs: Vec<AbiType>,
}

/// Parse a function signature.
pub fn parse_signature(input: &str) -> Result<ParsedSignature, ParseError> {
    SignatureParser::new(input).parse()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_signature() {
        let sig = parse_signature("transfer(address,uint256)").unwrap();
        assert_eq!(sig.name, "transfer");
        assert_eq!(sig.inputs.len(), 2);
        assert!(sig.outputs.is_empty());
    }

    #[test]
    fn test_with_returns() {
        let sig = parse_signature("balanceOf(address) returns (uint256)").unwrap();
        assert_eq!(sig.name, "balanceOf");
        assert_eq!(sig.inputs.len(), 1);
        assert_eq!(sig.outputs.len(), 1);
    }

    #[test]
    fn test_no_parens() {
        let err = parse_signature("transfer").unwrap_err();
        // After parsing "transfer", we expect '(' but get EOF
        assert!(matches!(err, ParseError::UnexpectedEnd { .. }));
    }

    #[test]
    fn test_returns_no_open_paren() {
        let err = parse_signature("foo()returns").unwrap_err();
        assert!(matches!(err, ParseError::UnexpectedEnd { .. }));
    }

    #[test]
    fn test_returns_no_close_paren() {
        let err = parse_signature("foo()returns(uint256").unwrap_err();
        // Inside parse_type_list after "uint256": expects ',' or ')', finds EOF
        assert!(
            matches!(err, ParseError::UnexpectedEnd { .. })
                || matches!(err, ParseError::UnexpectedChar { .. }),
            "Got unexpected error type: {err:?}",
        );
    }

    #[test]
    fn test_returns_empty() {
        let sig = parse_signature("foo()returns()").unwrap();
        assert!(sig.outputs.is_empty());
    }

    #[test]
    fn test_only_open_paren() {
        let err = parse_signature("transfer(").unwrap_err();
        // After seeing '(', parse_type_list is called.
        // It skips whitespace, sees ')' or EOF. For "transfer(", chars ends.
        // peek_char(')') returns false, so it tries to parse a type.
        // collect_type_string sees EOF, returns empty string.
        // We check if type_str.is_empty() and return UnexpectedChar.
        assert!(
            matches!(err, ParseError::UnexpectedEnd { .. })
                || matches!(err, ParseError::UnexpectedChar { .. }),
            "Got unexpected error type: {err:?}",
        );
    }

    #[test]
    fn test_nested_arrays() {
        let sig = parse_signature("foo(address[][3])").unwrap();
        assert_eq!(sig.inputs.len(), 1);
    }

    #[test]
    fn test_case_insensitive_returns() {
        let sig = parse_signature("foo()RETURNS(uint256)").unwrap();
        assert_eq!(sig.outputs.len(), 1);
    }

    #[test]
    fn test_trailing_garbage() {
        let err = parse_signature("foo() bar").unwrap_err();
        assert!(matches!(err, ParseError::UnexpectedChar { .. }));
    }

    #[test]
    fn test_empty_function_name() {
        let err = parse_signature("()").unwrap_err();
        // ')' is not a valid start for an identifier
        assert!(matches!(err, ParseError::UnexpectedChar { .. }));
    }
}
