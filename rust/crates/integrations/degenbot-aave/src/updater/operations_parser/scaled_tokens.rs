use super::{
    addr_to_hex, log_idx_value, AaveV3Erc20TransferEvent, AaveV3ScaledTokenBalanceTransferEvent,
    AaveV3ScaledTokenBurnEvent, AaveV3ScaledTokenMintEvent, Address, DegenbotDb, Log,
    OptionalExtension, ScaledTokenEvent, ScaledTokenEventData, ScaledTokenEventType,
    TransactionOperationsParser, U256,
};

impl<'a> TransactionOperationsParser<'a> {
    // ── the decode wrappers (mint/burn/balance-transfer/ERC20-transfer) ──

    /// Decodes a `ScaledTokenMint` log via
    /// the event decoder + classifies by emitter-address → [`ScaledTokenEventType`]
    /// (`GhoDebtMint` if the emitter is `gho_vtoken_address`; `CollateralMint` if
    /// aToken; `DebtMint` if vToken).
    pub(super) fn decode_mint_event(
        &self,
        log: &'a Log,
        ev: &AaveV3ScaledTokenMintEvent,
    ) -> ScaledTokenEvent<'a> {
        let token_address = ev.token_address;
        let event_type = self.classify_mint_burn(token_address, "mint");
        ScaledTokenEvent {
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
        }
    }

    /// Decodes a `ScaledTokenBurn` log +
    /// classifies by emitter-address (`GhoDebtBurn` / `CollateralBurn` / `DebtBurn`).
    pub(super) fn decode_burn_event(
        &self,
        log: &'a Log,
        ev: &AaveV3ScaledTokenBurnEvent,
    ) -> ScaledTokenEvent<'a> {
        let token_address = ev.token_address;
        let event_type = self.classify_mint_burn(token_address, "burn");
        ScaledTokenEvent {
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
        }
    }

    /// Decodes a
    /// `ScaledTokenBalanceTransfer` log + classifies by emitter-address
    /// (`CollateralTransfer` / `DebtTransfer` / `GhoDebtTransfer`). NB: GHO vToken
    /// doesn't emit `BalanceTransfer` in practice (the GHO mechanism uses plain
    /// ERC20 Transfer for the user→user movement) — but the classification
    /// path covers it defensively.
    pub(super) fn decode_balance_transfer_event(
        &self,
        log: &'a Log,
        ev: &AaveV3ScaledTokenBalanceTransferEvent,
    ) -> ScaledTokenEvent<'a> {
        let token_address = ev.token_address;
        let event_type = self.classify_transfer(token_address);
        // BalanceTransfer has balance_increase = 0.
        ScaledTokenEvent {
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
        }
    }

    /// Decodes a plain ERC20 `Transfer`
    /// + classifies: GHO vToken → `GhoDebtTransfer`; aToken →
    ///   `Erc20CollateralTransfer`; vToken → `Erc20DebtTransfer`;
    ///   the GHO-discount-token → `DiscountTransfer`; otherwise `None` (the log is
    ///   for an unrelated contract — the parser skips it).
    pub(super) fn decode_transfer_event(
        &self,
        log: &'a Log,
        ev: &AaveV3Erc20TransferEvent,
    ) -> Option<ScaledTokenEvent<'a>> {
        let token_address = ev.token_address;
        let event_type = if self.gho_vtoken_address == Some(token_address) {
            ScaledTokenEventType::GhoDebtTransfer
        } else {
            self.classify_token_type(token_address)?
        };
        Some(ScaledTokenEvent {
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
        })
    }

    /// Classify a Mint/Burn event's emitter token address. Returns the matching
    /// [`ScaledTokenEventType`]; the caller's match-arm on the
    /// `event_category` ("mint"/"burn") maps to `CollateralMint` / `DebtMint` /
    /// `GhoDebtMint` (and the corresponding burn variants).
    ///
    /// # Panics
    /// Panics if the emitter is neither GHO-vToken, a known aToken, nor a
    /// known vToken for this market. The caller should pre-filter via
    /// `classify_token_type` if a non-panic is needed.
    fn classify_mint_burn(
        &self,
        token_address: Address,
        event_category: &str,
    ) -> ScaledTokenEventType {
        if self.gho_vtoken_address == Some(token_address) {
            return match event_category {
                "mint" => ScaledTokenEventType::GhoDebtMint,
                "burn" => ScaledTokenEventType::GhoDebtBurn,
                _ => unreachable!("event_category is mint|burn"),
            };
        }
        #[expect(clippy::panic)] // unexpected token = invariant break; fail loudly (documented)
        let token_type = self.classify_token_type(token_address).unwrap_or_else(|| {
            panic!(
                "unexpected token at {token_address} for market {}",
                self.market_id
            )
        });
        match (token_type, event_category) {
            (
                ScaledTokenEventType::CollateralMint
                | ScaledTokenEventType::CollateralTransfer
                | ScaledTokenEventType::Erc20CollateralTransfer,
                "mint",
            ) => ScaledTokenEventType::CollateralMint,
            (
                ScaledTokenEventType::CollateralBurn
                | ScaledTokenEventType::CollateralTransfer
                | ScaledTokenEventType::Erc20CollateralTransfer,
                "burn",
            ) => ScaledTokenEventType::CollateralBurn,
            (
                ScaledTokenEventType::DebtMint
                | ScaledTokenEventType::DebtTransfer
                | ScaledTokenEventType::Erc20DebtTransfer,
                "mint",
            ) => ScaledTokenEventType::DebtMint,
            (
                ScaledTokenEventType::DebtBurn
                | ScaledTokenEventType::DebtTransfer
                | ScaledTokenEventType::Erc20DebtTransfer,
                "burn",
            ) => ScaledTokenEventType::DebtBurn,
            // classify_token_type returns a transfer variant on aToken/vToken;
            // re-derive here (the Python's helper). The conversion is
            // aToken → CollateralMint/Burn, vToken → DebtMint/Burn.
            _ => {
                #[expect(clippy::panic)] // non-token-type variant = invariant break (loud)
                {
                    panic!("classify_token_type returned a non-token-type variant for token at {token_address} (event_category={event_category})")
                }
            }
        }
    }

    /// Classify a `BalanceTransfer` event's emitter → `CollateralTransfer` /
    /// `DebtTransfer` / `GhoDebtTransfer`.
    fn classify_transfer(&self, token_address: Address) -> ScaledTokenEventType {
        if self.gho_vtoken_address == Some(token_address) {
            return ScaledTokenEventType::GhoDebtTransfer;
        }
        #[expect(clippy::panic)] // unexpected token = invariant break; fail loudly (documented)
        let token_type = self.classify_token_type(token_address).unwrap_or_else(|| {
            panic!(
                "unexpected token at {token_address} for market {}",
                self.market_id
            )
        });
        // classify_token_type returned a transfer variant — re-derive.
        match token_type {
            ScaledTokenEventType::DebtTransfer | ScaledTokenEventType::Erc20DebtTransfer => {
                ScaledTokenEventType::DebtTransfer
            }
            _ => ScaledTokenEventType::CollateralTransfer,
        }
    }

    /// Classify a token-address by which asset-table column matches (aToken /
    /// vToken / GHO-discount-token). Returns `None`
    /// if the address isn't any of those (the parser skips the log).
    ///
    /// Returns a [`ScaledTokenEventType`] *transfer variant* as the
    /// discriminator (the caller's match-arm maps mint/burn variants as
    /// needed). This is a slight overloading of the enum but matches the
    /// Python's three-way classification surface.
    fn classify_token_type(&self, token_address: Address) -> Option<ScaledTokenEventType> {
        let addr_hex = addr_to_hex(token_address);
        // Try aToken first.
        if DegenbotDb::lookup_asset_id_by_token_address_on_conn(
            self.conn,
            self.market_id,
            &addr_hex,
            "a_token",
        )
        .ok()
        .flatten()
        .is_some()
        {
            return Some(ScaledTokenEventType::Erc20CollateralTransfer);
        }
        if DegenbotDb::lookup_asset_id_by_token_address_on_conn(
            self.conn,
            self.market_id,
            &addr_hex,
            "v_token",
        )
        .ok()
        .flatten()
        .is_some()
        {
            return Some(ScaledTokenEventType::Erc20DebtTransfer);
        }
        // GHO-discount-token check (the v_gho_discount_token column on
        // aave_gho_tokens; only set if the discount mechanism is active).
        if self
            .conn
            .query_row(
                "SELECT g.v_gho_discount_token FROM aave_gho_tokens g
                 JOIN erc20_tokens t ON t.id = g.token_id
                 WHERE t.chain = ?1 AND g.v_gho_discount_token IS NOT NULL",
                rusqlite::params![self.chain_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten()
            .is_some_and(|s| s.eq_ignore_ascii_case(&addr_hex))
        {
            return Some(ScaledTokenEventType::DiscountTransfer);
        }
        None
    }
}
