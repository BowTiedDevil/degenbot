//! The canonical delta vocabulary's decoder: the inverse of the encoding a
//! journal row persists (ADR-105 D7, D9).
//!
//! A journal row states the delta's exact position, the generation relationship
//! its reduction consumed, and the encoding of its effect. Recovery replays those
//! rows forward, so the vocabulary must reconstruct a delta from what the row
//! holds — otherwise recovery would need inverse protocol arithmetic or a second,
//! lossless payload channel.
//!
//! Identity comes from the ROW, never from the text: the encoding is a digest of
//! the position and the effect, and the two facts it elides are properties of the
//! row itself — a [`ReserveDataUpdated`] was emitted in the block its row states,
//! and an [`Upgraded`] belongs to the market whose journal holds it. Every
//! decoded delta is re-encoded and required to match the text it came from, so a
//! disagreement between the digest and the row is a refusal rather than a
//! plausible-looking reconstruction.

use alloy::primitives::{I256, U256};
use degenbot_core::canonical_head::{ChainAnchor, LogRef};
use degenbot_db::{GenerationRecord, ScaledTokenPosition};

use super::checkpoint::log_ref;
use super::configuration::ConfigurationEffect;
use super::{
    CanonicalDelta, DeltaEffect, GenerationActivation, GenerationEntity, GenerationRef,
    PositionSubject, ProjectionError,
};

/// What a journal row states: the exact position, the generation relationship,
/// the market whose journal holds it, and the versioned encoding.
#[derive(Debug, Clone)]
pub struct JournaledDelta<'a> {
    /// The row's exact chain position.
    pub source: LogRef,
    /// The generation relationship the row states.
    pub generation: Option<GenerationRecord>,
    /// The market whose journal holds the row.
    pub market_id: i64,
    /// The delta's canonical encoding.
    pub encoding: &'a str,
    /// The encoding version the row states.
    pub encoding_version: &'a str,
}

/// One effect body's field list, parsed by known key. A field's value ends at the
/// next comma unless it is taken as the remainder, which is how a free-text label
/// round-trips.
struct Fields<'a> {
    rest: &'a str,
}

impl<'a> Fields<'a> {
    const fn new(rest: &'a str) -> Self {
        Self { rest }
    }

    fn bad(&self) -> ProjectionError {
        ProjectionError::DeltaEncoding {
            encoding: self.rest.to_string(),
        }
    }

    fn strip(&mut self, key: &str) -> Result<&'a str, ProjectionError> {
        let rest = self
            .rest
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
            .ok_or_else(|| self.bad())?;
        Ok(rest)
    }

    fn take(&mut self, key: &str) -> Result<&'a str, ProjectionError> {
        let rest = self.strip(key)?;
        if let Some((value, tail)) = rest.split_once(',') {
            self.rest = tail;
            return Ok(value);
        }
        self.rest = "";
        Ok(rest)
    }

    fn take_rest(&mut self, key: &str) -> Result<&'a str, ProjectionError> {
        let value = self.strip(key)?;
        self.rest = "";
        Ok(value)
    }

    fn number(&mut self, key: &str) -> Result<i64, ProjectionError> {
        self.take(key)?.parse().map_err(|_| self.bad())
    }

    fn wide(&mut self, key: &str) -> Result<U256, ProjectionError> {
        parse_u256(self.take(key)?).ok_or_else(|| self.bad())
    }

    fn flag(&mut self, key: &str) -> Result<bool, ProjectionError> {
        match self.take(key)? {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(self.bad()),
        }
    }

    fn optional_number(&mut self, key: &str) -> Result<Option<i64>, ProjectionError> {
        match self.take(key)? {
            "none" => Ok(None),
            value => value.parse().map(Some).map_err(|_| self.bad()),
        }
    }

    fn optional_text(&mut self, key: &str) -> Result<Option<String>, ProjectionError> {
        match self.take(key)? {
            "none" => Ok(None),
            value => Ok(Some(value.to_string())),
        }
    }

    fn text(&mut self, key: &str) -> Result<String, ProjectionError> {
        Ok(self.take(key)?.to_string())
    }

    fn text_rest(&mut self, key: &str) -> Result<String, ProjectionError> {
        Ok(self.take_rest(key)?.to_string())
    }

    fn done(&self) -> bool {
        self.rest.is_empty()
    }
}

fn parse_u256(text: &str) -> Option<U256> {
    U256::from_str_radix(text, 10).ok()
}

fn encoding_error(encoding: &str) -> ProjectionError {
    ProjectionError::DeltaEncoding {
        encoding: encoding.to_string(),
    }
}

fn relation(table: &str) -> Result<ScaledTokenPosition, ProjectionError> {
    ScaledTokenPosition::ALL
        .iter()
        .copied()
        .find(|candidate| candidate.table() == table)
        .ok_or_else(|| encoding_error(table))
}

