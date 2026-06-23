///Module containing a contract's types and functions.
/**

```solidity
library StepMerging {
    struct Hop { bytes32 dex; bytes32 fromToken; bytes32 toToken; uint256 amountIn; uint256 amountOut; uint256 gas; uint256 poolLiquidity; }
    struct MergedGroup { bytes32 signatureHash; uint256 mergedCount; uint256 mergedAmountAtIntermediate; uint256 mergedOutput; uint256 originalBestOutput; uint256 mergedGas; uint256 originalTotalGas; }
    struct Route { Hop[] hops; uint256 totalOutput; uint256 totalGas; }
}
```*/
#[allow(
    non_camel_case_types,
    non_snake_case,
    clippy::pub_underscore_fields,
    clippy::style,
    clippy::empty_structs_with_brackets
)]
pub mod StepMerging {
    use super::*;
    use alloy::sol_types as alloy_sol_types;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**```solidity
struct Hop { bytes32 dex; bytes32 fromToken; bytes32 toToken; uint256 amountIn; uint256 amountOut; uint256 gas; uint256 poolLiquidity; }
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Hop {
        #[allow(missing_docs)]
        pub dex: alloy::sol_types::private::FixedBytes<32>,
        #[allow(missing_docs)]
        pub fromToken: alloy::sol_types::private::FixedBytes<32>,
        #[allow(missing_docs)]
        pub toToken: alloy::sol_types::private::FixedBytes<32>,
        #[allow(missing_docs)]
        pub amountIn: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub amountOut: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub gas: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub poolLiquidity: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[doc(hidden)]
        #[allow(dead_code)]
        type UnderlyingSolTuple<'a> = (
            alloy::sol_types::sol_data::FixedBytes<32>,
            alloy::sol_types::sol_data::FixedBytes<32>,
            alloy::sol_types::sol_data::FixedBytes<32>,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Uint<256>,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
            alloy::sol_types::private::FixedBytes<32>,
            alloy::sol_types::private::FixedBytes<32>,
            alloy::sol_types::private::FixedBytes<32>,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::primitives::aliases::U256,
        );
        #[cfg(test)]
        #[allow(dead_code, unreachable_patterns)]
        fn _type_assertion(
            _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
        ) {
            match _t {
                alloy_sol_types::private::AssertTypeEq::<
                    <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                >(_) => {}
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<Hop> for UnderlyingRustTuple<'_> {
            fn from(value: Hop) -> Self {
                (
                    value.dex,
                    value.fromToken,
                    value.toToken,
                    value.amountIn,
                    value.amountOut,
                    value.gas,
                    value.poolLiquidity,
                )
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for Hop {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    dex: tuple.0,
                    fromToken: tuple.1,
                    toToken: tuple.2,
                    amountIn: tuple.3,
                    amountOut: tuple.4,
                    gas: tuple.5,
                    poolLiquidity: tuple.6,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolValue for Hop {
            type SolType = Self;
        }
        #[automatically_derived]
        impl alloy_sol_types::private::SolTypeValue<Self> for Hop {
            #[inline]
            fn stv_to_tokens(&self) -> <Self as alloy_sol_types::SolType>::Token<'_> {
                (
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.dex),
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.fromToken),
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.toToken),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amountIn),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amountOut),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.gas),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.poolLiquidity),
                )
            }
            #[inline]
            fn stv_abi_encoded_size(&self) -> usize {
                if let Some(size) = <Self as alloy_sol_types::SolType>::ENCODED_SIZE {
                    return size;
                }
                let tuple = <UnderlyingRustTuple<
                    '_,
                > as ::core::convert::From<Self>>::from(self.clone());
                <UnderlyingSolTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_encoded_size(&tuple)
            }
            #[inline]
            fn stv_eip712_data_word(&self) -> alloy_sol_types::Word {
                <Self as alloy_sol_types::SolStruct>::eip712_hash_struct(self)
            }
            #[inline]
            fn stv_abi_encode_packed_to(
                &self,
                out: &mut alloy_sol_types::private::Vec<u8>,
            ) {
                let tuple = <UnderlyingRustTuple<
                    '_,
                > as ::core::convert::From<Self>>::from(self.clone());
                <UnderlyingSolTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_encode_packed_to(&tuple, out)
            }
            #[inline]
            fn stv_abi_packed_encoded_size(&self) -> usize {
                if let Some(size) = <Self as alloy_sol_types::SolType>::PACKED_ENCODED_SIZE {
                    return size;
                }
                let tuple = <UnderlyingRustTuple<
                    '_,
                > as ::core::convert::From<Self>>::from(self.clone());
                <UnderlyingSolTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_packed_encoded_size(&tuple)
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolType for Hop {
            type RustType = Self;
            type Token<'a> = <UnderlyingSolTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SOL_NAME: &'static str = <Self as alloy_sol_types::SolStruct>::NAME;
            const ENCODED_SIZE: Option<usize> = <UnderlyingSolTuple<
                '_,
            > as alloy_sol_types::SolType>::ENCODED_SIZE;
            const PACKED_ENCODED_SIZE: Option<usize> = <UnderlyingSolTuple<
                '_,
            > as alloy_sol_types::SolType>::PACKED_ENCODED_SIZE;
            #[inline]
            fn valid_token(token: &Self::Token<'_>) -> bool {
                <UnderlyingSolTuple<'_> as alloy_sol_types::SolType>::valid_token(token)
            }
            #[inline]
            fn detokenize(token: Self::Token<'_>) -> Self::RustType {
                let tuple = <UnderlyingSolTuple<
                    '_,
                > as alloy_sol_types::SolType>::detokenize(token);
                <Self as ::core::convert::From<UnderlyingRustTuple<'_>>>::from(tuple)
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolStruct for Hop {
            const NAME: &'static str = "Hop";
            #[inline]
            fn eip712_root_type() -> alloy_sol_types::private::Cow<'static, str> {
                alloy_sol_types::private::Cow::Borrowed(
                    "Hop(bytes32 dex,bytes32 fromToken,bytes32 toToken,uint256 amountIn,uint256 amountOut,uint256 gas,uint256 poolLiquidity)",
                )
            }
            #[inline]
            fn eip712_components() -> alloy_sol_types::private::Vec<
                alloy_sol_types::private::Cow<'static, str>,
            > {
                alloy_sol_types::private::Vec::new()
            }
            #[inline]
            fn eip712_encode_type() -> alloy_sol_types::private::Cow<'static, str> {
                <Self as alloy_sol_types::SolStruct>::eip712_root_type()
            }
            #[inline]
            fn eip712_encode_data(&self) -> alloy_sol_types::private::Vec<u8> {
                [
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.dex)
                        .0,
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.fromToken)
                        .0,
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.toToken)
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.amountIn)
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.amountOut)
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.gas)
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.poolLiquidity)
                        .0,
                ]
                    .concat()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::EventTopic for Hop {
            #[inline]
            fn topic_preimage_length(rust: &Self::RustType) -> usize {
                0usize
                    + <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(&rust.dex)
                    + <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.fromToken,
                    )
                    + <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.toToken,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.amountIn,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.amountOut,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(&rust.gas)
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.poolLiquidity,
                    )
            }
            #[inline]
            fn encode_topic_preimage(
                rust: &Self::RustType,
                out: &mut alloy_sol_types::private::Vec<u8>,
            ) {
                out.reserve(
                    <Self as alloy_sol_types::EventTopic>::topic_preimage_length(rust),
                );
                <alloy::sol_types::sol_data::FixedBytes<
                    32,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(&rust.dex, out);
                <alloy::sol_types::sol_data::FixedBytes<
                    32,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.fromToken,
                    out,
                );
                <alloy::sol_types::sol_data::FixedBytes<
                    32,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.toToken,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.amountIn,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.amountOut,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(&rust.gas, out);
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.poolLiquidity,
                    out,
                );
            }
            #[inline]
            fn encode_topic(
                rust: &Self::RustType,
            ) -> alloy_sol_types::abi::token::WordToken {
                let mut out = alloy_sol_types::private::Vec::new();
                <Self as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    rust,
                    &mut out,
                );
                alloy_sol_types::abi::token::WordToken(
                    alloy_sol_types::private::keccak256(out),
                )
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**```solidity
struct MergedGroup { bytes32 signatureHash; uint256 mergedCount; uint256 mergedAmountAtIntermediate; uint256 mergedOutput; uint256 originalBestOutput; uint256 mergedGas; uint256 originalTotalGas; }
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MergedGroup {
        #[allow(missing_docs)]
        pub signatureHash: alloy::sol_types::private::FixedBytes<32>,
        #[allow(missing_docs)]
        pub mergedCount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub mergedAmountAtIntermediate: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub mergedOutput: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub originalBestOutput: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub mergedGas: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub originalTotalGas: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[doc(hidden)]
        #[allow(dead_code)]
        type UnderlyingSolTuple<'a> = (
            alloy::sol_types::sol_data::FixedBytes<32>,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Uint<256>,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
            alloy::sol_types::private::FixedBytes<32>,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::primitives::aliases::U256,
        );
        #[cfg(test)]
        #[allow(dead_code, unreachable_patterns)]
        fn _type_assertion(
            _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
        ) {
            match _t {
                alloy_sol_types::private::AssertTypeEq::<
                    <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                >(_) => {}
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<MergedGroup> for UnderlyingRustTuple<'_> {
            fn from(value: MergedGroup) -> Self {
                (
                    value.signatureHash,
                    value.mergedCount,
                    value.mergedAmountAtIntermediate,
                    value.mergedOutput,
                    value.originalBestOutput,
                    value.mergedGas,
                    value.originalTotalGas,
                )
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for MergedGroup {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    signatureHash: tuple.0,
                    mergedCount: tuple.1,
                    mergedAmountAtIntermediate: tuple.2,
                    mergedOutput: tuple.3,
                    originalBestOutput: tuple.4,
                    mergedGas: tuple.5,
                    originalTotalGas: tuple.6,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolValue for MergedGroup {
            type SolType = Self;
        }
        #[automatically_derived]
        impl alloy_sol_types::private::SolTypeValue<Self> for MergedGroup {
            #[inline]
            fn stv_to_tokens(&self) -> <Self as alloy_sol_types::SolType>::Token<'_> {
                (
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.signatureHash),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.mergedCount),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(
                        &self.mergedAmountAtIntermediate,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.mergedOutput),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.originalBestOutput),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.mergedGas),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.originalTotalGas),
                )
            }
            #[inline]
            fn stv_abi_encoded_size(&self) -> usize {
                if let Some(size) = <Self as alloy_sol_types::SolType>::ENCODED_SIZE {
                    return size;
                }
                let tuple = <UnderlyingRustTuple<
                    '_,
                > as ::core::convert::From<Self>>::from(self.clone());
                <UnderlyingSolTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_encoded_size(&tuple)
            }
            #[inline]
            fn stv_eip712_data_word(&self) -> alloy_sol_types::Word {
                <Self as alloy_sol_types::SolStruct>::eip712_hash_struct(self)
            }
            #[inline]
            fn stv_abi_encode_packed_to(
                &self,
                out: &mut alloy_sol_types::private::Vec<u8>,
            ) {
                let tuple = <UnderlyingRustTuple<
                    '_,
                > as ::core::convert::From<Self>>::from(self.clone());
                <UnderlyingSolTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_encode_packed_to(&tuple, out)
            }
            #[inline]
            fn stv_abi_packed_encoded_size(&self) -> usize {
                if let Some(size) = <Self as alloy_sol_types::SolType>::PACKED_ENCODED_SIZE {
                    return size;
                }
                let tuple = <UnderlyingRustTuple<
                    '_,
                > as ::core::convert::From<Self>>::from(self.clone());
                <UnderlyingSolTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_packed_encoded_size(&tuple)
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolType for MergedGroup {
            type RustType = Self;
            type Token<'a> = <UnderlyingSolTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SOL_NAME: &'static str = <Self as alloy_sol_types::SolStruct>::NAME;
            const ENCODED_SIZE: Option<usize> = <UnderlyingSolTuple<
                '_,
            > as alloy_sol_types::SolType>::ENCODED_SIZE;
            const PACKED_ENCODED_SIZE: Option<usize> = <UnderlyingSolTuple<
                '_,
            > as alloy_sol_types::SolType>::PACKED_ENCODED_SIZE;
            #[inline]
            fn valid_token(token: &Self::Token<'_>) -> bool {
                <UnderlyingSolTuple<'_> as alloy_sol_types::SolType>::valid_token(token)
            }
            #[inline]
            fn detokenize(token: Self::Token<'_>) -> Self::RustType {
                let tuple = <UnderlyingSolTuple<
                    '_,
                > as alloy_sol_types::SolType>::detokenize(token);
                <Self as ::core::convert::From<UnderlyingRustTuple<'_>>>::from(tuple)
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolStruct for MergedGroup {
            const NAME: &'static str = "MergedGroup";
            #[inline]
            fn eip712_root_type() -> alloy_sol_types::private::Cow<'static, str> {
                alloy_sol_types::private::Cow::Borrowed(
                    "MergedGroup(bytes32 signatureHash,uint256 mergedCount,uint256 mergedAmountAtIntermediate,uint256 mergedOutput,uint256 originalBestOutput,uint256 mergedGas,uint256 originalTotalGas)",
                )
            }
            #[inline]
            fn eip712_components() -> alloy_sol_types::private::Vec<
                alloy_sol_types::private::Cow<'static, str>,
            > {
                alloy_sol_types::private::Vec::new()
            }
            #[inline]
            fn eip712_encode_type() -> alloy_sol_types::private::Cow<'static, str> {
                <Self as alloy_sol_types::SolStruct>::eip712_root_type()
            }
            #[inline]
            fn eip712_encode_data(&self) -> alloy_sol_types::private::Vec<u8> {
                [
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.signatureHash)
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.mergedCount)
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(
                            &self.mergedAmountAtIntermediate,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.mergedOutput)
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(
                            &self.originalBestOutput,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.mergedGas)
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(
                            &self.originalTotalGas,
                        )
                        .0,
                ]
                    .concat()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::EventTopic for MergedGroup {
            #[inline]
            fn topic_preimage_length(rust: &Self::RustType) -> usize {
                0usize
                    + <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.signatureHash,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.mergedCount,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.mergedAmountAtIntermediate,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.mergedOutput,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.originalBestOutput,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.mergedGas,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.originalTotalGas,
                    )
            }
            #[inline]
            fn encode_topic_preimage(
                rust: &Self::RustType,
                out: &mut alloy_sol_types::private::Vec<u8>,
            ) {
                out.reserve(
                    <Self as alloy_sol_types::EventTopic>::topic_preimage_length(rust),
                );
                <alloy::sol_types::sol_data::FixedBytes<
                    32,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.signatureHash,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.mergedCount,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.mergedAmountAtIntermediate,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.mergedOutput,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.originalBestOutput,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.mergedGas,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.originalTotalGas,
                    out,
                );
            }
            #[inline]
            fn encode_topic(
                rust: &Self::RustType,
            ) -> alloy_sol_types::abi::token::WordToken {
                let mut out = alloy_sol_types::private::Vec::new();
                <Self as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    rust,
                    &mut out,
                );
                alloy_sol_types::abi::token::WordToken(
                    alloy_sol_types::private::keccak256(out),
                )
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**```solidity
struct Route { Hop[] hops; uint256 totalOutput; uint256 totalGas; }
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Route {
        #[allow(missing_docs)]
        pub hops: alloy::sol_types::private::Vec<
            <Hop as alloy::sol_types::SolType>::RustType,
        >,
        #[allow(missing_docs)]
        pub totalOutput: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub totalGas: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[doc(hidden)]
        #[allow(dead_code)]
        type UnderlyingSolTuple<'a> = (
            alloy::sol_types::sol_data::Array<Hop>,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Uint<256>,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
            alloy::sol_types::private::Vec<<Hop as alloy::sol_types::SolType>::RustType>,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::primitives::aliases::U256,
        );
        #[cfg(test)]
        #[allow(dead_code, unreachable_patterns)]
        fn _type_assertion(
            _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
        ) {
            match _t {
                alloy_sol_types::private::AssertTypeEq::<
                    <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                >(_) => {}
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<Route> for UnderlyingRustTuple<'_> {
            fn from(value: Route) -> Self {
                (value.hops, value.totalOutput, value.totalGas)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for Route {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    hops: tuple.0,
                    totalOutput: tuple.1,
                    totalGas: tuple.2,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolValue for Route {
            type SolType = Self;
        }
        #[automatically_derived]
        impl alloy_sol_types::private::SolTypeValue<Self> for Route {
            #[inline]
            fn stv_to_tokens(&self) -> <Self as alloy_sol_types::SolType>::Token<'_> {
                (
                    <alloy::sol_types::sol_data::Array<
                        Hop,
                    > as alloy_sol_types::SolType>::tokenize(&self.hops),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.totalOutput),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.totalGas),
                )
            }
            #[inline]
            fn stv_abi_encoded_size(&self) -> usize {
                if let Some(size) = <Self as alloy_sol_types::SolType>::ENCODED_SIZE {
                    return size;
                }
                let tuple = <UnderlyingRustTuple<
                    '_,
                > as ::core::convert::From<Self>>::from(self.clone());
                <UnderlyingSolTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_encoded_size(&tuple)
            }
            #[inline]
            fn stv_eip712_data_word(&self) -> alloy_sol_types::Word {
                <Self as alloy_sol_types::SolStruct>::eip712_hash_struct(self)
            }
            #[inline]
            fn stv_abi_encode_packed_to(
                &self,
                out: &mut alloy_sol_types::private::Vec<u8>,
            ) {
                let tuple = <UnderlyingRustTuple<
                    '_,
                > as ::core::convert::From<Self>>::from(self.clone());
                <UnderlyingSolTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_encode_packed_to(&tuple, out)
            }
            #[inline]
            fn stv_abi_packed_encoded_size(&self) -> usize {
                if let Some(size) = <Self as alloy_sol_types::SolType>::PACKED_ENCODED_SIZE {
                    return size;
                }
                let tuple = <UnderlyingRustTuple<
                    '_,
                > as ::core::convert::From<Self>>::from(self.clone());
                <UnderlyingSolTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_packed_encoded_size(&tuple)
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolType for Route {
            type RustType = Self;
            type Token<'a> = <UnderlyingSolTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SOL_NAME: &'static str = <Self as alloy_sol_types::SolStruct>::NAME;
            const ENCODED_SIZE: Option<usize> = <UnderlyingSolTuple<
                '_,
            > as alloy_sol_types::SolType>::ENCODED_SIZE;
            const PACKED_ENCODED_SIZE: Option<usize> = <UnderlyingSolTuple<
                '_,
            > as alloy_sol_types::SolType>::PACKED_ENCODED_SIZE;
            #[inline]
            fn valid_token(token: &Self::Token<'_>) -> bool {
                <UnderlyingSolTuple<'_> as alloy_sol_types::SolType>::valid_token(token)
            }
            #[inline]
            fn detokenize(token: Self::Token<'_>) -> Self::RustType {
                let tuple = <UnderlyingSolTuple<
                    '_,
                > as alloy_sol_types::SolType>::detokenize(token);
                <Self as ::core::convert::From<UnderlyingRustTuple<'_>>>::from(tuple)
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolStruct for Route {
            const NAME: &'static str = "Route";
            #[inline]
            fn eip712_root_type() -> alloy_sol_types::private::Cow<'static, str> {
                alloy_sol_types::private::Cow::Borrowed(
                    "Route(Hop[] hops,uint256 totalOutput,uint256 totalGas)",
                )
            }
            #[inline]
            fn eip712_components() -> alloy_sol_types::private::Vec<
                alloy_sol_types::private::Cow<'static, str>,
            > {
                let mut components = alloy_sol_types::private::Vec::with_capacity(1);
                components.push(<Hop as alloy_sol_types::SolStruct>::eip712_root_type());
                components
                    .extend(<Hop as alloy_sol_types::SolStruct>::eip712_components());
                components
            }
            #[inline]
            fn eip712_encode_data(&self) -> alloy_sol_types::private::Vec<u8> {
                [
                    <alloy::sol_types::sol_data::Array<
                        Hop,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.hops)
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.totalOutput)
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.totalGas)
                        .0,
                ]
                    .concat()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::EventTopic for Route {
            #[inline]
            fn topic_preimage_length(rust: &Self::RustType) -> usize {
                0usize
                    + <alloy::sol_types::sol_data::Array<
                        Hop,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(&rust.hops)
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.totalOutput,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.totalGas,
                    )
            }
            #[inline]
            fn encode_topic_preimage(
                rust: &Self::RustType,
                out: &mut alloy_sol_types::private::Vec<u8>,
            ) {
                out.reserve(
                    <Self as alloy_sol_types::EventTopic>::topic_preimage_length(rust),
                );
                <alloy::sol_types::sol_data::Array<
                    Hop,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.hops,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.totalOutput,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.totalGas,
                    out,
                );
            }
            #[inline]
            fn encode_topic(
                rust: &Self::RustType,
            ) -> alloy_sol_types::abi::token::WordToken {
                let mut out = alloy_sol_types::private::Vec::new();
                <Self as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    rust,
                    &mut out,
                );
                alloy_sol_types::abi::token::WordToken(
                    alloy_sol_types::private::keccak256(out),
                )
            }
        }
    };
    use alloy::contract as alloy_contract;
    /**Creates a new wrapper around an on-chain [`StepMerging`](self) contract instance.

See the [wrapper's documentation](`StepMergingInstance`) for more details.*/
    #[inline]
    pub const fn new<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    >(
        address: alloy_sol_types::private::Address,
        __provider: P,
    ) -> StepMergingInstance<P, N> {
        StepMergingInstance::<P, N>::new(address, __provider)
    }
    /**A [`StepMerging`](self) instance.

Contains type-safe methods for interacting with an on-chain instance of the
[`StepMerging`](self) contract located at a given `address`, using a given
provider `P`.

If the contract bytecode is available (see the [`sol!`](alloy_sol_types::sol!)
documentation on how to provide it), the `deploy` and `deploy_builder` methods can
be used to deploy a new instance of the contract.

See the [module-level documentation](self) for all the available methods.*/
    #[derive(Clone)]
    pub struct StepMergingInstance<P, N = alloy_contract::private::Ethereum> {
        address: alloy_sol_types::private::Address,
        provider: P,
        _network: ::core::marker::PhantomData<N>,
    }
    #[automatically_derived]
    impl<P, N> ::core::fmt::Debug for StepMergingInstance<P, N> {
        #[inline]
        fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
            f.debug_tuple("StepMergingInstance").field(&self.address).finish()
        }
    }
    /// Instantiation and getters/setters.
    impl<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    > StepMergingInstance<P, N> {
        /**Creates a new wrapper around an on-chain [`StepMerging`](self) contract instance.

See the [wrapper's documentation](`StepMergingInstance`) for more details.*/
        #[inline]
        pub const fn new(
            address: alloy_sol_types::private::Address,
            __provider: P,
        ) -> Self {
            Self {
                address,
                provider: __provider,
                _network: ::core::marker::PhantomData,
            }
        }
        /// Returns a reference to the address.
        #[inline]
        pub const fn address(&self) -> &alloy_sol_types::private::Address {
            &self.address
        }
        /// Sets the address.
        #[inline]
        pub fn set_address(&mut self, address: alloy_sol_types::private::Address) {
            self.address = address;
        }
        /// Sets the address and returns `self`.
        pub fn at(mut self, address: alloy_sol_types::private::Address) -> Self {
            self.set_address(address);
            self
        }
        /// Returns a reference to the provider.
        #[inline]
        pub const fn provider(&self) -> &P {
            &self.provider
        }
    }
    impl<P: ::core::clone::Clone, N> StepMergingInstance<&P, N> {
        /// Clones the provider and returns a new instance with the cloned provider.
        #[inline]
        pub fn with_cloned_provider(self) -> StepMergingInstance<P, N> {
            StepMergingInstance {
                address: self.address,
                provider: ::core::clone::Clone::clone(&self.provider),
                _network: ::core::marker::PhantomData,
            }
        }
    }
    /// Function calls.
    impl<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    > StepMergingInstance<P, N> {
        /// Creates a new call builder using this contract instance's provider and address.
        ///
        /// Note that the call can be any function call, not just those defined in this
        /// contract. Prefer using the other methods for building type-safe contract calls.
        pub fn call_builder<C: alloy_sol_types::SolCall>(
            &self,
            call: &C,
        ) -> alloy_contract::SolCallBuilder<&P, C, N> {
            alloy_contract::SolCallBuilder::new_sol(&self.provider, &self.address, call)
        }
    }
    /// Event filters.
    impl<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    > StepMergingInstance<P, N> {
        /// Creates a new event filter using this contract instance's provider and address.
        ///
        /// Note that the type can be any event, not just those defined in this contract.
        /// Prefer using the other methods for building type-safe event filters.
        pub fn event_filter<E: alloy_sol_types::SolEvent>(
            &self,
        ) -> alloy_contract::Event<&P, E, N> {
            alloy_contract::Event::new_sol(&self.provider, &self.address)
        }
    }
}
/**

Generated by the following Solidity interface...
```solidity
library StepMerging {
    struct Hop {
        bytes32 dex;
        bytes32 fromToken;
        bytes32 toToken;
        uint256 amountIn;
        uint256 amountOut;
        uint256 gas;
        uint256 poolLiquidity;
    }
    struct MergedGroup {
        bytes32 signatureHash;
        uint256 mergedCount;
        uint256 mergedAmountAtIntermediate;
        uint256 mergedOutput;
        uint256 originalBestOutput;
        uint256 mergedGas;
        uint256 originalTotalGas;
    }
    struct Route {
        Hop[] hops;
        uint256 totalOutput;
        uint256 totalGas;
    }
}

interface PathFinder {
    struct Route {
        address[] path;
        uint8[] venues;
        uint24[] fees;
        uint256 amountOut;
    }

    error DivisionByZero();
    error PathFinder__NoRoute();
    error PathFinder__SameToken();
    error PathFinder__SlippageOutOfRange();
    error PathFinder__VenueNotImplemented(uint8 venue);
    error PathFinder__ZeroAmount();
    error ZeroInput();

