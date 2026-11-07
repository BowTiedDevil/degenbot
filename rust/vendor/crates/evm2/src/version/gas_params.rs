use crate::{evm::SStore, utils::num_words};
use alloy_primitives::U256;
use core::ops::{Index, IndexMut};

macro_rules! gas_ids {
    ($($tokens:tt)*) => {
        gas_ids_find_last! { [] $($tokens)* }
    };
}

macro_rules! gas_ids_find_last {
    ([$($variants:tt)*] #[$last_doc:meta] $last_variant:ident;) => {
        gas_ids_impl! { [$($variants)*] #[$last_doc] $last_variant; }
    };
    ([$($variants:tt)*] #[$doc:meta] $variant:ident; $($rest:tt)+) => {
        gas_ids_find_last! { [$($variants)* #[$doc] $variant;] $($rest)+ }
    };
}

macro_rules! gas_ids_impl {
    (
        [#[$first_doc:meta] $first_variant:ident; $(#[$doc:meta] $variant:ident;)*]
        #[$last_doc:meta] $last_variant:ident;
    ) => {
        /// Gas parameter identifier.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        #[repr(u8)]
        pub enum GasId {
            #[$first_doc]
            $first_variant = 0,
            $(
                #[$doc]
                $variant,
            )*
            #[$last_doc]
            $last_variant,
        }

        impl GasId {
            /// Smallest gas parameter identifier.
            pub const MIN: Self = Self::$first_variant;

            /// Largest gas parameter identifier.
            pub const MAX: Self = Self::$last_variant;

            /// Number of gas parameter identifiers.
            pub const COUNT: usize = Self::MAX as usize + 1;

            /// Returns the gas parameter for a raw identifier.
            #[inline]
            pub const fn from_usize(value: usize) -> Option<Self> {
                if value <= (Self::MAX as usize) {
                    // SAFETY: `GasId` is `repr(u8)`, starts at 0, and every variant up to
                    // `MAX` is assigned contiguously by the enum declaration.
                    return Some(unsafe { core::mem::transmute::<u8, Self>(value as u8) });
                }
                None
            }

            // TODO: Do we even need string names for gas IDs?
            // pub fn from_name(name: &str) -> Option<Self> { ... }
            // pub const fn name(self) -> &'static str { ... }

            /// Returns the gas parameter identifier as a table index.
            #[inline]
            pub const fn as_usize(self) -> usize {
                self as usize
            }
        }
    };
}

gas_ids! {
    /// Gas charged per non-zero byte in `EXP` exponent.
    ExpByte;
    /// Gas charged per copied word in `EXTCODECOPY`.
    ExtcodecopyPerWord;
    /// Gas charged per copied word.
    CopyPerWord;
    /// Gas charged per byte of log data.
    Logdata;
    /// Gas charged per log topic.
    Logtopic;
    /// Gas charged per copied word in `MCOPY`.
    McopyPerWord;
    /// Gas charged per hashed word in `KECCAK256`.
    Keccak256PerWord;
    /// Linear memory gas coefficient.
    MemoryLinearCost;
    /// Quadratic memory gas divisor.
    MemoryQuadraticReduction;
    /// Gas charged per initcode word.
    InitcodePerWord;
    /// Gas charged by `CREATE`.
    Create;
    /// Call gas stipend reduction divisor.
    CallStipendReduction;
    /// Gas charged when a call transfers value.
    TransferValueCost;
    /// Additional gas charged for a cold account access.
    ColdAccountAdditionalCost;
    /// Gas charged for creating a new account.
    NewAccountCost;
    /// Gas charged for a warm storage read.
    WarmStorageReadCost;
    /// Static `SSTORE` gas.
    SstoreStatic;
    /// Gas charged by `SSTORE` for setting a slot, excluding the load.
    SstoreSetWithoutLoadCost;
    /// Gas charged by `SSTORE` for resetting a slot, excluding a cold load.
    SstoreResetWithoutColdLoadCost;
    /// Refund for clearing a storage slot.
    SstoreClearingSlotRefund;
    /// `SELFDESTRUCT` refund.
    SelfdestructRefund;
    /// Gas stipend for a value-transferring call.
    CallStipend;
    /// Maximum transaction gas refund quotient.
    MaxRefundQuotient;
    /// Additional gas charged for cold storage.
    ColdStorageAdditionalCost;
    /// Gas charged for cold storage.
    ColdStorageCost;
    /// New account cost charged by `SELFDESTRUCT`.
    NewAccountCostForSelfdestruct;
    /// Gas charged per deposited code byte.
    CodeDepositCost;
    /// EIP-7702 transaction cost per empty account.
    TxEip7702PerEmptyAccountCost;
    /// Transaction token multiplier for non-zero bytes.
    TxTokenNonZeroByteMultiplier;
    /// Transaction token base cost.
    TxTokenCost;
    /// Transaction floor cost per token.
    TxFloorCostPerToken;
    /// Transaction floor base gas.
    TxFloorCostBase;
    /// Multiplier for a zero calldata byte in the floor-tokens calculation (EIP-7623 `1`, EIP-7976 `4`).
    TxFloorZeroByteMultiplier;
    /// Transaction access-list address cost.
    TxAccessListAddressCost;
    /// Transaction access-list storage-key cost.
    TxAccessListStorageKeyCost;
    /// Floor tokens charged per access-list byte (EIP-7981).
    TxAccessListFloorByteMultiplier;
    /// Transaction base stipend.
    TxBaseStipend;
    /// Transaction create cost.
    TxCreateCost;
    /// Transaction initcode cost.
    TxInitcodeCost;
    /// `SSTORE` set refund.
    SstoreSetRefund;
    /// `SSTORE` reset refund.
    SstoreResetRefund;
    /// EIP-7702 transaction authorization refund.
    TxEip7702AuthRefund;
    /// `SSTORE` set state gas.
    SstoreSetState;
    /// New account state gas.
    NewAccountState;
    /// Code deposit state gas.
    CodeDepositState;
    /// `CREATE` state gas.
    CreateState;
    /// EIP-7702 transaction state gas per authorization.
    TxEip7702PerAuthState;

    /// EIP-2780 additional intrinsic charge for a value-bearing non-create, non-self transaction.
    TxValueCost;
    /// EIP-2780/EIP-8038 execution gas cost of a top-level CREATE access.
    TxCreateAccessCost;

    // Reserved custom gas parameter slots.

    /// Reserved custom gas parameter slot 0.
    Custom0;
    /// Reserved custom gas parameter slot 1.
    Custom1;
    /// Reserved custom gas parameter slot 2.
    Custom2;
    /// Reserved custom gas parameter slot 3.
    Custom3;
    /// Reserved custom gas parameter slot 4.
    Custom4;
    /// Reserved custom gas parameter slot 5.
    Custom5;
    /// Reserved custom gas parameter slot 6.
    Custom6;
    /// Reserved custom gas parameter slot 7.
    Custom7;
}

/// Dynamic gas parameter table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GasParams {
    table: [u32; GasId::COUNT],
    _align: [usize; 0],
}

impl Index<GasId> for GasParams {
    type Output = u32;

    #[inline]
    fn index(&self, id: GasId) -> &Self::Output {
        &self.table[id.as_usize()]
    }
}

impl IndexMut<GasId> for GasParams {
    #[inline]
    fn index_mut(&mut self, id: GasId) -> &mut Self::Output {
        self.get_mut(id)
    }
}

impl GasParams {
    /// Creates empty gas parameters.
    #[inline]
    pub(super) const fn empty() -> Self {
        Self { table: [0; GasId::COUNT], _align: [] }
    }

    /// Returns the gas cost for `id`.
    #[inline]
    pub const fn get(&self, id: GasId) -> u64 {
        self.table[id.as_usize()] as u64
    }

    /// Returns the mutable gas cost slot for `id`.
    #[inline]
    pub const fn get_mut(&mut self, id: GasId) -> &mut u32 {
        &mut self.table[id.as_usize()]
    }

    /// Sets the gas cost for `id`.
    #[inline]
    pub const fn set(&mut self, id: GasId, cost: u32) {
        self.table[id.as_usize()] = cost;
    }

    /// Calculates memory expansion cost for `len` words.
    #[inline]
    pub const fn memory_cost(&self, len: usize) -> u64 {
        let len = len as u64;
        self.get(GasId::MemoryLinearCost)
            .saturating_mul(len)
            .saturating_add(len.saturating_mul(len) / self.get(GasId::MemoryQuadraticReduction))
    }

    /// Calculates dynamic `EXP` gas.
    #[inline]
    pub const fn exp_cost(&self, power: U256) -> u64 {
        if power.const_is_zero() {
            return 0;
        }
        self.get(GasId::ExpByte).saturating_mul(power.bit_len().div_ceil(8) as u64)
    }

    /// Calculates the per-word gas `id` for `len` bytes.
    #[inline]
    const fn per_word_cost(&self, id: GasId, len: usize) -> u64 {
        self.get(id).saturating_mul(num_words(len) as u64)
    }

    /// Calculates copy gas for `len` bytes.
    #[inline]
    pub const fn copy_cost(&self, len: usize) -> u64 {
        self.per_word_cost(GasId::CopyPerWord, len)
    }

    /// Calculates `EXTCODECOPY` copy gas for `len` bytes.
    #[inline]
    pub const fn extcodecopy_cost(&self, len: usize) -> u64 {
        self.per_word_cost(GasId::ExtcodecopyPerWord, len)
    }

    /// Calculates `MCOPY` copy gas for `len` bytes.
    #[inline]
    pub const fn mcopy_cost(&self, len: usize) -> u64 {
        self.per_word_cost(GasId::McopyPerWord, len)
    }

    /// Calculates `KECCAK256` word gas for `len` bytes.
    #[inline]
    pub const fn keccak256_word_cost(&self, len: usize) -> u64 {
        self.per_word_cost(GasId::Keccak256PerWord, len)
    }

    /// Calculates dynamic `LOG` gas.
    #[inline]
    pub const fn log_cost(&self, n: u8, len: usize) -> u64 {
        self.get(GasId::Logdata)
            .saturating_mul(len as u64)
            .saturating_add(self.get(GasId::Logtopic).saturating_mul(n as u64))
    }

    /// Calculates initcode word gas for `len` bytes.
    #[inline]
    pub const fn initcode_cost(&self, len: usize) -> u64 {
        self.per_word_cost(GasId::InitcodePerWord, len)
    }

    /// Calculates dynamic `CREATE2` gas for `len` bytes.
    #[inline]
    pub const fn create2_cost(&self, len: usize) -> u64 {
        self.get(GasId::Create).saturating_add(self.keccak256_word_cost(len))
    }

    /// Returns `CALL` stipend reduction.
    #[inline]
    pub const fn call_stipend_reduction(&self, gas_limit: u64) -> u64 {
        gas_limit - gas_limit / self.get(GasId::CallStipendReduction)
    }

    /// Calculates dynamic `SSTORE` gas.
    #[inline]
    pub fn sstore_dynamic_gas(&self, is_eip2200: bool, vals: &SStore) -> u64 {
        if !is_eip2200 {
            if vals.present_is_zero() && !vals.new_is_zero() {
                return self.get(GasId::SstoreSetWithoutLoadCost);
            }
            return self.get(GasId::SstoreResetWithoutColdLoadCost);
        }

        let mut gas = 0;
        if vals.is_cold {
            gas += self.get(GasId::ColdStorageCost);
        }

        if !vals.is_noop() && vals.is_clean() {
            gas += if vals.original_is_zero() {
                self.get(GasId::SstoreSetWithoutLoadCost)
            } else {
                self.get(GasId::SstoreResetWithoutColdLoadCost)
            };
        }
        gas
    }

    /// Calculates `SSTORE` refund.
    #[inline]
    pub fn sstore_refund(&self, is_eip2200: bool, vals: &SStore) -> i64 {
        let clearing_slot_refund = self.get(GasId::SstoreClearingSlotRefund) as i64;

        if !is_eip2200 {
            if !vals.present_is_zero() && vals.new_is_zero() {
                return clearing_slot_refund;
            }
            return 0;
        }

        if vals.is_noop() {
            return 0;
        }

        if vals.is_clean() && vals.new_is_zero() {
            return clearing_slot_refund;
        }

        let mut refund = 0;
        if !vals.original_is_zero() {
            if vals.present_is_zero() {
                refund -= clearing_slot_refund;
            } else if vals.new_is_zero() {
                refund += clearing_slot_refund;
            }
        }

        if vals.resets_original() {
            if vals.original_is_zero() {
                refund += self.get(GasId::SstoreSetRefund) as i64;
            } else {
                refund += self.get(GasId::SstoreResetRefund) as i64;
            }
        }
        refund
    }

    /// Calculates `SSTORE` state gas for new slot creation.
    #[inline]
    pub fn sstore_state_gas(&self, vals: &SStore) -> u64 {
        if !vals.is_noop() && vals.is_clean() && vals.original_is_zero() {
            self.get(GasId::SstoreSetState)
        } else {
            0
        }
    }

    /// Calculates the `SSTORE` state gas to refill into the reservoir for a
    /// 0→x→0 storage restoration (EIP-8037).
    ///
    /// When a slot that began the transaction at zero is restored to zero
    /// (`new == original == 0`) by an actual change (`new != present`), the state
    /// gas charged for the initial 0→x transition is returned to the reservoir.
    #[inline]
    pub fn sstore_state_gas_refill(&self, vals: &SStore) -> u64 {
        if !vals.is_noop() && vals.resets_original() && vals.original_is_zero() {
            self.get(GasId::SstoreSetState)
        } else {
            0
        }
    }

    /// Returns the `CREATE`/`CREATE2` upfront state gas (EIP-8037).
    #[inline]
    pub const fn create_state_gas(&self) -> u64 {
        self.get(GasId::CreateState)
    }

    /// Returns the new-account creation state gas (EIP-8037).
    #[inline]
    pub const fn new_account_state_gas(&self) -> u64 {
        self.get(GasId::NewAccountState)
    }

    /// Returns the EIP-8037 state gas charged per EIP-7702 authorization: the per-account portion
    /// ([`Self::new_account_state_gas`]) plus the per-bytecode portion
    /// ([`GasId::TxEip7702PerAuthState`]). Zero before Amsterdam.
    #[inline]
    pub const fn eip7702_auth_state_gas(&self) -> u64 {
        self.new_account_state_gas().saturating_add(self.get(GasId::TxEip7702PerAuthState))
    }

    /// Calculates the code-deposit state gas for `len` bytes (EIP-8037).
    #[inline]
    pub const fn code_deposit_state_gas(&self, len: usize) -> u64 {
        self.get(GasId::CodeDepositState).saturating_mul(len as u64)
    }

    /// Returns `SELFDESTRUCT` cold account cost.
    #[inline]
    pub const fn selfdestruct_cold_cost(&self) -> u64 {
        self.cold_account_additional_cost().saturating_add(self.get(GasId::WarmStorageReadCost))
    }

    /// Calculates `SELFDESTRUCT` dynamic gas.
    #[inline]
    pub const fn selfdestruct_cost(&self, should_charge_topup: bool, is_cold: bool) -> u64 {
        let mut gas = 0;
        if should_charge_topup {
            gas += self.get(GasId::NewAccountCostForSelfdestruct);
        }
        if is_cold {
            gas += self.selfdestruct_cold_cost();
        }
        gas
    }

    /// Returns additional cold account access gas.
    #[inline]
    pub const fn cold_account_additional_cost(&self) -> u64 {
        self.get(GasId::ColdAccountAdditionalCost)
    }

    /// Returns the `(intrinsic_tokens, floor_tokens)` of transaction calldata `input`.
    ///
    /// Both weigh non-zero bytes at `TxTokenNonZeroByteMultiplier`. Intrinsic tokens count a zero
    /// byte as one token; floor tokens weigh it at `TxFloorZeroByteMultiplier` (EIP-7623: one;
    /// EIP-7976: the non-zero weight).
    ///
    /// Intrinsic gas is `intrinsic_tokens * TxTokenCost`. Sum the tokens over several inputs
    /// before pricing them, and pass the summed floor tokens to
    /// [`tx_floor_cost_with_tokens`](Self::tx_floor_cost_with_tokens).
    #[inline]
    pub fn tx_calldata_tokens(&self, input: &[u8]) -> (u64, u64) {
        let zero_bytes = count_zero_bytes(input);
        let non_zero_tokens =
            (input.len() as u64 - zero_bytes) * self.get(GasId::TxTokenNonZeroByteMultiplier);
        (
            zero_bytes + non_zero_tokens,
            zero_bytes * self.get(GasId::TxFloorZeroByteMultiplier) + non_zero_tokens,
        )
    }

    /// Returns the EIP-7623 calldata floor gas: `TxFloorCostBase` plus `TxFloorCostPerToken` for
    /// each of `floor_tokens` (see [`tx_calldata_tokens`](Self::tx_calldata_tokens)) and for the
    /// access list's bytes, weighted by `TxAccessListFloorByteMultiplier`.
    ///
    /// The caller checks that EIP-7623 is active. Under EIP-2780 the transaction's floor is based
    /// on its decomposed intrinsic base instead of `TxFloorCostBase`, which this function does
    /// not know.
    #[inline]
    pub const fn tx_floor_cost_with_tokens(
        &self,
        floor_tokens: u64,
        access_list_accounts: u64,
        access_list_storage_keys: u64,
    ) -> u64 {
        self.tx_floor_cost_with_base(
            self.get(GasId::TxFloorCostBase),
            floor_tokens,
            access_list_accounts,
            access_list_storage_keys,
        )
    }

    /// Like [`tx_floor_cost_with_tokens`](Self::tx_floor_cost_with_tokens), with an explicit base.
    #[inline]
    pub(crate) const fn tx_floor_cost_with_base(
        &self,
        base: u64,
        floor_tokens: u64,
        access_list_accounts: u64,
        access_list_storage_keys: u64,
    ) -> u64 {
        let access_list_bytes = access_list_accounts
            .saturating_mul(20)
            .saturating_add(access_list_storage_keys.saturating_mul(32));
        let tokens = floor_tokens.saturating_add(
            access_list_bytes.saturating_mul(self.get(GasId::TxAccessListFloorByteMultiplier)),
        );
        base.saturating_add(tokens.saturating_mul(self.get(GasId::TxFloorCostPerToken)))
    }
}

/// Counts the zero bytes in `data`.
///
/// Each chunk's count fits in a `u8`, so the compiler can compare and sum whole vector
/// registers of byte lanes and widen once per chunk; a `usize` count would widen every flag
/// to a full lane. 192 is a multiple of every power-of-two vector width up to 64 bytes, so only
/// the last chunk runs a scalar remainder.
///
/// With the aarch64 dot-product extension, LLVM turns a sum of 0/1 flags into a `udot` and
/// `addp` per vector, which ran slower than byte adds on an Apple M4. Counting 2 per zero byte
/// in 96-byte chunks keeps byte adds, at the cost of a mask per vector; a chunk still sums to at
/// most 192.
fn count_zero_bytes(data: &[u8]) -> u64 {
    const STEP: u8 = if cfg!(target_feature = "dotprod") { 2 } else { 1 };
    data.chunks(192 / STEP as usize)
        .map(|c| c.iter().map(|&b| if b == 0 { STEP } else { 0 }).sum::<u8>() as u64)
        .sum::<u64>()
        / STEP as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SpecId, Version};

    fn gas_params(spec: SpecId) -> &'static GasParams {
        &Version::base(spec).gas_params
    }

    #[test]
    fn gas_id_roundtrips_values() {
        assert_eq!(GasId::from_usize(0), Some(GasId::ExpByte));
        assert_eq!(GasId::ExpByte.as_usize(), 0);
        assert_eq!(GasId::from_usize(GasId::MAX.as_usize()), Some(GasId::Custom7));
        assert_eq!(GasId::from_usize(GasId::COUNT), None);
    }

    #[test]
    fn gas_params_match_frontier_defaults() {
        let params = gas_params(SpecId::FRONTIER);
        assert_eq!(params.get(GasId::ExpByte), 10);
        assert_eq!(params.get(GasId::MemoryLinearCost), 3);
        assert_eq!(params.get(GasId::MemoryQuadraticReduction), 512);
        assert_eq!(params.get(GasId::SstoreStatic), 5000);
        assert_eq!(params.get(GasId::SstoreSetWithoutLoadCost), 15000);
        assert_eq!(params.get(GasId::TxCreateCost), 0);
        assert_eq!(params.get(GasId::MaxRefundQuotient), 2);
    }

    #[test]
    fn gas_params_apply_homestead_defaults() {
        let params = gas_params(SpecId::HOMESTEAD);
        assert_eq!(params.get(GasId::TxCreateCost), 32000);
    }

    #[test]
    fn gas_params_apply_fork_defaults() {
        let tangerine = gas_params(SpecId::TANGERINE);
        assert_eq!(tangerine.get(GasId::NewAccountCostForSelfdestruct), 25000);

        let spurious_dragon = gas_params(SpecId::SPURIOUS_DRAGON);
        assert_eq!(spurious_dragon.get(GasId::ExpByte), 50);

        let istanbul = gas_params(SpecId::ISTANBUL);
        assert_eq!(istanbul.get(GasId::SstoreStatic), 800);
        assert_eq!(istanbul.get(GasId::TxTokenNonZeroByteMultiplier), 4);

        let berlin = gas_params(SpecId::BERLIN);
        assert_eq!(berlin.get(GasId::SstoreStatic), 100);
        assert_eq!(berlin.cold_account_additional_cost(), 2500);
        assert_eq!(berlin.get(GasId::ColdStorageCost), 2100);
        assert_eq!(berlin.get(GasId::MaxRefundQuotient), 2);

        let london = gas_params(SpecId::LONDON);
        assert_eq!(london.get(GasId::SstoreClearingSlotRefund), 4800);
        assert_eq!(london.get(GasId::SelfdestructRefund), 0);
        assert_eq!(london.get(GasId::MaxRefundQuotient), 5);

        let shanghai = gas_params(SpecId::SHANGHAI);
        assert_eq!(shanghai.get(GasId::TxInitcodeCost), 2);

        let prague = gas_params(SpecId::PRAGUE);
        assert_eq!(prague.get(GasId::TxEip7702PerEmptyAccountCost), 25000);
        assert_eq!(prague.get(GasId::TxEip7702AuthRefund), 12500);
        assert_eq!(prague.get(GasId::TxFloorCostPerToken), 10);
        // EIP-7623: zero calldata bytes weigh one floor token each.
        assert_eq!(prague.get(GasId::TxFloorZeroByteMultiplier), 1);

        let osaka = gas_params(SpecId::OSAKA);
        assert_eq!(osaka.get(GasId::TxBaseStipend), 21_000);

        let amsterdam = gas_params(SpecId::AMSTERDAM);
        assert_eq!(amsterdam.get(GasId::TxBaseStipend), 12_000);
        // EIP-8038 state-access cost values (glamsterdam devnet-8).
        assert_eq!(amsterdam.get(GasId::Create), 12_000);
        assert_eq!(amsterdam.get(GasId::WarmStorageReadCost), 100); // unchanged
        assert_eq!(amsterdam.get(GasId::ColdStorageCost), 2000);
        assert_eq!(amsterdam.cold_account_additional_cost(), 2900);
        assert_eq!(amsterdam.get(GasId::TransferValueCost), 11_300);
        // CALL folds the account-write surcharge into CALL_VALUE, so a new target
        // adds no extra execution gas (only NEW_ACCOUNT state gas).
        assert_eq!(amsterdam.get(GasId::NewAccountCost), 0);
        assert_eq!(amsterdam.get(GasId::SstoreSetWithoutLoadCost), 10_000);
        assert_eq!(amsterdam.get(GasId::SstoreClearingSlotRefund), 11_616);
        // EIP-2780/Amsterdam intrinsic per-auth execution gas: the state-independent
        // REGULAR_PER_AUTH_BASE_COST = 101*16 + 3000 + 3000 + 2*100 = 7_816 (the ACCOUNT_WRITE
        // and state-gas remainder is charged at the runtime gas phase).
        assert_eq!(amsterdam.get(GasId::TxEip7702PerEmptyAccountCost), 7_816);
        assert_eq!(amsterdam.get(GasId::TxEip7702AuthRefund), 0);
        assert_eq!(amsterdam.get(GasId::SstoreSetState), 64 * 1530);
        assert_eq!(amsterdam.get(GasId::TxEip7702PerAuthState), 23 * 1530);
        assert_eq!(amsterdam.get(GasId::TxAccessListAddressCost), 2900 + 20 * 64);
        assert_eq!(amsterdam.get(GasId::TxAccessListStorageKeyCost), 2000 + 32 * 64);
        assert_eq!(amsterdam.get(GasId::TxAccessListFloorByteMultiplier), 4);
        // EIP-7976: zero bytes weigh the same as non-zero bytes in the floor.
        assert_eq!(amsterdam.get(GasId::TxFloorZeroByteMultiplier), 4);
    }

    #[test]
    fn gas_params_override_values() {
        let mut params = *gas_params(SpecId::default());
        params[GasId::MemoryLinearCost] = 7;
        params[GasId::MemoryQuadraticReduction] = 1024;
        assert_eq!(params[GasId::MemoryLinearCost], 7);
        assert_eq!(params[GasId::MemoryQuadraticReduction], 1024);
    }

    #[test]
    fn gas_params_calculate_costs() {
        let params = gas_params(SpecId::FRONTIER);
        assert_eq!(num_words(0), 0);
        assert_eq!(num_words(33), 2);
        assert_eq!(params.memory_cost(10), 30);
        assert_eq!(params.copy_cost(33), 6);
        assert_eq!(params.extcodecopy_cost(33), 6);
        assert_eq!(params.mcopy_cost(33), 6);
        assert_eq!(params.keccak256_word_cost(33), 12);
        assert_eq!(params.exp_cost(U256::ZERO), 0);
        assert_eq!(params.exp_cost(U256::from(0xff)), 10);
        assert_eq!(params.exp_cost(U256::from(0x100)), 20);
    }

    #[test]
    fn calldata_tokens_and_floor_cost_follow_the_fork() {
        // Zero bytes weigh 1 and non-zero bytes 4 in the intrinsic tokens.
        let input = [0, 0, 1];

        // EIP-7623: the floor weighs zero bytes like the intrinsic tokens and ignores the
        // access list.
        let prague = gas_params(SpecId::PRAGUE);
        assert_eq!(prague.tx_calldata_tokens(&input), (6, 6));
        assert_eq!(prague.tx_calldata_tokens(&[]), (0, 0));
        assert_eq!(prague.tx_floor_cost_with_tokens(6, 0, 0), 21_000 + 6 * 10);
        assert_eq!(prague.tx_floor_cost_with_tokens(6, 3, 4), 21_000 + 6 * 10);

        // EIP-7976: the floor weighs every byte by 4. EIP-7981 adds the access list bytes.
        let amsterdam = gas_params(SpecId::AMSTERDAM);
        assert_eq!(amsterdam.tx_calldata_tokens(&input), (6, 12));
        assert_eq!(amsterdam.tx_floor_cost_with_tokens(12, 0, 0), 12_000 + 12 * 16);
        assert_eq!(
            amsterdam.tx_floor_cost_with_tokens(12, 1, 1),
            12_000 + (12 + (20 + 32) * 4) * 16
        );
    }
}
