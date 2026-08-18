//! Aave V3 row structs (read-only). One per `models/aave.py` table; the
//! `BigInteger` (`IntMappedToString` `VARCHAR(78)`) columns decode to
//! [`alloy::primitives::U256`] at the row boundary, addresses to
//! [`alloy::primitives::Address`]. Column SELECT order matches each table's
//! `from_row` layout below.
//!
//! Writes for Aave belong to the AZGJUN writer-orchestration scope (Non-goals);
//! only the row structs + read decoding land here, so the snapshot reader can
//! surface them.

#![expect(dead_code)] // Aave read structs decode here (spike AC); the fetch_aave_*
                      // read fns + writer upserts land in AZGJUN, at which point these
                      // decoders are exercised. Defined now so the typed surface is
                      // ready for the consumer tasks.

use alloy::primitives::{Address, U256};
use rusqlite::Row;

use crate::error::DbError;
use crate::rows::decode::{decode_address, decode_opt_address, decode_u256};

macro_rules! aave_row {
    ($(#[$m:meta])* $name:ident { $($(#[$f:meta])* $field:ident : $ty:ty),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name { $($(#[$f])* pub $field : $ty),* }

        impl $name {
            $(#[$m])*
            #[doc = concat!(" Decode from a `SELECT` laid out in the column order documented above.")]
            pub(crate) fn from_row(row: &Row<'_>) -> Result<Self, DbError> {
                let mut idx: usize = 0;
                $(
                    let $field = row_get(row, idx, stringify!($field))?;
                    idx += 1;
                )*
                let _ = idx;
                Ok(Self { $($field),* })
            }
        }
    };
}

/// helper: pull column `idx` as the right type. The macro routes each field
/// through this so the `U256`/`Address`/`Option<…>` decode is centralized.
fn row_get<T>(row: &Row<'_>, idx: usize, _name: &str) -> Result<T, DbError>
where
    T: RowGet,
{
    T::row_get(row, idx)
}

trait RowGet: Sized {
    fn row_get(row: &Row<'_>, idx: usize) -> Result<Self, DbError>;
}

impl RowGet for i64 {
    fn row_get(row: &Row<'_>, idx: usize) -> Result<Self, DbError> {
        Ok(row.get(idx)?)
    }
}

impl RowGet for i32 {
    fn row_get(row: &Row<'_>, idx: usize) -> Result<Self, DbError> {
        Ok(row.get(idx)?)
    }
}

impl RowGet for bool {
    fn row_get(row: &Row<'_>, idx: usize) -> Result<Self, DbError> {
        Ok(row.get(idx)?)
    }
}

impl RowGet for String {
    fn row_get(row: &Row<'_>, idx: usize) -> Result<Self, DbError> {
        Ok(row.get(idx)?)
    }
}

impl RowGet for U256 {
    fn row_get(row: &Row<'_>, idx: usize) -> Result<Self, DbError> {
        decode_u256(&row.get::<_, String>(idx)?)
    }
}

impl RowGet for Address {
    fn row_get(row: &Row<'_>, idx: usize) -> Result<Self, DbError> {
        decode_address(&row.get::<_, String>(idx)?)
    }
}

impl RowGet for Option<i64> {
    fn row_get(row: &Row<'_>, idx: usize) -> Result<Self, DbError> {
        Ok(row.get(idx)?)
    }
}

impl RowGet for Option<String> {
    fn row_get(row: &Row<'_>, idx: usize) -> Result<Self, DbError> {
        Ok(row.get(idx)?)
    }
}

impl RowGet for Option<U256> {
    fn row_get(row: &Row<'_>, idx: usize) -> Result<Self, DbError> {
        // Inlined rather than `decode_opt_u256` so that helper stays
        // reference-free while this module is dead (see its `expect` in
        // rows/decode.rs).
        row.get::<_, Option<String>>(idx)?
            .as_deref()
            .map(decode_u256)
            .transpose()
    }
}

impl RowGet for Option<Address> {
    fn row_get(row: &Row<'_>, idx: usize) -> Result<Self, DbError> {
        decode_opt_address(row.get::<_, Option<String>>(idx)?.as_deref())
    }
}

aave_row! {
    /// `aave_v3_markets` row (columns: `id, chain_id, name, active, last_update_block`).
    AaveV3MarketRow {
        id: i64,
        chain_id: i64,
        name: String,
        active: bool,
        last_update_block: Option<i64>,
    }
}
aave_row! {
    /// `aave_v3_emode_categories` row. Columns: `id, market_id, category_id, label,
    /// ltv, liquidation_threshold, liquidation_bonus, price_source`.
    AaveV3EModeCategoryRow {
        id: i64,
        market_id: i64,
        category_id: i64,
        label: Option<String>,
        ltv: i64,
        liquidation_threshold: i64,
        liquidation_bonus: i64,
        price_source: Option<Address>,
    }
}
aave_row! {
    /// `aave_v3_contracts` row. Columns: `id, market_id, name, address, revision`.
    AaveV3ContractRow {
        id: i64,
        market_id: i64,
        name: String,
        address: Address,
        revision: Option<i64>,
    }
}
aave_row! {
    /// `aave_v3_users` row. Columns: `id, market_id, address, e_mode, gho_discount,
    /// stk_aave_balance, isolation_mode_collateral_asset_id, isolation_mode_debt`.
    AaveV3UserRow {
        id: i64,
        market_id: i64,
        address: Address,
        e_mode: i64,
        gho_discount: i64,
        stk_aave_balance: Option<U256>,
        isolation_mode_collateral_asset_id: Option<i64>,
        isolation_mode_debt: U256,
    }
}
aave_row! {
    /// `aave_v3_assets` row. Columns: `id, market_id, underlying_asset_id,
    /// a_token_id, a_token_revision, v_token_id, v_token_revision,
    /// e_mode_category_id, price_source, last_update_block, liquidity_index,
    /// liquidity_rate, borrow_index, borrow_rate`.
    AaveV3AssetRow {
        id: i64,
        market_id: i64,
        underlying_asset_id: i64,
        a_token_id: i64,
        a_token_revision: i64,
        v_token_id: i64,
        v_token_revision: i64,
        e_mode_category_id: Option<i64>,
        price_source: Option<Address>,
        last_update_block: Option<i64>,
        liquidity_index: U256,
        liquidity_rate: U256,
        borrow_index: U256,
        borrow_rate: U256,
    }
}
aave_row! {
    /// `aave_v3_asset_configs` row. Columns: `id, asset_id, ltv,
    /// liquidation_threshold, liquidation_bonus, e_mode_category_id,
    /// borrowing_enabled, stable_borrowing_enabled, flash_loan_enabled,
    /// isolation_mode, borrowable_in_isolation, debt_ceiling`.
    AaveV3AssetConfigRow {
        id: i64,
        asset_id: i64,
        ltv: i64,
        liquidation_threshold: i64,
        liquidation_bonus: i64,
        e_mode_category_id: Option<i64>,
        borrowing_enabled: bool,
        stable_borrowing_enabled: bool,
        flash_loan_enabled: bool,
        isolation_mode: bool,
        borrowable_in_isolation: bool,
        debt_ceiling: Option<U256>,
    }
}
aave_row! {
    /// `aave_v3_collateral_positions` row. Columns: `id, user_id, asset_id,
    /// balance, last_index`.
    AaveV3CollateralPositionRow {
        id: i64,
        user_id: i64,
        asset_id: i64,
        balance: U256,
        last_index: Option<U256>,
    }
}
aave_row! {
    /// `aave_v3_debt_positions` row. Columns: `id, user_id, asset_id, balance,
    /// last_index`.
    AaveV3DebtPositionRow {
        id: i64,
        user_id: i64,
        asset_id: i64,
        balance: U256,
        last_index: Option<U256>,
    }
}
aave_row! {
    /// `aave_v3_user_collateral_configs` row. Columns: `id, user_id, asset_id,
    /// enabled`.
    AaveV3UserCollateralConfigRow {
        id: i64,
        user_id: i64,
        asset_id: i64,
        enabled: bool,
    }
}
aave_row! {
    /// `aave_gho_tokens` row. Columns: `id, token_id, v_token_id,
    /// v_gho_discount_rate_strategy, v_gho_discount_token`.
    AaveGhoTokenRow {
        id: i64,
        token_id: i64,
        v_token_id: Option<i64>,
        v_gho_discount_rate_strategy: Option<Address>,
        v_gho_discount_token: Option<Address>,
    }
}
