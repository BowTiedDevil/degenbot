//! Typed block selectors for the construction-I/O seam.
//!
//! A block argument handed to the seam is a *typed* value: a concrete number
//! or a canonical position tag. This wraps alloy's [`BlockNumberOrTag`] — the
//! tag ladder the RPC layer already queries with — rather than re-inventing
//! it, so a tag is carried as a tag and never has to be spelled as the number
//! it was coerced into (or silently dropped to `None`, which reads as the
//! head).
//!
//! Resolution against the chain head lives with the RPC clock
//! ([`RpcConstruction::resolve_block_selector`][super::RpcConstruction::resolve_block_selector]);
//! this module owns only the shape and the tag parse.

use alloy::eips::BlockNumberOrTag;

/// A block selector: a concrete number or a canonical position tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockSelector(BlockNumberOrTag);

/// A block argument the seam cannot read as a selector.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum BlockSelectorError {
    /// A string that is not one of the canonical tags.
    #[error(
        "unsupported block identifier {0:?}: expected an int or a canonical tag \
         (earliest, latest, pending)"
    )]
    Unsupported(String),
}

impl BlockSelector {
    /// A concrete block number.
    #[must_use]
    pub fn number(block: u64) -> Self {
        Self(BlockNumberOrTag::Number(block))
    }

    /// A typed tag.
    #[must_use]
    pub fn tag(tag: BlockNumberOrTag) -> Self {
        Self(tag)
    }

    /// Parse one canonical tag string (`"earliest"` / `"latest"` / `"pending"`).
    ///
    /// # Errors
    /// [`BlockSelectorError::Unsupported`] for any other string.
    pub fn parse_tag(raw: &str) -> Result<Self, BlockSelectorError> {
        match raw {
            "earliest" => Ok(Self(BlockNumberOrTag::Earliest)),
            "latest" => Ok(Self(BlockNumberOrTag::Latest)),
            "pending" => Ok(Self(BlockNumberOrTag::Pending)),
            other => Err(BlockSelectorError::Unsupported(other.to_owned())),
        }
    }

    /// The concrete number, when the selector is not a tag.
    #[must_use]
    pub fn as_number(self) -> Option<u64> {
        match self.0 {
            BlockNumberOrTag::Number(block) => Some(block),
            _ => None,
        }
    }

    /// The wrapped alloy number/tag.
    #[must_use]
    pub fn as_number_or_tag(self) -> BlockNumberOrTag {
        self.0
    }
}

impl From<u64> for BlockSelector {
    fn from(block: u64) -> Self {
        Self::number(block)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used)]

    use alloy::eips::BlockNumberOrTag;

    use super::BlockSelector;

    #[test]
    fn number_passes_through() {
        let selector = BlockSelector::number(2_000_000);
        assert_eq!(selector.as_number(), Some(2_000_000));
        assert_eq!(
            selector.as_number_or_tag(),
            BlockNumberOrTag::Number(2_000_000)
        );
    }

    #[test]
    fn canonical_tags_parse_to_their_members() {
        assert_eq!(
            BlockSelector::parse_tag("earliest")
                .unwrap()
                .as_number_or_tag(),
            BlockNumberOrTag::Earliest
        );
        assert_eq!(
            BlockSelector::parse_tag("latest")
                .unwrap()
                .as_number_or_tag(),
            BlockNumberOrTag::Latest
        );
        assert_eq!(
            BlockSelector::parse_tag("pending")
                .unwrap()
                .as_number_or_tag(),
            BlockNumberOrTag::Pending
        );
        // A tag is never a number, however it was spelled.
        assert_eq!(
            BlockSelector::parse_tag("latest").unwrap().as_number(),
            None
        );
    }

    #[test]
    fn non_tag_strings_are_refused() {
        assert_eq!(
            BlockSelector::parse_tag("0x10").unwrap_err(),
            super::BlockSelectorError::Unsupported("0x10".to_owned())
        );
        assert_eq!(
            BlockSelector::parse_tag("123").unwrap_err(),
            super::BlockSelectorError::Unsupported("123".to_owned())
        );
    }
}