    function findRoute(address tokenIn, address tokenOut, uint256 amountIn, uint256 slippageBps) external returns (Route memory route);
    function findRouteWithHints(address tokenIn, address tokenOut, uint256 amountIn, uint256 slippageBps, bytes memory extraData) external returns (Route memory route);
    function mergeRoutes(StepMerging.Route[] memory routes, bytes32 finalToken) external pure returns (StepMerging.Route[] memory optimised, StepMerging.MergedGroup[] memory groups);
}
```

...which was generated by the following JSON ABI:
```json
[
  {
    "type": "function",
    "name": "findRoute",
    "inputs": [
      {
        "name": "tokenIn",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "tokenOut",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "amountIn",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "slippageBps",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "outputs": [
      {
        "name": "route",
        "type": "tuple",
        "internalType": "struct Route",
        "components": [
          {
            "name": "path",
            "type": "address[]",
            "internalType": "address[]"
          },
          {
            "name": "venues",
            "type": "uint8[]",
            "internalType": "uint8[]"
          },
          {
            "name": "fees",
            "type": "uint24[]",
            "internalType": "uint24[]"
          },
          {
            "name": "amountOut",
            "type": "uint256",
            "internalType": "uint256"
          }
        ]
      }
    ],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "findRouteWithHints",
    "inputs": [
      {
        "name": "tokenIn",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "tokenOut",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "amountIn",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "slippageBps",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "extraData",
        "type": "bytes",
        "internalType": "bytes"
      }
    ],
    "outputs": [
      {
        "name": "route",
        "type": "tuple",
        "internalType": "struct Route",
        "components": [
          {
            "name": "path",
            "type": "address[]",
            "internalType": "address[]"
          },
          {
            "name": "venues",
            "type": "uint8[]",
            "internalType": "uint8[]"
          },
          {
            "name": "fees",
            "type": "uint24[]",
            "internalType": "uint24[]"
          },
          {
            "name": "amountOut",
            "type": "uint256",
            "internalType": "uint256"
          }
        ]
      }
    ],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "mergeRoutes",
    "inputs": [
      {
        "name": "routes",
        "type": "tuple[]",
        "internalType": "struct StepMerging.Route[]",
        "components": [
          {
            "name": "hops",
            "type": "tuple[]",
            "internalType": "struct StepMerging.Hop[]",
            "components": [
              {
                "name": "dex",
                "type": "bytes32",
                "internalType": "bytes32"
              },
              {
                "name": "fromToken",
                "type": "bytes32",
                "internalType": "bytes32"
              },
              {
                "name": "toToken",
                "type": "bytes32",
                "internalType": "bytes32"
              },
              {
                "name": "amountIn",
                "type": "uint256",
                "internalType": "uint256"
              },
              {
                "name": "amountOut",
                "type": "uint256",
                "internalType": "uint256"
              },
              {
                "name": "gas",
                "type": "uint256",
                "internalType": "uint256"
              },
              {
                "name": "poolLiquidity",
                "type": "uint256",
                "internalType": "uint256"
              }
            ]
          },
          {
            "name": "totalOutput",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "totalGas",
            "type": "uint256",
            "internalType": "uint256"
          }
        ]
      },
      {
        "name": "finalToken",
        "type": "bytes32",
        "internalType": "bytes32"
      }
    ],
    "outputs": [
      {
        "name": "optimised",
        "type": "tuple[]",
        "internalType": "struct StepMerging.Route[]",
        "components": [
          {
            "name": "hops",
            "type": "tuple[]",
            "internalType": "struct StepMerging.Hop[]",
            "components": [
              {
                "name": "dex",
                "type": "bytes32",
                "internalType": "bytes32"
              },
              {
                "name": "fromToken",
                "type": "bytes32",
                "internalType": "bytes32"
              },
              {
                "name": "toToken",
                "type": "bytes32",
                "internalType": "bytes32"
              },
              {
                "name": "amountIn",
                "type": "uint256",
                "internalType": "uint256"
              },
              {
                "name": "amountOut",
                "type": "uint256",
                "internalType": "uint256"
              },
              {
                "name": "gas",
                "type": "uint256",
                "internalType": "uint256"
              },
              {
                "name": "poolLiquidity",
                "type": "uint256",
                "internalType": "uint256"
              }
            ]
          },
          {
            "name": "totalOutput",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "totalGas",
            "type": "uint256",
            "internalType": "uint256"
          }
        ]
      },
      {
        "name": "groups",
        "type": "tuple[]",
        "internalType": "struct StepMerging.MergedGroup[]",
        "components": [
          {
            "name": "signatureHash",
            "type": "bytes32",
            "internalType": "bytes32"
          },
          {
            "name": "mergedCount",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "mergedAmountAtIntermediate",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "mergedOutput",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "originalBestOutput",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "mergedGas",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "originalTotalGas",
            "type": "uint256",
            "internalType": "uint256"
          }
        ]
      }
    ],
    "stateMutability": "pure"
  },
  {
    "type": "error",
    "name": "DivisionByZero",
    "inputs": []
  },
  {
    "type": "error",
    "name": "PathFinder__NoRoute",
    "inputs": []
  },
  {
    "type": "error",
    "name": "PathFinder__SameToken",
    "inputs": []
  },
  {
    "type": "error",
    "name": "PathFinder__SlippageOutOfRange",
    "inputs": []
  },
  {
    "type": "error",
    "name": "PathFinder__VenueNotImplemented",
    "inputs": [
      {
        "name": "venue",
        "type": "uint8",
        "internalType": "uint8"
      }
    ]
  },
  {
    "type": "error",
    "name": "PathFinder__ZeroAmount",
    "inputs": []
  },
  {
    "type": "error",
    "name": "ZeroInput",
    "inputs": []
  }
]
```*/
#[allow(
    non_camel_case_types,
    non_snake_case,
    clippy::pub_underscore_fields,
    clippy::style,
    clippy::empty_structs_with_brackets
)]
pub mod PathFinder {
    use super::*;
    use alloy::sol_types as alloy_sol_types;
    /// The creation / init bytecode of the contract.
    ///
    /// ```text
    ///0x608080604052346015576129c9908161001a8239f35b5f80fdfe6101806040526004361015610012575f80fd5b5f3560e01c806321bf9f26146104e857806381c6ecd6146101ea5763c036c8ea1461003b575f80fd5b346101e65760a03660031901126101e657610054610696565b61005c6106ac565b6084359160643591604435906001600160401b0385116101e657366023860112156101e65784600401356001600160401b0381116101e65785019260248401933685116101e6576040906100ae61085c565b506100bb8786868661089b565b879003126101e65760248601359560ff87168097036101e6576044810135906001600160401b0382116101e65701846043820112156101e65760248101359061010382610880565b956101116040519788610824565b828752604482840101116101e657815f92604460209301838901378601015260028603610179576101439495506117c1565b60608101511561016a576101669161015a91610bf1565b604051918291826106c2565b0390f35b630541871160e01b5f5260045ffd5b600886036101915761018c949550611764565b610143565b600386036101a45761018c9495506116b4565b600986036101b75761018c9495506115b6565b85600581036101d457631602da9b60e21b5f52600560045260245ffd5b631602da9b60e21b5f5260045260245ffd5b5f80fd5b346101e65760403660031901126101e6576004356001600160401b0381116101e657366023820112156101e65780600401359061022682610845565b906102346040519283610824565b8282526024602083019360051b820101903682116101e65760248101935b8285106103ce5761026560243585610d48565b90604051918291604083016040845281518091526060840190602060608260051b8701019301915f905b82821061031757505050508281036020840152602080835192838152019201905f5b8181106102bf575050500390f35b91935091602060e060019260c0875180518352848101518584015260408101516040840152606081015160608401526080810151608084015260a081015160a0840152015160c08201520194019101918493926102b1565b91939092949550605f1987820301825284519060608101918051926060835283518091526020608084019401905f905b80821061037a5750505060019260209260408084868096015186850152015191015296019201920186959493919261028f565b909194602060e060019260c0895180518352848101518584015260408101516040840152606081015160608401526080810151608084015260a081015160a0840152015160c0820152019601920190610347565b84356001600160401b0381116101e6578201606060231982360301126101e657604051906103fb826107a4565b60248101356001600160401b0381116101e65760249082010136601f820112156101e657803561042a81610845565b916104386040519384610824565b818352602060e08185019302820101903682116101e657602001915b81831061048457505050916064602094928594835260448101358584015201356040820152815201940193610252565b60e0833603126101e657602060e09160405161049f816107d3565b85358152828601358382015260408601356040820152606086013560608201526080860135608082015260a086013560a082015260c086013560c0820152815201920191610454565b346101e65760803660031901126101e657610501610696565b6105096106ac565b6064359160443561051861085c565b506105258482858561089b565b6105308184846108e9565b9261053c828285610939565b846060808301519101511061068e575b506001600160a01b0383167382af49447d8a07e3bd95bd0d56f35241523fbab181141580610667575b610605575b73af88d065e77c8cc2239327c5edb3a432268e58311415806105de575b6105b6575b50505060608101511561016a576101669161015a91610bf1565b6105bf92610b21565b60608101516060830151106105d6575b808061059c565b9050826105cf565b506001600160a01b03811673af88d065e77c8cc2239327c5edb3a432268e58311415610597565b61061083838661099e565b61061b848487610a88565b90606081015160608801511061065f575b506060810151606087015110610643575b5061057a565b945073af88d065e77c8cc2239327c5edb3a432268e583161063d565b95508761062c565b506001600160a01b0382167382af49447d8a07e3bd95bd0d56f35241523fbab11415610575565b93508561054c565b600435906001600160a01b03821682036101e657565b602435906001600160a01b03821682036101e657565b6020815260a0810191805192608060208401528351809152602060c084019401905f5b81811061078557505050602081810151838503601f190160408501528051808652948201949101905f5b81811061076c57505050604081015192601f19838203016060840152602080855192838152019401905f5b818110610751575050506060608091015191015290565b825162ffffff1686526020958601959092019160010161073a565b825160ff1686526020958601959092019160010161070f565b82516001600160a01b03168652602095860195909201916001016106e5565b606081019081106001600160401b038211176107bf57604052565b634e487b7160e01b5f52604160045260245ffd5b60e081019081106001600160401b038211176107bf57604052565b608081019081106001600160401b038211176107bf57604052565b60a081019081106001600160401b038211176107bf57604052565b90601f801991011681019081106001600160401b038211176107bf57604052565b6001600160401b0381116107bf5760051b60200190565b60405190610869826107ee565b5f6060838181528160208201528160408201520152565b6001600160401b0381116107bf57601f01601f191660200190565b6001600160a01b039081169116146108da57156108cb576103e8106108bc57565b632a8406b960e01b5f5260045ffd5b63857e4aa960e01b5f5260045ffd5b634181f73f60e11b5f5260045ffd5b909291926109006108f861085c565b9482846119cd565b918215610934579061091191611bb7565b835261091b611c02565b6020840152610928611c02565b60408401526060830152565b505050565b9092919261095061094861085c565b948284611c57565b919092831561099857610928929161096791611bb7565b8552604051610977604082610824565b6001815260203681830137600161098d82610d03565b526020860152611c29565b50505050565b909291926109c96109ad61085c565b947382af49447d8a07e3bd95bd0d56f35241523fbab1846119cd565b8015610934576109ee90827382af49447d8a07e3bd95bd0d56f35241523fbab16119cd565b91821561093457907382af49447d8a07e3bd95bd0d56f35241523fbab1610a1492611cdf565b8352604051610a24606082610824565b60028152604090813660208301375f610a3c82610d03565b525f610a4782610d24565b52602085015260405190610a5c606083610824565b600282523660208301375f610a7082610d03565b525f610a7b82610d24565b5260408401526060830152565b90929192610ab3610a9761085c565b947382af49447d8a07e3bd95bd0d56f35241523fbab184611c57565b90801561099857610ad990837382af49447d8a07e3bd95bd0d56f35241523fbab1611c57565b9290938415610b1a576109289392917382af49447d8a07e3bd95bd0d56f35241523fbab1610b0692611cdf565b8652610b10611d49565b6020870152611d7a565b5050505050565b90929192610b4c610b3061085c565b9473af88d065e77c8cc2239327c5edb3a432268e583184611c57565b90801561099857610b72908373af88d065e77c8cc2239327c5edb3a432268e5831611c57565b9290938415610b1a5761092893929173af88d065e77c8cc2239327c5edb3a432268e5831610b0692611cdf565b91908203918211610bac57565b634e487b7160e01b5f52601160045260245ffd5b81810292918115918404141715610bac57565b8115610bdd570490565b634e487b7160e01b5f52601260045260245ffd5b90610bfa61085c565b5080610c04575090565b606082019081519061271003906127108211610bac5761271091610c2791610bc0565b04905290565b60405190610c3a826107a4565b5f604083606081528260208201520152565b90610c5682610845565b610c636040519182610824565b8281528092610c74601f1991610845565b01905f5b828110610c8457505050565b602090610c8f610c2d565b82828501015201610c78565b60405190610ca8826107d3565b5f60c0838281528260208201528260408201528260608201528260808201528260a08201520152565b90610cdb82610845565b610ce86040519182610824565b8281528092610cf9601f1991610845565b0190602036910137565b805115610d105760200190565b634e487b7160e01b5f52603260045260245ffd5b805160011015610d105760400190565b8051821015610d105760209160051b010190565b80511561153257610d598151610cd1565b905f5b8151811015610da957806002610d7460019385610d34565b5151511015610d90575f5b610d898286610d34565b5201610d5c565b610da4610d9d8285610d34565b5151611db2565b610d7f565b5091610db58351610cd1565b91835194610dc286610845565b95610dd06040519788610824565b808752610ddf601f1991610845565b013660208801375f925f5b8651811015610e8f57610dfd8185610d34565b515f81610e3a575b15610e14575b50600101610dea565b856001929691610e2584938a610d34565b5281610e31828c610d34565b52019490610e0b565b5f5b878110610e4a575b50610e05565b82610e55828b610d34565b5114610e6357600101610e3c565b9050610e6f818b610d34565b515f198114610bac576001610e869101918b610d34565b5260015f610e44565b50939590949195610e9f84610c4c565b945f975f5b8681106114ee5750610eb589610845565b98610ec36040519a8b610824565b808a52610ed2601f1991610845565b015f5b8181106114d057505060405194610eeb86610809565b8552602085015285604085015287606085015260808401525f935f5f935b828510610f1b57505050505050509190565b610f2c8585989b979a969995610d34565b51610100526001610f3d8983610d34565b5103610fc8575f5b895151811015610fb45761010051610f618260208d0151610d34565b5114610f6f57600101610f45565b8a610fa48b989c610f8d60019597999d9486959d9c97999d51610d34565b5160408b015190610f9e8383610d34565b52610d34565b505b01965b019391929092610f09565b509193979498600180919897929498610fa6565b9492610fdc88879a99969a98949398610d34565b5161014052875198602089015161016052608089015196610ffb610c2d565b50611004610c9b565b5061101161014051610c4c565b610120525f955f5b8c518110156114be578c610100516110348361016051610d34565b5114611044575b50600101611019565b97611052826001939a610d34565b516110608261012051610d34565b5261106e8161012051610d34565b500196610140518814611081578c61103b565b509391955093919596979899505b5f60e05261109f61012051610d03565b51516110ad61012051610d03565b5151515f198101908111610bac576110c491610d34565b5195608087015180670de0b6b3a7640000810204670de0b6b3a76400001481151715610bac57606088015161110291670de0b6b3a764000002610bd3565b60c0525f5b61012051518110156111dc576111208161012051610d34565b5151516111308261012051610d34565b51516001198201828111610bac5761114791610d34565b51906111568361012051610d34565b5151915f198201918211610bac5761117360809261118094610d34565b518252015160e0516119c0565b60e052608080510151670de0b6b3a7640000810290808204670de0b6b3a76400001490151715610bac57608051606001516111ba91610bd3565b60c05181116111cd575b50600101611107565b60c052608051975060016111c4565b50909192939a959697989994670de0b6b3a764000061120c61120460c08b015160e051612758565b60e051610bc0565b049760a081015180603e810204603e1481151715610bac57604051996112318b610809565b5f60808c0152610100518b526101405160208c015260e05160408c01528060608c015261126061012051610d03565b515161126e61012051610d03565b51515180600119810111610bac576001190161128991610d34565b51604001519c611297610c9b565b809e855182526020820152604001528c60e051906060015260808d0152603e026064900460a08c015260c0015160c08b0152610120516112d690610d03565b51519b8c516112e481610845565b60405160a05260a051906112f791610824565b8060a05152601f199061130990610845565b015f5b8181106114a55750505f5b600181018111610bac578d5160018201101561135c578061133b8f92600193610d34565b516113488260a051610d34565b526113558160a051610d34565b5001611317565b5092989a979990939b9594919551805f19810111610bac57611393916113875f19830160a051610d34565b525f190160a051610d34565b5061139f60a0516128da565b92604084015160808b01526113b2610c9b565b955f995f945f955b6101205151871015611427578c60206113d68961012051610d34565b51015111611405575b6113fd60019160406113f48a61012051610d34565b510151906119c0565b9601956113ba565b9b5060016113fd602061141b8961012051610d34565b5101519d9150506113df565b600196509886959f9786959f9a9b80959f918e9f8c9f959c61149b979f608061148a9660409481518a52602082015160208b015285820151868b0152606082015160608b0152828a0152015160a088015260c0870152015190610f9e8383610d34565b5060608c015190610f9e8383610d34565b5001970193610fa9565b6020906114b0610c9b565b828260a0510101520161130c565b5093919550939195969798995061108f565b808b602080936114e19b999b610c9b565b9201015201969496610ed5565b60016114fc82879997610d34565b51118061151f575b611514575b600101959395610ea4565b600190990198611509565b5061152a8185610d34565b511515611504565b505060405190611543602083610824565b5f82525f805b81811061158b57505060405191611561602084610824565b5f83525f805b8181106115745750509190565b60209061157f610c9b565b82828801015201611567565b602090611596610c2d565b82828701015201611549565b51906001600160a01b03821682036101e657565b9193926115c161085c565b946060828051810103126101e6576115db602083016115a2565b91606060408201519101519260ff84168094036101e6576001600160a01b03168015801561169e575b6116965784918691600286036116645761161e9550611f39565b915b8215610934579061163091611bb7565b8352604051611640604082610824565b6001815260203681830137600561165682610d03565b526020840152610928611c02565b9394909250600314159050610b1a576001600160a01b0316908115610b1a578461169093928592611e7c565b91611620565b505050505050565b50803b15611604565b519081151582036101e657565b9193926116bf61085c565b946080828051810103126101e6576116d9602083016115a2565b9160408101516116f06080606084015193016116a7565b936001600160a01b03168015801561175b575b61175257828214611752579061171b94939291612266565b918215610934579061172c91611bb7565b835260405161173c604082610824565b6001815260203681830137600361165682610d03565b50505050505050565b50803b15611703565b9193929361177061085c565b946040818051810103126101e657611796604061178f602084016115a2565b92016116a7565b506001600160a01b0316801580156117b8575b61099857908361090092612333565b50803b156117a9565b919392906117cd61085c565b9482518301908360208301920360e081126101e65760a0136101e657604051936117f685610809565b611802602082016115a2565b8552611810604082016115a2565b946020810195865260608201519562ffffff871687036101e6576040820196875260808301518060020b81036101e657606083015261185160a084016115a2565b608083015261186260c084016116a7565b9260e0810151906001600160401b0382116101e6570185603f820112156101e65760208101519061189282610880565b966118a06040519889610824565b828852604082840101116101e657815f92604060209301838a015e8701015282156119525781516001600160a01b03898116911614908161193b575b505b1561175257906118ef939291612377565b928315610998579161190862ffffff9261092894611bb7565b8652604051611918604082610824565b6001815260203681830137600261192e82610d03565b5260208701525116611c29565b516001600160a01b0387811691161490505f6118dc565b516001600160a01b0388811691161480156118de575080516001600160a01b038681169116146118de565b3d156119a7573d9061198e82610880565b9161199c6040519384610824565b82523d5f602084013e565b606090565b51906001600160701b03821682036101e657565b91908201809211610bac57565b906119d89082612487565b6001600160a01b038116158015611bae575b611ba7575f806040516020810190630240bc6b60e21b825260048152611a11602482610824565b5190845afa91611a1f61197d565b92158015611b9c575b611b66576060838051810103126101e657611a45602084016119ac565b916060611a54604086016119ac565b94015163ffffffff8116036101e6575f80916040516020810190630dfe168160e01b825260048152611a87602482610824565b51915afa611a9361197d565b90158015611b91575b611b88576020818051810103126101e6576001600160a01b0390611ac2906020016115a2565b6001600160a01b03909216911603611b76576001600160701b0391821691165b801591828015611b6e575b611b6657611b19604051611b00816107a4565b60108152602860208201526010604082015282846126c0565b15611b66576103e58402938085046103e51490151715610bac57611b3d9084610bc0565b916103e882029182046103e8141715610bac57611b6392611b5d916119c0565b90610bd3565b90565b505050505f90565b508015611aed565b6001600160701b039081169116611ae2565b50505050505f90565b506020815110611a9c565b506060835110611a28565b5050505f90565b50803b156119ea565b9190611bf3604051611bca606082610824565b6002815260403660208301378094611be182610d03565b6001600160a01b039091169052610d24565b6001600160a01b039091169052565b60405190611c11604083610824565b60018252602036818401375f611c2683610d03565b52565b9060405191611c39604084610824565b600183526020368185013762ffffff611c5184610d03565b91169052565b919290925f935f93611c6a838383612526565b80611cd3575b50611c7c83838361259d565b868111611cc6575b50611c908383836125f1565b868111611cb7575b5090611ca49291612645565b838111611cae5750565b92506127109150565b9550610bb89450611ca4611c98565b95506101f494505f611c84565b9550606494505f611c70565b92919060405190611cf1608083610824565b6003825260603660208401378194611d0883610d03565b6001600160a01b039091169052611d1e82610d24565b6001600160a01b039091169052805160021015610d10576001600160a01b0390911660609190910152565b60405190611d58606083610824565b6002825260403660208401376001611c268382611d7482610d03565b52610d24565b919062ffffff611c51604051611d91606082610824565b600281526040366020830137809583611da983610d03565b91169052610d24565b90815160028110611e52575f198101908111610bac57611dd181610cd1565b905f5b818110611e2f5750509091506040516020810181819360208151939101925f5b818110611e16575050611e10925003601f198101835282610824565b51902090565b8451835260209485019486945090920191600101611df4565b806040611e3e60019388610d34565b510151611e4b8286610d34565b5201611dd4565b505f9150565b805180835260209291819084018484015e5f828201840152601f01601f1916010190565b905f8094611efa8295611eec60209960405190611e998c83610824565b8682526040516307d245e960e41b8d82019081526001600160a01b03998a1660248301529489166044820152959097166064860152608485019690965260a060a4850152909483919060c4830190611e58565b03601f198101835282610824565b51925af190611f0761197d565b91158015611f2f575b611f2957815181830192018101829003126101e6575190565b50505f90565b5080825110611f10565b9091939293604094855193611f4e8786610824565b60018552601f1987015f5b8181106122325750508651602096611f718883610824565b5f8252885192611f8084610809565b83525f8884015260018984015260608301526080820152611fa085610d03565b52611faa84610d03565b50606093865191611fbb8684610824565b6002835286830193601f198701368637611fd484610d03565b6001600160a01b039091169052611fea83610d24565b6001600160a01b0390911690528651612002816107ee565b308152868101905f82528881019230845288888301955f87528b80519a637c26833760e11b848d01528b6101048101915f602483015260e060448301528651809352610124820190866101248560051b8501019801945f935b8585106121d05750505050508b85036023190160648d0152505051808352910195905f5b8a8282106121b357505091516001600160a01b0390811660848a01529251151560a4890152505090511660c485015251151560e4840152829003601f19810183525f92839290916120d09083610824565b828583519301915af16120e161197d565b901580156121a9575b611ba757805181019082818184019303126101e65782810151906001600160401b0382116101e657019281603f850112156101e657828401519061212d82610845565b9461213a82519687610824565b82865284808088019460051b830101019384116101e65701905b82821061219a575050505060028151106121955761217190610d24565b515f811361219557801561219557600160ff1b8114610bac57611b63905f03612699565b505f90565b81518152908301908301612154565b50828151106120ea565b83516001600160a01b03168952978801979092019160010161207f565b889294969960a06080600196989a9b9461221d9461012319908503018a528d5190815185528682015187860152808201519085015288810151898501520151918160808201520190611e58565b98019301930190928f938f969593948f61205b565b602090895161224081610809565b5f81525f838201525f8b8201525f60608201526060608082015282828a01015201611f59565b5f94859491939290156122fd5761227f612285916126ae565b926126ae565b60405192635e0d443f60e01b6020850152600f0b6024840152600f0b60448301526064820152606481526122ba608482610824565b905b602082519201905afa6122cd61197d565b901580156122f2575b61219557602081519181808201938492010103126101e6575190565b5060208151106122d6565b916040519263556d6e9f60e01b60208501526024840152604483015260648201526064815261232d608482610824565b906122bc565b5f9283926040519060208201926378a051ad60e11b8452602483015260018060a01b031660448201526044815261236b606482610824565b51915afa6122cd61197d565b9091600160801b81101561247a576124585f9493611eec86956040519561239d876107ee565b86526020860192151583526001600160801b036040870195168552606086019081526001600160801b03604051958694602086019863aa9d21cb60e01b8a52602060248801525160018060a01b03815116604488015260018060a01b03602082015116606488015262ffffff6040820151166084880152606081015160020b60a4880152608060018060a01b039101511660c487015251151560e4860152511661010484015251610100610124840152610144830190611e58565b519082733972c00f7ed4885e145823eb7c655375d275a1c55af16122cd61197d565b6335278d125f526004601cfd5b60405163e6a4390560e01b602082019081526001600160a01b0392831660248301529290911660448083019190915281525f9182916124c7606482610824565b519073f1d7cc64fb4452f05c498126312ebe29f30fbcf95afa6124e861197d565b9015801561251b575b612195576020818051810103126101e6576001600160a01b0390612517906020016115a2565b1690565b5060208151106124f1565b604051636352813560e11b602082019081526001600160a01b03928316602483015291909216604483015260648083019390935260848201929092525f60a4808301829052825291829161257b60c482610824565b5190827361ffe014ba17989e743c5f6cb21bf9697530b21e5af16122cd61197d565b604051636352813560e11b602082019081526001600160a01b03928316602483015291909216604483015260648201929092526101f460848201525f60a4808301829052825291829161257b60c482610824565b604051636352813560e11b602082019081526001600160a01b0392831660248301529190921660448301526064820192909252610bb860848201525f60a4808301829052825291829161257b60c482610824565b604051636352813560e11b602082019081526001600160a01b039283166024830152919092166044830152606482019290925261271060848201525f60a4808301829052825291829161257b60c482610824565b5f811215611b63576335278d125f526004601cfd5b6001607f1b81101561247a57600f0b90565b80158015612714575b611ba7576126e761ffff84511661ffff60208601511690848461271c565b611ba757612702818360409361ffff95109082180218612943565b920151161161271057600190565b5f90565b5081156126c9565b918061272784612943565b1061274f5761273582612943565b106127475761274391612953565b1190565b505050600190565b50505050600190565b908015611f295781612769916119c0565b80156128cb57670de0b6b3a7640000820291818115670de0b6b3a764000083860414170215612856575090045b6003810290606481156003838504141702156127f85750606490045b660aa87bee5380008101670de0b6b3a764000011156127f157670dd60e37b9108000035b8067016345785d8a00001167016345785d8a00008218021890565b505f6127d6565b606460035f1981840984811085019003920990806064111561284957828211900360fe1b910360021c177f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c29026127b2565b63ae47f7025f526004601cfd5b81670de0b6b3a76400005f1981840985811086019003920990825f03831692818111156128495783900480600302600218808202600203028082026002030280820260020302808202600203028082026002030280910260020302936001848483030494805f03040192119003021702612796565b6323d359a360e01b5f5260045ffd5b906128e3610c2d565b8281528251801561293e575f198101908111610bac5761290560809185610d34565b51015160208201525f90815b84518310156129345761292c60019160a06113f48689610d34565b920191612911565b6040820152925050565b509150565b8015612195571e60ff1860010190565b80158015612995575b61298e5761296c6129729161299d565b9161299d565b90818111156129855790611b6391610b9f565b611b6391610b9f565b50505f1990565b50811561295c565b80156129ad576001171e60ff0390565b63af458c0760e01b5f5260045ffdfea164736f6c6343000822000a
    /// ```
    #[rustfmt::skip]
    #[allow(clippy::all)]
    pub static BYTECODE: alloy_sol_types::private::Bytes = alloy_sol_types::private::Bytes::from_static(
        b"`\x80\x80`@R4`\x15Wa)\xC9\x90\x81a\0\x1A\x829\xF3[_\x80\xFD\xFEa\x01\x80`@R`\x046\x10\x15a\0\x12W_\x80\xFD[_5`\xE0\x1C\x80c!\xBF\x9F&\x14a\x04\xE8W\x80c\x81\xC6\xEC\xD6\x14a\x01\xEAWc\xC06\xC8\xEA\x14a\0;W_\x80\xFD[4a\x01\xE6W`\xA06`\x03\x19\x01\x12a\x01\xE6Wa\0Ta\x06\x96V[a\0\\a\x06\xACV[`\x845\x91`d5\x91`D5\x90`\x01`\x01`@\x1B\x03\x85\x11a\x01\xE6W6`#\x86\x01\x12\x15a\x01\xE6W\x84`\x04\x015`\x01`\x01`@\x1B\x03\x81\x11a\x01\xE6W\x85\x01\x92`$\x84\x01\x936\x85\x11a\x01\xE6W`@\x90a\0\xAEa\x08\\V[Pa\0\xBB\x87\x86\x86\x86a\x08\x9BV[\x87\x90\x03\x12a\x01\xE6W`$\x86\x015\x95`\xFF\x87\x16\x80\x97\x03a\x01\xE6W`D\x81\x015\x90`\x01`\x01`@\x1B\x03\x82\x11a\x01\xE6W\x01\x84`C\x82\x01\x12\x15a\x01\xE6W`$\x81\x015\x90a\x01\x03\x82a\x08\x80V[\x95a\x01\x11`@Q\x97\x88a\x08$V[\x82\x87R`D\x82\x84\x01\x01\x11a\x01\xE6W\x81_\x92`D` \x93\x01\x83\x89\x017\x86\x01\x01R`\x02\x86\x03a\x01yWa\x01C\x94\x95Pa\x17\xC1V[``\x81\x01Q\x15a\x01jWa\x01f\x91a\x01Z\x91a\x0B\xF1V[`@Q\x91\x82\x91\x82a\x06\xC2V[\x03\x90\xF3[c\x05A\x87\x11`\xE0\x1B_R`\x04_\xFD[`\x08\x86\x03a\x01\x91Wa\x01\x8C\x94\x95Pa\x17dV[a\x01CV[`\x03\x86\x03a\x01\xA4Wa\x01\x8C\x94\x95Pa\x16\xB4V[`\t\x86\x03a\x01\xB7Wa\x01\x8C\x94\x95Pa\x15\xB6V[\x85`\x05\x81\x03a\x01\xD4Wc\x16\x02\xDA\x9B`\xE2\x1B_R`\x05`\x04R`$_\xFD[c\x16\x02\xDA\x9B`\xE2\x1B_R`\x04R`$_\xFD[_\x80\xFD[4a\x01\xE6W`@6`\x03\x19\x01\x12a\x01\xE6W`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x01\xE6W6`#\x82\x01\x12\x15a\x01\xE6W\x80`\x04\x015\x90a\x02&\x82a\x08EV[\x90a\x024`@Q\x92\x83a\x08$V[\x82\x82R`$` \x83\x01\x93`\x05\x1B\x82\x01\x01\x906\x82\x11a\x01\xE6W`$\x81\x01\x93[\x82\x85\x10a\x03\xCEWa\x02e`$5\x85a\rHV[\x90`@Q\x91\x82\x91`@\x83\x01`@\x84R\x81Q\x80\x91R``\x84\x01\x90` ``\x82`\x05\x1B\x87\x01\x01\x93\x01\x91_\x90[\x82\x82\x10a\x03\x17WPPPP\x82\x81\x03` \x84\x01R` \x80\x83Q\x92\x83\x81R\x01\x92\x01\x90_[\x81\x81\x10a\x02\xBFWPPP\x03\x90\xF3[\x91\x93P\x91` `\xE0`\x01\x92`\xC0\x87Q\x80Q\x83R\x84\x81\x01Q\x85\x84\x01R`@\x81\x01Q`@\x84\x01R``\x81\x01Q``\x84\x01R`\x80\x81\x01Q`\x80\x84\x01R`\xA0\x81\x01Q`\xA0\x84\x01R\x01Q`\xC0\x82\x01R\x01\x94\x01\x91\x01\x91\x84\x93\x92a\x02\xB1V[\x91\x93\x90\x92\x94\x95P`_\x19\x87\x82\x03\x01\x82R\x84Q\x90``\x81\x01\x91\x80Q\x92``\x83R\x83Q\x80\x91R` `\x80\x84\x01\x94\x01\x90_\x90[\x80\x82\x10a\x03zWPPP`\x01\x92` \x92`@\x80\x84\x86\x80\x96\x01Q\x86\x85\x01R\x01Q\x91\x01R\x96\x01\x92\x01\x92\x01\x86\x95\x94\x93\x91\x92a\x02\x8FV[\x90\x91\x94` `\xE0`\x01\x92`\xC0\x89Q\x80Q\x83R\x84\x81\x01Q\x85\x84\x01R`@\x81\x01Q`@\x84\x01R``\x81\x01Q``\x84\x01R`\x80\x81\x01Q`\x80\x84\x01R`\xA0\x81\x01Q`\xA0\x84\x01R\x01Q`\xC0\x82\x01R\x01\x96\x01\x92\x01\x90a\x03GV[\x845`\x01`\x01`@\x1B\x03\x81\x11a\x01\xE6W\x82\x01```#\x19\x826\x03\x01\x12a\x01\xE6W`@Q\x90a\x03\xFB\x82a\x07\xA4V[`$\x81\x015`\x01`\x01`@\x1B\x03\x81\x11a\x01\xE6W`$\x90\x82\x01\x016`\x1F\x82\x01\x12\x15a\x01\xE6W\x805a\x04*\x81a\x08EV[\x91a\x048`@Q\x93\x84a\x08$V[\x81\x83R` `\xE0\x81\x85\x01\x93\x02\x82\x01\x01\x906\x82\x11a\x01\xE6W` \x01\x91[\x81\x83\x10a\x04\x84WPPP\x91`d` \x94\x92\x85\x94\x83R`D\x81\x015\x85\x84\x01R\x015`@\x82\x01R\x81R\x01\x94\x01\x93a\x02RV[`\xE0\x836\x03\x12a\x01\xE6W` `\xE0\x91`@Qa\x04\x9F\x81a\x07\xD3V[\x855\x81R\x82\x86\x015\x83\x82\x01R`@\x86\x015`@\x82\x01R``\x86\x015``\x82\x01R`\x80\x86\x015`\x80\x82\x01R`\xA0\x86\x015`\xA0\x82\x01R`\xC0\x86\x015`\xC0\x82\x01R\x81R\x01\x92\x01\x91a\x04TV[4a\x01\xE6W`\x806`\x03\x19\x01\x12a\x01\xE6Wa\x05\x01a\x06\x96V[a\x05\ta\x06\xACV[`d5\x91`D5a\x05\x18a\x08\\V[Pa\x05%\x84\x82\x85\x85a\x08\x9BV[a\x050\x81\x84\x84a\x08\xE9V[\x92a\x05<\x82\x82\x85a\t9V[\x84``\x80\x83\x01Q\x91\x01Q\x10a\x06\x8EW[P`\x01`\x01`\xA0\x1B\x03\x83\x16s\x82\xAFID}\x8A\x07\xE3\xBD\x95\xBD\rV\xF3RAR?\xBA\xB1\x81\x14\x15\x80a\x06gW[a\x06\x05W[s\xAF\x88\xD0e\xE7|\x8C\xC2#\x93'\xC5\xED\xB3\xA42&\x8EX1\x14\x15\x80a\x05\xDEW[a\x05\xB6W[PPP``\x81\x01Q\x15a\x01jWa\x01f\x91a\x01Z\x91a\x0B\xF1V[a\x05\xBF\x92a\x0B!V[``\x81\x01Q``\x83\x01Q\x10a\x05\xD6W[\x80\x80a\x05\x9CV[\x90P\x82a\x05\xCFV[P`\x01`\x01`\xA0\x1B\x03\x81\x16s\xAF\x88\xD0e\xE7|\x8C\xC2#\x93'\xC5\xED\xB3\xA42&\x8EX1\x14\x15a\x05\x97V[a\x06\x10\x83\x83\x86a\t\x9EV[a\x06\x1B\x84\x84\x87a\n\x88V[\x90``\x81\x01Q``\x88\x01Q\x10a\x06_W[P``\x81\x01Q``\x87\x01Q\x10a\x06CW[Pa\x05zV[\x94Ps\xAF\x88\xD0e\xE7|\x8C\xC2#\x93'\xC5\xED\xB3\xA42&\x8EX1a\x06=V[\x95P\x87a\x06,V[P`\x01`\x01`\xA0\x1B\x03\x82\x16s\x82\xAFID}\x8A\x07\xE3\xBD\x95\xBD\rV\xF3RAR?\xBA\xB1\x14\x15a\x05uV[\x93P\x85a\x05LV[`\x045\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x82\x03a\x01\xE6WV[`$5\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x82\x03a\x01\xE6WV[` \x81R`\xA0\x81\x01\x91\x80Q\x92`\x80` \x84\x01R\x83Q\x80\x91R` `\xC0\x84\x01\x94\x01\x90_[\x81\x81\x10a\x07\x85WPPP` \x81\x81\x01Q\x83\x85\x03`\x1F\x19\x01`@\x85\x01R\x80Q\x80\x86R\x94\x82\x01\x94\x91\x01\x90_[\x81\x81\x10a\x07lWPPP`@\x81\x01Q\x92`\x1F\x19\x83\x82\x03\x01``\x84\x01R` \x80\x85Q\x92\x83\x81R\x01\x94\x01\x90_[\x81\x81\x10a\x07QWPPP```\x80\x91\x01Q\x91\x01R\x90V[\x82Qb\xFF\xFF\xFF\x16\x86R` \x95\x86\x01\x95\x90\x92\x01\x91`\x01\x01a\x07:V[\x82Q`\xFF\x16\x86R` \x95\x86\x01\x95\x90\x92\x01\x91`\x01\x01a\x07\x0FV[\x82Q`\x01`\x01`\xA0\x1B\x03\x16\x86R` \x95\x86\x01\x95\x90\x92\x01\x91`\x01\x01a\x06\xE5V[``\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x07\xBFW`@RV[cNH{q`\xE0\x1B_R`A`\x04R`$_\xFD[`\xE0\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x07\xBFW`@RV[`\x80\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x07\xBFW`@RV[`\xA0\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x07\xBFW`@RV[\x90`\x1F\x80\x19\x91\x01\x16\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x07\xBFW`@RV[`\x01`\x01`@\x1B\x03\x81\x11a\x07\xBFW`\x05\x1B` \x01\x90V[`@Q\x90a\x08i\x82a\x07\xEEV[_``\x83\x81\x81R\x81` \x82\x01R\x81`@\x82\x01R\x01RV[`\x01`\x01`@\x1B\x03\x81\x11a\x07\xBFW`\x1F\x01`\x1F\x19\x16` \x01\x90V[`\x01`\x01`\xA0\x1B\x03\x90\x81\x16\x91\x16\x14a\x08\xDAW\x15a\x08\xCBWa\x03\xE8\x10a\x08\xBCWV[c*\x84\x06\xB9`\xE0\x1B_R`\x04_\xFD[c\x85~J\xA9`\xE0\x1B_R`\x04_\xFD[cA\x81\xF7?`\xE1\x1B_R`\x04_\xFD[\x90\x92\x91\x92a\t\0a\x08\xF8a\x08\\V[\x94\x82\x84a\x19\xCDV[\x91\x82\x15a\t4W\x90a\t\x11\x91a\x1B\xB7V[\x83Ra\t\x1Ba\x1C\x02V[` \x84\x01Ra\t(a\x1C\x02V[`@\x84\x01R``\x83\x01RV[PPPV[\x90\x92\x91\x92a\tPa\tHa\x08\\V[\x94\x82\x84a\x1CWV[\x91\x90\x92\x83\x15a\t\x98Wa\t(\x92\x91a\tg\x91a\x1B\xB7V[\x85R`@Qa\tw`@\x82a\x08$V[`\x01\x81R` 6\x81\x83\x017`\x01a\t\x8D\x82a\r\x03V[R` \x86\x01Ra\x1C)V[PPPPV[\x90\x92\x91\x92a\t\xC9a\t\xADa\x08\\V[\x94s\x82\xAFID}\x8A\x07\xE3\xBD\x95\xBD\rV\xF3RAR?\xBA\xB1\x84a\x19\xCDV[\x80\x15a\t4Wa\t\xEE\x90\x82s\x82\xAFID}\x8A\x07\xE3\xBD\x95\xBD\rV\xF3RAR?\xBA\xB1a\x19\xCDV[\x91\x82\x15a\t4W\x90s\x82\xAFID}\x8A\x07\xE3\xBD\x95\xBD\rV\xF3RAR?\xBA\xB1a\n\x14\x92a\x1C\xDFV[\x83R`@Qa\n$``\x82a\x08$V[`\x02\x81R`@\x90\x816` \x83\x017_a\n<\x82a\r\x03V[R_a\nG\x82a\r$V[R` \x85\x01R`@Q\x90a\n\\``\x83a\x08$V[`\x02\x82R6` \x83\x017_a\np\x82a\r\x03V[R_a\n{\x82a\r$V[R`@\x84\x01R``\x83\x01RV[\x90\x92\x91\x92a\n\xB3a\n\x97a\x08\\V[\x94s\x82\xAFID}\x8A\x07\xE3\xBD\x95\xBD\rV\xF3RAR?\xBA\xB1\x84a\x1CWV[\x90\x80\x15a\t\x98Wa\n\xD9\x90\x83s\x82\xAFID}\x8A\x07\xE3\xBD\x95\xBD\rV\xF3RAR?\xBA\xB1a\x1CWV[\x92\x90\x93\x84\x15a\x0B\x1AWa\t(\x93\x92\x91s\x82\xAFID}\x8A\x07\xE3\xBD\x95\xBD\rV\xF3RAR?\xBA\xB1a\x0B\x06\x92a\x1C\xDFV[\x86Ra\x0B\x10a\x1DIV[` \x87\x01Ra\x1DzV[PPPPPV[\x90\x92\x91\x92a\x0BLa\x0B0a\x08\\V[\x94s\xAF\x88\xD0e\xE7|\x8C\xC2#\x93'\xC5\xED\xB3\xA42&\x8EX1\x84a\x1CWV[\x90\x80\x15a\t\x98Wa\x0Br\x90\x83s\xAF\x88\xD0e\xE7|\x8C\xC2#\x93'\xC5\xED\xB3\xA42&\x8EX1a\x1CWV[\x92\x90\x93\x84\x15a\x0B\x1AWa\t(\x93\x92\x91s\xAF\x88\xD0e\xE7|\x8C\xC2#\x93'\xC5\xED\xB3\xA42&\x8EX1a\x0B\x06\x92a\x1C\xDFV[\x91\x90\x82\x03\x91\x82\x11a\x0B\xACWV[cNH{q`\xE0\x1B_R`\x11`\x04R`$_\xFD[\x81\x81\x02\x92\x91\x81\x15\x91\x84\x04\x14\x17\x15a\x0B\xACWV[\x81\x15a\x0B\xDDW\x04\x90V[cNH{q`\xE0\x1B_R`\x12`\x04R`$_\xFD[\x90a\x0B\xFAa\x08\\V[P\x80a\x0C\x04WP\x90V[``\x82\x01\x90\x81Q\x90a'\x10\x03\x90a'\x10\x82\x11a\x0B\xACWa'\x10\x91a\x0C'\x91a\x0B\xC0V[\x04\x90R\x90V[`@Q\x90a\x0C:\x82a\x07\xA4V[_`@\x83``\x81R\x82` \x82\x01R\x01RV[\x90a\x0CV\x82a\x08EV[a\x0Cc`@Q\x91\x82a\x08$V[\x82\x81R\x80\x92a\x0Ct`\x1F\x19\x91a\x08EV[\x01\x90_[\x82\x81\x10a\x0C\x84WPPPV[` \x90a\x0C\x8Fa\x0C-V[\x82\x82\x85\x01\x01R\x01a\x0CxV[`@Q\x90a\x0C\xA8\x82a\x07\xD3V[_`\xC0\x83\x82\x81R\x82` \x82\x01R\x82`@\x82\x01R\x82``\x82\x01R\x82`\x80\x82\x01R\x82`\xA0\x82\x01R\x01RV[\x90a\x0C\xDB\x82a\x08EV[a\x0C\xE8`@Q\x91\x82a\x08$V[\x82\x81R\x80\x92a\x0C\xF9`\x1F\x19\x91a\x08EV[\x01\x90` 6\x91\x017V[\x80Q\x15a\r\x10W` \x01\x90V[cNH{q`\xE0\x1B_R`2`\x04R`$_\xFD[\x80Q`\x01\x10\x15a\r\x10W`@\x01\x90V[\x80Q\x82\x10\x15a\r\x10W` \x91`\x05\x1B\x01\x01\x90V[\x80Q\x15a\x152Wa\rY\x81Qa\x0C\xD1V[\x90_[\x81Q\x81\x10\x15a\r\xA9W\x80`\x02a\rt`\x01\x93\x85a\r4V[QQQ\x10\x15a\r\x90W_[a\r\x89\x82\x86a\r4V[R\x01a\r\\V[a\r\xA4a\r\x9D\x82\x85a\r4V[QQa\x1D\xB2V[a\r\x7FV[P\x91a\r\xB5\x83Qa\x0C\xD1V[\x91\x83Q\x94a\r\xC2\x86a\x08EV[\x95a\r\xD0`@Q\x97\x88a\x08$V[\x80\x87Ra\r\xDF`\x1F\x19\x91a\x08EV[\x016` \x88\x017_\x92_[\x86Q\x81\x10\x15a\x0E\x8FWa\r\xFD\x81\x85a\r4V[Q_\x81a\x0E:W[\x15a\x0E\x14W[P`\x01\x01a\r\xEAV[\x85`\x01\x92\x96\x91a\x0E%\x84\x93\x8Aa\r4V[R\x81a\x0E1\x82\x8Ca\r4V[R\x01\x94\x90a\x0E\x0BV[_[\x87\x81\x10a\x0EJW[Pa\x0E\x05V[\x82a\x0EU\x82\x8Ba\r4V[Q\x14a\x0EcW`\x01\x01a\x0E<V[\x90Pa\x0Eo\x81\x8Ba\r4V[Q_\x19\x81\x14a\x0B\xACW`\x01a\x0E\x86\x91\x01\x91\x8Ba\r4V[R`\x01_a\x0EDV[P\x93\x95\x90\x94\x91\x95a\x0E\x9F\x84a\x0CLV[\x94_\x97_[\x86\x81\x10a\x14\xEEWPa\x0E\xB5\x89a\x08EV[\x98a\x0E\xC3`@Q\x9A\x8Ba\x08$V[\x80\x8ARa\x0E\xD2`\x1F\x19\x91a\x08EV[\x01_[\x81\x81\x10a\x14\xD0WPP`@Q\x94a\x0E\xEB\x86a\x08\tV[\x85R` \x85\x01R\x85`@\x85\x01R\x87``\x85\x01R`\x80\x84\x01R_\x93__\x93[\x82\x85\x10a\x0F\x1BWPPPPPPP\x91\x90V[a\x0F,\x85\x85\x98\x9B\x97\x9A\x96\x99\x95a\r4V[Qa\x01\0R`\x01a\x0F=\x89\x83a\r4V[Q\x03a\x0F\xC8W_[\x89QQ\x81\x10\x15a\x0F\xB4Wa\x01\0Qa\x0Fa\x82` \x8D\x01Qa\r4V[Q\x14a\x0FoW`\x01\x01a\x0FEV[\x8Aa\x0F\xA4\x8B\x98\x9Ca\x0F\x8D`\x01\x95\x97\x99\x9D\x94\x86\x95\x9D\x9C\x97\x99\x9DQa\r4V[Q`@\x8B\x01Q\x90a\x0F\x9E\x83\x83a\r4V[Ra\r4V[P[\x01\x96[\x01\x93\x91\x92\x90\x92a\x0F\tV[P\x91\x93\x97\x94\x98`\x01\x80\x91\x98\x97\x92\x94\x98a\x0F\xA6V[\x94\x92a\x0F\xDC\x88\x87\x9A\x99\x96\x9A\x98\x94\x93\x98a\r4V[Qa\x01@R\x87Q\x98` \x89\x01Qa\x01`R`\x80\x89\x01Q\x96a\x0F\xFBa\x0C-V[Pa\x10\x04a\x0C\x9BV[Pa\x10\x11a\x01@Qa\x0CLV[a\x01 R_\x95_[\x8CQ\x81\x10\x15a\x14\xBEW\x8Ca\x01\0Qa\x104\x83a\x01`Qa\r4V[Q\x14a\x10DW[P`\x01\x01a\x10\x19V[\x97a\x10R\x82`\x01\x93\x9Aa\r4V[Qa\x10`\x82a\x01 Qa\r4V[Ra\x10n\x81a\x01 Qa\r4V[P\x01\x96a\x01@Q\x88\x14a\x10\x81W\x8Ca\x10;V[P\x93\x91\x95P\x93\x91\x95\x96\x97\x98\x99P[_`\xE0Ra\x10\x9Fa\x01 Qa\r\x03V[QQa\x10\xADa\x01 Qa\r\x03V[QQQ_\x19\x81\x01\x90\x81\x11a\x0B\xACWa\x10\xC4\x91a\r4V[Q\x95`\x80\x87\x01Q\x80g\r\xE0\xB6\xB3\xA7d\0\0\x81\x02\x04g\r\xE0\xB6\xB3\xA7d\0\0\x14\x81\x15\x17\x15a\x0B\xACW``\x88\x01Qa\x11\x02\x91g\r\xE0\xB6\xB3\xA7d\0\0\x02a\x0B\xD3V[`\xC0R_[a\x01 QQ\x81\x10\x15a\x11\xDCWa\x11 \x81a\x01 Qa\r4V[QQQa\x110\x82a\x01 Qa\r4V[QQ`\x01\x19\x82\x01\x82\x81\x11a\x0B\xACWa\x11G\x91a\r4V[Q\x90a\x11V\x83a\x01 Qa\r4V[QQ\x91_\x19\x82\x01\x91\x82\x11a\x0B\xACWa\x11s`\x80\x92a\x11\x80\x94a\r4V[Q\x82R\x01Q`\xE0Qa\x19\xC0V[`\xE0R`\x80\x80Q\x01Qg\r\xE0\xB6\xB3\xA7d\0\0\x81\x02\x90\x80\x82\x04g\r\xE0\xB6\xB3\xA7d\0\0\x14\x90\x15\x17\x15a\x0B\xACW`\x80Q``\x01Qa\x11\xBA\x91a\x0B\xD3V[`\xC0Q\x81\x11a\x11\xCDW[P`\x01\x01a\x11\x07V[`\xC0R`\x80Q\x97P`\x01a\x11\xC4V[P\x90\x91\x92\x93\x9A\x95\x96\x97\x98\x99\x94g\r\xE0\xB6\xB3\xA7d\0\0a\x12\x0Ca\x12\x04`\xC0\x8B\x01Q`\xE0Qa'XV[`\xE0Qa\x0B\xC0V[\x04\x97`\xA0\x81\x01Q\x80`>\x81\x02\x04`>\x14\x81\x15\x17\x15a\x0B\xACW`@Q\x99a\x121\x8Ba\x08\tV[_`\x80\x8C\x01Ra\x01\0Q\x8BRa\x01@Q` \x8C\x01R`\xE0Q`@\x8C\x01R\x80``\x8C\x01Ra\x12`a\x01 Qa\r\x03V[QQa\x12na\x01 Qa\r\x03V[QQQ\x80`\x01\x19\x81\x01\x11a\x0B\xACW`\x01\x19\x01a\x12\x89\x91a\r4V[Q`@\x01Q\x9Ca\x12\x97a\x0C\x9BV[\x80\x9E\x85Q\x82R` \x82\x01R`@\x01R\x8C`\xE0Q\x90``\x01R`\x80\x8D\x01R`>\x02`d\x90\x04`\xA0\x8C\x01R`\xC0\x01Q`\xC0\x8B\x01Ra\x01 Qa\x12\xD6\x90a\r\x03V[QQ\x9B\x8CQa\x12\xE4\x81a\x08EV[`@Q`\xA0R`\xA0Q\x90a\x12\xF7\x91a\x08$V[\x80`\xA0QR`\x1F\x19\x90a\x13\t\x90a\x08EV[\x01_[\x81\x81\x10a\x14\xA5WPP_[`\x01\x81\x01\x81\x11a\x0B\xACW\x8DQ`\x01\x82\x01\x10\x15a\x13\\W\x80a\x13;\x8F\x92`\x01\x93a\r4V[Qa\x13H\x82`\xA0Qa\r4V[Ra\x13U\x81`\xA0Qa\r4V[P\x01a\x13\x17V[P\x92\x98\x9A\x97\x99\x90\x93\x9B\x95\x94\x91\x95Q\x80_\x19\x81\x01\x11a\x0B\xACWa\x13\x93\x91a\x13\x87_\x19\x83\x01`\xA0Qa\r4V[R_\x19\x01`\xA0Qa\r4V[Pa\x13\x9F`\xA0Qa(\xDAV[\x92`@\x84\x01Q`\x80\x8B\x01Ra\x13\xB2a\x0C\x9BV[\x95_\x99_\x94_\x95[a\x01 QQ\x87\x10\x15a\x14'W\x8C` a\x13\xD6\x89a\x01 Qa\r4V[Q\x01Q\x11a\x14\x05W[a\x13\xFD`\x01\x91`@a\x13\xF4\x8Aa\x01 Qa\r4V[Q\x01Q\x90a\x19\xC0V[\x96\x01\x95a\x13\xBAV[\x9BP`\x01a\x13\xFD` a\x14\x1B\x89a\x01 Qa\r4V[Q\x01Q\x9D\x91PPa\x13\xDFV[`\x01\x96P\x98\x86\x95\x9F\x97\x86\x95\x9F\x9A\x9B\x80\x95\x9F\x91\x8E\x9F\x8C\x9F\x95\x9Ca\x14\x9B\x97\x9F`\x80a\x14\x8A\x96`@\x94\x81Q\x8AR` \x82\x01Q` \x8B\x01R\x85\x82\x01Q\x86\x8B\x01R``\x82\x01Q``\x8B\x01R\x82\x8A\x01R\x01Q`\xA0\x88\x01R`\xC0\x87\x01R\x01Q\x90a\x0F\x9E\x83\x83a\r4V[P``\x8C\x01Q\x90a\x0F\x9E\x83\x83a\r4V[P\x01\x97\x01\x93a\x0F\xA9V[` \x90a\x14\xB0a\x0C\x9BV[\x82\x82`\xA0Q\x01\x01R\x01a\x13\x0CV[P\x93\x91\x95P\x93\x91\x95\x96\x97\x98\x99Pa\x10\x8FV[\x80\x8B` \x80\x93a\x14\xE1\x9B\x99\x9Ba\x0C\x9BV[\x92\x01\x01R\x01\x96\x94\x96a\x0E\xD5V[`\x01a\x14\xFC\x82\x87\x99\x97a\r4V[Q\x11\x80a\x15\x1FW[a\x15\x14W[`\x01\x01\x95\x93\x95a\x0E\xA4V[`\x01\x90\x99\x01\x98a\x15\tV[Pa\x15*\x81\x85a\r4V[Q\x15\x15a\x15\x04V[PP`@Q\x90a\x15C` \x83a\x08$V[_\x82R_\x80[\x81\x81\x10a\x15\x8BWPP`@Q\x91a\x15a` \x84a\x08$V[_\x83R_\x80[\x81\x81\x10a\x15tWPP\x91\x90V[` \x90a\x15\x7Fa\x0C\x9BV[\x82\x82\x88\x01\x01R\x01a\x15gV[` \x90a\x15\x96a\x0C-V[\x82\x82\x87\x01\x01R\x01a\x15IV[Q\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x82\x03a\x01\xE6WV[\x91\x93\x92a\x15\xC1a\x08\\V[\x94``\x82\x80Q\x81\x01\x03\x12a\x01\xE6Wa\x15\xDB` \x83\x01a\x15\xA2V[\x91```@\x82\x01Q\x91\x01Q\x92`\xFF\x84\x16\x80\x94\x03a\x01\xE6W`\x01`\x01`\xA0\x1B\x03\x16\x80\x15\x80\x15a\x16\x9EW[a\x16\x96W\x84\x91\x86\x91`\x02\x86\x03a\x16dWa\x16\x1E\x95Pa\x1F9V[\x91[\x82\x15a\t4W\x90a\x160\x91a\x1B\xB7V[\x83R`@Qa\x16@`@\x82a\x08$V[`\x01\x81R` 6\x81\x83\x017`\x05a\x16V\x82a\r\x03V[R` \x84\x01Ra\t(a\x1C\x02V[\x93\x94\x90\x92P`\x03\x14\x15\x90Pa\x0B\x1AW`\x01`\x01`\xA0\x1B\x03\x16\x90\x81\x15a\x0B\x1AW\x84a\x16\x90\x93\x92\x85\x92a\x1E|V[\x91a\x16 V[PPPPPPV[P\x80;\x15a\x16\x04V[Q\x90\x81\x15\x15\x82\x03a\x01\xE6WV[\x91\x93\x92a\x16\xBFa\x08\\V[\x94`\x80\x82\x80Q\x81\x01\x03\x12a\x01\xE6Wa\x16\xD9` \x83\x01a\x15\xA2V[\x91`@\x81\x01Qa\x16\xF0`\x80``\x84\x01Q\x93\x01a\x16\xA7V[\x93`\x01`\x01`\xA0\x1B\x03\x16\x80\x15\x80\x15a\x17[W[a\x17RW\x82\x82\x14a\x17RW\x90a\x17\x1B\x94\x93\x92\x91a\"fV[\x91\x82\x15a\t4W\x90a\x17,\x91a\x1B\xB7V[\x83R`@Qa\x17<`@\x82a\x08$V[`\x01\x81R` 6\x81\x83\x017`\x03a\x16V\x82a\r\x03V[PPPPPPPV[P\x80;\x15a\x17\x03V[\x91\x93\x92\x93a\x17pa\x08\\V[\x94`@\x81\x80Q\x81\x01\x03\x12a\x01\xE6Wa\x17\x96`@a\x17\x8F` \x84\x01a\x15\xA2V[\x92\x01a\x16\xA7V[P`\x01`\x01`\xA0\x1B\x03\x16\x80\x15\x80\x15a\x17\xB8W[a\t\x98W\x90\x83a\t\0\x92a#3V[P\x80;\x15a\x17\xA9V[\x91\x93\x92\x90a\x17\xCDa\x08\\V[\x94\x82Q\x83\x01\x90\x83` \x83\x01\x92\x03`\xE0\x81\x12a\x01\xE6W`\xA0\x13a\x01\xE6W`@Q\x93a\x17\xF6\x85a\x08\tV[a\x18\x02` \x82\x01a\x15\xA2V[\x85Ra\x18\x10`@\x82\x01a\x15\xA2V[\x94` \x81\x01\x95\x86R``\x82\x01Q\x95b\xFF\xFF\xFF\x87\x16\x87\x03a\x01\xE6W`@\x82\x01\x96\x87R`\x80\x83\x01Q\x80`\x02\x0B\x81\x03a\x01\xE6W``\x83\x01Ra\x18Q`\xA0\x84\x01a\x15\xA2V[`\x80\x83\x01Ra\x18b`\xC0\x84\x01a\x16\xA7V[\x92`\xE0\x81\x01Q\x90`\x01`\x01`@\x1B\x03\x82\x11a\x01\xE6W\x01\x85`?\x82\x01\x12\x15a\x01\xE6W` \x81\x01Q\x90a\x18\x92\x82a\x08\x80V[\x96a\x18\xA0`@Q\x98\x89a\x08$V[\x82\x88R`@\x82\x84\x01\x01\x11a\x01\xE6W\x81_\x92`@` \x93\x01\x83\x8A\x01^\x87\x01\x01R\x82\x15a\x19RW\x81Q`\x01`\x01`\xA0\x1B\x03\x89\x81\x16\x91\x16\x14\x90\x81a\x19;W[P[\x15a\x17RW\x90a\x18\xEF\x93\x92\x91a#wV[\x92\x83\x15a\t\x98W\x91a\x19\x08b\xFF\xFF\xFF\x92a\t(\x94a\x1B\xB7V[\x86R`@Qa\x19\x18`@\x82a\x08$V[`\x01\x81R` 6\x81\x83\x017`\x02a\x19.\x82a\r\x03V[R` \x87\x01RQ\x16a\x1C)V[Q`\x01`\x01`\xA0\x1B\x03\x87\x81\x16\x91\x16\x14\x90P_a\x18\xDCV[Q`\x01`\x01`\xA0\x1B\x03\x88\x81\x16\x91\x16\x14\x80\x15a\x18\xDEWP\x80Q`\x01`\x01`\xA0\x1B\x03\x86\x81\x16\x91\x16\x14a\x18\xDEV[=\x15a\x19\xA7W=\x90a\x19\x8E\x82a\x08\x80V[\x91a\x19\x9C`@Q\x93\x84a\x08$V[\x82R=_` \x84\x01>V[``\x90V[Q\x90`\x01`\x01`p\x1B\x03\x82\x16\x82\x03a\x01\xE6WV[\x91\x90\x82\x01\x80\x92\x11a\x0B\xACWV[\x90a\x19\xD8\x90\x82a$\x87V[`\x01`\x01`\xA0\x1B\x03\x81\x16\x15\x80\x15a\x1B\xAEW[a\x1B\xA7W_\x80`@Q` \x81\x01\x90c\x02@\xBCk`\xE2\x1B\x82R`\x04\x81Ra\x1A\x11`$\x82a\x08$V[Q\x90\x84Z\xFA\x91a\x1A\x1Fa\x19}V[\x92\x15\x80\x15a\x1B\x9CW[a\x1BfW``\x83\x80Q\x81\x01\x03\x12a\x01\xE6Wa\x1AE` \x84\x01a\x19\xACV[\x91``a\x1AT`@\x86\x01a\x19\xACV[\x94\x01Qc\xFF\xFF\xFF\xFF\x81\x16\x03a\x01\xE6W_\x80\x91`@Q` \x81\x01\x90c\r\xFE\x16\x81`\xE0\x1B\x82R`\x04\x81Ra\x1A\x87`$\x82a\x08$V[Q\x91Z\xFAa\x1A\x93a\x19}V[\x90\x15\x80\x15a\x1B\x91W[a\x1B\x88W` \x81\x80Q\x81\x01\x03\x12a\x01\xE6W`\x01`\x01`\xA0\x1B\x03\x90a\x1A\xC2\x90` \x01a\x15\xA2V[`\x01`\x01`\xA0\x1B\x03\x90\x92\x16\x91\x16\x03a\x1BvW`\x01`\x01`p\x1B\x03\x91\x82\x16\x91\x16[\x80\x15\x91\x82\x80\x15a\x1BnW[a\x1BfWa\x1B\x19`@Qa\x1B\0\x81a\x07\xA4V[`\x10\x81R`(` \x82\x01R`\x10`@\x82\x01R\x82\x84a&\xC0V[\x15a\x1BfWa\x03\xE5\x84\x02\x93\x80\x85\x04a\x03\xE5\x14\x90\x15\x17\x15a\x0B\xACWa\x1B=\x90\x84a\x0B\xC0V[\x91a\x03\xE8\x82\x02\x91\x82\x04a\x03\xE8\x14\x17\x15a\x0B\xACWa\x1Bc\x92a\x1B]\x91a\x19\xC0V[\x90a\x0B\xD3V[\x90V[PPPP_\x90V[P\x80\x15a\x1A\xEDV[`\x01`\x01`p\x1B\x03\x90\x81\x16\x91\x16a\x1A\xE2V[PPPPP_\x90V[P` \x81Q\x10a\x1A\x9CV[P``\x83Q\x10a\x1A(V[PPP_\x90V[P\x80;\x15a\x19\xEAV[\x91\x90a\x1B\xF3`@Qa\x1B\xCA``\x82a\x08$V[`\x02\x81R`@6` \x83\x017\x80\x94a\x1B\xE1\x82a\r\x03V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x90Ra\r$V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x90RV[`@Q\x90a\x1C\x11`@\x83a\x08$V[`\x01\x82R` 6\x81\x84\x017_a\x1C&\x83a\r\x03V[RV[\x90`@Q\x91a\x1C9`@\x84a\x08$V[`\x01\x83R` 6\x81\x85\x017b\xFF\xFF\xFFa\x1CQ\x84a\r\x03V[\x91\x16\x90RV[\x91\x92\x90\x92_\x93_\x93a\x1Cj\x83\x83\x83a%&V[\x80a\x1C\xD3W[Pa\x1C|\x83\x83\x83a%\x9DV[\x86\x81\x11a\x1C\xC6W[Pa\x1C\x90\x83\x83\x83a%\xF1V[\x86\x81\x11a\x1C\xB7W[P\x90a\x1C\xA4\x92\x91a&EV[\x83\x81\x11a\x1C\xAEWPV[\x92Pa'\x10\x91PV[\x95Pa\x0B\xB8\x94Pa\x1C\xA4a\x1C\x98V[\x95Pa\x01\xF4\x94P_a\x1C\x84V[\x95P`d\x94P_a\x1CpV[\x92\x91\x90`@Q\x90a\x1C\xF1`\x80\x83a\x08$V[`\x03\x82R``6` \x84\x017\x81\x94a\x1D\x08\x83a\r\x03V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x90Ra\x1D\x1E\x82a\r$V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x90R\x80Q`\x02\x10\x15a\r\x10W`\x01`\x01`\xA0\x1B\x03\x90\x91\x16``\x91\x90\x91\x01RV[`@Q\x90a\x1DX``\x83a\x08$V[`\x02\x82R`@6` \x84\x017`\x01a\x1C&\x83\x82a\x1Dt\x82a\r\x03V[Ra\r$V[\x91\x90b\xFF\xFF\xFFa\x1CQ`@Qa\x1D\x91``\x82a\x08$V[`\x02\x81R`@6` \x83\x017\x80\x95\x83a\x1D\xA9\x83a\r\x03V[\x91\x16\x90Ra\r$V[\x90\x81Q`\x02\x81\x10a\x1ERW_\x19\x81\x01\x90\x81\x11a\x0B\xACWa\x1D\xD1\x81a\x0C\xD1V[\x90_[\x81\x81\x10a\x1E/WPP\x90\x91P`@Q` \x81\x01\x81\x81\x93` \x81Q\x93\x91\x01\x92_[\x81\x81\x10a\x1E\x16WPPa\x1E\x10\x92P\x03`\x1F\x19\x81\x01\x83R\x82a\x08$V[Q\x90 \x90V[\x84Q\x83R` \x94\x85\x01\x94\x86\x94P\x90\x92\x01\x91`\x01\x01a\x1D\xF4V[\x80`@a\x1E>`\x01\x93\x88a\r4V[Q\x01Qa\x1EK\x82\x86a\r4V[R\x01a\x1D\xD4V[P_\x91PV[\x80Q\x80\x83R` \x92\x91\x81\x90\x84\x01\x84\x84\x01^_\x82\x82\x01\x84\x01R`\x1F\x01`\x1F\x19\x16\x01\x01\x90V[\x90_\x80\x94a\x1E\xFA\x82\x95a\x1E\xEC` \x99`@Q\x90a\x1E\x99\x8C\x83a\x08$V[\x86\x82R`@Qc\x07\xD2E\xE9`\xE4\x1B\x8D\x82\x01\x90\x81R`\x01`\x01`\xA0\x1B\x03\x99\x8A\x16`$\x83\x01R\x94\x89\x16`D\x82\x01R\x95\x90\x97\x16`d\x86\x01R`\x84\x85\x01\x96\x90\x96R`\xA0`\xA4\x85\x01R\x90\x94\x83\x91\x90`\xC4\x83\x01\x90a\x1EXV[\x03`\x1F\x19\x81\x01\x83R\x82a\x08$V[Q\x92Z\xF1\x90a\x1F\x07a\x19}V[\x91\x15\x80\x15a\x1F/W[a\x1F)W\x81Q\x81\x83\x01\x92\x01\x81\x01\x82\x90\x03\x12a\x01\xE6WQ\x90V[PP_\x90V[P\x80\x82Q\x10a\x1F\x10V[\x90\x91\x93\x92\x93`@\x94\x85Q\x93a\x1FN\x87\x86a\x08$V[`\x01\x85R`\x1F\x19\x87\x01_[\x81\x81\x10a\"2WPP\x86Q` \x96a\x1Fq\x88\x83a\x08$V[_\x82R\x88Q\x92a\x1F\x80\x84a\x08\tV[\x83R_\x88\x84\x01R`\x01\x89\x84\x01R``\x83\x01R`\x80\x82\x01Ra\x1F\xA0\x85a\r\x03V[Ra\x1F\xAA\x84a\r\x03V[P``\x93\x86Q\x91a\x1F\xBB\x86\x84a\x08$V[`\x02\x83R\x86\x83\x01\x93`\x1F\x19\x87\x016\x867a\x1F\xD4\x84a\r\x03V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x90Ra\x1F\xEA\x83a\r$V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x90R\x86Qa \x02\x81a\x07\xEEV[0\x81R\x86\x81\x01\x90_\x82R\x88\x81\x01\x920\x84R\x88\x88\x83\x01\x95_\x87R\x8B\x80Q\x9Ac|&\x837`\xE1\x1B\x84\x8D\x01R\x8Ba\x01\x04\x81\x01\x91_`$\x83\x01R`\xE0`D\x83\x01R\x86Q\x80\x93Ra\x01$\x82\x01\x90\x86a\x01$\x85`\x05\x1B\x85\x01\x01\x98\x01\x94_\x93[\x85\x85\x10a!\xD0WPPPPP\x8B\x85\x03`#\x19\x01`d\x8D\x01RPPQ\x80\x83R\x91\x01\x95\x90_[\x8A\x82\x82\x10a!\xB3WPP\x91Q`\x01`\x01`\xA0\x1B\x03\x90\x81\x16`\x84\x8A\x01R\x92Q\x15\x15`\xA4\x89\x01RPP\x90Q\x16`\xC4\x85\x01RQ\x15\x15`\xE4\x84\x01R\x82\x90\x03`\x1F\x19\x81\x01\x83R_\x92\x83\x92\x90\x91a \xD0\x90\x83a\x08$V[\x82\x85\x83Q\x93\x01\x91Z\xF1a \xE1a\x19}V[\x90\x15\x80\x15a!\xA9W[a\x1B\xA7W\x80Q\x81\x01\x90\x82\x81\x81\x84\x01\x93\x03\x12a\x01\xE6W\x82\x81\x01Q\x90`\x01`\x01`@\x1B\x03\x82\x11a\x01\xE6W\x01\x92\x81`?\x85\x01\x12\x15a\x01\xE6W\x82\x84\x01Q\x90a!-\x82a\x08EV[\x94a!:\x82Q\x96\x87a\x08$V[\x82\x86R\x84\x80\x80\x88\x01\x94`\x05\x1B\x83\x01\x01\x01\x93\x84\x11a\x01\xE6W\x01\x90[\x82\x82\x10a!\x9AWPPPP`\x02\x81Q\x10a!\x95Wa!q\x90a\r$V[Q_\x81\x13a!\x95W\x80\x15a!\x95W`\x01`\xFF\x1B\x81\x14a\x0B\xACWa\x1Bc\x90_\x03a&\x99V[P_\x90V[\x81Q\x81R\x90\x83\x01\x90\x83\x01a!TV[P\x82\x81Q\x10a \xEAV[\x83Q`\x01`\x01`\xA0\x1B\x03\x16\x89R\x97\x88\x01\x97\x90\x92\x01\x91`\x01\x01a \x7FV[\x88\x92\x94\x96\x99`\xA0`\x80`\x01\x96\x98\x9A\x9B\x94a\"\x1D\x94a\x01#\x19\x90\x85\x03\x01\x8AR\x8DQ\x90\x81Q\x85R\x86\x82\x01Q\x87\x86\x01R\x80\x82\x01Q\x90\x85\x01R\x88\x81\x01Q\x89\x85\x01R\x01Q\x91\x81`\x80\x82\x01R\x01\x90a\x1EXV[\x98\x01\x93\x01\x93\x01\x90\x92\x8F\x93\x8F\x96\x95\x93\x94\x8Fa [V[` \x90\x89Qa\"@\x81a\x08\tV[_\x81R_\x83\x82\x01R_\x8B\x82\x01R_``\x82\x01R```\x80\x82\x01R\x82\x82\x8A\x01\x01R\x01a\x1FYV[_\x94\x85\x94\x91\x93\x92\x90\x15a\"\xFDWa\"\x7Fa\"\x85\x91a&\xAEV[\x92a&\xAEV[`@Q\x92c^\rD?`\xE0\x1B` \x85\x01R`\x0F\x0B`$\x84\x01R`\x0F\x0B`D\x83\x01R`d\x82\x01R`d\x81Ra\"\xBA`\x84\x82a\x08$V[\x90[` \x82Q\x92\x01\x90Z\xFAa\"\xCDa\x19}V[\x90\x15\x80\x15a\"\xF2W[a!\x95W` \x81Q\x91\x81\x80\x82\x01\x93\x84\x92\x01\x01\x03\x12a\x01\xE6WQ\x90V[P` \x81Q\x10a\"\xD6V[\x91`@Q\x92cUmn\x9F`\xE0\x1B` \x85\x01R`$\x84\x01R`D\x83\x01R`d\x82\x01R`d\x81Ra#-`\x84\x82a\x08$V[\x90a\"\xBCV[_\x92\x83\x92`@Q\x90` \x82\x01\x92cx\xA0Q\xAD`\xE1\x1B\x84R`$\x83\x01R`\x01\x80`\xA0\x1B\x03\x16`D\x82\x01R`D\x81Ra#k`d\x82a\x08$V[Q\x91Z\xFAa\"\xCDa\x19}V[\x90\x91`\x01`\x80\x1B\x81\x10\x15a$zWa$X_\x94\x93a\x1E\xEC\x86\x95`@Q\x95a#\x9D\x87a\x07\xEEV[\x86R` \x86\x01\x92\x15\x15\x83R`\x01`\x01`\x80\x1B\x03`@\x87\x01\x95\x16\x85R``\x86\x01\x90\x81R`\x01`\x01`\x80\x1B\x03`@Q\x95\x86\x94` \x86\x01\x98c\xAA\x9D!\xCB`\xE0\x1B\x8AR` `$\x88\x01RQ`\x01\x80`\xA0\x1B\x03\x81Q\x16`D\x88\x01R`\x01\x80`\xA0\x1B\x03` \x82\x01Q\x16`d\x88\x01Rb\xFF\xFF\xFF`@\x82\x01Q\x16`\x84\x88\x01R``\x81\x01Q`\x02\x0B`\xA4\x88\x01R`\x80`\x01\x80`\xA0\x1B\x03\x91\x01Q\x16`\xC4\x87\x01RQ\x15\x15`\xE4\x86\x01RQ\x16a\x01\x04\x84\x01RQa\x01\0a\x01$\x84\x01Ra\x01D\x83\x01\x90a\x1EXV[Q\x90\x82s9r\xC0\x0F~\xD4\x88^\x14X#\xEB|eSu\xD2u\xA1\xC5Z\xF1a\"\xCDa\x19}V[c5'\x8D\x12_R`\x04`\x1C\xFD[`@Qc\xE6\xA49\x05`\xE0\x1B` \x82\x01\x90\x81R`\x01`\x01`\xA0\x1B\x03\x92\x83\x16`$\x83\x01R\x92\x90\x91\x16`D\x80\x83\x01\x91\x90\x91R\x81R_\x91\x82\x91a$\xC7`d\x82a\x08$V[Q\x90s\xF1\xD7\xCCd\xFBDR\xF0\\I\x81&1.\xBE)\xF3\x0F\xBC\xF9Z\xFAa$\xE8a\x19}V[\x90\x15\x80\x15a%\x1BW[a!\x95W` \x81\x80Q\x81\x01\x03\x12a\x01\xE6W`\x01`\x01`\xA0\x1B\x03\x90a%\x17\x90` \x01a\x15\xA2V[\x16\x90V[P` \x81Q\x10a$\xF1V[`@QccR\x815`\xE1\x1B` \x82\x01\x90\x81R`\x01`\x01`\xA0\x1B\x03\x92\x83\x16`$\x83\x01R\x91\x90\x92\x16`D\x83\x01R`d\x80\x83\x01\x93\x90\x93R`\x84\x82\x01\x92\x90\x92R_`\xA4\x80\x83\x01\x82\x90R\x82R\x91\x82\x91a%{`\xC4\x82a\x08$V[Q\x90\x82sa\xFF\xE0\x14\xBA\x17\x98\x9Et<_l\xB2\x1B\xF9iu0\xB2\x1EZ\xF1a\"\xCDa\x19}V[`@QccR\x815`\xE1\x1B` \x82\x01\x90\x81R`\x01`\x01`\xA0\x1B\x03\x92\x83\x16`$\x83\x01R\x91\x90\x92\x16`D\x83\x01R`d\x82\x01\x92\x90\x92Ra\x01\xF4`\x84\x82\x01R_`\xA4\x80\x83\x01\x82\x90R\x82R\x91\x82\x91a%{`\xC4\x82a\x08$V[`@QccR\x815`\xE1\x1B` \x82\x01\x90\x81R`\x01`\x01`\xA0\x1B\x03\x92\x83\x16`$\x83\x01R\x91\x90\x92\x16`D\x83\x01R`d\x82\x01\x92\x90\x92Ra\x0B\xB8`\x84\x82\x01R_`\xA4\x80\x83\x01\x82\x90R\x82R\x91\x82\x91a%{`\xC4\x82a\x08$V[`@QccR\x815`\xE1\x1B` \x82\x01\x90\x81R`\x01`\x01`\xA0\x1B\x03\x92\x83\x16`$\x83\x01R\x91\x90\x92\x16`D\x83\x01R`d\x82\x01\x92\x90\x92Ra'\x10`\x84\x82\x01R_`\xA4\x80\x83\x01\x82\x90R\x82R\x91\x82\x91a%{`\xC4\x82a\x08$V[_\x81\x12\x15a\x1BcWc5'\x8D\x12_R`\x04`\x1C\xFD[`\x01`\x7F\x1B\x81\x10\x15a$zW`\x0F\x0B\x90V[\x80\x15\x80\x15a'\x14W[a\x1B\xA7Wa&\xE7a\xFF\xFF\x84Q\x16a\xFF\xFF` \x86\x01Q\x16\x90\x84\x84a'\x1CV[a\x1B\xA7Wa'\x02\x81\x83`@\x93a\xFF\xFF\x95\x10\x90\x82\x18\x02\x18a)CV[\x92\x01Q\x16\x11a'\x10W`\x01\x90V[_\x90V[P\x81\x15a&\xC9V[\x91\x80a''\x84a)CV[\x10a'OWa'5\x82a)CV[\x10a'GWa'C\x91a)SV[\x11\x90V[PPP`\x01\x90V[PPPP`\x01\x90V[\x90\x80\x15a\x1F)W\x81a'i\x91a\x19\xC0V[\x80\x15a(\xCBWg\r\xE0\xB6\xB3\xA7d\0\0\x82\x02\x91\x81\x81\x15g\r\xE0\xB6\xB3\xA7d\0\0\x83\x86\x04\x14\x17\x02\x15a(VWP\x90\x04[`\x03\x81\x02\x90`d\x81\x15`\x03\x83\x85\x04\x14\x17\x02\x15a'\xF8WP`d\x90\x04[f\n\xA8{\xEES\x80\0\x81\x01g\r\xE0\xB6\xB3\xA7d\0\0\x11\x15a'\xF1Wg\r\xD6\x0E7\xB9\x10\x80\0\x03[\x80g\x01cEx]\x8A\0\0\x11g\x01cEx]\x8A\0\0\x82\x18\x02\x18\x90V[P_a'\xD6V[`d`\x03_\x19\x81\x84\t\x84\x81\x10\x85\x01\x90\x03\x92\t\x90\x80`d\x11\x15a(IW\x82\x82\x11\x90\x03`\xFE\x1B\x91\x03`\x02\x1C\x17\x7F\\(\xF5\xC2\x8F\\(\xF5\xC2\x8F\\(\xF5\xC2\x8F\\(\xF5\xC2\x8F\\(\xF5\xC2\x8F\\(\xF5\xC2\x8F\\)\x02a'\xB2V[c\xAEG\xF7\x02_R`\x04`\x1C\xFD[\x81g\r\xE0\xB6\xB3\xA7d\0\0_\x19\x81\x84\t\x85\x81\x10\x86\x01\x90\x03\x92\t\x90\x82_\x03\x83\x16\x92\x81\x81\x11\x15a(IW\x83\x90\x04\x80`\x03\x02`\x02\x18\x80\x82\x02`\x02\x03\x02\x80\x82\x02`\x02\x03\x02\x80\x82\x02`\x02\x03\x02\x80\x82\x02`\x02\x03\x02\x80\x82\x02`\x02\x03\x02\x80\x91\x02`\x02\x03\x02\x93`\x01\x84\x84\x83\x03\x04\x94\x80_\x03\x04\x01\x92\x11\x90\x03\x02\x17\x02a'\x96V[c#\xD3Y\xA3`\xE0\x1B_R`\x04_\xFD[\x90a(\xE3a\x0C-V[\x82\x81R\x82Q\x80\x15a)>W_\x19\x81\x01\x90\x81\x11a\x0B\xACWa)\x05`\x80\x91\x85a\r4V[Q\x01Q` \x82\x01R_\x90\x81[\x84Q\x83\x10\x15a)4Wa),`\x01\x91`\xA0a\x13\xF4\x86\x89a\r4V[\x92\x01\x91a)\x11V[`@\x82\x01R\x92PPV[P\x91PV[\x80\x15a!\x95W\x1E`\xFF\x18`\x01\x01\x90V[\x80\x15\x80\x15a)\x95W[a)\x8EWa)la)r\x91a)\x9DV[\x91a)\x9DV[\x90\x81\x81\x11\x15a)\x85W\x90a\x1Bc\x91a\x0B\x9FV[a\x1Bc\x91a\x0B\x9FV[PP_\x19\x90V[P\x81\x15a)\\V[\x80\x15a)\xADW`\x01\x17\x1E`\xFF\x03\x90V[c\xAFE\x8C\x07`\xE0\x1B_R`\x04_\xFD\xFE\xA1dsolcC\0\x08\"\0\n",
    );
    /// The runtime bytecode of the contract, as deployed on the network.
    ///
    /// ```text
    ///0x6101806040526004361015610012575f80fd5b5f3560e01c806321bf9f26146104e857806381c6ecd6146101ea5763c036c8ea1461003b575f80fd5b346101e65760a03660031901126101e657610054610696565b61005c6106ac565b6084359160643591604435906001600160401b0385116101e657366023860112156101e65784600401356001600160401b0381116101e65785019260248401933685116101e6576040906100ae61085c565b506100bb8786868661089b565b879003126101e65760248601359560ff87168097036101e6576044810135906001600160401b0382116101e65701846043820112156101e65760248101359061010382610880565b956101116040519788610824565b828752604482840101116101e657815f92604460209301838901378601015260028603610179576101439495506117c1565b60608101511561016a576101669161015a91610bf1565b604051918291826106c2565b0390f35b630541871160e01b5f5260045ffd5b600886036101915761018c949550611764565b610143565b600386036101a45761018c9495506116b4565b600986036101b75761018c9495506115b6565b85600581036101d457631602da9b60e21b5f52600560045260245ffd5b631602da9b60e21b5f5260045260245ffd5b5f80fd5b346101e65760403660031901126101e6576004356001600160401b0381116101e657366023820112156101e65780600401359061022682610845565b906102346040519283610824565b8282526024602083019360051b820101903682116101e65760248101935b8285106103ce5761026560243585610d48565b90604051918291604083016040845281518091526060840190602060608260051b8701019301915f905b82821061031757505050508281036020840152602080835192838152019201905f5b8181106102bf575050500390f35b91935091602060e060019260c0875180518352848101518584015260408101516040840152606081015160608401526080810151608084015260a081015160a0840152015160c08201520194019101918493926102b1565b91939092949550605f1987820301825284519060608101918051926060835283518091526020608084019401905f905b80821061037a5750505060019260209260408084868096015186850152015191015296019201920186959493919261028f565b909194602060e060019260c0895180518352848101518584015260408101516040840152606081015160608401526080810151608084015260a081015160a0840152015160c0820152019601920190610347565b84356001600160401b0381116101e6578201606060231982360301126101e657604051906103fb826107a4565b60248101356001600160401b0381116101e65760249082010136601f820112156101e657803561042a81610845565b916104386040519384610824565b818352602060e08185019302820101903682116101e657602001915b81831061048457505050916064602094928594835260448101358584015201356040820152815201940193610252565b60e0833603126101e657602060e09160405161049f816107d3565b85358152828601358382015260408601356040820152606086013560608201526080860135608082015260a086013560a082015260c086013560c0820152815201920191610454565b346101e65760803660031901126101e657610501610696565b6105096106ac565b6064359160443561051861085c565b506105258482858561089b565b6105308184846108e9565b9261053c828285610939565b846060808301519101511061068e575b506001600160a01b0383167382af49447d8a07e3bd95bd0d56f35241523fbab181141580610667575b610605575b73af88d065e77c8cc2239327c5edb3a432268e58311415806105de575b6105b6575b50505060608101511561016a576101669161015a91610bf1565b6105bf92610b21565b60608101516060830151106105d6575b808061059c565b9050826105cf565b506001600160a01b03811673af88d065e77c8cc2239327c5edb3a432268e58311415610597565b61061083838661099e565b61061b848487610a88565b90606081015160608801511061065f575b506060810151606087015110610643575b5061057a565b945073af88d065e77c8cc2239327c5edb3a432268e583161063d565b95508761062c565b506001600160a01b0382167382af49447d8a07e3bd95bd0d56f35241523fbab11415610575565b93508561054c565b600435906001600160a01b03821682036101e657565b602435906001600160a01b03821682036101e657565b6020815260a0810191805192608060208401528351809152602060c084019401905f5b81811061078557505050602081810151838503601f190160408501528051808652948201949101905f5b81811061076c57505050604081015192601f19838203016060840152602080855192838152019401905f5b818110610751575050506060608091015191015290565b825162ffffff1686526020958601959092019160010161073a565b825160ff1686526020958601959092019160010161070f565b82516001600160a01b03168652602095860195909201916001016106e5565b606081019081106001600160401b038211176107bf57604052565b634e487b7160e01b5f52604160045260245ffd5b60e081019081106001600160401b038211176107bf57604052565b608081019081106001600160401b038211176107bf57604052565b60a081019081106001600160401b038211176107bf57604052565b90601f801991011681019081106001600160401b038211176107bf57604052565b6001600160401b0381116107bf5760051b60200190565b60405190610869826107ee565b5f6060838181528160208201528160408201520152565b6001600160401b0381116107bf57601f01601f191660200190565b6001600160a01b039081169116146108da57156108cb576103e8106108bc57565b632a8406b960e01b5f5260045ffd5b63857e4aa960e01b5f5260045ffd5b634181f73f60e11b5f5260045ffd5b909291926109006108f861085c565b9482846119cd565b918215610934579061091191611bb7565b835261091b611c02565b6020840152610928611c02565b60408401526060830152565b505050565b9092919261095061094861085c565b948284611c57565b919092831561099857610928929161096791611bb7565b8552604051610977604082610824565b6001815260203681830137600161098d82610d03565b526020860152611c29565b50505050565b909291926109c96109ad61085c565b947382af49447d8a07e3bd95bd0d56f35241523fbab1846119cd565b8015610934576109ee90827382af49447d8a07e3bd95bd0d56f35241523fbab16119cd565b91821561093457907382af49447d8a07e3bd95bd0d56f35241523fbab1610a1492611cdf565b8352604051610a24606082610824565b60028152604090813660208301375f610a3c82610d03565b525f610a4782610d24565b52602085015260405190610a5c606083610824565b600282523660208301375f610a7082610d03565b525f610a7b82610d24565b5260408401526060830152565b90929192610ab3610a9761085c565b947382af49447d8a07e3bd95bd0d56f35241523fbab184611c57565b90801561099857610ad990837382af49447d8a07e3bd95bd0d56f35241523fbab1611c57565b9290938415610b1a576109289392917382af49447d8a07e3bd95bd0d56f35241523fbab1610b0692611cdf565b8652610b10611d49565b6020870152611d7a565b5050505050565b90929192610b4c610b3061085c565b9473af88d065e77c8cc2239327c5edb3a432268e583184611c57565b90801561099857610b72908373af88d065e77c8cc2239327c5edb3a432268e5831611c57565b9290938415610b1a5761092893929173af88d065e77c8cc2239327c5edb3a432268e5831610b0692611cdf565b91908203918211610bac57565b634e487b7160e01b5f52601160045260245ffd5b81810292918115918404141715610bac57565b8115610bdd570490565b634e487b7160e01b5f52601260045260245ffd5b90610bfa61085c565b5080610c04575090565b606082019081519061271003906127108211610bac5761271091610c2791610bc0565b04905290565b60405190610c3a826107a4565b5f604083606081528260208201520152565b90610c5682610845565b610c636040519182610824565b8281528092610c74601f1991610845565b01905f5b828110610c8457505050565b602090610c8f610c2d565b82828501015201610c78565b60405190610ca8826107d3565b5f60c0838281528260208201528260408201528260608201528260808201528260a08201520152565b90610cdb82610845565b610ce86040519182610824565b8281528092610cf9601f1991610845565b0190602036910137565b805115610d105760200190565b634e487b7160e01b5f52603260045260245ffd5b805160011015610d105760400190565b8051821015610d105760209160051b010190565b80511561153257610d598151610cd1565b905f5b8151811015610da957806002610d7460019385610d34565b5151511015610d90575f5b610d898286610d34565b5201610d5c565b610da4610d9d8285610d34565b5151611db2565b610d7f565b5091610db58351610cd1565b91835194610dc286610845565b95610dd06040519788610824565b808752610ddf601f1991610845565b013660208801375f925f5b8651811015610e8f57610dfd8185610d34565b515f81610e3a575b15610e14575b50600101610dea565b856001929691610e2584938a610d34565b5281610e31828c610d34565b52019490610e0b565b5f5b878110610e4a575b50610e05565b82610e55828b610d34565b5114610e6357600101610e3c565b9050610e6f818b610d34565b515f198114610bac576001610e869101918b610d34565b5260015f610e44565b50939590949195610e9f84610c4c565b945f975f5b8681106114ee5750610eb589610845565b98610ec36040519a8b610824565b808a52610ed2601f1991610845565b015f5b8181106114d057505060405194610eeb86610809565b8552602085015285604085015287606085015260808401525f935f5f935b828510610f1b57505050505050509190565b610f2c8585989b979a969995610d34565b51610100526001610f3d8983610d34565b5103610fc8575f5b895151811015610fb45761010051610f618260208d0151610d34565b5114610f6f57600101610f45565b8a610fa48b989c610f8d60019597999d9486959d9c97999d51610d34565b5160408b015190610f9e8383610d34565b52610d34565b505b01965b019391929092610f09565b509193979498600180919897929498610fa6565b9492610fdc88879a99969a98949398610d34565b5161014052875198602089015161016052608089015196610ffb610c2d565b50611004610c9b565b5061101161014051610c4c565b610120525f955f5b8c518110156114be578c610100516110348361016051610d34565b5114611044575b50600101611019565b97611052826001939a610d34565b516110608261012051610d34565b5261106e8161012051610d34565b500196610140518814611081578c61103b565b509391955093919596979899505b5f60e05261109f61012051610d03565b51516110ad61012051610d03565b5151515f198101908111610bac576110c491610d34565b5195608087015180670de0b6b3a7640000810204670de0b6b3a76400001481151715610bac57606088015161110291670de0b6b3a764000002610bd3565b60c0525f5b61012051518110156111dc576111208161012051610d34565b5151516111308261012051610d34565b51516001198201828111610bac5761114791610d34565b51906111568361012051610d34565b5151915f198201918211610bac5761117360809261118094610d34565b518252015160e0516119c0565b60e052608080510151670de0b6b3a7640000810290808204670de0b6b3a76400001490151715610bac57608051606001516111ba91610bd3565b60c05181116111cd575b50600101611107565b60c052608051975060016111c4565b50909192939a959697989994670de0b6b3a764000061120c61120460c08b015160e051612758565b60e051610bc0565b049760a081015180603e810204603e1481151715610bac57604051996112318b610809565b5f60808c0152610100518b526101405160208c015260e05160408c01528060608c015261126061012051610d03565b515161126e61012051610d03565b51515180600119810111610bac576001190161128991610d34565b51604001519c611297610c9b565b809e855182526020820152604001528c60e051906060015260808d0152603e026064900460a08c015260c0015160c08b0152610120516112d690610d03565b51519b8c516112e481610845565b60405160a05260a051906112f791610824565b8060a05152601f199061130990610845565b015f5b8181106114a55750505f5b600181018111610bac578d5160018201101561135c578061133b8f92600193610d34565b516113488260a051610d34565b526113558160a051610d34565b5001611317565b5092989a979990939b9594919551805f19810111610bac57611393916113875f19830160a051610d34565b525f190160a051610d34565b5061139f60a0516128da565b92604084015160808b01526113b2610c9b565b955f995f945f955b6101205151871015611427578c60206113d68961012051610d34565b51015111611405575b6113fd60019160406113f48a61012051610d34565b510151906119c0565b9601956113ba565b9b5060016113fd602061141b8961012051610d34565b5101519d9150506113df565b600196509886959f9786959f9a9b80959f918e9f8c9f959c61149b979f608061148a9660409481518a52602082015160208b015285820151868b0152606082015160608b0152828a0152015160a088015260c0870152015190610f9e8383610d34565b5060608c015190610f9e8383610d34565b5001970193610fa9565b6020906114b0610c9b565b828260a0510101520161130c565b5093919550939195969798995061108f565b808b602080936114e19b999b610c9b565b9201015201969496610ed5565b60016114fc82879997610d34565b51118061151f575b611514575b600101959395610ea4565b600190990198611509565b5061152a8185610d34565b511515611504565b505060405190611543602083610824565b5f82525f805b81811061158b57505060405191611561602084610824565b5f83525f805b8181106115745750509190565b60209061157f610c9b565b82828801015201611567565b602090611596610c2d565b82828701015201611549565b51906001600160a01b03821682036101e657565b9193926115c161085c565b946060828051810103126101e6576115db602083016115a2565b91606060408201519101519260ff84168094036101e6576001600160a01b03168015801561169e575b6116965784918691600286036116645761161e9550611f39565b915b8215610934579061163091611bb7565b8352604051611640604082610824565b6001815260203681830137600561165682610d03565b526020840152610928611c02565b9394909250600314159050610b1a576001600160a01b0316908115610b1a578461169093928592611e7c565b91611620565b505050505050565b50803b15611604565b519081151582036101e657565b9193926116bf61085c565b946080828051810103126101e6576116d9602083016115a2565b9160408101516116f06080606084015193016116a7565b936001600160a01b03168015801561175b575b61175257828214611752579061171b94939291612266565b918215610934579061172c91611bb7565b835260405161173c604082610824565b6001815260203681830137600361165682610d03565b50505050505050565b50803b15611703565b9193929361177061085c565b946040818051810103126101e657611796604061178f602084016115a2565b92016116a7565b506001600160a01b0316801580156117b8575b61099857908361090092612333565b50803b156117a9565b919392906117cd61085c565b9482518301908360208301920360e081126101e65760a0136101e657604051936117f685610809565b611802602082016115a2565b8552611810604082016115a2565b946020810195865260608201519562ffffff871687036101e6576040820196875260808301518060020b81036101e657606083015261185160a084016115a2565b608083015261186260c084016116a7565b9260e0810151906001600160401b0382116101e6570185603f820112156101e65760208101519061189282610880565b966118a06040519889610824565b828852604082840101116101e657815f92604060209301838a015e8701015282156119525781516001600160a01b03898116911614908161193b575b505b1561175257906118ef939291612377565b928315610998579161190862ffffff9261092894611bb7565b8652604051611918604082610824565b6001815260203681830137600261192e82610d03565b5260208701525116611c29565b516001600160a01b0387811691161490505f6118dc565b516001600160a01b0388811691161480156118de575080516001600160a01b038681169116146118de565b3d156119a7573d9061198e82610880565b9161199c6040519384610824565b82523d5f602084013e565b606090565b51906001600160701b03821682036101e657565b91908201809211610bac57565b906119d89082612487565b6001600160a01b038116158015611bae575b611ba7575f806040516020810190630240bc6b60e21b825260048152611a11602482610824565b5190845afa91611a1f61197d565b92158015611b9c575b611b66576060838051810103126101e657611a45602084016119ac565b916060611a54604086016119ac565b94015163ffffffff8116036101e6575f80916040516020810190630dfe168160e01b825260048152611a87602482610824565b51915afa611a9361197d565b90158015611b91575b611b88576020818051810103126101e6576001600160a01b0390611ac2906020016115a2565b6001600160a01b03909216911603611b76576001600160701b0391821691165b801591828015611b6e575b611b6657611b19604051611b00816107a4565b60108152602860208201526010604082015282846126c0565b15611b66576103e58402938085046103e51490151715610bac57611b3d9084610bc0565b916103e882029182046103e8141715610bac57611b6392611b5d916119c0565b90610bd3565b90565b505050505f90565b508015611aed565b6001600160701b039081169116611ae2565b50505050505f90565b506020815110611a9c565b506060835110611a28565b5050505f90565b50803b156119ea565b9190611bf3604051611bca606082610824565b6002815260403660208301378094611be182610d03565b6001600160a01b039091169052610d24565b6001600160a01b039091169052565b60405190611c11604083610824565b60018252602036818401375f611c2683610d03565b52565b9060405191611c39604084610824565b600183526020368185013762ffffff611c5184610d03565b91169052565b919290925f935f93611c6a838383612526565b80611cd3575b50611c7c83838361259d565b868111611cc6575b50611c908383836125f1565b868111611cb7575b5090611ca49291612645565b838111611cae5750565b92506127109150565b9550610bb89450611ca4611c98565b95506101f494505f611c84565b9550606494505f611c70565b92919060405190611cf1608083610824565b6003825260603660208401378194611d0883610d03565b6001600160a01b039091169052611d1e82610d24565b6001600160a01b039091169052805160021015610d10576001600160a01b0390911660609190910152565b60405190611d58606083610824565b6002825260403660208401376001611c268382611d7482610d03565b52610d24565b919062ffffff611c51604051611d91606082610824565b600281526040366020830137809583611da983610d03565b91169052610d24565b90815160028110611e52575f198101908111610bac57611dd181610cd1565b905f5b818110611e2f5750509091506040516020810181819360208151939101925f5b818110611e16575050611e10925003601f198101835282610824565b51902090565b8451835260209485019486945090920191600101611df4565b806040611e3e60019388610d34565b510151611e4b8286610d34565b5201611dd4565b505f9150565b805180835260209291819084018484015e5f828201840152601f01601f1916010190565b905f8094611efa8295611eec60209960405190611e998c83610824565b8682526040516307d245e960e41b8d82019081526001600160a01b03998a1660248301529489166044820152959097166064860152608485019690965260a060a4850152909483919060c4830190611e58565b03601f198101835282610824565b51925af190611f0761197d565b91158015611f2f575b611f2957815181830192018101829003126101e6575190565b50505f90565b5080825110611f10565b9091939293604094855193611f4e8786610824565b60018552601f1987015f5b8181106122325750508651602096611f718883610824565b5f8252885192611f8084610809565b83525f8884015260018984015260608301526080820152611fa085610d03565b52611faa84610d03565b50606093865191611fbb8684610824565b6002835286830193601f198701368637611fd484610d03565b6001600160a01b039091169052611fea83610d24565b6001600160a01b0390911690528651612002816107ee565b308152868101905f82528881019230845288888301955f87528b80519a637c26833760e11b848d01528b6101048101915f602483015260e060448301528651809352610124820190866101248560051b8501019801945f935b8585106121d05750505050508b85036023190160648d0152505051808352910195905f5b8a8282106121b357505091516001600160a01b0390811660848a01529251151560a4890152505090511660c485015251151560e4840152829003601f19810183525f92839290916120d09083610824565b828583519301915af16120e161197d565b901580156121a9575b611ba757805181019082818184019303126101e65782810151906001600160401b0382116101e657019281603f850112156101e657828401519061212d82610845565b9461213a82519687610824565b82865284808088019460051b830101019384116101e65701905b82821061219a575050505060028151106121955761217190610d24565b515f811361219557801561219557600160ff1b8114610bac57611b63905f03612699565b505f90565b81518152908301908301612154565b50828151106120ea565b83516001600160a01b03168952978801979092019160010161207f565b889294969960a06080600196989a9b9461221d9461012319908503018a528d5190815185528682015187860152808201519085015288810151898501520151918160808201520190611e58565b98019301930190928f938f969593948f61205b565b602090895161224081610809565b5f81525f838201525f8b8201525f60608201526060608082015282828a01015201611f59565b5f94859491939290156122fd5761227f612285916126ae565b926126ae565b60405192635e0d443f60e01b6020850152600f0b6024840152600f0b60448301526064820152606481526122ba608482610824565b905b602082519201905afa6122cd61197d565b901580156122f2575b61219557602081519181808201938492010103126101e6575190565b5060208151106122d6565b916040519263556d6e9f60e01b60208501526024840152604483015260648201526064815261232d608482610824565b906122bc565b5f9283926040519060208201926378a051ad60e11b8452602483015260018060a01b031660448201526044815261236b606482610824565b51915afa6122cd61197d565b9091600160801b81101561247a576124585f9493611eec86956040519561239d876107ee565b86526020860192151583526001600160801b036040870195168552606086019081526001600160801b03604051958694602086019863aa9d21cb60e01b8a52602060248801525160018060a01b03815116604488015260018060a01b03602082015116606488015262ffffff6040820151166084880152606081015160020b60a4880152608060018060a01b039101511660c487015251151560e4860152511661010484015251610100610124840152610144830190611e58565b519082733972c00f7ed4885e145823eb7c655375d275a1c55af16122cd61197d565b6335278d125f526004601cfd5b60405163e6a4390560e01b602082019081526001600160a01b0392831660248301529290911660448083019190915281525f9182916124c7606482610824565b519073f1d7cc64fb4452f05c498126312ebe29f30fbcf95afa6124e861197d565b9015801561251b575b612195576020818051810103126101e6576001600160a01b0390612517906020016115a2565b1690565b5060208151106124f1565b604051636352813560e11b602082019081526001600160a01b03928316602483015291909216604483015260648083019390935260848201929092525f60a4808301829052825291829161257b60c482610824565b5190827361ffe014ba17989e743c5f6cb21bf9697530b21e5af16122cd61197d565b604051636352813560e11b602082019081526001600160a01b03928316602483015291909216604483015260648201929092526101f460848201525f60a4808301829052825291829161257b60c482610824565b604051636352813560e11b602082019081526001600160a01b0392831660248301529190921660448301526064820192909252610bb860848201525f60a4808301829052825291829161257b60c482610824565b604051636352813560e11b602082019081526001600160a01b039283166024830152919092166044830152606482019290925261271060848201525f60a4808301829052825291829161257b60c482610824565b5f811215611b63576335278d125f526004601cfd5b6001607f1b81101561247a57600f0b90565b80158015612714575b611ba7576126e761ffff84511661ffff60208601511690848461271c565b611ba757612702818360409361ffff95109082180218612943565b920151161161271057600190565b5f90565b5081156126c9565b918061272784612943565b1061274f5761273582612943565b106127475761274391612953565b1190565b505050600190565b50505050600190565b908015611f295781612769916119c0565b80156128cb57670de0b6b3a7640000820291818115670de0b6b3a764000083860414170215612856575090045b6003810290606481156003838504141702156127f85750606490045b660aa87bee5380008101670de0b6b3a764000011156127f157670dd60e37b9108000035b8067016345785d8a00001167016345785d8a00008218021890565b505f6127d6565b606460035f1981840984811085019003920990806064111561284957828211900360fe1b910360021c177f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c29026127b2565b63ae47f7025f526004601cfd5b81670de0b6b3a76400005f1981840985811086019003920990825f03831692818111156128495783900480600302600218808202600203028082026002030280820260020302808202600203028082026002030280910260020302936001848483030494805f03040192119003021702612796565b6323d359a360e01b5f5260045ffd5b906128e3610c2d565b8281528251801561293e575f198101908111610bac5761290560809185610d34565b51015160208201525f90815b84518310156129345761292c60019160a06113f48689610d34565b920191612911565b6040820152925050565b509150565b8015612195571e60ff1860010190565b80158015612995575b61298e5761296c6129729161299d565b9161299d565b90818111156129855790611b6391610b9f565b611b6391610b9f565b50505f1990565b50811561295c565b80156129ad576001171e60ff0390565b63af458c0760e01b5f5260045ffdfea164736f6c6343000822000a
    /// ```
    #[rustfmt::skip]
    #[allow(clippy::all)]
    pub static DEPLOYED_BYTECODE: alloy_sol_types::private::Bytes = alloy_sol_types::private::Bytes::from_static(
        b"a\x01\x80`@R`\x046\x10\x15a\0\x12W_\x80\xFD[_5`\xE0\x1C\x80c!\xBF\x9F&\x14a\x04\xE8W\x80c\x81\xC6\xEC\xD6\x14a\x01\xEAWc\xC06\xC8\xEA\x14a\0;W_\x80\xFD[4a\x01\xE6W`\xA06`\x03\x19\x01\x12a\x01\xE6Wa\0Ta\x06\x96V[a\0\\a\x06\xACV[`\x845\x91`d5\x91`D5\x90`\x01`\x01`@\x1B\x03\x85\x11a\x01\xE6W6`#\x86\x01\x12\x15a\x01\xE6W\x84`\x04\x015`\x01`\x01`@\x1B\x03\x81\x11a\x01\xE6W\x85\x01\x92`$\x84\x01\x936\x85\x11a\x01\xE6W`@\x90a\0\xAEa\x08\\V[Pa\0\xBB\x87\x86\x86\x86a\x08\x9BV[\x87\x90\x03\x12a\x01\xE6W`$\x86\x015\x95`\xFF\x87\x16\x80\x97\x03a\x01\xE6W`D\x81\x015\x90`\x01`\x01`@\x1B\x03\x82\x11a\x01\xE6W\x01\x84`C\x82\x01\x12\x15a\x01\xE6W`$\x81\x015\x90a\x01\x03\x82a\x08\x80V[\x95a\x01\x11`@Q\x97\x88a\x08$V[\x82\x87R`D\x82\x84\x01\x01\x11a\x01\xE6W\x81_\x92`D` \x93\x01\x83\x89\x017\x86\x01\x01R`\x02\x86\x03a\x01yWa\x01C\x94\x95Pa\x17\xC1V[``\x81\x01Q\x15a\x01jWa\x01f\x91a\x01Z\x91a\x0B\xF1V[`@Q\x91\x82\x91\x82a\x06\xC2V[\x03\x90\xF3[c\x05A\x87\x11`\xE0\x1B_R`\x04_\xFD[`\x08\x86\x03a\x01\x91Wa\x01\x8C\x94\x95Pa\x17dV[a\x01CV[`\x03\x86\x03a\x01\xA4Wa\x01\x8C\x94\x95Pa\x16\xB4V[`\t\x86\x03a\x01\xB7Wa\x01\x8C\x94\x95Pa\x15\xB6V[\x85`\x05\x81\x03a\x01\xD4Wc\x16\x02\xDA\x9B`\xE2\x1B_R`\x05`\x04R`$_\xFD[c\x16\x02\xDA\x9B`\xE2\x1B_R`\x04R`$_\xFD[_\x80\xFD[4a\x01\xE6W`@6`\x03\x19\x01\x12a\x01\xE6W`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x01\xE6W6`#\x82\x01\x12\x15a\x01\xE6W\x80`\x04\x015\x90a\x02&\x82a\x08EV[\x90a\x024`@Q\x92\x83a\x08$V[\x82\x82R`$` \x83\x01\x93`\x05\x1B\x82\x01\x01\x906\x82\x11a\x01\xE6W`$\x81\x01\x93[\x82\x85\x10a\x03\xCEWa\x02e`$5\x85a\rHV[\x90`@Q\x91\x82\x91`@\x83\x01`@\x84R\x81Q\x80\x91R``\x84\x01\x90` ``\x82`\x05\x1B\x87\x01\x01\x93\x01\x91_\x90[\x82\x82\x10a\x03\x17WPPPP\x82\x81\x03` \x84\x01R` \x80\x83Q\x92\x83\x81R\x01\x92\x01\x90_[\x81\x81\x10a\x02\xBFWPPP\x03\x90\xF3[\x91\x93P\x91` `\xE0`\x01\x92`\xC0\x87Q\x80Q\x83R\x84\x81\x01Q\x85\x84\x01R`@\x81\x01Q`@\x84\x01R``\x81\x01Q``\x84\x01R`\x80\x81\x01Q`\x80\x84\x01R`\xA0\x81\x01Q`\xA0\x84\x01R\x01Q`\xC0\x82\x01R\x01\x94\x01\x91\x01\x91\x84\x93\x92a\x02\xB1V[\x91\x93\x90\x92\x94\x95P`_\x19\x87\x82\x03\x01\x82R\x84Q\x90``\x81\x01\x91\x80Q\x92``\x83R\x83Q\x80\x91R` `\x80\x84\x01\x94\x01\x90_\x90[\x80\x82\x10a\x03zWPPP`\x01\x92` \x92`@\x80\x84\x86\x80\x96\x01Q\x86\x85\x01R\x01Q\x91\x01R\x96\x01\x92\x01\x92\x01\x86\x95\x94\x93\x91\x92a\x02\x8FV[\x90\x91\x94` `\xE0`\x01\x92`\xC0\x89Q\x80Q\x83R\x84\x81\x01Q\x85\x84\x01R`@\x81\x01Q`@\x84\x01R``\x81\x01Q``\x84\x01R`\x80\x81\x01Q`\x80\x84\x01R`\xA0\x81\x01Q`\xA0\x84\x01R\x01Q`\xC0\x82\x01R\x01\x96\x01\x92\x01\x90a\x03GV[\x845`\x01`\x01`@\x1B\x03\x81\x11a\x01\xE6W\x82\x01```#\x19\x826\x03\x01\x12a\x01\xE6W`@Q\x90a\x03\xFB\x82a\x07\xA4V[`$\x81\x015`\x01`\x01`@\x1B\x03\x81\x11a\x01\xE6W`$\x90\x82\x01\x016`\x1F\x82\x01\x12\x15a\x01\xE6W\x805a\x04*\x81a\x08EV[\x91a\x048`@Q\x93\x84a\x08$V[\x81\x83R` `\xE0\x81\x85\x01\x93\x02\x82\x01\x01\x906\x82\x11a\x01\xE6W` \x01\x91[\x81\x83\x10a\x04\x84WPPP\x91`d` \x94\x92\x85\x94\x83R`D\x81\x015\x85\x84\x01R\x015`@\x82\x01R\x81R\x01\x94\x01\x93a\x02RV[`\xE0\x836\x03\x12a\x01\xE6W` `\xE0\x91`@Qa\x04\x9F\x81a\x07\xD3V[\x855\x81R\x82\x86\x015\x83\x82\x01R`@\x86\x015`@\x82\x01R``\x86\x015``\x82\x01R`\x80\x86\x015`\x80\x82\x01R`\xA0\x86\x015`\xA0\x82\x01R`\xC0\x86\x015`\xC0\x82\x01R\x81R\x01\x92\x01\x91a\x04TV[4a\x01\xE6W`\x806`\x03\x19\x01\x12a\x01\xE6Wa\x05\x01a\x06\x96V[a\x05\ta\x06\xACV[`d5\x91`D5a\x05\x18a\x08\\V[Pa\x05%\x84\x82\x85\x85a\x08\x9BV[a\x050\x81\x84\x84a\x08\xE9V[\x92a\x05<\x82\x82\x85a\t9V[\x84``\x80\x83\x01Q\x91\x01Q\x10a\x06\x8EW[P`\x01`\x01`\xA0\x1B\x03\x83\x16s\x82\xAFID}\x8A\x07\xE3\xBD\x95\xBD\rV\xF3RAR?\xBA\xB1\x81\x14\x15\x80a\x06gW[a\x06\x05W[s\xAF\x88\xD0e\xE7|\x8C\xC2#\x93'\xC5\xED\xB3\xA42&\x8EX1\x14\x15\x80a\x05\xDEW[a\x05\xB6W[PPP``\x81\x01Q\x15a\x01jWa\x01f\x91a\x01Z\x91a\x0B\xF1V[a\x05\xBF\x92a\x0B!V[``\x81\x01Q``\x83\x01Q\x10a\x05\xD6W[\x80\x80a\x05\x9CV[\x90P\x82a\x05\xCFV[P`\x01`\x01`\xA0\x1B\x03\x81\x16s\xAF\x88\xD0e\xE7|\x8C\xC2#\x93'\xC5\xED\xB3\xA42&\x8EX1\x14\x15a\x05\x97V[a\x06\x10\x83\x83\x86a\t\x9EV[a\x06\x1B\x84\x84\x87a\n\x88V[\x90``\x81\x01Q``\x88\x01Q\x10a\x06_W[P``\x81\x01Q``\x87\x01Q\x10a\x06CW[Pa\x05zV[\x94Ps\xAF\x88\xD0e\xE7|\x8C\xC2#\x93'\xC5\xED\xB3\xA42&\x8EX1a\x06=V[\x95P\x87a\x06,V[P`\x01`\x01`\xA0\x1B\x03\x82\x16s\x82\xAFID}\x8A\x07\xE3\xBD\x95\xBD\rV\xF3RAR?\xBA\xB1\x14\x15a\x05uV[\x93P\x85a\x05LV[`\x045\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x82\x03a\x01\xE6WV[`$5\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x82\x03a\x01\xE6WV[` \x81R`\xA0\x81\x01\x91\x80Q\x92`\x80` \x84\x01R\x83Q\x80\x91R` `\xC0\x84\x01\x94\x01\x90_[\x81\x81\x10a\x07\x85WPPP` \x81\x81\x01Q\x83\x85\x03`\x1F\x19\x01`@\x85\x01R\x80Q\x80\x86R\x94\x82\x01\x94\x91\x01\x90_[\x81\x81\x10a\x07lWPPP`@\x81\x01Q\x92`\x1F\x19\x83\x82\x03\x01``\x84\x01R` \x80\x85Q\x92\x83\x81R\x01\x94\x01\x90_[\x81\x81\x10a\x07QWPPP```\x80\x91\x01Q\x91\x01R\x90V[\x82Qb\xFF\xFF\xFF\x16\x86R` \x95\x86\x01\x95\x90\x92\x01\x91`\x01\x01a\x07:V[\x82Q`\xFF\x16\x86R` \x95\x86\x01\x95\x90\x92\x01\x91`\x01\x01a\x07\x0FV[\x82Q`\x01`\x01`\xA0\x1B\x03\x16\x86R` \x95\x86\x01\x95\x90\x92\x01\x91`\x01\x01a\x06\xE5V[``\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x07\xBFW`@RV[cNH{q`\xE0\x1B_R`A`\x04R`$_\xFD[`\xE0\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x07\xBFW`@RV[`\x80\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x07\xBFW`@RV[`\xA0\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x07\xBFW`@RV[\x90`\x1F\x80\x19\x91\x01\x16\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x07\xBFW`@RV[`\x01`\x01`@\x1B\x03\x81\x11a\x07\xBFW`\x05\x1B` \x01\x90V[`@Q\x90a\x08i\x82a\x07\xEEV[_``\x83\x81\x81R\x81` \x82\x01R\x81`@\x82\x01R\x01RV[`\x01`\x01`@\x1B\x03\x81\x11a\x07\xBFW`\x1F\x01`\x1F\x19\x16` \x01\x90V[`\x01`\x01`\xA0\x1B\x03\x90\x81\x16\x91\x16\x14a\x08\xDAW\x15a\x08\xCBWa\x03\xE8\x10a\x08\xBCWV[c*\x84\x06\xB9`\xE0\x1B_R`\x04_\xFD[c\x85~J\xA9`\xE0\x1B_R`\x04_\xFD[cA\x81\xF7?`\xE1\x1B_R`\x04_\xFD[\x90\x92\x91\x92a\t\0a\x08\xF8a\x08\\V[\x94\x82\x84a\x19\xCDV[\x91\x82\x15a\t4W\x90a\t\x11\x91a\x1B\xB7V[\x83Ra\t\x1Ba\x1C\x02V[` \x84\x01Ra\t(a\x1C\x02V[`@\x84\x01R``\x83\x01RV[PPPV[\x90\x92\x91\x92a\tPa\tHa\x08\\V[\x94\x82\x84a\x1CWV[\x91\x90\x92\x83\x15a\t\x98Wa\t(\x92\x91a\tg\x91a\x1B\xB7V[\x85R`@Qa\tw`@\x82a\x08$V[`\x01\x81R` 6\x81\x83\x017`\x01a\t\x8D\x82a\r\x03V[R` \x86\x01Ra\x1C)V[PPPPV[\x90\x92\x91\x92a\t\xC9a\t\xADa\x08\\V[\x94s\x82\xAFID}\x8A\x07\xE3\xBD\x95\xBD\rV\xF3RAR?\xBA\xB1\x84a\x19\xCDV[\x80\x15a\t4Wa\t\xEE\x90\x82s\x82\xAFID}\x8A\x07\xE3\xBD\x95\xBD\rV\xF3RAR?\xBA\xB1a\x19\xCDV[\x91\x82\x15a\t4W\x90s\x82\xAFID}\x8A\x07\xE3\xBD\x95\xBD\rV\xF3RAR?\xBA\xB1a\n\x14\x92a\x1C\xDFV[\x83R`@Qa\n$``\x82a\x08$V[`\x02\x81R`@\x90\x816` \x83\x017_a\n<\x82a\r\x03V[R_a\nG\x82a\r$V[R` \x85\x01R`@Q\x90a\n\\``\x83a\x08$V[`\x02\x82R6` \x83\x017_a\np\x82a\r\x03V[R_a\n{\x82a\r$V[R`@\x84\x01R``\x83\x01RV[\x90\x92\x91\x92a\n\xB3a\n\x97a\x08\\V[\x94s\x82\xAFID}\x8A\x07\xE3\xBD\x95\xBD\rV\xF3RAR?\xBA\xB1\x84a\x1CWV[\x90\x80\x15a\t\x98Wa\n\xD9\x90\x83s\x82\xAFID}\x8A\x07\xE3\xBD\x95\xBD\rV\xF3RAR?\xBA\xB1a\x1CWV[\x92\x90\x93\x84\x15a\x0B\x1AWa\t(\x93\x92\x91s\x82\xAFID}\x8A\x07\xE3\xBD\x95\xBD\rV\xF3RAR?\xBA\xB1a\x0B\x06\x92a\x1C\xDFV[\x86Ra\x0B\x10a\x1DIV[` \x87\x01Ra\x1DzV[PPPPPV[\x90\x92\x91\x92a\x0BLa\x0B0a\x08\\V[\x94s\xAF\x88\xD0e\xE7|\x8C\xC2#\x93'\xC5\xED\xB3\xA42&\x8EX1\x84a\x1CWV[\x90\x80\x15a\t\x98Wa\x0Br\x90\x83s\xAF\x88\xD0e\xE7|\x8C\xC2#\x93'\xC5\xED\xB3\xA42&\x8EX1a\x1CWV[\x92\x90\x93\x84\x15a\x0B\x1AWa\t(\x93\x92\x91s\xAF\x88\xD0e\xE7|\x8C\xC2#\x93'\xC5\xED\xB3\xA42&\x8EX1a\x0B\x06\x92a\x1C\xDFV[\x91\x90\x82\x03\x91\x82\x11a\x0B\xACWV[cNH{q`\xE0\x1B_R`\x11`\x04R`$_\xFD[\x81\x81\x02\x92\x91\x81\x15\x91\x84\x04\x14\x17\x15a\x0B\xACWV[\x81\x15a\x0B\xDDW\x04\x90V[cNH{q`\xE0\x1B_R`\x12`\x04R`$_\xFD[\x90a\x0B\xFAa\x08\\V[P\x80a\x0C\x04WP\x90V[``\x82\x01\x90\x81Q\x90a'\x10\x03\x90a'\x10\x82\x11a\x0B\xACWa'\x10\x91a\x0C'\x91a\x0B\xC0V[\x04\x90R\x90V[`@Q\x90a\x0C:\x82a\x07\xA4V[_`@\x83``\x81R\x82` \x82\x01R\x01RV[\x90a\x0CV\x82a\x08EV[a\x0Cc`@Q\x91\x82a\x08$V[\x82\x81R\x80\x92a\x0Ct`\x1F\x19\x91a\x08EV[\x01\x90_[\x82\x81\x10a\x0C\x84WPPPV[` \x90a\x0C\x8Fa\x0C-V[\x82\x82\x85\x01\x01R\x01a\x0CxV[`@Q\x90a\x0C\xA8\x82a\x07\xD3V[_`\xC0\x83\x82\x81R\x82` \x82\x01R\x82`@\x82\x01R\x82``\x82\x01R\x82`\x80\x82\x01R\x82`\xA0\x82\x01R\x01RV[\x90a\x0C\xDB\x82a\x08EV[a\x0C\xE8`@Q\x91\x82a\x08$V[\x82\x81R\x80\x92a\x0C\xF9`\x1F\x19\x91a\x08EV[\x01\x90` 6\x91\x017V[\x80Q\x15a\r\x10W` \x01\x90V[cNH{q`\xE0\x1B_R`2`\x04R`$_\xFD[\x80Q`\x01\x10\x15a\r\x10W`@\x01\x90V[\x80Q\x82\x10\x15a\r\x10W` \x91`\x05\x1B\x01\x01\x90V[\x80Q\x15a\x152Wa\rY\x81Qa\x0C\xD1V[\x90_[\x81Q\x81\x10\x15a\r\xA9W\x80`\x02a\rt`\x01\x93\x85a\r4V[QQQ\x10\x15a\r\x90W_[a\r\x89\x82\x86a\r4V[R\x01a\r\\V[a\r\xA4a\r\x9D\x82\x85a\r4V[QQa\x1D\xB2V[a\r\x7FV[P\x91a\r\xB5\x83Qa\x0C\xD1V[\x91\x83Q\x94a\r\xC2\x86a\x08EV[\x95a\r\xD0`@Q\x97\x88a\x08$V[\x80\x87Ra\r\xDF`\x1F\x19\x91a\x08EV[\x016` \x88\x017_\x92_[\x86Q\x81\x10\x15a\x0E\x8FWa\r\xFD\x81\x85a\r4V[Q_\x81a\x0E:W[\x15a\x0E\x14W[P`\x01\x01a\r\xEAV[\x85`\x01\x92\x96\x91a\x0E%\x84\x93\x8Aa\r4V[R\x81a\x0E1\x82\x8Ca\r4V[R\x01\x94\x90a\x0E\x0BV[_[\x87\x81\x10a\x0EJW[Pa\x0E\x05V[\x82a\x0EU\x82\x8Ba\r4V[Q\x14a\x0EcW`\x01\x01a\x0E<V[\x90Pa\x0Eo\x81\x8Ba\r4V[Q_\x19\x81\x14a\x0B\xACW`\x01a\x0E\x86\x91\x01\x91\x8Ba\r4V[R`\x01_a\x0EDV[P\x93\x95\x90\x94\x91\x95a\x0E\x9F\x84a\x0CLV[\x94_\x97_[\x86\x81\x10a\x14\xEEWPa\x0E\xB5\x89a\x08EV[\x98a\x0E\xC3`@Q\x9A\x8Ba\x08$V[\x80\x8ARa\x0E\xD2`\x1F\x19\x91a\x08EV[\x01_[\x81\x81\x10a\x14\xD0WPP`@Q\x94a\x0E\xEB\x86a\x08\tV[\x85R` \x85\x01R\x85`@\x85\x01R\x87``\x85\x01R`\x80\x84\x01R_\x93__\x93[\x82\x85\x10a\x0F\x1BWPPPPPPP\x91\x90V[a\x0F,\x85\x85\x98\x9B\x97\x9A\x96\x99\x95a\r4V[Qa\x01\0R`\x01a\x0F=\x89\x83a\r4V[Q\x03a\x0F\xC8W_[\x89QQ\x81\x10\x15a\x0F\xB4Wa\x01\0Qa\x0Fa\x82` \x8D\x01Qa\r4V[Q\x14a\x0FoW`\x01\x01a\x0FEV[\x8Aa\x0F\xA4\x8B\x98\x9Ca\x0F\x8D`\x01\x95\x97\x99\x9D\x94\x86\x95\x9D\x9C\x97\x99\x9DQa\r4V[Q`@\x8B\x01Q\x90a\x0F\x9E\x83\x83a\r4V[Ra\r4V[P[\x01\x96[\x01\x93\x91\x92\x90\x92a\x0F\tV[P\x91\x93\x97\x94\x98`\x01\x80\x91\x98\x97\x92\x94\x98a\x0F\xA6V[\x94\x92a\x0F\xDC\x88\x87\x9A\x99\x96\x9A\x98\x94\x93\x98a\r4V[Qa\x01@R\x87Q\x98` \x89\x01Qa\x01`R`\x80\x89\x01Q\x96a\x0F\xFBa\x0C-V[Pa\x10\x04a\x0C\x9BV[Pa\x10\x11a\x01@Qa\x0CLV[a\x01 R_\x95_[\x8CQ\x81\x10\x15a\x14\xBEW\x8Ca\x01\0Qa\x104\x83a\x01`Qa\r4V[Q\x14a\x10DW[P`\x01\x01a\x10\x19V[\x97a\x10R\x82`\x01\x93\x9Aa\r4V[Qa\x10`\x82a\x01 Qa\r4V[Ra\x10n\x81a\x01 Qa\r4V[P\x01\x96a\x01@Q\x88\x14a\x10\x81W\x8Ca\x10;V[P\x93\x91\x95P\x93\x91\x95\x96\x97\x98\x99P[_`\xE0Ra\x10\x9Fa\x01 Qa\r\x03V[QQa\x10\xADa\x01 Qa\r\x03V[QQQ_\x19\x81\x01\x90\x81\x11a\x0B\xACWa\x10\xC4\x91a\r4V[Q\x95`\x80\x87\x01Q\x80g\r\xE0\xB6\xB3\xA7d\0\0\x81\x02\x04g\r\xE0\xB6\xB3\xA7d\0\0\x14\x81\x15\x17\x15a\x0B\xACW``\x88\x01Qa\x11\x02\x91g\r\xE0\xB6\xB3\xA7d\0\0\x02a\x0B\xD3V[`\xC0R_[a\x01 QQ\x81\x10\x15a\x11\xDCWa\x11 \x81a\x01 Qa\r4V[QQQa\x110\x82a\x01 Qa\r4V[QQ`\x01\x19\x82\x01\x82\x81\x11a\x0B\xACWa\x11G\x91a\r4V[Q\x90a\x11V\x83a\x01 Qa\r4V[QQ\x91_\x19\x82\x01\x91\x82\x11a\x0B\xACWa\x11s`\x80\x92a\x11\x80\x94a\r4V[Q\x82R\x01Q`\xE0Qa\x19\xC0V[`\xE0R`\x80\x80Q\x01Qg\r\xE0\xB6\xB3\xA7d\0\0\x81\x02\x90\x80\x82\x04g\r\xE0\xB6\xB3\xA7d\0\0\x14\x90\x15\x17\x15a\x0B\xACW`\x80Q``\x01Qa\x11\xBA\x91a\x0B\xD3V[`\xC0Q\x81\x11a\x11\xCDW[P`\x01\x01a\x11\x07V[`\xC0R`\x80Q\x97P`\x01a\x11\xC4V[P\x90\x91\x92\x93\x9A\x95\x96\x97\x98\x99\x94g\r\xE0\xB6\xB3\xA7d\0\0a\x12\x0Ca\x12\x04`\xC0\x8B\x01Q`\xE0Qa'XV[`\xE0Qa\x0B\xC0V[\x04\x97`\xA0\x81\x01Q\x80`>\x81\x02\x04`>\x14\x81\x15\x17\x15a\x0B\xACW`@Q\x99a\x121\x8Ba\x08\tV[_`\x80\x8C\x01Ra\x01\0Q\x8BRa\x01@Q` \x8C\x01R`\xE0Q`@\x8C\x01R\x80``\x8C\x01Ra\x12`a\x01 Qa\r\x03V[QQa\x12na\x01 Qa\r\x03V[QQQ\x80`\x01\x19\x81\x01\x11a\x0B\xACW`\x01\x19\x01a\x12\x89\x91a\r4V[Q`@\x01Q\x9Ca\x12\x97a\x0C\x9BV[\x80\x9E\x85Q\x82R` \x82\x01R`@\x01R\x8C`\xE0Q\x90``\x01R`\x80\x8D\x01R`>\x02`d\x90\x04`\xA0\x8C\x01R`\xC0\x01Q`\xC0\x8B\x01Ra\x01 Qa\x12\xD6\x90a\r\x03V[QQ\x9B\x8CQa\x12\xE4\x81a\x08EV[`@Q`\xA0R`\xA0Q\x90a\x12\xF7\x91a\x08$V[\x80`\xA0QR`\x1F\x19\x90a\x13\t\x90a\x08EV[\x01_[\x81\x81\x10a\x14\xA5WPP_[`\x01\x81\x01\x81\x11a\x0B\xACW\x8DQ`\x01\x82\x01\x10\x15a\x13\\W\x80a\x13;\x8F\x92`\x01\x93a\r4V[Qa\x13H\x82`\xA0Qa\r4V[Ra\x13U\x81`\xA0Qa\r4V[P\x01a\x13\x17V[P\x92\x98\x9A\x97\x99\x90\x93\x9B\x95\x94\x91\x95Q\x80_\x19\x81\x01\x11a\x0B\xACWa\x13\x93\x91a\x13\x87_\x19\x83\x01`\xA0Qa\r4V[R_\x19\x01`\xA0Qa\r4V[Pa\x13\x9F`\xA0Qa(\xDAV[\x92`@\x84\x01Q`\x80\x8B\x01Ra\x13\xB2a\x0C\x9BV[\x95_\x99_\x94_\x95[a\x01 QQ\x87\x10\x15a\x14'W\x8C` a\x13\xD6\x89a\x01 Qa\r4V[Q\x01Q\x11a\x14\x05W[a\x13\xFD`\x01\x91`@a\x13\xF4\x8Aa\x01 Qa\r4V[Q\x01Q\x90a\x19\xC0V[\x96\x01\x95a\x13\xBAV[\x9BP`\x01a\x13\xFD` a\x14\x1B\x89a\x01 Qa\r4V[Q\x01Q\x9D\x91PPa\x13\xDFV[`\x01\x96P\x98\x86\x95\x9F\x97\x86\x95\x9F\x9A\x9B\x80\x95\x9F\x91\x8E\x9F\x8C\x9F\x95\x9Ca\x14\x9B\x97\x9F`\x80a\x14\x8A\x96`@\x94\x81Q\x8AR` \x82\x01Q` \x8B\x01R\x85\x82\x01Q\x86\x8B\x01R``\x82\x01Q``\x8B\x01R\x82\x8A\x01R\x01Q`\xA0\x88\x01R`\xC0\x87\x01R\x01Q\x90a\x0F\x9E\x83\x83a\r4V[P``\x8C\x01Q\x90a\x0F\x9E\x83\x83a\r4V[P\x01\x97\x01\x93a\x0F\xA9V[` \x90a\x14\xB0a\x0C\x9BV[\x82\x82`\xA0Q\x01\x01R\x01a\x13\x0CV[P\x93\x91\x95P\x93\x91\x95\x96\x97\x98\x99Pa\x10\x8FV[\x80\x8B` \x80\x93a\x14\xE1\x9B\x99\x9Ba\x0C\x9BV[\x92\x01\x01R\x01\x96\x94\x96a\x0E\xD5V[`\x01a\x14\xFC\x82\x87\x99\x97a\r4V[Q\x11\x80a\x15\x1FW[a\x15\x14W[`\x01\x01\x95\x93\x95a\x0E\xA4V[`\x01\x90\x99\x01\x98a\x15\tV[Pa\x15*\x81\x85a\r4V[Q\x15\x15a\x15\x04V[PP`@Q\x90a\x15C` \x83a\x08$V[_\x82R_\x80[\x81\x81\x10a\x15\x8BWPP`@Q\x91a\x15a` \x84a\x08$V[_\x83R_\x80[\x81\x81\x10a\x15tWPP\x91\x90V[` \x90a\x15\x7Fa\x0C\x9BV[\x82\x82\x88\x01\x01R\x01a\x15gV[` \x90a\x15\x96a\x0C-V[\x82\x82\x87\x01\x01R\x01a\x15IV[Q\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x82\x03a\x01\xE6WV[\x91\x93\x92a\x15\xC1a\x08\\V[\x94``\x82\x80Q\x81\x01\x03\x12a\x01\xE6Wa\x15\xDB` \x83\x01a\x15\xA2V[\x91```@\x82\x01Q\x91\x01Q\x92`\xFF\x84\x16\x80\x94\x03a\x01\xE6W`\x01`\x01`\xA0\x1B\x03\x16\x80\x15\x80\x15a\x16\x9EW[a\x16\x96W\x84\x91\x86\x91`\x02\x86\x03a\x16dWa\x16\x1E\x95Pa\x1F9V[\x91[\x82\x15a\t4W\x90a\x160\x91a\x1B\xB7V[\x83R`@Qa\x16@`@\x82a\x08$V[`\x01\x81R` 6\x81\x83\x017`\x05a\x16V\x82a\r\x03V[R` \x84\x01Ra\t(a\x1C\x02V[\x93\x94\x90\x92P`\x03\x14\x15\x90Pa\x0B\x1AW`\x01`\x01`\xA0\x1B\x03\x16\x90\x81\x15a\x0B\x1AW\x84a\x16\x90\x93\x92\x85\x92a\x1E|V[\x91a\x16 V[PPPPPPV[P\x80;\x15a\x16\x04V[Q\x90\x81\x15\x15\x82\x03a\x01\xE6WV[\x91\x93\x92a\x16\xBFa\x08\\V[\x94`\x80\x82\x80Q\x81\x01\x03\x12a\x01\xE6Wa\x16\xD9` \x83\x01a\x15\xA2V[\x91`@\x81\x01Qa\x16\xF0`\x80``\x84\x01Q\x93\x01a\x16\xA7V[\x93`\x01`\x01`\xA0\x1B\x03\x16\x80\x15\x80\x15a\x17[W[a\x17RW\x82\x82\x14a\x17RW\x90a\x17\x1B\x94\x93\x92\x91a\"fV[\x91\x82\x15a\t4W\x90a\x17,\x91a\x1B\xB7V[\x83R`@Qa\x17<`@\x82a\x08$V[`\x01\x81R` 6\x81\x83\x017`\x03a\x16V\x82a\r\x03V[PPPPPPPV[P\x80;\x15a\x17\x03V[\x91\x93\x92\x93a\x17pa\x08\\V[\x94`@\x81\x80Q\x81\x01\x03\x12a\x01\xE6Wa\x17\x96`@a\x17\x8F` \x84\x01a\x15\xA2V[\x92\x01a\x16\xA7V[P`\x01`\x01`\xA0\x1B\x03\x16\x80\x15\x80\x15a\x17\xB8W[a\t\x98W\x90\x83a\t\0\x92a#3V[P\x80;\x15a\x17\xA9V[\x91\x93\x92\x90a\x17\xCDa\x08\\V[\x94\x82Q\x83\x01\x90\x83` \x83\x01\x92\x03`\xE0\x81\x12a\x01\xE6W`\xA0\x13a\x01\xE6W`@Q\x93a\x17\xF6\x85a\x08\tV[a\x18\x02` \x82\x01a\x15\xA2V[\x85Ra\x18\x10`@\x82\x01a\x15\xA2V[\x94` \x81\x01\x95\x86R``\x82\x01Q\x95b\xFF\xFF\xFF\x87\x16\x87\x03a\x01\xE6W`@\x82\x01\x96\x87R`\x80\x83\x01Q\x80`\x02\x0B\x81\x03a\x01\xE6W``\x83\x01Ra\x18Q`\xA0\x84\x01a\x15\xA2V[`\x80\x83\x01Ra\x18b`\xC0\x84\x01a\x16\xA7V[\x92`\xE0\x81\x01Q\x90`\x01`\x01`@\x1B\x03\x82\x11a\x01\xE6W\x01\x85`?\x82\x01\x12\x15a\x01\xE6W` \x81\x01Q\x90a\x18\x92\x82a\x08\x80V[\x96a\x18\xA0`@Q\x98\x89a\x08$V[\x82\x88R`@\x82\x84\x01\x01\x11a\x01\xE6W\x81_\x92`@` \x93\x01\x83\x8A\x01^\x87\x01\x01R\x82\x15a\x19RW\x81Q`\x01`\x01`\xA0\x1B\x03\x89\x81\x16\x91\x16\x14\x90\x81a\x19;W[P[\x15a\x17RW\x90a\x18\xEF\x93\x92\x91a#wV[\x92\x83\x15a\t\x98W\x91a\x19\x08b\xFF\xFF\xFF\x92a\t(\x94a\x1B\xB7V[\x86R`@Qa\x19\x18`@\x82a\x08$V[`\x01\x81R` 6\x81\x83\x017`\x02a\x19.\x82a\r\x03V[R` \x87\x01RQ\x16a\x1C)V[Q`\x01`\x01`\xA0\x1B\x03\x87\x81\x16\x91\x16\x14\x90P_a\x18\xDCV[Q`\x01`\x01`\xA0\x1B\x03\x88\x81\x16\x91\x16\x14\x80\x15a\x18\xDEWP\x80Q`\x01`\x01`\xA0\x1B\x03\x86\x81\x16\x91\x16\x14a\x18\xDEV[=\x15a\x19\xA7W=\x90a\x19\x8E\x82a\x08\x80V[\x91a\x19\x9C`@Q\x93\x84a\x08$V[\x82R=_` \x84\x01>V[``\x90V[Q\x90`\x01`\x01`p\x1B\x03\x82\x16\x82\x03a\x01\xE6WV[\x91\x90\x82\x01\x80\x92\x11a\x0B\xACWV[\x90a\x19\xD8\x90\x82a$\x87V[`\x01`\x01`\xA0\x1B\x03\x81\x16\x15\x80\x15a\x1B\xAEW[a\x1B\xA7W_\x80`@Q` \x81\x01\x90c\x02@\xBCk`\xE2\x1B\x82R`\x04\x81Ra\x1A\x11`$\x82a\x08$V[Q\x90\x84Z\xFA\x91a\x1A\x1Fa\x19}V[\x92\x15\x80\x15a\x1B\x9CW[a\x1BfW``\x83\x80Q\x81\x01\x03\x12a\x01\xE6Wa\x1AE` \x84\x01a\x19\xACV[\x91``a\x1AT`@\x86\x01a\x19\xACV[\x94\x01Qc\xFF\xFF\xFF\xFF\x81\x16\x03a\x01\xE6W_\x80\x91`@Q` \x81\x01\x90c\r\xFE\x16\x81`\xE0\x1B\x82R`\x04\x81Ra\x1A\x87`$\x82a\x08$V[Q\x91Z\xFAa\x1A\x93a\x19}V[\x90\x15\x80\x15a\x1B\x91W[a\x1B\x88W` \x81\x80Q\x81\x01\x03\x12a\x01\xE6W`\x01`\x01`\xA0\x1B\x03\x90a\x1A\xC2\x90` \x01a\x15\xA2V[`\x01`\x01`\xA0\x1B\x03\x90\x92\x16\x91\x16\x03a\x1BvW`\x01`\x01`p\x1B\x03\x91\x82\x16\x91\x16[\x80\x15\x91\x82\x80\x15a\x1BnW[a\x1BfWa\x1B\x19`@Qa\x1B\0\x81a\x07\xA4V[`\x10\x81R`(` \x82\x01R`\x10`@\x82\x01R\x82\x84a&\xC0V[\x15a\x1BfWa\x03\xE5\x84\x02\x93\x80\x85\x04a\x03\xE5\x14\x90\x15\x17\x15a\x0B\xACWa\x1B=\x90\x84a\x0B\xC0V[\x91a\x03\xE8\x82\x02\x91\x82\x04a\x03\xE8\x14\x17\x15a\x0B\xACWa\x1Bc\x92a\x1B]\x91a\x19\xC0V[\x90a\x0B\xD3V[\x90V[PPPP_\x90V[P\x80\x15a\x1A\xEDV[`\x01`\x01`p\x1B\x03\x90\x81\x16\x91\x16a\x1A\xE2V[PPPPP_\x90V[P` \x81Q\x10a\x1A\x9CV[P``\x83Q\x10a\x1A(V[PPP_\x90V[P\x80;\x15a\x19\xEAV[\x91\x90a\x1B\xF3`@Qa\x1B\xCA``\x82a\x08$V[`\x02\x81R`@6` \x83\x017\x80\x94a\x1B\xE1\x82a\r\x03V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x90Ra\r$V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x90RV[`@Q\x90a\x1C\x11`@\x83a\x08$V[`\x01\x82R` 6\x81\x84\x017_a\x1C&\x83a\r\x03V[RV[\x90`@Q\x91a\x1C9`@\x84a\x08$V[`\x01\x83R` 6\x81\x85\x017b\xFF\xFF\xFFa\x1CQ\x84a\r\x03V[\x91\x16\x90RV[\x91\x92\x90\x92_\x93_\x93a\x1Cj\x83\x83\x83a%&V[\x80a\x1C\xD3W[Pa\x1C|\x83\x83\x83a%\x9DV[\x86\x81\x11a\x1C\xC6W[Pa\x1C\x90\x83\x83\x83a%\xF1V[\x86\x81\x11a\x1C\xB7W[P\x90a\x1C\xA4\x92\x91a&EV[\x83\x81\x11a\x1C\xAEWPV[\x92Pa'\x10\x91PV[\x95Pa\x0B\xB8\x94Pa\x1C\xA4a\x1C\x98V[\x95Pa\x01\xF4\x94P_a\x1C\x84V[\x95P`d\x94P_a\x1CpV[\x92\x91\x90`@Q\x90a\x1C\xF1`\x80\x83a\x08$V[`\x03\x82R``6` \x84\x017\x81\x94a\x1D\x08\x83a\r\x03V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x90Ra\x1D\x1E\x82a\r$V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x90R\x80Q`\x02\x10\x15a\r\x10W`\x01`\x01`\xA0\x1B\x03\x90\x91\x16``\x91\x90\x91\x01RV[`@Q\x90a\x1DX``\x83a\x08$V[`\x02\x82R`@6` \x84\x017`\x01a\x1C&\x83\x82a\x1Dt\x82a\r\x03V[Ra\r$V[\x91\x90b\xFF\xFF\xFFa\x1CQ`@Qa\x1D\x91``\x82a\x08$V[`\x02\x81R`@6` \x83\x017\x80\x95\x83a\x1D\xA9\x83a\r\x03V[\x91\x16\x90Ra\r$V[\x90\x81Q`\x02\x81\x10a\x1ERW_\x19\x81\x01\x90\x81\x11a\x0B\xACWa\x1D\xD1\x81a\x0C\xD1V[\x90_[\x81\x81\x10a\x1E/WPP\x90\x91P`@Q` \x81\x01\x81\x81\x93` \x81Q\x93\x91\x01\x92_[\x81\x81\x10a\x1E\x16WPPa\x1E\x10\x92P\x03`\x1F\x19\x81\x01\x83R\x82a\x08$V[Q\x90 \x90V[\x84Q\x83R` \x94\x85\x01\x94\x86\x94P\x90\x92\x01\x91`\x01\x01a\x1D\xF4V[\x80`@a\x1E>`\x01\x93\x88a\r4V[Q\x01Qa\x1EK\x82\x86a\r4V[R\x01a\x1D\xD4V[P_\x91PV[\x80Q\x80\x83R` \x92\x91\x81\x90\x84\x01\x84\x84\x01^_\x82\x82\x01\x84\x01R`\x1F\x01`\x1F\x19\x16\x01\x01\x90V[\x90_\x80\x94a\x1E\xFA\x82\x95a\x1E\xEC` \x99`@Q\x90a\x1E\x99\x8C\x83a\x08$V[\x86\x82R`@Qc\x07\xD2E\xE9`\xE4\x1B\x8D\x82\x01\x90\x81R`\x01`\x01`\xA0\x1B\x03\x99\x8A\x16`$\x83\x01R\x94\x89\x16`D\x82\x01R\x95\x90\x97\x16`d\x86\x01R`\x84\x85\x01\x96\x90\x96R`\xA0`\xA4\x85\x01R\x90\x94\x83\x91\x90`\xC4\x83\x01\x90a\x1EXV[\x03`\x1F\x19\x81\x01\x83R\x82a\x08$V[Q\x92Z\xF1\x90a\x1F\x07a\x19}V[\x91\x15\x80\x15a\x1F/W[a\x1F)W\x81Q\x81\x83\x01\x92\x01\x81\x01\x82\x90\x03\x12a\x01\xE6WQ\x90V[PP_\x90V[P\x80\x82Q\x10a\x1F\x10V[\x90\x91\x93\x92\x93`@\x94\x85Q\x93a\x1FN\x87\x86a\x08$V[`\x01\x85R`\x1F\x19\x87\x01_[\x81\x81\x10a\"2WPP\x86Q` \x96a\x1Fq\x88\x83a\x08$V[_\x82R\x88Q\x92a\x1F\x80\x84a\x08\tV[\x83R_\x88\x84\x01R`\x01\x89\x84\x01R``\x83\x01R`\x80\x82\x01Ra\x1F\xA0\x85a\r\x03V[Ra\x1F\xAA\x84a\r\x03V[P``\x93\x86Q\x91a\x1F\xBB\x86\x84a\x08$V[`\x02\x83R\x86\x83\x01\x93`\x1F\x19\x87\x016\x867a\x1F\xD4\x84a\r\x03V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x90Ra\x1F\xEA\x83a\r$V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x90R\x86Qa \x02\x81a\x07\xEEV[0\x81R\x86\x81\x01\x90_\x82R\x88\x81\x01\x920\x84R\x88\x88\x83\x01\x95_\x87R\x8B\x80Q\x9Ac|&\x837`\xE1\x1B\x84\x8D\x01R\x8Ba\x01\x04\x81\x01\x91_`$\x83\x01R`\xE0`D\x83\x01R\x86Q\x80\x93Ra\x01$\x82\x01\x90\x86a\x01$\x85`\x05\x1B\x85\x01\x01\x98\x01\x94_\x93[\x85\x85\x10a!\xD0WPPPPP\x8B\x85\x03`#\x19\x01`d\x8D\x01RPPQ\x80\x83R\x91\x01\x95\x90_[\x8A\x82\x82\x10a!\xB3WPP\x91Q`\x01`\x01`\xA0\x1B\x03\x90\x81\x16`\x84\x8A\x01R\x92Q\x15\x15`\xA4\x89\x01RPP\x90Q\x16`\xC4\x85\x01RQ\x15\x15`\xE4\x84\x01R\x82\x90\x03`\x1F\x19\x81\x01\x83R_\x92\x83\x92\x90\x91a \xD0\x90\x83a\x08$V[\x82\x85\x83Q\x93\x01\x91Z\xF1a \xE1a\x19}V[\x90\x15\x80\x15a!\xA9W[a\x1B\xA7W\x80Q\x81\x01\x90\x82\x81\x81\x84\x01\x93\x03\x12a\x01\xE6W\x82\x81\x01Q\x90`\x01`\x01`@\x1B\x03\x82\x11a\x01\xE6W\x01\x92\x81`?\x85\x01\x12\x15a\x01\xE6W\x82\x84\x01Q\x90a!-\x82a\x08EV[\x94a!:\x82Q\x96\x87a\x08$V[\x82\x86R\x84\x80\x80\x88\x01\x94`\x05\x1B\x83\x01\x01\x01\x93\x84\x11a\x01\xE6W\x01\x90[\x82\x82\x10a!\x9AWPPPP`\x02\x81Q\x10a!\x95Wa!q\x90a\r$V[Q_\x81\x13a!\x95W\x80\x15a!\x95W`\x01`\xFF\x1B\x81\x14a\x0B\xACWa\x1Bc\x90_\x03a&\x99V[P_\x90V[\x81Q\x81R\x90\x83\x01\x90\x83\x01a!TV[P\x82\x81Q\x10a \xEAV[\x83Q`\x01`\x01`\xA0\x1B\x03\x16\x89R\x97\x88\x01\x97\x90\x92\x01\x91`\x01\x01a \x7FV[\x88\x92\x94\x96\x99`\xA0`\x80`\x01\x96\x98\x9A\x9B\x94a\"\x1D\x94a\x01#\x19\x90\x85\x03\x01\x8AR\x8DQ\x90\x81Q\x85R\x86\x82\x01Q\x87\x86\x01R\x80\x82\x01Q\x90\x85\x01R\x88\x81\x01Q\x89\x85\x01R\x01Q\x91\x81`\x80\x82\x01R\x01\x90a\x1EXV[\x98\x01\x93\x01\x93\x01\x90\x92\x8F\x93\x8F\x96\x95\x93\x94\x8Fa [V[` \x90\x89Qa\"@\x81a\x08\tV[_\x81R_\x83\x82\x01R_\x8B\x82\x01R_``\x82\x01R```\x80\x82\x01R\x82\x82\x8A\x01\x01R\x01a\x1FYV[_\x94\x85\x94\x91\x93\x92\x90\x15a\"\xFDWa\"\x7Fa\"\x85\x91a&\xAEV[\x92a&\xAEV[`@Q\x92c^\rD?`\xE0\x1B` \x85\x01R`\x0F\x0B`$\x84\x01R`\x0F\x0B`D\x83\x01R`d\x82\x01R`d\x81Ra\"\xBA`\x84\x82a\x08$V[\x90[` \x82Q\x92\x01\x90Z\xFAa\"\xCDa\x19}V[\x90\x15\x80\x15a\"\xF2W[a!\x95W` \x81Q\x91\x81\x80\x82\x01\x93\x84\x92\x01\x01\x03\x12a\x01\xE6WQ\x90V[P` \x81Q\x10a\"\xD6V[\x91`@Q\x92cUmn\x9F`\xE0\x1B` \x85\x01R`$\x84\x01R`D\x83\x01R`d\x82\x01R`d\x81Ra#-`\x84\x82a\x08$V[\x90a\"\xBCV[_\x92\x83\x92`@Q\x90` \x82\x01\x92cx\xA0Q\xAD`\xE1\x1B\x84R`$\x83\x01R`\x01\x80`\xA0\x1B\x03\x16`D\x82\x01R`D\x81Ra#k`d\x82a\x08$V[Q\x91Z\xFAa\"\xCDa\x19}V[\x90\x91`\x01`\x80\x1B\x81\x10\x15a$zWa$X_\x94\x93a\x1E\xEC\x86\x95`@Q\x95a#\x9D\x87a\x07\xEEV[\x86R` \x86\x01\x92\x15\x15\x83R`\x01`\x01`\x80\x1B\x03`@\x87\x01\x95\x16\x85R``\x86\x01\x90\x81R`\x01`\x01`\x80\x1B\x03`@Q\x95\x86\x94` \x86\x01\x98c\xAA\x9D!\xCB`\xE0\x1B\x8AR` `$\x88\x01RQ`\x01\x80`\xA0\x1B\x03\x81Q\x16`D\x88\x01R`\x01\x80`\xA0\x1B\x03` \x82\x01Q\x16`d\x88\x01Rb\xFF\xFF\xFF`@\x82\x01Q\x16`\x84\x88\x01R``\x81\x01Q`\x02\x0B`\xA4\x88\x01R`\x80`\x01\x80`\xA0\x1B\x03\x91\x01Q\x16`\xC4\x87\x01RQ\x15\x15`\xE4\x86\x01RQ\x16a\x01\x04\x84\x01RQa\x01\0a\x01$\x84\x01Ra\x01D\x83\x01\x90a\x1EXV[Q\x90\x82s9r\xC0\x0F~\xD4\x88^\x14X#\xEB|eSu\xD2u\xA1\xC5Z\xF1a\"\xCDa\x19}V[c5'\x8D\x12_R`\x04`\x1C\xFD[`@Qc\xE6\xA49\x05`\xE0\x1B` \x82\x01\x90\x81R`\x01`\x01`\xA0\x1B\x03\x92\x83\x16`$\x83\x01R\x92\x90\x91\x16`D\x80\x83\x01\x91\x90\x91R\x81R_\x91\x82\x91a$\xC7`d\x82a\x08$V[Q\x90s\xF1\xD7\xCCd\xFBDR\xF0\\I\x81&1.\xBE)\xF3\x0F\xBC\xF9Z\xFAa$\xE8a\x19}V[\x90\x15\x80\x15a%\x1BW[a!\x95W` \x81\x80Q\x81\x01\x03\x12a\x01\xE6W`\x01`\x01`\xA0\x1B\x03\x90a%\x17\x90` \x01a\x15\xA2V[\x16\x90V[P` \x81Q\x10a$\xF1V[`@QccR\x815`\xE1\x1B` \x82\x01\x90\x81R`\x01`\x01`\xA0\x1B\x03\x92\x83\x16`$\x83\x01R\x91\x90\x92\x16`D\x83\x01R`d\x80\x83\x01\x93\x90\x93R`\x84\x82\x01\x92\x90\x92R_`\xA4\x80\x83\x01\x82\x90R\x82R\x91\x82\x91a%{`\xC4\x82a\x08$V[Q\x90\x82sa\xFF\xE0\x14\xBA\x17\x98\x9Et<_l\xB2\x1B\xF9iu0\xB2\x1EZ\xF1a\"\xCDa\x19}V[`@QccR\x815`\xE1\x1B` \x82\x01\x90\x81R`\x01`\x01`\xA0\x1B\x03\x92\x83\x16`$\x83\x01R\x91\x90\x92\x16`D\x83\x01R`d\x82\x01\x92\x90\x92Ra\x01\xF4`\x84\x82\x01R_`\xA4\x80\x83\x01\x82\x90R\x82R\x91\x82\x91a%{`\xC4\x82a\x08$V[`@QccR\x815`\xE1\x1B` \x82\x01\x90\x81R`\x01`\x01`\xA0\x1B\x03\x92\x83\x16`$\x83\x01R\x91\x90\x92\x16`D\x83\x01R`d\x82\x01\x92\x90\x92Ra\x0B\xB8`\x84\x82\x01R_`\xA4\x80\x83\x01\x82\x90R\x82R\x91\x82\x91a%{`\xC4\x82a\x08$V[`@QccR\x815`\xE1\x1B` \x82\x01\x90\x81R`\x01`\x01`\xA0\x1B\x03\x92\x83\x16`$\x83\x01R\x91\x90\x92\x16`D\x83\x01R`d\x82\x01\x92\x90\x92Ra'\x10`\x84\x82\x01R_`\xA4\x80\x83\x01\x82\x90R\x82R\x91\x82\x91a%{`\xC4\x82a\x08$V[_\x81\x12\x15a\x1BcWc5'\x8D\x12_R`\x04`\x1C\xFD[`\x01`\x7F\x1B\x81\x10\x15a$zW`\x0F\x0B\x90V[\x80\x15\x80\x15a'\x14W[a\x1B\xA7Wa&\xE7a\xFF\xFF\x84Q\x16a\xFF\xFF` \x86\x01Q\x16\x90\x84\x84a'\x1CV[a\x1B\xA7Wa'\x02\x81\x83`@\x93a\xFF\xFF\x95\x10\x90\x82\x18\x02\x18a)CV[\x92\x01Q\x16\x11a'\x10W`\x01\x90V[_\x90V[P\x81\x15a&\xC9V[\x91\x80a''\x84a)CV[\x10a'OWa'5\x82a)CV[\x10a'GWa'C\x91a)SV[\x11\x90V[PPP`\x01\x90V[PPPP`\x01\x90V[\x90\x80\x15a\x1F)W\x81a'i\x91a\x19\xC0V[\x80\x15a(\xCBWg\r\xE0\xB6\xB3\xA7d\0\0\x82\x02\x91\x81\x81\x15g\r\xE0\xB6\xB3\xA7d\0\0\x83\x86\x04\x14\x17\x02\x15a(VWP\x90\x04[`\x03\x81\x02\x90`d\x81\x15`\x03\x83\x85\x04\x14\x17\x02\x15a'\xF8WP`d\x90\x04[f\n\xA8{\xEES\x80\0\x81\x01g\r\xE0\xB6\xB3\xA7d\0\0\x11\x15a'\xF1Wg\r\xD6\x0E7\xB9\x10\x80\0\x03[\x80g\x01cEx]\x8A\0\0\x11g\x01cEx]\x8A\0\0\x82\x18\x02\x18\x90V[P_a'\xD6V[`d`\x03_\x19\x81\x84\t\x84\x81\x10\x85\x01\x90\x03\x92\t\x90\x80`d\x11\x15a(IW\x82\x82\x11\x90\x03`\xFE\x1B\x91\x03`\x02\x1C\x17\x7F\\(\xF5\xC2\x8F\\(\xF5\xC2\x8F\\(\xF5\xC2\x8F\\(\xF5\xC2\x8F\\(\xF5\xC2\x8F\\(\xF5\xC2\x8F\\)\x02a'\xB2V[c\xAEG\xF7\x02_R`\x04`\x1C\xFD[\x81g\r\xE0\xB6\xB3\xA7d\0\0_\x19\x81\x84\t\x85\x81\x10\x86\x01\x90\x03\x92\t\x90\x82_\x03\x83\x16\x92\x81\x81\x11\x15a(IW\x83\x90\x04\x80`\x03\x02`\x02\x18\x80\x82\x02`\x02\x03\x02\x80\x82\x02`\x02\x03\x02\x80\x82\x02`\x02\x03\x02\x80\x82\x02`\x02\x03\x02\x80\x82\x02`\x02\x03\x02\x80\x91\x02`\x02\x03\x02\x93`\x01\x84\x84\x83\x03\x04\x94\x80_\x03\x04\x01\x92\x11\x90\x03\x02\x17\x02a'\x96V[c#\xD3Y\xA3`\xE0\x1B_R`\x04_\xFD[\x90a(\xE3a\x0C-V[\x82\x81R\x82Q\x80\x15a)>W_\x19\x81\x01\x90\x81\x11a\x0B\xACWa)\x05`\x80\x91\x85a\r4V[Q\x01Q` \x82\x01R_\x90\x81[\x84Q\x83\x10\x15a)4Wa),`\x01\x91`\xA0a\x13\xF4\x86\x89a\r4V[\x92\x01\x91a)\x11V[`@\x82\x01R\x92PPV[P\x91PV[\x80\x15a!\x95W\x1E`\xFF\x18`\x01\x01\x90V[\x80\x15\x80\x15a)\x95W[a)\x8EWa)la)r\x91a)\x9DV[\x91a)\x9DV[\x90\x81\x81\x11\x15a)\x85W\x90a\x1Bc\x91a\x0B\x9FV[a\x1Bc\x91a\x0B\x9FV[PP_\x19\x90V[P\x81\x15a)\\V[\x80\x15a)\xADW`\x01\x17\x1E`\xFF\x03\x90V[c\xAFE\x8C\x07`\xE0\x1B_R`\x04_\xFD\xFE\xA1dsolcC\0\x08\"\0\n",
    );
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**```solidity
struct Route { address[] path; uint8[] venues; uint24[] fees; uint256 amountOut; }
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Route {
        #[allow(missing_docs)]
        pub path: alloy::sol_types::private::Vec<alloy::sol_types::private::Address>,
        #[allow(missing_docs)]
        pub venues: alloy::sol_types::private::Vec<u8>,
        #[allow(missing_docs)]
        pub fees: alloy::sol_types::private::Vec<
            alloy::sol_types::private::primitives::aliases::U24,
        >,
        #[allow(missing_docs)]
        pub amountOut: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[doc(hidden)]
        #[allow(dead_code)]
        type UnderlyingSolTuple<'a> = (
            alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Address>,
            alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Uint<8>>,
            alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Uint<24>>,
            alloy::sol_types::sol_data::Uint<256>,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
            alloy::sol_types::private::Vec<alloy::sol_types::private::Address>,
            alloy::sol_types::private::Vec<u8>,
            alloy::sol_types::private::Vec<
                alloy::sol_types::private::primitives::aliases::U24,
            >,
            alloy::sol_types::private::primitives::aliases::U256,
        );
        #[cfg(test)]
        #[allow(dead_code, unreachable_patterns)]
        fn _type_assertion(
            _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
        ) {
            match _t {
                alloy_sol_types::private::AssertTypeEq::<
                    <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                >(_) => {}
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<Route> for UnderlyingRustTuple<'_> {
            fn from(value: Route) -> Self {
                (value.path, value.venues, value.fees, value.amountOut)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for Route {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    path: tuple.0,
                    venues: tuple.1,
                    fees: tuple.2,
                    amountOut: tuple.3,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolValue for Route {
            type SolType = Self;
        }
        #[automatically_derived]
        impl alloy_sol_types::private::SolTypeValue<Self> for Route {
            #[inline]
            fn stv_to_tokens(&self) -> <Self as alloy_sol_types::SolType>::Token<'_> {
                (
                    <alloy::sol_types::sol_data::Array<
                        alloy::sol_types::sol_data::Address,
                    > as alloy_sol_types::SolType>::tokenize(&self.path),
                    <alloy::sol_types::sol_data::Array<
                        alloy::sol_types::sol_data::Uint<8>,
                    > as alloy_sol_types::SolType>::tokenize(&self.venues),
                    <alloy::sol_types::sol_data::Array<
                        alloy::sol_types::sol_data::Uint<24>,
                    > as alloy_sol_types::SolType>::tokenize(&self.fees),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amountOut),
                )
            }
            #[inline]
            fn stv_abi_encoded_size(&self) -> usize {
                if let Some(size) = <Self as alloy_sol_types::SolType>::ENCODED_SIZE {
                    return size;
                }
                let tuple = <UnderlyingRustTuple<
                    '_,
                > as ::core::convert::From<Self>>::from(self.clone());
                <UnderlyingSolTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_encoded_size(&tuple)
            }
            #[inline]
            fn stv_eip712_data_word(&self) -> alloy_sol_types::Word {
                <Self as alloy_sol_types::SolStruct>::eip712_hash_struct(self)
            }
            #[inline]
            fn stv_abi_encode_packed_to(
                &self,
                out: &mut alloy_sol_types::private::Vec<u8>,
            ) {
                let tuple = <UnderlyingRustTuple<
                    '_,
                > as ::core::convert::From<Self>>::from(self.clone());
                <UnderlyingSolTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_encode_packed_to(&tuple, out)
            }
            #[inline]
            fn stv_abi_packed_encoded_size(&self) -> usize {
                if let Some(size) = <Self as alloy_sol_types::SolType>::PACKED_ENCODED_SIZE {
                    return size;
                }
                let tuple = <UnderlyingRustTuple<
                    '_,
                > as ::core::convert::From<Self>>::from(self.clone());
                <UnderlyingSolTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_packed_encoded_size(&tuple)
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolType for Route {
            type RustType = Self;
            type Token<'a> = <UnderlyingSolTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SOL_NAME: &'static str = <Self as alloy_sol_types::SolStruct>::NAME;
            const ENCODED_SIZE: Option<usize> = <UnderlyingSolTuple<
                '_,
            > as alloy_sol_types::SolType>::ENCODED_SIZE;
            const PACKED_ENCODED_SIZE: Option<usize> = <UnderlyingSolTuple<
                '_,
            > as alloy_sol_types::SolType>::PACKED_ENCODED_SIZE;
            #[inline]
            fn valid_token(token: &Self::Token<'_>) -> bool {
                <UnderlyingSolTuple<'_> as alloy_sol_types::SolType>::valid_token(token)
            }
            #[inline]
            fn detokenize(token: Self::Token<'_>) -> Self::RustType {
                let tuple = <UnderlyingSolTuple<
                    '_,
                > as alloy_sol_types::SolType>::detokenize(token);
                <Self as ::core::convert::From<UnderlyingRustTuple<'_>>>::from(tuple)
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolStruct for Route {
            const NAME: &'static str = "Route";
            #[inline]
            fn eip712_root_type() -> alloy_sol_types::private::Cow<'static, str> {
                alloy_sol_types::private::Cow::Borrowed(
                    "Route(address[] path,uint8[] venues,uint24[] fees,uint256 amountOut)",
                )
            }
            #[inline]
            fn eip712_components() -> alloy_sol_types::private::Vec<
                alloy_sol_types::private::Cow<'static, str>,
            > {
                alloy_sol_types::private::Vec::new()
            }
            #[inline]
            fn eip712_encode_type() -> alloy_sol_types::private::Cow<'static, str> {
                <Self as alloy_sol_types::SolStruct>::eip712_root_type()
            }
            #[inline]
            fn eip712_encode_data(&self) -> alloy_sol_types::private::Vec<u8> {
                [
                    <alloy::sol_types::sol_data::Array<
                        alloy::sol_types::sol_data::Address,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.path)
                        .0,
                    <alloy::sol_types::sol_data::Array<
                        alloy::sol_types::sol_data::Uint<8>,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.venues)
                        .0,
                    <alloy::sol_types::sol_data::Array<
                        alloy::sol_types::sol_data::Uint<24>,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.fees)
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.amountOut)
                        .0,
                ]
                    .concat()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::EventTopic for Route {
            #[inline]
            fn topic_preimage_length(rust: &Self::RustType) -> usize {
                0usize
                    + <alloy::sol_types::sol_data::Array<
                        alloy::sol_types::sol_data::Address,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(&rust.path)
                    + <alloy::sol_types::sol_data::Array<
                        alloy::sol_types::sol_data::Uint<8>,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.venues,
                    )
                    + <alloy::sol_types::sol_data::Array<
                        alloy::sol_types::sol_data::Uint<24>,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(&rust.fees)
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.amountOut,
                    )
            }
            #[inline]
            fn encode_topic_preimage(
                rust: &Self::RustType,
                out: &mut alloy_sol_types::private::Vec<u8>,
            ) {
                out.reserve(
                    <Self as alloy_sol_types::EventTopic>::topic_preimage_length(rust),
                );
                <alloy::sol_types::sol_data::Array<
                    alloy::sol_types::sol_data::Address,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.path,
                    out,
                );
                <alloy::sol_types::sol_data::Array<
                    alloy::sol_types::sol_data::Uint<8>,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.venues,
                    out,
                );
                <alloy::sol_types::sol_data::Array<
                    alloy::sol_types::sol_data::Uint<24>,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.fees,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.amountOut,
                    out,
                );
            }
            #[inline]
            fn encode_topic(
                rust: &Self::RustType,
            ) -> alloy_sol_types::abi::token::WordToken {
                let mut out = alloy_sol_types::private::Vec::new();
                <Self as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    rust,
                    &mut out,
                );
                alloy_sol_types::abi::token::WordToken(
                    alloy_sol_types::private::keccak256(out),
                )
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Custom error with signature `DivisionByZero()` and selector `0x23d359a3`.
```solidity
error DivisionByZero();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct DivisionByZero;
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[doc(hidden)]
        #[allow(dead_code)]
        type UnderlyingSolTuple<'a> = ();
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = ();
        #[cfg(test)]
        #[allow(dead_code, unreachable_patterns)]
        fn _type_assertion(
            _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
        ) {
            match _t {
                alloy_sol_types::private::AssertTypeEq::<
                    <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                >(_) => {}
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<DivisionByZero> for UnderlyingRustTuple<'_> {
            fn from(value: DivisionByZero) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for DivisionByZero {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for DivisionByZero {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "DivisionByZero()";
            const SELECTOR: [u8; 4] = [35u8, 211u8, 89u8, 163u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                ()
            }
            #[inline]
            fn abi_decode_raw_validate(data: &[u8]) -> alloy_sol_types::Result<Self> {
                <Self::Parameters<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence_validate(data)
                    .map(Self::new)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Custom error with signature `PathFinder__NoRoute()` and selector `0x05418711`.
```solidity
error PathFinder__NoRoute();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct PathFinder__NoRoute;
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[doc(hidden)]
        #[allow(dead_code)]
        type UnderlyingSolTuple<'a> = ();
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = ();
        #[cfg(test)]
        #[allow(dead_code, unreachable_patterns)]
        fn _type_assertion(
            _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
        ) {
            match _t {
                alloy_sol_types::private::AssertTypeEq::<
                    <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                >(_) => {}
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<PathFinder__NoRoute> for UnderlyingRustTuple<'_> {
            fn from(value: PathFinder__NoRoute) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for PathFinder__NoRoute {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for PathFinder__NoRoute {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "PathFinder__NoRoute()";
            const SELECTOR: [u8; 4] = [5u8, 65u8, 135u8, 17u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                ()
            }
            #[inline]
            fn abi_decode_raw_validate(data: &[u8]) -> alloy_sol_types::Result<Self> {
                <Self::Parameters<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence_validate(data)
                    .map(Self::new)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Custom error with signature `PathFinder__SameToken()` and selector `0x8303ee7e`.
```solidity
error PathFinder__SameToken();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct PathFinder__SameToken;
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[doc(hidden)]
        #[allow(dead_code)]
        type UnderlyingSolTuple<'a> = ();
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = ();
        #[cfg(test)]
        #[allow(dead_code, unreachable_patterns)]
        fn _type_assertion(
            _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
        ) {
            match _t {
                alloy_sol_types::private::AssertTypeEq::<
                    <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                >(_) => {}
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<PathFinder__SameToken> for UnderlyingRustTuple<'_> {
            fn from(value: PathFinder__SameToken) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for PathFinder__SameToken {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for PathFinder__SameToken {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "PathFinder__SameToken()";
            const SELECTOR: [u8; 4] = [131u8, 3u8, 238u8, 126u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                ()
            }
            #[inline]
            fn abi_decode_raw_validate(data: &[u8]) -> alloy_sol_types::Result<Self> {
                <Self::Parameters<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence_validate(data)
                    .map(Self::new)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Custom error with signature `PathFinder__SlippageOutOfRange()` and selector `0x2a8406b9`.
```solidity
error PathFinder__SlippageOutOfRange();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct PathFinder__SlippageOutOfRange;
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[doc(hidden)]
        #[allow(dead_code)]
        type UnderlyingSolTuple<'a> = ();
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = ();
        #[cfg(test)]
        #[allow(dead_code, unreachable_patterns)]
        fn _type_assertion(
            _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
        ) {
            match _t {
                alloy_sol_types::private::AssertTypeEq::<
                    <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                >(_) => {}
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<PathFinder__SlippageOutOfRange>
        for UnderlyingRustTuple<'_> {
            fn from(value: PathFinder__SlippageOutOfRange) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for PathFinder__SlippageOutOfRange {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for PathFinder__SlippageOutOfRange {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "PathFinder__SlippageOutOfRange()";
            const SELECTOR: [u8; 4] = [42u8, 132u8, 6u8, 185u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                ()
            }
            #[inline]
            fn abi_decode_raw_validate(data: &[u8]) -> alloy_sol_types::Result<Self> {
                <Self::Parameters<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence_validate(data)
                    .map(Self::new)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Custom error with signature `PathFinder__VenueNotImplemented(uint8)` and selector `0x580b6a6c`.
```solidity
error PathFinder__VenueNotImplemented(uint8 venue);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct PathFinder__VenueNotImplemented {
        #[allow(missing_docs)]
        pub venue: u8,
    }
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[doc(hidden)]
        #[allow(dead_code)]
        type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Uint<8>,);
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (u8,);
        #[cfg(test)]
        #[allow(dead_code, unreachable_patterns)]
        fn _type_assertion(
            _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
        ) {
            match _t {
                alloy_sol_types::private::AssertTypeEq::<
                    <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                >(_) => {}
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<PathFinder__VenueNotImplemented>
        for UnderlyingRustTuple<'_> {
            fn from(value: PathFinder__VenueNotImplemented) -> Self {
                (value.venue,)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for PathFinder__VenueNotImplemented {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self { venue: tuple.0 }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for PathFinder__VenueNotImplemented {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "PathFinder__VenueNotImplemented(uint8)";
            const SELECTOR: [u8; 4] = [88u8, 11u8, 106u8, 108u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                (
                    <alloy::sol_types::sol_data::Uint<
                        8,
                    > as alloy_sol_types::SolType>::tokenize(&self.venue),
                )
            }
            #[inline]
            fn abi_decode_raw_validate(data: &[u8]) -> alloy_sol_types::Result<Self> {
                <Self::Parameters<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence_validate(data)
                    .map(Self::new)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Custom error with signature `PathFinder__ZeroAmount()` and selector `0x857e4aa9`.
```solidity
error PathFinder__ZeroAmount();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct PathFinder__ZeroAmount;
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[doc(hidden)]
        #[allow(dead_code)]
        type UnderlyingSolTuple<'a> = ();
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = ();
        #[cfg(test)]
        #[allow(dead_code, unreachable_patterns)]
        fn _type_assertion(
            _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
        ) {
            match _t {
                alloy_sol_types::private::AssertTypeEq::<
                    <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                >(_) => {}
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<PathFinder__ZeroAmount> for UnderlyingRustTuple<'_> {
            fn from(value: PathFinder__ZeroAmount) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for PathFinder__ZeroAmount {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for PathFinder__ZeroAmount {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "PathFinder__ZeroAmount()";
            const SELECTOR: [u8; 4] = [133u8, 126u8, 74u8, 169u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                ()
            }
            #[inline]
            fn abi_decode_raw_validate(data: &[u8]) -> alloy_sol_types::Result<Self> {
                <Self::Parameters<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence_validate(data)
                    .map(Self::new)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Custom error with signature `ZeroInput()` and selector `0xaf458c07`.
```solidity
error ZeroInput();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ZeroInput;
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[doc(hidden)]
        #[allow(dead_code)]
        type UnderlyingSolTuple<'a> = ();
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = ();
        #[cfg(test)]
        #[allow(dead_code, unreachable_patterns)]
        fn _type_assertion(
            _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
        ) {
            match _t {
                alloy_sol_types::private::AssertTypeEq::<
                    <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                >(_) => {}
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<ZeroInput> for UnderlyingRustTuple<'_> {
            fn from(value: ZeroInput) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for ZeroInput {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for ZeroInput {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "ZeroInput()";
            const SELECTOR: [u8; 4] = [175u8, 69u8, 140u8, 7u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                ()
            }
            #[inline]
            fn abi_decode_raw_validate(data: &[u8]) -> alloy_sol_types::Result<Self> {
                <Self::Parameters<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence_validate(data)
                    .map(Self::new)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `findRoute(address,address,uint256,uint256)` and selector `0x21bf9f26`.
```solidity
function findRoute(address tokenIn, address tokenOut, uint256 amountIn, uint256 slippageBps) external returns (Route memory route);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct findRouteCall {
        #[allow(missing_docs)]
        pub tokenIn: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub tokenOut: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub amountIn: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub slippageBps: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`findRoute(address,address,uint256,uint256)`](findRouteCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct findRouteReturn {
        #[allow(missing_docs)]
        pub route: <Route as alloy::sol_types::SolType>::RustType,
    }
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Address,
                alloy::sol_types::private::Address,
                alloy::sol_types::private::primitives::aliases::U256,
                alloy::sol_types::private::primitives::aliases::U256,
            );
            #[cfg(test)]
            #[allow(dead_code, unreachable_patterns)]
            fn _type_assertion(
                _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
            ) {
                match _t {
                    alloy_sol_types::private::AssertTypeEq::<
                        <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                    >(_) => {}
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<findRouteCall> for UnderlyingRustTuple<'_> {
                fn from(value: findRouteCall) -> Self {
                    (value.tokenIn, value.tokenOut, value.amountIn, value.slippageBps)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for findRouteCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        tokenIn: tuple.0,
                        tokenOut: tuple.1,
                        amountIn: tuple.2,
                        slippageBps: tuple.3,
                    }
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (Route,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                <Route as alloy::sol_types::SolType>::RustType,
            );
            #[cfg(test)]
            #[allow(dead_code, unreachable_patterns)]
            fn _type_assertion(
                _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
            ) {
                match _t {
                    alloy_sol_types::private::AssertTypeEq::<
                        <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                    >(_) => {}
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<findRouteReturn> for UnderlyingRustTuple<'_> {
                fn from(value: findRouteReturn) -> Self {
                    (value.route,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for findRouteReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { route: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for findRouteCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = <Route as alloy::sol_types::SolType>::RustType;
            type ReturnTuple<'a> = (Route,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "findRoute(address,address,uint256,uint256)";
            const SELECTOR: [u8; 4] = [33u8, 191u8, 159u8, 38u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                (
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.tokenIn,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.tokenOut,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amountIn),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.slippageBps),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (<Route as alloy_sol_types::SolType>::tokenize(ret),)
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: findRouteReturn = r.into();
                        r.route
                    })
            }
            #[inline]
            fn abi_decode_returns_validate(
                data: &[u8],
            ) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence_validate(data)
                    .map(|r| {
                        let r: findRouteReturn = r.into();
                        r.route
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `findRouteWithHints(address,address,uint256,uint256,bytes)` and selector `0xc036c8ea`.
```solidity
function findRouteWithHints(address tokenIn, address tokenOut, uint256 amountIn, uint256 slippageBps, bytes memory extraData) external returns (Route memory route);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct findRouteWithHintsCall {
        #[allow(missing_docs)]
        pub tokenIn: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub tokenOut: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub amountIn: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub slippageBps: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub extraData: alloy::sol_types::private::Bytes,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`findRouteWithHints(address,address,uint256,uint256,bytes)`](findRouteWithHintsCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct findRouteWithHintsReturn {
        #[allow(missing_docs)]
        pub route: <Route as alloy::sol_types::SolType>::RustType,
    }
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Bytes,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Address,
                alloy::sol_types::private::Address,
                alloy::sol_types::private::primitives::aliases::U256,
                alloy::sol_types::private::primitives::aliases::U256,
                alloy::sol_types::private::Bytes,
            );
            #[cfg(test)]
            #[allow(dead_code, unreachable_patterns)]
            fn _type_assertion(
                _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
            ) {
                match _t {
                    alloy_sol_types::private::AssertTypeEq::<
                        <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                    >(_) => {}
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<findRouteWithHintsCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: findRouteWithHintsCall) -> Self {
                    (
                        value.tokenIn,
                        value.tokenOut,
                        value.amountIn,
                        value.slippageBps,
                        value.extraData,
                    )
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for findRouteWithHintsCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        tokenIn: tuple.0,
                        tokenOut: tuple.1,
                        amountIn: tuple.2,
                        slippageBps: tuple.3,
                        extraData: tuple.4,
                    }
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (Route,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                <Route as alloy::sol_types::SolType>::RustType,
            );
            #[cfg(test)]
            #[allow(dead_code, unreachable_patterns)]
            fn _type_assertion(
                _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
            ) {
                match _t {
                    alloy_sol_types::private::AssertTypeEq::<
                        <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                    >(_) => {}
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<findRouteWithHintsReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: findRouteWithHintsReturn) -> Self {
                    (value.route,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for findRouteWithHintsReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { route: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for findRouteWithHintsCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Bytes,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = <Route as alloy::sol_types::SolType>::RustType;
            type ReturnTuple<'a> = (Route,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "findRouteWithHints(address,address,uint256,uint256,bytes)";
            const SELECTOR: [u8; 4] = [192u8, 54u8, 200u8, 234u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                (
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.tokenIn,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.tokenOut,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amountIn),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.slippageBps),
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self.extraData,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (<Route as alloy_sol_types::SolType>::tokenize(ret),)
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: findRouteWithHintsReturn = r.into();
                        r.route
                    })
            }
            #[inline]
            fn abi_decode_returns_validate(
                data: &[u8],
            ) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence_validate(data)
                    .map(|r| {
                        let r: findRouteWithHintsReturn = r.into();
                        r.route
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive()]
    /**Function with signature `mergeRoutes(((bytes32,bytes32,bytes32,uint256,uint256,uint256,uint256)[],uint256,uint256)[],bytes32)` and selector `0x81c6ecd6`.
```solidity
function mergeRoutes(StepMerging.Route[] memory routes, bytes32 finalToken) external pure returns (StepMerging.Route[] memory optimised, StepMerging.MergedGroup[] memory groups);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct mergeRoutesCall {
        #[allow(missing_docs)]
        pub routes: alloy::sol_types::private::Vec<
            <StepMerging::Route as alloy::sol_types::SolType>::RustType,
        >,
        #[allow(missing_docs)]
        pub finalToken: alloy::sol_types::private::FixedBytes<32>,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive()]
    ///Container type for the return parameters of the [`mergeRoutes(((bytes32,bytes32,bytes32,uint256,uint256,uint256,uint256)[],uint256,uint256)[],bytes32)`](mergeRoutesCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct mergeRoutesReturn {
        #[allow(missing_docs)]
        pub optimised: alloy::sol_types::private::Vec<
            <StepMerging::Route as alloy::sol_types::SolType>::RustType,
        >,
        #[allow(missing_docs)]
        pub groups: alloy::sol_types::private::Vec<
            <StepMerging::MergedGroup as alloy::sol_types::SolType>::RustType,
        >,
    }
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (
                alloy::sol_types::sol_data::Array<StepMerging::Route>,
                alloy::sol_types::sol_data::FixedBytes<32>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Vec<
                    <StepMerging::Route as alloy::sol_types::SolType>::RustType,
                >,
                alloy::sol_types::private::FixedBytes<32>,
            );
            #[cfg(test)]
            #[allow(dead_code, unreachable_patterns)]
            fn _type_assertion(
                _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
            ) {
                match _t {
                    alloy_sol_types::private::AssertTypeEq::<
                        <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                    >(_) => {}
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<mergeRoutesCall> for UnderlyingRustTuple<'_> {
                fn from(value: mergeRoutesCall) -> Self {
                    (value.routes, value.finalToken)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for mergeRoutesCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        routes: tuple.0,
                        finalToken: tuple.1,
                    }
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (
                alloy::sol_types::sol_data::Array<StepMerging::Route>,
                alloy::sol_types::sol_data::Array<StepMerging::MergedGroup>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Vec<
                    <StepMerging::Route as alloy::sol_types::SolType>::RustType,
                >,
                alloy::sol_types::private::Vec<
                    <StepMerging::MergedGroup as alloy::sol_types::SolType>::RustType,
                >,
            );
            #[cfg(test)]
            #[allow(dead_code, unreachable_patterns)]
            fn _type_assertion(
                _t: alloy_sol_types::private::AssertTypeEq<UnderlyingRustTuple>,
            ) {
                match _t {
                    alloy_sol_types::private::AssertTypeEq::<
                        <UnderlyingSolTuple as alloy_sol_types::SolType>::RustType,
                    >(_) => {}
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<mergeRoutesReturn> for UnderlyingRustTuple<'_> {
                fn from(value: mergeRoutesReturn) -> Self {
                    (value.optimised, value.groups)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for mergeRoutesReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        optimised: tuple.0,
                        groups: tuple.1,
                    }
                }
            }
        }
        impl mergeRoutesReturn {
            fn _tokenize(
                &self,
            ) -> <mergeRoutesCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::Array<
                        StepMerging::Route,
                    > as alloy_sol_types::SolType>::tokenize(&self.optimised),
                    <alloy::sol_types::sol_data::Array<
                        StepMerging::MergedGroup,
                    > as alloy_sol_types::SolType>::tokenize(&self.groups),
                )
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for mergeRoutesCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Array<StepMerging::Route>,
                alloy::sol_types::sol_data::FixedBytes<32>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = mergeRoutesReturn;
            type ReturnTuple<'a> = (
                alloy::sol_types::sol_data::Array<StepMerging::Route>,
                alloy::sol_types::sol_data::Array<StepMerging::MergedGroup>,
            );
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "mergeRoutes(((bytes32,bytes32,bytes32,uint256,uint256,uint256,uint256)[],uint256,uint256)[],bytes32)";
            const SELECTOR: [u8; 4] = [129u8, 198u8, 236u8, 214u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                (
                    <alloy::sol_types::sol_data::Array<
                        StepMerging::Route,
                    > as alloy_sol_types::SolType>::tokenize(&self.routes),
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.finalToken),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                mergeRoutesReturn::_tokenize(ret)
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(Into::into)
            }
            #[inline]
            fn abi_decode_returns_validate(
                data: &[u8],
            ) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence_validate(data)
                    .map(Into::into)
            }
        }
    };
    ///Container for all the [`PathFinder`](self) function calls.
    #[derive(Clone)]
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive()]
    pub enum PathFinderCalls {
        #[allow(missing_docs)]
        findRoute(findRouteCall),
        #[allow(missing_docs)]
        findRouteWithHints(findRouteWithHintsCall),
        #[allow(missing_docs)]
        mergeRoutes(mergeRoutesCall),
    }
    impl PathFinderCalls {
        /// All the selectors of this enum.
        ///
        /// Note that the selectors might not be in the same order as the variants.
        /// No guarantees are made about the order of the selectors.
        ///
        /// Prefer using `SolInterface` methods instead.
        pub const SELECTORS: &'static [[u8; 4usize]] = &[
            [33u8, 191u8, 159u8, 38u8],
            [129u8, 198u8, 236u8, 214u8],
            [192u8, 54u8, 200u8, 234u8],
        ];
        /// The names of the variants in the same order as `SELECTORS`.
        pub const VARIANT_NAMES: &'static [&'static str] = &[
            ::core::stringify!(findRoute),
            ::core::stringify!(mergeRoutes),
            ::core::stringify!(findRouteWithHints),
        ];
        /// The signatures in the same order as `SELECTORS`.
        pub const SIGNATURES: &'static [&'static str] = &[
            <findRouteCall as alloy_sol_types::SolCall>::SIGNATURE,
            <mergeRoutesCall as alloy_sol_types::SolCall>::SIGNATURE,
            <findRouteWithHintsCall as alloy_sol_types::SolCall>::SIGNATURE,
        ];
        /// Returns the signature for the given selector, if known.
        #[inline]
        pub fn signature_by_selector(
            selector: [u8; 4usize],
        ) -> ::core::option::Option<&'static str> {
            match Self::SELECTORS.binary_search(&selector) {
                ::core::result::Result::Ok(idx) => {
                    ::core::option::Option::Some(Self::SIGNATURES[idx])
                }
                ::core::result::Result::Err(_) => ::core::option::Option::None,
            }
        }
        /// Returns the enum variant name for the given selector, if known.
        #[inline]
        pub fn name_by_selector(
            selector: [u8; 4usize],
        ) -> ::core::option::Option<&'static str> {
            let sig = Self::signature_by_selector(selector)?;
            sig.split_once('(').map(|(name, _)| name)
        }
    }
    #[automatically_derived]
    impl alloy_sol_types::SolInterface for PathFinderCalls {
        const NAME: &'static str = "PathFinderCalls";
        const MIN_DATA_LENGTH: usize = 96usize;
        const COUNT: usize = 3usize;
        #[inline]
        fn selector(&self) -> [u8; 4] {
            match self {
                Self::findRoute(_) => {
                    <findRouteCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::findRouteWithHints(_) => {
                    <findRouteWithHintsCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::mergeRoutes(_) => {
                    <mergeRoutesCall as alloy_sol_types::SolCall>::SELECTOR
                }
            }
        }
        #[inline]
        fn selector_at(i: usize) -> ::core::option::Option<[u8; 4]> {
            Self::SELECTORS.get(i).copied()
        }
        #[inline]
        fn valid_selector(selector: [u8; 4]) -> bool {
            Self::SELECTORS.binary_search(&selector).is_ok()
        }
        #[inline]
        #[allow(non_snake_case)]
        fn abi_decode_raw(
            selector: [u8; 4],
            data: &[u8],
        ) -> alloy_sol_types::Result<Self> {
            static DECODE_SHIMS: &[fn(
                &[u8],
            ) -> alloy_sol_types::Result<PathFinderCalls>] = &[
                {
                    fn findRoute(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderCalls> {
                        <findRouteCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(PathFinderCalls::findRoute)
                    }
                    findRoute
                },
                {
                    fn mergeRoutes(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderCalls> {
                        <mergeRoutesCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(PathFinderCalls::mergeRoutes)
                    }
                    mergeRoutes
                },
                {
                    fn findRouteWithHints(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderCalls> {
                        <findRouteWithHintsCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(PathFinderCalls::findRouteWithHints)
                    }
                    findRouteWithHints
                },
            ];
            let Ok(idx) = Self::SELECTORS.binary_search(&selector) else {
                return Err(
                    alloy_sol_types::Error::unknown_selector(
                        <Self as alloy_sol_types::SolInterface>::NAME,
                        selector,
                    ),
                );
            };
            DECODE_SHIMS[idx](data)
        }
        #[inline]
        #[allow(non_snake_case)]
        fn abi_decode_raw_validate(
            selector: [u8; 4],
            data: &[u8],
        ) -> alloy_sol_types::Result<Self> {
            static DECODE_VALIDATE_SHIMS: &[fn(
                &[u8],
            ) -> alloy_sol_types::Result<PathFinderCalls>] = &[
                {
                    fn findRoute(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderCalls> {
                        <findRouteCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(PathFinderCalls::findRoute)
                    }
                    findRoute
                },
                {
                    fn mergeRoutes(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderCalls> {
                        <mergeRoutesCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(PathFinderCalls::mergeRoutes)
                    }
                    mergeRoutes
                },
                {
                    fn findRouteWithHints(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderCalls> {
                        <findRouteWithHintsCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(PathFinderCalls::findRouteWithHints)
                    }
                    findRouteWithHints
                },
            ];
            let Ok(idx) = Self::SELECTORS.binary_search(&selector) else {
                return Err(
                    alloy_sol_types::Error::unknown_selector(
                        <Self as alloy_sol_types::SolInterface>::NAME,
                        selector,
                    ),
                );
            };
            DECODE_VALIDATE_SHIMS[idx](data)
        }
        #[inline]
        fn abi_encoded_size(&self) -> usize {
            match self {
                Self::findRoute(inner) => {
                    <findRouteCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::findRouteWithHints(inner) => {
                    <findRouteWithHintsCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::mergeRoutes(inner) => {
                    <mergeRoutesCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
            }
        }
        #[inline]
        fn abi_encode_raw(&self, out: &mut alloy_sol_types::private::Vec<u8>) {
            match self {
                Self::findRoute(inner) => {
                    <findRouteCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::findRouteWithHints(inner) => {
                    <findRouteWithHintsCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::mergeRoutes(inner) => {
                    <mergeRoutesCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
            }
        }
    }
    ///Container for all the [`PathFinder`](self) custom errors.
    #[derive(Clone)]
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub enum PathFinderErrors {
        #[allow(missing_docs)]
        DivisionByZero(DivisionByZero),
        #[allow(missing_docs)]
        PathFinder__NoRoute(PathFinder__NoRoute),
        #[allow(missing_docs)]
        PathFinder__SameToken(PathFinder__SameToken),
        #[allow(missing_docs)]
        PathFinder__SlippageOutOfRange(PathFinder__SlippageOutOfRange),
        #[allow(missing_docs)]
        PathFinder__VenueNotImplemented(PathFinder__VenueNotImplemented),
        #[allow(missing_docs)]
        PathFinder__ZeroAmount(PathFinder__ZeroAmount),
        #[allow(missing_docs)]
        ZeroInput(ZeroInput),
    }
    impl PathFinderErrors {
        /// All the selectors of this enum.
        ///
        /// Note that the selectors might not be in the same order as the variants.
        /// No guarantees are made about the order of the selectors.
        ///
        /// Prefer using `SolInterface` methods instead.
        pub const SELECTORS: &'static [[u8; 4usize]] = &[
            [5u8, 65u8, 135u8, 17u8],
            [35u8, 211u8, 89u8, 163u8],
            [42u8, 132u8, 6u8, 185u8],
            [88u8, 11u8, 106u8, 108u8],
            [131u8, 3u8, 238u8, 126u8],
            [133u8, 126u8, 74u8, 169u8],
            [175u8, 69u8, 140u8, 7u8],
        ];
        /// The names of the variants in the same order as `SELECTORS`.
        pub const VARIANT_NAMES: &'static [&'static str] = &[
            ::core::stringify!(PathFinder__NoRoute),
            ::core::stringify!(DivisionByZero),
            ::core::stringify!(PathFinder__SlippageOutOfRange),
            ::core::stringify!(PathFinder__VenueNotImplemented),
            ::core::stringify!(PathFinder__SameToken),
            ::core::stringify!(PathFinder__ZeroAmount),
            ::core::stringify!(ZeroInput),
        ];
        /// The signatures in the same order as `SELECTORS`.
        pub const SIGNATURES: &'static [&'static str] = &[
            <PathFinder__NoRoute as alloy_sol_types::SolError>::SIGNATURE,
            <DivisionByZero as alloy_sol_types::SolError>::SIGNATURE,
            <PathFinder__SlippageOutOfRange as alloy_sol_types::SolError>::SIGNATURE,
            <PathFinder__VenueNotImplemented as alloy_sol_types::SolError>::SIGNATURE,
            <PathFinder__SameToken as alloy_sol_types::SolError>::SIGNATURE,
            <PathFinder__ZeroAmount as alloy_sol_types::SolError>::SIGNATURE,
            <ZeroInput as alloy_sol_types::SolError>::SIGNATURE,
        ];
        /// Returns the signature for the given selector, if known.
        #[inline]
        pub fn signature_by_selector(
            selector: [u8; 4usize],
        ) -> ::core::option::Option<&'static str> {
            match Self::SELECTORS.binary_search(&selector) {
                ::core::result::Result::Ok(idx) => {
                    ::core::option::Option::Some(Self::SIGNATURES[idx])
                }
                ::core::result::Result::Err(_) => ::core::option::Option::None,
            }
        }
        /// Returns the enum variant name for the given selector, if known.
        #[inline]
        pub fn name_by_selector(
            selector: [u8; 4usize],
        ) -> ::core::option::Option<&'static str> {
            let sig = Self::signature_by_selector(selector)?;
            sig.split_once('(').map(|(name, _)| name)
        }
    }
    #[automatically_derived]
    impl alloy_sol_types::SolInterface for PathFinderErrors {
        const NAME: &'static str = "PathFinderErrors";
        const MIN_DATA_LENGTH: usize = 0usize;
        const COUNT: usize = 7usize;
        #[inline]
        fn selector(&self) -> [u8; 4] {
            match self {
                Self::DivisionByZero(_) => {
                    <DivisionByZero as alloy_sol_types::SolError>::SELECTOR
                }
                Self::PathFinder__NoRoute(_) => {
                    <PathFinder__NoRoute as alloy_sol_types::SolError>::SELECTOR
                }
                Self::PathFinder__SameToken(_) => {
                    <PathFinder__SameToken as alloy_sol_types::SolError>::SELECTOR
                }
                Self::PathFinder__SlippageOutOfRange(_) => {
                    <PathFinder__SlippageOutOfRange as alloy_sol_types::SolError>::SELECTOR
                }
                Self::PathFinder__VenueNotImplemented(_) => {
                    <PathFinder__VenueNotImplemented as alloy_sol_types::SolError>::SELECTOR
                }
                Self::PathFinder__ZeroAmount(_) => {
                    <PathFinder__ZeroAmount as alloy_sol_types::SolError>::SELECTOR
                }
                Self::ZeroInput(_) => <ZeroInput as alloy_sol_types::SolError>::SELECTOR,
            }
        }
        #[inline]
        fn selector_at(i: usize) -> ::core::option::Option<[u8; 4]> {
            Self::SELECTORS.get(i).copied()
        }
        #[inline]
        fn valid_selector(selector: [u8; 4]) -> bool {
            Self::SELECTORS.binary_search(&selector).is_ok()
        }
        #[inline]
        #[allow(non_snake_case)]
        fn abi_decode_raw(
            selector: [u8; 4],
            data: &[u8],
        ) -> alloy_sol_types::Result<Self> {
            static DECODE_SHIMS: &[fn(
                &[u8],
            ) -> alloy_sol_types::Result<PathFinderErrors>] = &[
                {
                    fn PathFinder__NoRoute(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderErrors> {
                        <PathFinder__NoRoute as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(PathFinderErrors::PathFinder__NoRoute)
                    }
                    PathFinder__NoRoute
                },
                {
                    fn DivisionByZero(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderErrors> {
                        <DivisionByZero as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(PathFinderErrors::DivisionByZero)
                    }
                    DivisionByZero
                },
                {
                    fn PathFinder__SlippageOutOfRange(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderErrors> {
                        <PathFinder__SlippageOutOfRange as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(PathFinderErrors::PathFinder__SlippageOutOfRange)
                    }
                    PathFinder__SlippageOutOfRange
                },
                {
                    fn PathFinder__VenueNotImplemented(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderErrors> {
                        <PathFinder__VenueNotImplemented as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(PathFinderErrors::PathFinder__VenueNotImplemented)
                    }
                    PathFinder__VenueNotImplemented
                },
                {
                    fn PathFinder__SameToken(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderErrors> {
                        <PathFinder__SameToken as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(PathFinderErrors::PathFinder__SameToken)
                    }
                    PathFinder__SameToken
                },
                {
                    fn PathFinder__ZeroAmount(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderErrors> {
                        <PathFinder__ZeroAmount as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(PathFinderErrors::PathFinder__ZeroAmount)
                    }
                    PathFinder__ZeroAmount
                },
                {
                    fn ZeroInput(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderErrors> {
                        <ZeroInput as alloy_sol_types::SolError>::abi_decode_raw(data)
                            .map(PathFinderErrors::ZeroInput)
                    }
                    ZeroInput
                },
            ];
            let Ok(idx) = Self::SELECTORS.binary_search(&selector) else {
                return Err(
                    alloy_sol_types::Error::unknown_selector(
                        <Self as alloy_sol_types::SolInterface>::NAME,
                        selector,
                    ),
                );
            };
            DECODE_SHIMS[idx](data)
        }
        #[inline]
        #[allow(non_snake_case)]
        fn abi_decode_raw_validate(
            selector: [u8; 4],
            data: &[u8],
        ) -> alloy_sol_types::Result<Self> {
            static DECODE_VALIDATE_SHIMS: &[fn(
                &[u8],
            ) -> alloy_sol_types::Result<PathFinderErrors>] = &[
                {
                    fn PathFinder__NoRoute(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderErrors> {
                        <PathFinder__NoRoute as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(PathFinderErrors::PathFinder__NoRoute)
                    }
                    PathFinder__NoRoute
                },
                {
                    fn DivisionByZero(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderErrors> {
                        <DivisionByZero as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(PathFinderErrors::DivisionByZero)
                    }
                    DivisionByZero
                },
                {
                    fn PathFinder__SlippageOutOfRange(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderErrors> {
                        <PathFinder__SlippageOutOfRange as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(PathFinderErrors::PathFinder__SlippageOutOfRange)
                    }
                    PathFinder__SlippageOutOfRange
                },
                {
                    fn PathFinder__VenueNotImplemented(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderErrors> {
                        <PathFinder__VenueNotImplemented as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(PathFinderErrors::PathFinder__VenueNotImplemented)
                    }
                    PathFinder__VenueNotImplemented
                },
                {
                    fn PathFinder__SameToken(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderErrors> {
                        <PathFinder__SameToken as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(PathFinderErrors::PathFinder__SameToken)
                    }
                    PathFinder__SameToken
                },
                {
                    fn PathFinder__ZeroAmount(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderErrors> {
                        <PathFinder__ZeroAmount as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(PathFinderErrors::PathFinder__ZeroAmount)
                    }
                    PathFinder__ZeroAmount
                },
                {
                    fn ZeroInput(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<PathFinderErrors> {
                        <ZeroInput as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(PathFinderErrors::ZeroInput)
                    }
                    ZeroInput
                },
            ];
            let Ok(idx) = Self::SELECTORS.binary_search(&selector) else {
                return Err(
                    alloy_sol_types::Error::unknown_selector(
                        <Self as alloy_sol_types::SolInterface>::NAME,
                        selector,
                    ),
                );
            };
            DECODE_VALIDATE_SHIMS[idx](data)
        }
        #[inline]
        fn abi_encoded_size(&self) -> usize {
            match self {
                Self::DivisionByZero(inner) => {
                    <DivisionByZero as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::PathFinder__NoRoute(inner) => {
                    <PathFinder__NoRoute as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::PathFinder__SameToken(inner) => {
                    <PathFinder__SameToken as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::PathFinder__SlippageOutOfRange(inner) => {
                    <PathFinder__SlippageOutOfRange as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::PathFinder__VenueNotImplemented(inner) => {
                    <PathFinder__VenueNotImplemented as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::PathFinder__ZeroAmount(inner) => {
                    <PathFinder__ZeroAmount as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::ZeroInput(inner) => {
                    <ZeroInput as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
            }
        }
        #[inline]
        fn abi_encode_raw(&self, out: &mut alloy_sol_types::private::Vec<u8>) {
            match self {
                Self::DivisionByZero(inner) => {
                    <DivisionByZero as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::PathFinder__NoRoute(inner) => {
                    <PathFinder__NoRoute as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::PathFinder__SameToken(inner) => {
                    <PathFinder__SameToken as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::PathFinder__SlippageOutOfRange(inner) => {
                    <PathFinder__SlippageOutOfRange as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::PathFinder__VenueNotImplemented(inner) => {
                    <PathFinder__VenueNotImplemented as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::PathFinder__ZeroAmount(inner) => {
                    <PathFinder__ZeroAmount as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::ZeroInput(inner) => {
                    <ZeroInput as alloy_sol_types::SolError>::abi_encode_raw(inner, out)
                }
            }
        }
    }
    #[automatically_derived]
    impl PathFinderErrors {
        /**Creates a [`DivisionByZero`] error.

```solidity
error DivisionByZero()
```*/
        #[inline]
        pub fn division_by_zero() -> Self {
            Self::DivisionByZero(DivisionByZero)
        }
        /**Creates a [`PathFinder__NoRoute`] error.

```solidity
error PathFinder__NoRoute()
```*/
        #[inline]
        pub fn path_finder_no_route() -> Self {
            Self::PathFinder__NoRoute(PathFinder__NoRoute)
        }
        /**Creates a [`PathFinder__SameToken`] error.

```solidity
error PathFinder__SameToken()
```*/
        #[inline]
        pub fn path_finder_same_token() -> Self {
            Self::PathFinder__SameToken(PathFinder__SameToken)
        }
        /**Creates a [`PathFinder__SlippageOutOfRange`] error.

```solidity
error PathFinder__SlippageOutOfRange()
```*/
        #[inline]
        pub fn path_finder_slippage_out_of_range() -> Self {
            Self::PathFinder__SlippageOutOfRange(PathFinder__SlippageOutOfRange)
        }
        /**Creates a [`PathFinder__VenueNotImplemented`] error.

```solidity
error PathFinder__VenueNotImplemented(uint8)
```*/
        #[inline]
        pub fn path_finder_venue_not_implemented(venue: u8) -> Self {
            Self::PathFinder__VenueNotImplemented(PathFinder__VenueNotImplemented {
                venue: venue,
            })
        }
        /**Creates a [`PathFinder__ZeroAmount`] error.

```solidity
error PathFinder__ZeroAmount()
```*/
        #[inline]
        pub fn path_finder_zero_amount() -> Self {
            Self::PathFinder__ZeroAmount(PathFinder__ZeroAmount)
        }
        /**Creates a [`ZeroInput`] error.

```solidity
error ZeroInput()
```*/
        #[inline]
        pub fn zero_input() -> Self {
            Self::ZeroInput(ZeroInput)
        }
    }
    use alloy::contract as alloy_contract;
    /**Creates a new wrapper around an on-chain [`PathFinder`](self) contract instance.

See the [wrapper's documentation](`PathFinderInstance`) for more details.*/
    #[inline]
    pub const fn new<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    >(
        address: alloy_sol_types::private::Address,
        __provider: P,
    ) -> PathFinderInstance<P, N> {
        PathFinderInstance::<P, N>::new(address, __provider)
    }
    /**Deploys this contract using the given `provider` and constructor arguments, if any.

Returns a new instance of the contract, if the deployment was successful.

For more fine-grained control over the deployment process, use [`deploy_builder`] instead.*/
    #[inline]
    pub fn deploy<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    >(
        __provider: P,
    ) -> impl ::core::future::Future<
        Output = alloy_contract::Result<PathFinderInstance<P, N>>,
    > {
        PathFinderInstance::<P, N>::deploy(__provider)
    }
    /**Creates a `RawCallBuilder` for deploying this contract using the given `provider`
and constructor arguments, if any.

This is a simple wrapper around creating a `RawCallBuilder` with the data set to
the bytecode concatenated with the constructor's ABI-encoded arguments.*/
    #[inline]
    pub fn deploy_builder<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    >(__provider: P) -> alloy_contract::RawCallBuilder<P, N> {
        PathFinderInstance::<P, N>::deploy_builder(__provider)
    }
    /**A [`PathFinder`](self) instance.

Contains type-safe methods for interacting with an on-chain instance of the
[`PathFinder`](self) contract located at a given `address`, using a given
provider `P`.

If the contract bytecode is available (see the [`sol!`](alloy_sol_types::sol!)
documentation on how to provide it), the `deploy` and `deploy_builder` methods can
be used to deploy a new instance of the contract.

See the [module-level documentation](self) for all the available methods.*/
    #[derive(Clone)]
    pub struct PathFinderInstance<P, N = alloy_contract::private::Ethereum> {
        address: alloy_sol_types::private::Address,
        provider: P,
        _network: ::core::marker::PhantomData<N>,
    }
    #[automatically_derived]
    impl<P, N> ::core::fmt::Debug for PathFinderInstance<P, N> {
        #[inline]
        fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
            f.debug_tuple("PathFinderInstance").field(&self.address).finish()
        }
    }
    /// Instantiation and getters/setters.
    impl<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    > PathFinderInstance<P, N> {
        /**Creates a new wrapper around an on-chain [`PathFinder`](self) contract instance.

See the [wrapper's documentation](`PathFinderInstance`) for more details.*/
        #[inline]
        pub const fn new(
            address: alloy_sol_types::private::Address,
            __provider: P,
        ) -> Self {
            Self {
                address,
                provider: __provider,
                _network: ::core::marker::PhantomData,
            }
        }
        /**Deploys this contract using the given `provider` and constructor arguments, if any.

Returns a new instance of the contract, if the deployment was successful.

For more fine-grained control over the deployment process, use [`deploy_builder`] instead.*/
        #[inline]
        pub async fn deploy(
            __provider: P,
        ) -> alloy_contract::Result<PathFinderInstance<P, N>> {
            let call_builder = Self::deploy_builder(__provider);
            let contract_address = call_builder.deploy().await?;
            Ok(Self::new(contract_address, call_builder.provider))
        }
        /**Creates a `RawCallBuilder` for deploying this contract using the given `provider`
and constructor arguments, if any.

This is a simple wrapper around creating a `RawCallBuilder` with the data set to
the bytecode concatenated with the constructor's ABI-encoded arguments.*/
        #[inline]
        pub fn deploy_builder(__provider: P) -> alloy_contract::RawCallBuilder<P, N> {
            alloy_contract::RawCallBuilder::new_raw_deploy(
                __provider,
                ::core::clone::Clone::clone(&BYTECODE),
            )
        }
        /// Returns a reference to the address.
        #[inline]
        pub const fn address(&self) -> &alloy_sol_types::private::Address {
            &self.address
        }
        /// Sets the address.
        #[inline]
        pub fn set_address(&mut self, address: alloy_sol_types::private::Address) {
            self.address = address;
        }
        /// Sets the address and returns `self`.
        pub fn at(mut self, address: alloy_sol_types::private::Address) -> Self {
            self.set_address(address);
            self
        }
        /// Returns a reference to the provider.
        #[inline]
        pub const fn provider(&self) -> &P {
            &self.provider
        }
    }
    impl<P: ::core::clone::Clone, N> PathFinderInstance<&P, N> {
        /// Clones the provider and returns a new instance with the cloned provider.
        #[inline]
        pub fn with_cloned_provider(self) -> PathFinderInstance<P, N> {
            PathFinderInstance {
                address: self.address,
                provider: ::core::clone::Clone::clone(&self.provider),
                _network: ::core::marker::PhantomData,
            }
        }
    }
    /// Function calls.
    impl<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    > PathFinderInstance<P, N> {
        /// Creates a new call builder using this contract instance's provider and address.
        ///
        /// Note that the call can be any function call, not just those defined in this
        /// contract. Prefer using the other methods for building type-safe contract calls.
        pub fn call_builder<C: alloy_sol_types::SolCall>(
            &self,
            call: &C,
        ) -> alloy_contract::SolCallBuilder<&P, C, N> {
            alloy_contract::SolCallBuilder::new_sol(&self.provider, &self.address, call)
        }
        ///Creates a new call builder for the [`findRoute`] function.
        pub fn findRoute(
            &self,
            tokenIn: alloy::sol_types::private::Address,
            tokenOut: alloy::sol_types::private::Address,
            amountIn: alloy::sol_types::private::primitives::aliases::U256,
            slippageBps: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, findRouteCall, N> {
            self.call_builder(
                &findRouteCall {
                    tokenIn,
                    tokenOut,
                    amountIn,
                    slippageBps,
                },
            )
        }
        ///Creates a new call builder for the [`findRouteWithHints`] function.
        pub fn findRouteWithHints(
            &self,
            tokenIn: alloy::sol_types::private::Address,
            tokenOut: alloy::sol_types::private::Address,
            amountIn: alloy::sol_types::private::primitives::aliases::U256,
            slippageBps: alloy::sol_types::private::primitives::aliases::U256,
            extraData: alloy::sol_types::private::Bytes,
        ) -> alloy_contract::SolCallBuilder<&P, findRouteWithHintsCall, N> {
            self.call_builder(
                &findRouteWithHintsCall {
                    tokenIn,
                    tokenOut,
                    amountIn,
                    slippageBps,
                    extraData,
                },
            )
        }
        ///Creates a new call builder for the [`mergeRoutes`] function.
        pub fn mergeRoutes(
            &self,
            routes: alloy::sol_types::private::Vec<
                <StepMerging::Route as alloy::sol_types::SolType>::RustType,
            >,
            finalToken: alloy::sol_types::private::FixedBytes<32>,
        ) -> alloy_contract::SolCallBuilder<&P, mergeRoutesCall, N> {
            self.call_builder(
                &mergeRoutesCall {
                    routes,
                    finalToken,
                },
            )
        }
    }
    /// Event filters.
    impl<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    > PathFinderInstance<P, N> {
        /// Creates a new event filter using this contract instance's provider and address.
        ///
        /// Note that the type can be any event, not just those defined in this contract.
        /// Prefer using the other methods for building type-safe event filters.
        pub fn event_filter<E: alloy_sol_types::SolEvent>(
            &self,
        ) -> alloy_contract::Event<&P, E, N> {
            alloy_contract::Event::new_sol(&self.provider, &self.address)
        }
    }
}
