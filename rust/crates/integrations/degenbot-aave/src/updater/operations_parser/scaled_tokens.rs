use super::{
    addr_to_hex, log_idx_value, AaveV3Erc20TransferEvent, AaveV3ScaledTokenBalanceTransferEvent,
    AaveV3ScaledTokenBurnEvent, AaveV3ScaledTokenMintEvent, Address, Log, OptionalExtension,
    ParseError, ScaledTokenEvent, ScaledTokenEventData, ScaledTokenEventType,
    TransactionOperationsParser, U256,
};
use crate::operations::TokenType;

impl<'a> TransactionOperationsParser<'a, '_> {
    // ── the decode wrappers (mint/burn/balance-transfer/ERC20-transfer) ──

    /// Decodes a `ScaledTokenMint` log via
    /// the event decoder + classifies by emitter-address → [`ScaledTokenEventType`]
    /// (`GhoDebtMint` if the emitter is `gho_vtoken_address`; `CollateralMint` if
    /// aToken; `DebtMint` if vToken).
    pub(super) fn decode_mint_event(
        &mut self,
        log: &'a Log,
        ev: &AaveV3ScaledTokenMintEvent,
    ) -> Result<ScaledTokenEvent<'a>, ParseError> {
        let token_address = ev.token_address;
        let event_type = self.classify_mint_burn(token_address, "mint")?;
        Ok(ScaledTokenEvent {
            log,
            decoded: ScaledTokenEventData::Mint {
                caller: ev.caller,
                on_behalf_of: ev.on_behalf_of,
                value: ev.value,
                balance_increase: ev.balance_increase,
                index: ev.index,
            },
            event_type,
            token_address,
            user_address: ev.on_behalf_of,
            caller_address: Some(ev.caller),
            from_address: None,
            target_address: None,
            amount: ev.value,
            balance_increase: Some(ev.balance_increase),
            index: Some(ev.index),
            log_index: log_idx_value(log),
        })
    }

    /// Decodes a `ScaledTokenBurn` log +
    /// classifies by emitter-address (`GhoDebtBurn` / `CollateralBurn` / `DebtBurn`).
    pub(super) fn decode_burn_event(
        &mut self,
        log: &'a Log,
        ev: &AaveV3ScaledTokenBurnEvent,
    ) -> Result<ScaledTokenEvent<'a>, ParseError> {
        let token_address = ev.token_address;
        let event_type = self.classify_mint_burn(token_address, "burn")?;
        Ok(ScaledTokenEvent {
            log,
            decoded: ScaledTokenEventData::Burn {
                from: ev.from,
                target: ev.target,
                value: ev.value,
                balance_increase: ev.balance_increase,
                index: ev.index,
            },
            event_type,
            token_address,
            user_address: ev.from,
            caller_address: None,
            from_address: Some(ev.from),
            target_address: Some(ev.target),
            amount: ev.value,
            balance_increase: Some(ev.balance_increase),
            index: Some(ev.index),
            log_index: log_idx_value(log),
        })
    }

    /// Decodes a
    /// `ScaledTokenBalanceTransfer` log + classifies by emitter-address
    /// (`CollateralTransfer` / `DebtTransfer` / `GhoDebtTransfer`). NB: GHO vToken
    /// doesn't emit `BalanceTransfer` in practice (the GHO mechanism uses plain
    /// ERC20 Transfer for the user→user movement) — but the classification
    /// path covers it defensively.
    pub(super) fn decode_balance_transfer_event(
        &mut self,
        log: &'a Log,
        ev: &AaveV3ScaledTokenBalanceTransferEvent,
    ) -> Result<ScaledTokenEvent<'a>, ParseError> {
        let token_address = ev.token_address;
        let event_type = self.classify_transfer(token_address)?;
        // BalanceTransfer has balance_increase = 0.
        Ok(ScaledTokenEvent {
            log,
            decoded: ScaledTokenEventData::BalanceTransfer {
                from: ev.from,
                to: ev.to,
                value: ev.value,
                index: ev.index,
            },
            event_type,
            token_address,
            user_address: ev.from,
            caller_address: None,
            from_address: Some(ev.from),
            target_address: Some(ev.to),
            amount: ev.value,
            balance_increase: Some(U256::ZERO),
            index: Some(ev.index),
            log_index: log_idx_value(log),
        })
    }

    /// Decodes a plain ERC20 `Transfer`
    /// + classifies: GHO vToken → `GhoDebtTransfer`; aToken →
    ///   `Erc20CollateralTransfer`; vToken → `Erc20DebtTransfer`;
    ///   the GHO-discount-token → `DiscountTransfer`; otherwise `Ok(None)` (the
    ///   log is for an unrelated contract — the parser skips it). `Err` is a
    ///   failed classification lookup: the parse aborts, never a silent skip.
    pub(super) fn decode_transfer_event(
        &mut self,
        log: &'a Log,
        ev: &AaveV3Erc20TransferEvent,
    ) -> Result<Option<ScaledTokenEvent<'a>>, ParseError> {
        let token_address = ev.token_address;
        let event_type = if self.gho_vtoken_address == Some(token_address) {
            ScaledTokenEventType::GhoDebtTransfer
        } else {
            let Some(kind) = self.classify_token_type(token_address)? else {
                return Ok(None);
            };
            match kind {
                TokenType::Atoken => ScaledTokenEventType::Erc20CollateralTransfer,
                TokenType::Vtoken => ScaledTokenEventType::Erc20DebtTransfer,
                TokenType::GhoDiscount => ScaledTokenEventType::DiscountTransfer,
            }
        };
        Ok(Some(ScaledTokenEvent {
            log,
            decoded: ScaledTokenEventData::Transfer {
                from: ev.from,
                to: ev.to,
                value: ev.value,
            },
            event_type,
            token_address,
            user_address: ev.from,
            caller_address: None,
            from_address: Some(ev.from),
            target_address: Some(ev.to),
            amount: ev.value,
            balance_increase: None,
            index: None,
            log_index: log_idx_value(log),
        }))
    }

    /// Classify a Mint/Burn event's emitter: `(kind, event_category)` →
    /// `CollateralMint`/`CollateralBurn` (aToken), `DebtMint`/`DebtBurn`
    /// (vToken), `GhoDebtMint`/`GhoDebtBurn` (GHO vToken, matched first so that
    /// address never reaches the asset-table lookups).
    ///
    /// # Panics
    /// Panics if the emitter is neither GHO-vToken, a known aToken, nor a
    /// known vToken for this market. The caller should pre-filter via
    /// `classify_token_type` if a non-panic is needed.
    #[expect(clippy::panic_in_result_fn)] // the invariant-break panic is the documented contract
    fn classify_mint_burn(
        &mut self,
        token_address: Address,
        event_category: &str,
    ) -> Result<ScaledTokenEventType, ParseError> {
        if self.gho_vtoken_address == Some(token_address) {
            return Ok(match event_category {
                "mint" => ScaledTokenEventType::GhoDebtMint,
                "burn" => ScaledTokenEventType::GhoDebtBurn,
                _ => unreachable!("event_category is mint|burn"),
            });
        }
        Ok(
            match (self.require_token_kind(token_address)?, event_category) {
                (TokenType::Atoken, "mint") => ScaledTokenEventType::CollateralMint,
                (TokenType::Atoken, "burn") => ScaledTokenEventType::CollateralBurn,
                (TokenType::Vtoken, "mint") => ScaledTokenEventType::DebtMint,
                (TokenType::Vtoken, "burn") => ScaledTokenEventType::DebtBurn,
                // The discount token emits plain `Transfer`, never a scaled Mint/Burn.
                (TokenType::GhoDiscount, _) => {
                    #[expect(clippy::panic)] // impossible emitter = invariant break (loud)
                    {
                        panic!(
                        "discount token {token_address} emitted a scaled {event_category} (market {})",
                        self.market_id
                    )
                    }
                }
                (_, _) => unreachable!("event_category is mint|burn"),
            },
        )
    }

    /// Classify a `BalanceTransfer` event's emitter → `CollateralTransfer` /
    /// `DebtTransfer` / `GhoDebtTransfer`.
    fn classify_transfer(
        &mut self,
        token_address: Address,
    ) -> Result<ScaledTokenEventType, ParseError> {
        if self.gho_vtoken_address == Some(token_address) {
            return Ok(ScaledTokenEventType::GhoDebtTransfer);
        }
        Ok(match self.require_token_kind(token_address)? {
            TokenType::Atoken => ScaledTokenEventType::CollateralTransfer,
            TokenType::Vtoken => ScaledTokenEventType::DebtTransfer,
            TokenType::GhoDiscount => ScaledTokenEventType::DiscountTransfer,
        })
    }

    /// The emitter's asset-table kind. An address that is none of this
    /// market's tokens is an invariant break: the caller pre-filters with
    /// `classify_token_type` when a skip is wanted.
    #[expect(clippy::panic_in_result_fn)] // the invariant-break panic is the documented contract
    fn require_token_kind(&mut self, token_address: Address) -> Result<TokenType, ParseError> {
        #[expect(clippy::panic)] // unexpected token = invariant break; fail loudly (documented)
        match self.classify_token_type(token_address)? {
            Some(kind) => Ok(kind),
            None => {
                panic!(
                    "unexpected token at {token_address} for market {}",
                    self.market_id
                )
            }
        }
    }

    /// Classify a token-address by which asset-table column matches (aToken /
    /// vToken / GHO-discount-token). `Ok(None)` only after every lookup
    /// succeeds and proves the address isn't any of those (the parser skips
    /// the log); a failed lookup is `Err` — the parse aborts loudly, never a
    /// silent skip.
    pub(super) fn classify_token_type(
        &mut self,
        token_address: Address,
    ) -> Result<Option<TokenType>, ParseError> {
        let addr_hex = addr_to_hex(token_address);
        // Try aToken first.
        if self
            .substrate
            .lookup_asset_id(self.conn, self.market_id, "a_token", &addr_hex)
            .map_err(|e| ParseError::Substrate(format!("a_token lookup for {addr_hex}: {e}")))?
            .is_some()
        {
            return Ok(Some(TokenType::Atoken));
        }
        if self
            .substrate
            .lookup_asset_id(self.conn, self.market_id, "v_token", &addr_hex)
            .map_err(|e| ParseError::Substrate(format!("v_token lookup for {addr_hex}: {e}")))?
            .is_some()
        {
            return Ok(Some(TokenType::Vtoken));
        }
        // GHO-discount-token check (the v_gho_discount_token column on
        // aave_gho_tokens; only set if the discount mechanism is active).
        let discount_token_row: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT g.v_gho_discount_token FROM aave_gho_tokens g
                 JOIN erc20_tokens t ON t.id = g.token_id
                 WHERE t.chain = ?1 AND g.v_gho_discount_token IS NOT NULL",
                rusqlite::params![self.chain_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|e| {
                ParseError::Substrate(format!("discount-token lookup for {addr_hex}: {e}"))
            })?;
        if discount_token_row
            .flatten()
            .is_some_and(|s| s.eq_ignore_ascii_case(&addr_hex))
        {
            return Ok(Some(TokenType::GhoDiscount));
        }
        Ok(None)
    }
}