/// A table:id position subject inside an effect body.
fn subject(text: &str, encoding: &str) -> Result<PositionSubject, ProjectionError> {
    let (table, id) = text
        .split_once(':')
        .ok_or_else(|| encoding_error(encoding))?;
    Ok(PositionSubject {
        position: relation(table)?,
        position_id: id.parse().map_err(|_| encoding_error(encoding))?,
    })
}

/// A signed I256 as the encoding prints it (+5, -5).
fn signed(text: &str, encoding: &str) -> Result<I256, ProjectionError> {
    let bad = || encoding_error(encoding);
    let (negative, digits) = match text.strip_prefix('-') {
        Some(digits) => (true, digits),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let magnitude = parse_u256(digits).ok_or_else(bad)?;
    let value = I256::try_from(magnitude).map_err(|_| bad())?;
    if negative {
        value.checked_neg().ok_or_else(bad)
    } else {
        Ok(value)
    }
}

fn split_effect(encoding: &str) -> Result<(&str, &str), ProjectionError> {
    let (name, body) = encoding
        .split_once('{')
        .ok_or_else(|| encoding_error(encoding))?;
    let body = body
        .strip_suffix('}')
        .ok_or_else(|| encoding_error(encoding))?;
    Ok((name, body))
}

/// A revision-annotated token id (id at revision), as reserve-initialized states
/// it.
fn revisioned_token(text: &str, encoding: &str) -> Result<(i64, i64), ProjectionError> {
    let (id, revision) = text
        .split_once('@')
        .ok_or_else(|| encoding_error(encoding))?;
    Ok((
        id.parse().map_err(|_| encoding_error(encoding))?,
        revision.parse().map_err(|_| encoding_error(encoding))?,
    ))
}

fn anchor_text(anchor: ChainAnchor) -> String {
    format!(
        "{chain}/{number}/{hash}",
        chain = anchor.chain_id(),
        number = anchor.number(),
        hash = alloy::hex::encode(anchor.hash()),
    )
}

/// The position prefix every canonical encoding starts with.
fn position_text(source: LogRef) -> String {
    let tx = source.tx();
    format!(
        "{anchor}/{tx_hash}/{tx_index}/{log_index}",
        anchor = anchor_text(*tx.anchor()),
        tx_hash = alloy::hex::encode(tx.hash()),
        tx_index = tx.index(),
        log_index = source.index(),
    )
}

/// The generation relationship a journal row states, in the view's vocabulary.
fn generation_ref(record: &GenerationRecord) -> Result<GenerationRef, ProjectionError> {
    let entity = GenerationEntity::decode(&record.entity)?;
    let activation = match record.activation {
        degenbot_db::GenerationActivation::Seed => GenerationActivation::Seed,
        degenbot_db::GenerationActivation::Event(at) => GenerationActivation::Event(log_ref(at)?),
    };
    Ok(GenerationRef::restored(entity, activation, record.revision))
}

/// The effect a canonical encoding states, with the two facts the encoding elides
/// supplied by the row that carries it.
#[expect(clippy::too_many_lines)]
fn decode_effect(
    encoding: &str,
    source: LogRef,
    market_id: i64,
) -> Result<DeltaEffect, ProjectionError> {
    let (name, body) = split_effect(encoding)?;
    let mut fields = Fields::new(body);
    let effect = match name {
        "scaled-balance" => {
            let (subject_text, index) = body
                .split_once(',')
                .ok_or_else(|| encoding_error(encoding))?;
            let sign_at = subject_text
                .rfind(['+', '-'])
                .ok_or_else(|| encoding_error(encoding))?;
            let (position_text, delta) = subject_text.split_at(sign_at);
            fields.rest = "";
            DeltaEffect::ScaledBalance {
                subject: subject(position_text, encoding)?,
                balance_delta: signed(delta, encoding)?,
                index: parse_u256(index).ok_or_else(|| encoding_error(encoding))?,
            }
        }
        "scaled-transfer" => {
            let (from_text, rest) = body
                .split_once("->")
                .ok_or_else(|| encoding_error(encoding))?;
            let mut parts = rest.split(',');
            let to_text = parts.next().ok_or_else(|| encoding_error(encoding))?;
            let amount = parts.next().ok_or_else(|| encoding_error(encoding))?;
            let index = parts.next().ok_or_else(|| encoding_error(encoding))?;
            if parts.next().is_some() {
                return Err(encoding_error(encoding));
            }
            fields.rest = "";
            DeltaEffect::ScaledTransfer {
                from: subject(from_text, encoding)?,
                to: if to_text == "burn-address" {
                    None
                } else {
                    Some(subject(to_text, encoding)?)
                },
                scaled_amount: parse_u256(amount).ok_or_else(|| encoding_error(encoding))?,
                index: parse_u256(index).ok_or_else(|| encoding_error(encoding))?,
            }
        }
        "debt-position-reset" => {
            let (position, index) = body
                .split_once(',')
                .ok_or_else(|| encoding_error(encoding))?;
            fields.rest = "";
            DeltaEffect::DebtPositionReset {
                position_id: position.parse().map_err(|_| encoding_error(encoding))?,
                index: parse_u256(index).ok_or_else(|| encoding_error(encoding))?,
            }
        }
        "gho-refresh-discount" => {
            fields.rest = "";
            DeltaEffect::GhoRefreshDiscount {
                position_id: body.parse().map_err(|_| encoding_error(encoding))?,
            }
        }
        "stk-aave-balance-observed" => DeltaEffect::StkAaveBalanceObserved {
            user_id: fields.number("user")?,
            balance: fields.wide("balance")?,
        },
        "gho-discount-refreshed" => DeltaEffect::GhoDiscountRefreshed {
            user_id: fields.number("user")?,
            gho_discount: fields.number("discount")?,
        },
        "collateral-configuration" => {
            DeltaEffect::Configuration(ConfigurationEffect::CollateralConfigurationChanged {
                asset_id: fields.number("asset")?,
                config_bitmap: fields.wide("bitmap")?,
            })
        }
        "emode-category-added" => {
            let market = fields.number("market")?;
            let category_id = fields.number("category")?;
            let ltv = fields.number("ltv")?;
            let liquidation_threshold = fields.number("lt")?;
            let liquidation_bonus = fields.number("bonus")?;
            let price_source = fields.optional_text("source")?;
            let label = fields.text_rest("label")?;
            DeltaEffect::Configuration(ConfigurationEffect::EModeCategoryAdded {
                market_id: market,
                category_id,
                ltv: u64::try_from(ltv).map_err(|_| encoding_error(encoding))?,
                liquidation_threshold: u64::try_from(liquidation_threshold)
                    .map_err(|_| encoding_error(encoding))?,
                liquidation_bonus: u64::try_from(liquidation_bonus)
                    .map_err(|_| encoding_error(encoding))?,
                price_source,
                label,
            })
        }
        "emode-asset-category" => {
            DeltaEffect::Configuration(ConfigurationEffect::EModeAssetCategoryChanged {
                asset_id: fields.number("asset")?,
                new_category_id: fields.number("category")?,
            })
        }
        "asset-collateral-in-emode" => {
            DeltaEffect::Configuration(ConfigurationEffect::AssetCollateralInEModeChanged {
                asset_id: fields.number("asset")?,
                category_id: fields.number("category")?,
                is_collateral: fields.flag("collateral")?,
            })
        }
        "reserve-used-as-collateral" => {
            DeltaEffect::Configuration(ConfigurationEffect::ReserveUsedAsCollateral {
                user_id: fields.number("user")?,
                asset_id: fields.number("asset")?,
                enabled: fields.flag("enabled")?,
            })
        }
        "user-emode" => DeltaEffect::Configuration(ConfigurationEffect::UserEModeSet {
            user_id: fields.number("user")?,
            e_mode: fields.number("emode")?,
        }),
        "price-oracle-updated" => {
            DeltaEffect::Configuration(ConfigurationEffect::PriceOracleUpdated {
                market_id: fields.number("market")?,
                new_oracle_address: fields.text("oracle")?,
            })
        }
        "asset-source-updated" => {
            DeltaEffect::Configuration(ConfigurationEffect::AssetSourceUpdated {
                asset_id: fields.number("asset")?,
                source_address: fields.text("source")?,
            })
        }
        "reserve-data-updated" => {
            DeltaEffect::Configuration(ConfigurationEffect::ReserveDataUpdated {
                asset_id: fields.number("asset")?,
                liquidity_rate: fields.wide("lr")?,
                variable_borrow_rate: fields.wide("vbr")?,
                liquidity_index: fields.wide("li")?,
                variable_borrow_index: fields.wide("vbi")?,
                block_number: source.tx().anchor().number(),
            })
        }
        "reserve-initialized" => {
            let market = fields.number("market")?;
            let underlying_asset_id = fields.number("underlying")?;
            let (a_token_id, a_token_revision) =
                revisioned_token(fields.take("a-token")?, encoding)?;
            let (v_token_id, v_token_revision) =
                revisioned_token(fields.take("v-token")?, encoding)?;
            let price_source = fields.optional_text("source")?;
            let gho_link_token_id = fields.optional_number("gho-link")?;
            DeltaEffect::Configuration(ConfigurationEffect::ReserveInitialized {
                market_id: market,
                underlying_asset_id,
                a_token_id,
                a_token_revision,
                v_token_id,
                v_token_revision,
                price_source,
                gho_link_token_id,
            })
        }
        "gho-discount-percent-updated" => {
            DeltaEffect::Configuration(ConfigurationEffect::GhoDiscountPercentUpdated {
                user_id: fields.number("user")?,
                new_discount_percent: fields.number("percent")?,
            })
        }
        "gho-discount-rate-strategy-updated" => {
            DeltaEffect::Configuration(ConfigurationEffect::GhoDiscountRateStrategyUpdated {
                gho_token_id: fields.number("gho")?,
                new_strategy: fields.optional_text("strategy")?,
            })
        }
        "gho-discount-token-updated" => {
            DeltaEffect::Configuration(ConfigurationEffect::GhoDiscountTokenUpdated {
                gho_token_id: fields.number("gho")?,
                new_discount_token: fields.optional_text("token")?,
            })
        }
        "stk-aave-transfer" => DeltaEffect::Configuration(ConfigurationEffect::StkAaveTransfer {
            from_user_id: fields.optional_number("from")?,
            to_user_id: fields.optional_number("to")?,
            amount: fields.wide("amount")?,
        }),
        "rewards-claimed" => DeltaEffect::Configuration(ConfigurationEffect::RewardsClaimed {
            user_id: fields.number("user")?,
            reward_token_id: fields.number("reward")?,
            claimer_id: fields.number("claimer")?,
            claimed_amount: fields.wide("amount")?,
        }),
        "upgraded" => DeltaEffect::Configuration(ConfigurationEffect::Upgraded {
            asset_id: fields.number("asset")?,
            market_id,
            is_a_token: match fields.take("leg")? {
                "collateral" => true,
                "debt" => false,
                _ => return Err(encoding_error(encoding)),
            },
            new_revision: fields.number("revision")?,
            deprecated_gho_token_id: fields.optional_number("deprecated-gho")?,
        }),
        "contract-revision-updated" => {
            DeltaEffect::Configuration(ConfigurationEffect::ContractRevisionUpdated {
                market_id: fields.number("market")?,
                contract_name: fields.text("name")?,
                new_revision: fields.number("revision")?,
            })
        }
        "pool-data-provider-updated" => {
            DeltaEffect::Configuration(ConfigurationEffect::PoolDataProviderUpdated {
                market_id: fields.number("market")?,
                old_address: fields.optional_text("old")?,
                new_address: fields.text("new")?,
            })
        }
        "contract-inserted" => DeltaEffect::Configuration(ConfigurationEffect::ContractInserted {
            market_id: fields.number("market")?,
            name: fields.text("name")?,
            address: fields.text("address")?,
            revision: fields.optional_number("revision")?,
        }),
        "contract-address-changed" => {
            DeltaEffect::Configuration(ConfigurationEffect::ContractAddressChanged {
                market_id: fields.number("market")?,
                name: fields.text("name")?,
                old_address: fields.text("old")?,
                new_address: fields.text("new")?,
            })
        }
        _ => return Err(encoding_error(encoding)),
    };
    if !fields.done() {
        return Err(encoding_error(encoding));
    }
    Ok(effect)
}

#[cfg(test)]
#[expect(clippy::too_many_lines, clippy::unwrap_used)]
mod tests;

impl CanonicalDelta {
    /// Reconstruct a journaled delta from the exact identity its row states, the
    /// generation relationship the row carries, and the versioned encoding.
    ///
    /// # Errors
    ///
    /// `ProjectionError::DeltaEncodingVersion` for an encoding version the
    /// vocabulary does not decode, and `ProjectionError::DeltaEncoding` when the
    /// text does not name the row's own position and generation, when an effect
    /// body is malformed, or when the reconstruction does not re-encode to the
    /// bytes it came from.
    pub fn decode(journaled: &JournaledDelta<'_>) -> Result<Self, ProjectionError> {
        if journaled.encoding_version != super::DELTA_ENCODING_VERSION {
            return Err(ProjectionError::DeltaEncodingVersion {
                version: journaled.encoding_version.to_string(),
            });
        }
        let bad = || encoding_error(journaled.encoding);
        let (position, rest) = journaled
            .encoding
            .split_once(" generation ")
            .ok_or_else(bad)?;
        if position != position_text(journaled.source) {
            return Err(bad());
        }
        let (generation_text, effect_text) = rest.split_once(" effect=").ok_or_else(bad)?;
        let generation = journaled
            .generation
            .as_ref()
            .map(generation_ref)
            .transpose()?;
        let stated =
            generation.map_or_else(|| "none".to_string(), |generation| generation.encode());
        if generation_text != stated {
            return Err(bad());
        }
        let effect = decode_effect(effect_text, journaled.source, journaled.market_id)?;
        let delta = Self::new(journaled.source, generation, effect);
        if delta.encode() != journaled.encoding {
            return Err(bad());
        }
        Ok(delta)
    }
}
