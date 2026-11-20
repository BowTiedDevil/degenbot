//! Parses source-level signatures such as
//! `transfer(address,uint256) returns (uint256)`.

use crate::abi_types::type_::{AbiType, AbiTypeError};
use std::iter::Peekable;
use std::str::Chars;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseError {
    #[error("at position {pos}: expected {expected}, found '{found}'")]
    UnexpectedChar {
        pos: usize,
        expected: String,
        found: char,
    },
    #[error("at position {pos}: unexpected end of input, expected {expected}")]
    UnexpectedEnd { pos: usize, expected: String },
    #[error("at position {pos}: {msg}")]
    InvalidIdentifier { pos: usize, msg: String },
    #[error("at position {pos}: invalid type - {err}")]
    InvalidType { pos: usize, err: AbiTypeError },
    #[error("function name cannot be empty")]
    EmptyFunctionName,
    #[error("at position {pos}: unmatched '(' or '[' in type string")]
    UnmatchedBracket { pos: usize },
}

struct SignatureParser<'a> {
    chars: Peekable<Chars<'a>>,
    pos: usize,
}

impl<'a> SignatureParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            chars: input.chars().peekable(),
            pos: 0,
        }
    }

    fn parse(mut self) -> Result<ParsedSignature, ParseError> {
        self.skip_whitespace();

        let name = self.parse_identifier()?;
        if name.is_empty() {
            return Err(ParseError::EmptyFunctionName);
        }

        self.skip_whitespace();
        self.expect_char('(')?;
        let inputs = self.parse_type_list()?;
        self.expect_char(')')?;

        let outputs = self.parse_returns_clause()?;

        self.skip_whitespace();

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

    fn parse_returns_clause(&mut self) -> Result<Vec<AbiType>, ParseError> {
        self.skip_whitespace();

        if !self.peek_keyword("returns") {
            return Ok(Vec::new());
        }

        self.consume_keyword("returns")?;
        self.skip_whitespace();

        self.expect_char('(')?;

        let outputs = self.parse_type_list()?;

        self.expect_char(')')?;

        Ok(outputs)
    }

    fn parse_type_list(&mut self) -> Result<Vec<AbiType>, ParseError> {
        self.skip_whitespace();

        if self.peek_char(')') {
            return Ok(Vec::new());
        }

        let mut types = Vec::new();

        loop {
            self.skip_whitespace();

            let type_start = self.pos;
            let type_str = self.collect_type_string()?;

            if type_str.is_empty() {
                return Err(ParseError::UnexpectedChar {
                    pos: self.pos,
                    expected: "type name".to_string(),
                    found: self.chars.peek().copied().unwrap_or('\0'),
                });
            }

            let abi_type = AbiType::parse(&type_str).map_err(|e| ParseError::InvalidType {
                pos: type_start,
                err: e,
            })?;
            types.push(abi_type);

            self.skip_whitespace();

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

    /// Collect a type string (handles nested brackets for arrays and
    /// parentheses for tuple types, so a tuple's own commas and closing
    /// paren stay inside the token).
    fn collect_type_string(&mut self) -> Result<String, ParseError> {
        let mut bracket_depth = 0;
        let mut paren_depth = 0;
        let mut result = String::new();

        loop {
            match self.chars.peek() {
                // A comma or closing paren terminates the token only at the
                // parameter-list level; inside a tuple or array it is data.
                Some(')' | ',') if paren_depth == 0 && bracket_depth == 0 => break,
                Some('[') => {
                    bracket_depth += 1;
                    result.push('[');
                    self.advance();
                }
                Some('(') => {
                    paren_depth += 1;
                    result.push('(');
                    self.advance();
                }
                Some(']') => {
                    if bracket_depth > 0 {
                        bracket_depth -= 1;
                    } else if paren_depth == 0 {
                        break;
                    }
                    result.push(']');
                    self.advance();
                }
                Some(')') => {
                    if paren_depth > 0 {
                        paren_depth -= 1;
                    } else if bracket_depth == 0 {
                        break;
                    }
                    result.push(')');
                    self.advance();
                }
                Some(&c) => {
                    if c.is_whitespace() && paren_depth == 0 && bracket_depth == 0 {
                        break;
                    }
                    result.push(c);
                    self.advance();
                }
                None if paren_depth > 0 || bracket_depth > 0 => {
                    return Err(ParseError::UnmatchedBracket { pos: self.pos });
                }
                None => break,
            }
        }

        Ok(result)
    }

    fn parse_identifier(&mut self) -> Result<String, ParseError> {
        let mut result = String::new();

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSignature {
    pub name: String,
    pub inputs: Vec<AbiType>,
    pub outputs: Vec<AbiType>,
}

#[expect(clippy::missing_errors_doc)]
pub fn parse_signature(input: &str) -> Result<ParsedSignature, ParseError> {
    SignatureParser::new(input).parse()
}

impl ParsedSignature {
    /// Canonical signature text without the returns clause: the form a selector is
    /// hashed from.
    #[must_use]
    pub fn to_signature_string(&self) -> String {
        let input_types: Vec<String> = self.inputs.iter().map(ToString::to_string).collect();
        format!("{}({})", self.name, input_types.join(","))
    }

    /// Canonical signature text including the returns clause, for display.
    #[must_use]
    pub fn to_full_signature_string(&self) -> String {
        let input_types: Vec<String> = self.inputs.iter().map(ToString::to_string).collect();
        let output_types: Vec<String> = self.outputs.iter().map(ToString::to_string).collect();
        if self.outputs.is_empty() {
            format!("{}({})", self.name, input_types.join(","))
        } else {
            format!(
                "{}({}) returns ({})",
                self.name,
                input_types.join(","),
                output_types.join(",")
            )
        }
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::abi_types::type_::BitWidth;

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
    fn test_tuple_signature() {
        let sig =
            parse_signature("quoteExactInputSingle((address,address),bool,uint128,bytes)").unwrap();
        assert_eq!(sig.name, "quoteExactInputSingle");
        assert_eq!(
            sig.inputs,
            vec![
                AbiType::Tuple(vec![AbiType::Address, AbiType::Address]),
                AbiType::Bool,
                AbiType::Uint(BitWidth::new(128).unwrap()),
                AbiType::Bytes,
            ]
        );
        assert_eq!(
            sig.to_signature_string(),
            "quoteExactInputSingle((address,address),bool,uint128,bytes)"
        );
    }

    #[test]
    fn test_nested_tuple_signature() {
        let sig = parse_signature("f(((uint256,address),bool))").unwrap();
        assert_eq!(
            sig.inputs,
            vec![AbiType::Tuple(vec![
                AbiType::Tuple(vec![AbiType::Uint(BitWidth::BITS_256), AbiType::Address]),
                AbiType::Bool,
            ])]
        );
    }

    #[test]
    fn test_tuple_array_signature() {
        let sig = parse_signature("f((bool,bytes)[],(uint256,bool)[2])").unwrap();
        assert_eq!(
            sig.inputs[0],
            AbiType::Array(Box::new(AbiType::Tuple(vec![
                AbiType::Bool,
                AbiType::Bytes,
            ])))
        );
        assert_eq!(
            sig.inputs[1],
            AbiType::FixedArray(
                Box::new(AbiType::Tuple(vec![
                    AbiType::Uint(BitWidth::BITS_256),
                    AbiType::Bool
                ])),
                2
            )
        );
    }

    #[test]
    fn test_unmatched_tuple_paren() {
        let err = parse_signature("f((uint256,bool").unwrap_err();
        assert!(
            matches!(err, ParseError::UnmatchedBracket { .. }),
            "Got unexpected error type: {err:?}",
        );
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
        assert!(matches!(err, ParseError::UnexpectedChar { .. }));
    }

    #[test]
    fn test_unmatched_bracket() {
        let err = parse_signature("foo(uint256[)").unwrap_err();
        assert!(
            matches!(err, ParseError::UnmatchedBracket { .. }),
            "Got unexpected error type: {err:?}",
        );
    }
}

// =============================================================================
// Property-based tests for signature parsing
// =============================================================================

#[cfg(test)]
mod proptests {
    #![expect(clippy::unwrap_used)]

    use super::*;
    use proptest::prelude::*;

    fn arb_identifier() -> impl Strategy<Value = String> {
        (1..10usize).prop_flat_map(|len| {
            proptest::string::string_regex(format!("[a-zA-Z_][a-zA-Z0-9_]{{{}}}", len - 1).as_str())
                .unwrap()
        })
    }

    fn arb_abi_type_string() -> impl Strategy<Value = String> {
        let leaf = prop_oneof![
            Just("address".to_string()),
            Just("bool".to_string()),
            Just("string".to_string()),
            Just("bytes".to_string()),
            (1u8..=32).prop_map(|b| format!("uint{}", u16::from(b) * 8)),
            (1u8..=32).prop_map(|b| format!("int{}", u16::from(b) * 8)),
            (1u8..=32).prop_map(|n| format!("bytes{n}")),
        ];

        leaf.prop_recursive(3, 64, 2, |inner| {
            prop_oneof![
                inner.clone().prop_map(|t| format!("{t}[]")),
                (inner, 1usize..5).prop_map(|(t, n)| format!("{t}[{n}]")),
            ]
        })
    }

    fn arb_signature_string() -> impl Strategy<Value = String> {
        let types = prop::collection::vec(arb_abi_type_string(), 0..5);
        (arb_identifier(), types).prop_map(|(name, type_list)| {
            let type_str = type_list.join(",");
            format!("{name}({type_str})")
        })
    }

    fn arb_full_signature_string() -> impl Strategy<Value = String> {
        let input_types = prop::collection::vec(arb_abi_type_string(), 0..5);
        let output_types = prop::collection::vec(arb_abi_type_string(), 0..5);
        (arb_identifier(), input_types, output_types).prop_map(|(name, inputs, outputs)| {
            let input_str = inputs.join(",");
            let output_str = outputs.join(",");
            if outputs.is_empty() {
                format!("{name}({input_str})")
            } else {
                format!("{name}({input_str}) returns ({output_str})")
            }
        })
    }

    proptest! {
        #[test]
        fn valid_signatures_parse(sig in arb_signature_string()) {
            let result = parse_signature(&sig);
            prop_assert!(result.is_ok(), "Failed to parse '{}': {:?}", sig, result.err());
        }

        #[test]
        fn full_signatures_parse(sig in arb_full_signature_string()) {
            let result = parse_signature(&sig);
            prop_assert!(result.is_ok(), "Failed to parse '{}': {:?}", sig, result.err());
        }

        #[test]
        fn signature_roundtrip(sig in arb_signature_string()) {
            let parsed = parse_signature(&sig)?;
            let canonical = parsed.to_signature_string();
            let reparsed = parse_signature(&canonical)?;

            prop_assert_eq!(parsed.name, reparsed.name);
            prop_assert_eq!(parsed.inputs, reparsed.inputs);
        }

        #[test]
        fn full_signature_roundtrip(sig in arb_full_signature_string()) {
            let parsed = parse_signature(&sig)?;
            let canonical = parsed.to_full_signature_string();
            let reparsed = parse_signature(&canonical)?;

            prop_assert_eq!(parsed.name, reparsed.name);
            prop_assert_eq!(parsed.inputs, reparsed.inputs);
            prop_assert_eq!(parsed.outputs, reparsed.outputs);
        }

        #[test]
        fn whitespace_around_signature(sig in arb_signature_string()) {
            let padded = format!("  {sig}  ");
            let result = parse_signature(&padded);
            prop_assert!(result.is_ok(), "Failed to parse padded '{}': {:?}", padded, result.err());
        }

        #[test]
        fn case_insensitive_returns_keyword(name in arb_identifier(), n_bits in 1u8..=32u8) {
            let bit_width = u16::from(n_bits) * 8;
            let sig_lower = format!("{name}(uint256) returns (uint{bit_width})");
            let sig_upper = format!("{name}(uint256) RETURNS (uint{bit_width})");
            let sig_mixed = format!("{name}(uint256) Returns (uint{bit_width})");

            let parsed_lower = parse_signature(&sig_lower)?;
            let parsed_upper = parse_signature(&sig_upper)?;
            let parsed_mixed = parse_signature(&sig_mixed)?;

            prop_assert_eq!(parsed_lower.outputs.len(), 1);
            prop_assert!(
                parsed_upper.outputs.as_slice() == parsed_lower.outputs.as_slice(),
                "Uppercase RETURNS should match lowercase"
            );
            prop_assert!(
                parsed_mixed.outputs.as_slice() == parsed_lower.outputs.as_slice(),
                "Mixed case Returns should match lowercase"
            );
        }
    }
}
