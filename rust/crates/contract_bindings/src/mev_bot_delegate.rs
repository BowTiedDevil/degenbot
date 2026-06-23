///Module containing a contract's types and functions.
/**

```solidity
library BaseAccount {
    struct Call { address target; uint256 value; bytes data; }
}
```*/
#[allow(
    non_camel_case_types,
    non_snake_case,
    clippy::pub_underscore_fields,
    clippy::style,
    clippy::empty_structs_with_brackets
)]
pub mod BaseAccount {
    use super::*;
    use alloy::sol_types as alloy_sol_types;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**```solidity
struct Call { address target; uint256 value; bytes data; }
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Call {
        #[allow(missing_docs)]
        pub target: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub value: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub data: alloy::sol_types::private::Bytes,
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
            alloy::sol_types::sol_data::Address,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Bytes,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
            alloy::sol_types::private::Address,
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
        impl ::core::convert::From<Call> for UnderlyingRustTuple<'_> {
            fn from(value: Call) -> Self {
                (value.target, value.value, value.data)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for Call {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    target: tuple.0,
                    value: tuple.1,
                    data: tuple.2,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolValue for Call {
            type SolType = Self;
        }
        #[automatically_derived]
        impl alloy_sol_types::private::SolTypeValue<Self> for Call {
            #[inline]
            fn stv_to_tokens(&self) -> <Self as alloy_sol_types::SolType>::Token<'_> {
                (
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.target,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.value),
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self.data,
                    ),
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
        impl alloy_sol_types::SolType for Call {
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
        impl alloy_sol_types::SolStruct for Call {
            const NAME: &'static str = "Call";
            #[inline]
            fn eip712_root_type() -> alloy_sol_types::private::Cow<'static, str> {
                alloy_sol_types::private::Cow::Borrowed(
                    "Call(address target,uint256 value,bytes data)",
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
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::eip712_data_word(
                            &self.target,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.value)
                        .0,
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::eip712_data_word(
                            &self.data,
                        )
                        .0,
                ]
                    .concat()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::EventTopic for Call {
            #[inline]
            fn topic_preimage_length(rust: &Self::RustType) -> usize {
                0usize
                    + <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.target,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(&rust.value)
                    + <alloy::sol_types::sol_data::Bytes as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.data,
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
                <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.target,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.value,
                    out,
                );
                <alloy::sol_types::sol_data::Bytes as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.data,
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
    /**Creates a new wrapper around an on-chain [`BaseAccount`](self) contract instance.

See the [wrapper's documentation](`BaseAccountInstance`) for more details.*/
    #[inline]
    pub const fn new<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    >(
        address: alloy_sol_types::private::Address,
        __provider: P,
    ) -> BaseAccountInstance<P, N> {
        BaseAccountInstance::<P, N>::new(address, __provider)
    }
    /**A [`BaseAccount`](self) instance.

Contains type-safe methods for interacting with an on-chain instance of the
[`BaseAccount`](self) contract located at a given `address`, using a given
provider `P`.

If the contract bytecode is available (see the [`sol!`](alloy_sol_types::sol!)
documentation on how to provide it), the `deploy` and `deploy_builder` methods can
be used to deploy a new instance of the contract.

See the [module-level documentation](self) for all the available methods.*/
    #[derive(Clone)]
    pub struct BaseAccountInstance<P, N = alloy_contract::private::Ethereum> {
        address: alloy_sol_types::private::Address,
        provider: P,
        _network: ::core::marker::PhantomData<N>,
    }
    #[automatically_derived]
    impl<P, N> ::core::fmt::Debug for BaseAccountInstance<P, N> {
        #[inline]
        fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
            f.debug_tuple("BaseAccountInstance").field(&self.address).finish()
        }
    }
    /// Instantiation and getters/setters.
    impl<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    > BaseAccountInstance<P, N> {
        /**Creates a new wrapper around an on-chain [`BaseAccount`](self) contract instance.

See the [wrapper's documentation](`BaseAccountInstance`) for more details.*/
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
    impl<P: ::core::clone::Clone, N> BaseAccountInstance<&P, N> {
        /// Clones the provider and returns a new instance with the cloned provider.
        #[inline]
        pub fn with_cloned_provider(self) -> BaseAccountInstance<P, N> {
            BaseAccountInstance {
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
    > BaseAccountInstance<P, N> {
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
    > BaseAccountInstance<P, N> {
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
library BaseAccount {
    struct Call {
        address target;
        uint256 value;
        bytes data;
    }
}

interface MevBotDelegate {
    type Erc6909Op is uint8;
    struct Erc6909Call {
        Erc6909Op op;
        address token;
        address counterparty;
        uint256 id;
        uint256 amount;
        bool approved;
    }
    struct PackedUserOperation {
        address sender;
        uint256 nonce;
        bytes initCode;
        bytes callData;
        bytes32 accountGasLimits;
        uint256 preVerificationGas;
        bytes32 gasFees;
        bytes paymasterAndData;
        bytes signature;
    }

    error Erc6909Op_SetOperatorRequiresZeroIdAndAmount();
    error Erc6909Op_TransferRequiresZeroApproved();
    error Erc6909SetOperatorBlocked();
    error ExecuteError(uint256 index, bytes error);
    error InvalidParams();
    error MevBotDelegate__Erc6909SetOperatorFailed();
    error MevBotDelegate__Erc6909TransferFailed();
    error MevBotDelegate__InvalidSignatureLength();
    error MevBotDelegate__InvalidValidator(address validator);
    error MevBotDelegate__Unauthorized();
    error MevBotDelegate__ValidatorNotEnabled(address validator);
    error MevBotDelegate__ValidatorReverted(address validator);
    error MevBotDelegate__ZeroTarget();
    error NativeSweepFailed();
    error NotFromEntryPoint(address msgSender, address entity, address entryPoint);

    event BotDelegateActivated(address indexed target, uint256 value, bytes4 selector);
    event Erc20Swept(address indexed token, address indexed to, uint256 amount);
    event Erc3009Received(address indexed token, address indexed from, uint256 value, bytes32 nonce);
    event Erc3009Relayed(address indexed token, address indexed from, address indexed to, uint256 value, bytes32 nonce);
    event Erc6909OperatorSet(address indexed token, address indexed operator, bool approved);
    event Erc6909Transferred(address indexed token, address indexed to, uint256 indexed id, uint256 amount);
    event ValidatorAdded(address indexed validator);
    event ValidatorRemoved(address indexed validator);

    fallback() external payable;

    receive() external payable;

    function ENTRY_POINT_ADDR() external view returns (address);
    function ENTRY_POINT_V06() external view returns (address);
    function ENTRY_POINT_V07() external view returns (address);
    function ENTRY_POINT_V08() external view returns (address);
    function entryPoint() external pure returns (address);
    function execute(address target, uint256 value, bytes memory data) external;
    function executeBatch(BaseAccount.Call[] memory calls) external;
    function executeErc6909Batch(Erc6909Call[] memory calls) external;
    function getNonce() external view returns (uint256);
    function isEntryPoint(address caller) external pure returns (bool);
    function isValidSignature(bytes32 hash, bytes memory signature) external view returns (bytes4 magicValue);
    function isValidatorEnabled(address validator) external view returns (bool enabled);
    function nonceFor(address ep, uint192 key) external view returns (uint256);
    function onERC1155BatchReceived(address, address, uint256[] memory, uint256[] memory, bytes memory) external returns (bytes4);
    function onERC1155Received(address, address, uint256, uint256, bytes memory) external returns (bytes4);
    function onERC721Received(address, address, uint256, bytes memory) external returns (bytes4);
    function receiveErc3009(address token, address from, uint256 value, uint256 validAfter, uint256 validBefore, bytes32 nonce, uint8 v, bytes32 r, bytes32 s) external;
    function setValidator(address validator, bool enabled) external;
    function supportsInterface(bytes4 id) external pure returns (bool);
    function sweepERC20(address token, address to, uint256 amount) external;
    function sweepERC20Batch(address[] memory tokens, address to, uint256[] memory amounts) external;
    function transferErc3009(address token, address from, address to, uint256 value, uint256 validAfter, uint256 validBefore, bytes32 nonce, uint8 v, bytes32 r, bytes32 s) external;
    function validateUserOp(PackedUserOperation memory userOp, bytes32 userOpHash, uint256 missingAccountFunds) external returns (uint256 validationData);
}
```

...which was generated by the following JSON ABI:
```json
[
  {
    "type": "fallback",
    "stateMutability": "payable"
  },
  {
    "type": "receive",
    "stateMutability": "payable"
  },
  {
    "type": "function",
    "name": "ENTRY_POINT_ADDR",
    "inputs": [],
    "outputs": [
      {
        "name": "",
        "type": "address",
        "internalType": "address"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "ENTRY_POINT_V06",
    "inputs": [],
    "outputs": [
      {
        "name": "",
        "type": "address",
        "internalType": "address"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "ENTRY_POINT_V07",
    "inputs": [],
    "outputs": [
      {
        "name": "",
        "type": "address",
        "internalType": "address"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "ENTRY_POINT_V08",
    "inputs": [],
    "outputs": [
      {
        "name": "",
        "type": "address",
        "internalType": "address"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "entryPoint",
    "inputs": [],
    "outputs": [
      {
        "name": "",
        "type": "address",
        "internalType": "contract IEntryPoint"
      }
    ],
    "stateMutability": "pure"
  },
  {
    "type": "function",
    "name": "execute",
    "inputs": [
      {
        "name": "target",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "value",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "data",
        "type": "bytes",
        "internalType": "bytes"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "executeBatch",
    "inputs": [
      {
        "name": "calls",
        "type": "tuple[]",
        "internalType": "struct BaseAccount.Call[]",
        "components": [
          {
            "name": "target",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "value",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "data",
            "type": "bytes",
            "internalType": "bytes"
          }
        ]
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "executeErc6909Batch",
    "inputs": [
      {
        "name": "calls",
        "type": "tuple[]",
        "internalType": "struct Erc6909Call[]",
        "components": [
          {
            "name": "op",
            "type": "uint8",
            "internalType": "enum Erc6909Op"
          },
          {
            "name": "token",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "counterparty",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "id",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "amount",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "approved",
            "type": "bool",
            "internalType": "bool"
          }
        ]
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "getNonce",
    "inputs": [],
    "outputs": [
      {
        "name": "",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "isEntryPoint",
    "inputs": [
      {
        "name": "caller",
        "type": "address",
        "internalType": "address"
      }
    ],
    "outputs": [
      {
        "name": "",
        "type": "bool",
        "internalType": "bool"
      }
    ],
    "stateMutability": "pure"
  },
  {
    "type": "function",
    "name": "isValidSignature",
    "inputs": [
      {
        "name": "hash",
        "type": "bytes32",
        "internalType": "bytes32"
      },
      {
        "name": "signature",
        "type": "bytes",
        "internalType": "bytes"
      }
    ],
    "outputs": [
      {
        "name": "magicValue",
        "type": "bytes4",
        "internalType": "bytes4"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "isValidatorEnabled",
    "inputs": [
      {
        "name": "validator",
        "type": "address",
        "internalType": "contract IValidator"
      }
    ],
    "outputs": [
      {
        "name": "enabled",
        "type": "bool",
        "internalType": "bool"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "nonceFor",
    "inputs": [
      {
        "name": "ep",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "key",
        "type": "uint192",
        "internalType": "uint192"
      }
    ],
    "outputs": [
      {
        "name": "",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "onERC1155BatchReceived",
    "inputs": [
      {
        "name": "",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "",
        "type": "uint256[]",
        "internalType": "uint256[]"
      },
      {
        "name": "",
        "type": "uint256[]",
        "internalType": "uint256[]"
      },
      {
        "name": "",
        "type": "bytes",
        "internalType": "bytes"
      }
    ],
    "outputs": [
      {
        "name": "",
        "type": "bytes4",
        "internalType": "bytes4"
      }
    ],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "onERC1155Received",
    "inputs": [
      {
        "name": "",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "",
        "type": "bytes",
        "internalType": "bytes"
      }
    ],
    "outputs": [
      {
        "name": "",
        "type": "bytes4",
        "internalType": "bytes4"
      }
    ],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "onERC721Received",
    "inputs": [
      {
        "name": "",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "",
        "type": "bytes",
        "internalType": "bytes"
      }
    ],
    "outputs": [
      {
        "name": "",
        "type": "bytes4",
        "internalType": "bytes4"
      }
    ],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "receiveErc3009",
    "inputs": [
      {
        "name": "token",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "from",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "value",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "validAfter",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "validBefore",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "nonce",
        "type": "bytes32",
        "internalType": "bytes32"
      },
      {
        "name": "v",
        "type": "uint8",
        "internalType": "uint8"
      },
      {
        "name": "r",
        "type": "bytes32",
        "internalType": "bytes32"
      },
      {
        "name": "s",
        "type": "bytes32",
        "internalType": "bytes32"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "setValidator",
    "inputs": [
      {
        "name": "validator",
        "type": "address",
        "internalType": "contract IValidator"
      },
      {
        "name": "enabled",
        "type": "bool",
        "internalType": "bool"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "supportsInterface",
    "inputs": [
      {
        "name": "id",
        "type": "bytes4",
        "internalType": "bytes4"
      }
    ],
    "outputs": [
      {
        "name": "",
        "type": "bool",
        "internalType": "bool"
      }
    ],
    "stateMutability": "pure"
  },
  {
    "type": "function",
    "name": "sweepERC20",
    "inputs": [
      {
        "name": "token",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "to",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "amount",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "sweepERC20Batch",
    "inputs": [
      {
        "name": "tokens",
        "type": "address[]",
        "internalType": "address[]"
      },
      {
        "name": "to",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "amounts",
        "type": "uint256[]",
        "internalType": "uint256[]"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "transferErc3009",
    "inputs": [
      {
        "name": "token",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "from",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "to",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "value",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "validAfter",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "validBefore",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "nonce",
        "type": "bytes32",
        "internalType": "bytes32"
      },
      {
        "name": "v",
        "type": "uint8",
        "internalType": "uint8"
      },
      {
        "name": "r",
        "type": "bytes32",
        "internalType": "bytes32"
      },
      {
        "name": "s",
        "type": "bytes32",
        "internalType": "bytes32"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "validateUserOp",
    "inputs": [
      {
        "name": "userOp",
        "type": "tuple",
        "internalType": "struct PackedUserOperation",
        "components": [
          {
            "name": "sender",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "nonce",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "initCode",
            "type": "bytes",
            "internalType": "bytes"
          },
          {
            "name": "callData",
            "type": "bytes",
            "internalType": "bytes"
          },
          {
            "name": "accountGasLimits",
            "type": "bytes32",
            "internalType": "bytes32"
          },
          {
            "name": "preVerificationGas",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "gasFees",
            "type": "bytes32",
            "internalType": "bytes32"
          },
          {
            "name": "paymasterAndData",
            "type": "bytes",
            "internalType": "bytes"
          },
          {
            "name": "signature",
            "type": "bytes",
            "internalType": "bytes"
          }
        ]
      },
      {
        "name": "userOpHash",
        "type": "bytes32",
        "internalType": "bytes32"
      },
      {
        "name": "missingAccountFunds",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "outputs": [
      {
        "name": "validationData",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "stateMutability": "nonpayable"
  },
  {
    "type": "event",
    "name": "BotDelegateActivated",
    "inputs": [
      {
        "name": "target",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "value",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      },
      {
        "name": "selector",
        "type": "bytes4",
        "indexed": false,
        "internalType": "bytes4"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "Erc20Swept",
    "inputs": [
      {
        "name": "token",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "to",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "amount",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "Erc3009Received",
    "inputs": [
      {
        "name": "token",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "from",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "value",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      },
      {
        "name": "nonce",
        "type": "bytes32",
        "indexed": false,
        "internalType": "bytes32"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "Erc3009Relayed",
    "inputs": [
      {
        "name": "token",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "from",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "to",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "value",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      },
      {
        "name": "nonce",
        "type": "bytes32",
        "indexed": false,
        "internalType": "bytes32"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "Erc6909OperatorSet",
    "inputs": [
      {
        "name": "token",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "operator",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "approved",
        "type": "bool",
        "indexed": false,
        "internalType": "bool"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "Erc6909Transferred",
    "inputs": [
      {
        "name": "token",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "to",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "id",
        "type": "uint256",
        "indexed": true,
        "internalType": "uint256"
      },
      {
        "name": "amount",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "ValidatorAdded",
    "inputs": [
      {
        "name": "validator",
        "type": "address",
        "indexed": true,
        "internalType": "contract IValidator"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "ValidatorRemoved",
    "inputs": [
      {
        "name": "validator",
        "type": "address",
        "indexed": true,
        "internalType": "contract IValidator"
      }
    ],
    "anonymous": false
  },
  {
    "type": "error",
    "name": "Erc6909Op_SetOperatorRequiresZeroIdAndAmount",
    "inputs": []
  },
  {
    "type": "error",
    "name": "Erc6909Op_TransferRequiresZeroApproved",
    "inputs": []
  },
  {
    "type": "error",
    "name": "Erc6909SetOperatorBlocked",
    "inputs": []
  },
  {
    "type": "error",
    "name": "ExecuteError",
    "inputs": [
      {
        "name": "index",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "error",
        "type": "bytes",
        "internalType": "bytes"
      }
    ]
  },
  {
    "type": "error",
    "name": "InvalidParams",
    "inputs": []
  },
  {
    "type": "error",
    "name": "MevBotDelegate__Erc6909SetOperatorFailed",
    "inputs": []
  },
  {
    "type": "error",
    "name": "MevBotDelegate__Erc6909TransferFailed",
    "inputs": []
  },
  {
    "type": "error",
    "name": "MevBotDelegate__InvalidSignatureLength",
    "inputs": []
  },
  {
    "type": "error",
    "name": "MevBotDelegate__InvalidValidator",
    "inputs": [
      {
        "name": "validator",
        "type": "address",
        "internalType": "contract IValidator"
      }
    ]
  },
  {
    "type": "error",
    "name": "MevBotDelegate__Unauthorized",
    "inputs": []
  },
  {
    "type": "error",
    "name": "MevBotDelegate__ValidatorNotEnabled",
    "inputs": [
      {
        "name": "validator",
        "type": "address",
        "internalType": "contract IValidator"
      }
    ]
  },
  {
    "type": "error",
    "name": "MevBotDelegate__ValidatorReverted",
    "inputs": [
      {
        "name": "validator",
        "type": "address",
        "internalType": "contract IValidator"
      }
    ]
  },
  {
    "type": "error",
    "name": "MevBotDelegate__ZeroTarget",
    "inputs": []
  },
  {
    "type": "error",
    "name": "NativeSweepFailed",
    "inputs": []
  },
  {
    "type": "error",
    "name": "NotFromEntryPoint",
    "inputs": [
      {
        "name": "msgSender",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "entity",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "entryPoint",
        "type": "address",
        "internalType": "address"
      }
    ]
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
pub mod MevBotDelegate {
    use super::*;
    use alloy::sol_types as alloy_sol_types;
    /// The creation / init bytecode of the contract.
    ///
    /// ```text
    ///0x60808060405234601557611cb2908161001a8239f35b5f80fdfe60806040526004361015610010575b005b5f3560e01c806301ffc9a714610175578063150b7a02146101705780631626ba7e1461016b57806319822f7c1461016657806334fcd5be146101615780634203a9341461015c5780634623c91d14610157578063503690d114610152578063592b2b721461014d57806359cd3300146101485780636b50bc911461014357806375f251671461013e5780637a738545146101395780639195f83614610134578063a134f64e14610134578063b0d691fe14610134578063b61d27f61461012f578063bc197c811461012a578063bd3710b014610125578063c70b5baf14610120578063d087d2881461011b578063e9500529146101165763f23a6e610361000e57610fa9565b610f45565b610ee2565b610dc5565b610cb1565b610c08565b610a99565b610a6f565b610a3e565b610a09565b6109db565b6109ad565b6108eb565b6107fe565b61068e565b610629565b610480565b6103c0565b61036e565b6102e8565b610190565b6001600160e01b031981160361018c57565b5f80fd5b3461018c57602036600319011261018c576004356101ad8161017a565b63ffffffff60e01b166301ffc9a760e01b811490811561021e575b811561020d575b81156101fc575b81156101eb575b506040519015158152602090f35b630a85bd0160e11b1490505f6101dd565b630271189760e51b811491506101d6565b630b135d3f60e11b811491506101cf565b6306608bdf60e21b811491506101c8565b6001600160a01b0381160361018c57565b359061024b8261022f565b565b634e487b7160e01b5f52604160045260245ffd5b90601f801991011681019081106001600160401b0382111761028257604052565b61024d565b6001600160401b03811161028257601f01601f191660200190565b81601f8201121561018c578035906102b982610287565b926102c76040519485610261565b8284526020838301011161018c57815f926020809301838601378301015290565b3461018c57608036600319011261018c5761030460043561022f565b61030f60243561022f565b6064356001600160401b03811161018c5761032e9036906004016102a2565b50604051630a85bd0160e11b8152602090f35b9181601f8401121561018c578235916001600160401b03831161018c576020838186019501011161018c57565b3461018c57604036600319011261018c576004356024356001600160401b03811161018c576020916103a76103ad923690600401610341565b91611110565b6040516001600160e01b03199091168152f35b3461018c57606036600319011261018c576004356001600160401b03811161018c57610120600319823603011261018c576104349061041c6024356044359261041461040b33611684565b3090339061197d565b600401611ae1565b9080610438575b506040519081529081906020820190565b0390f35b5f80808093335af1506104496112d8565b505f610423565b9181601f8401121561018c578235916001600160401b03831161018c576020808501948460051b01011161018c57565b3461018c57602036600319011261018c576004356001600160401b03811161018c576104b0903690600401610450565b6104b8611c01565b5f915b8183106104c457005b6104cf838383611268565b926104e86104dc8561128f565b6001600160a01b031690565b1561061a57604084016105046104fe8287611299565b90611c2c565b946001600160e01b0319861663558a729760e01b8114908115610609575b506105fa575f80610545936105368461128f565b90602085013595869186611299565b9190610556604051809481936112cb565b03925af16105626112d8565b90156105c85750939492936001937f02cb5483f9c6b769c7b58fd7bd050427ea566bd357c86f6f5c724c42c0e1745a916001600160a01b03906105a49061128f565b604080519586526001600160e01b03199390931660208601521692a20191906104bb565b83600187146105f1576105ed604051928392635a15467560e01b845260048401611307565b0390fd5b50602081519101fd5b631fb7cca560e01b5f5260045ffd5b63426a849360e01b1490505f610522565b630ca622c760e21b5f5260045ffd5b3461018c57602036600319011261018c576004356001600160401b03811161018c573660238201121561018c5780600401356001600160401b03811161018c5736602460c083028401011161018c57602461000e9201611393565b8015150361018c57565b3461018c57604036600319011261018c576004356106ab8161022f565b6024356106b781610684565b3033036107ef576001600160a01b0382169182156107a357816106ee816106dd8461107e565b9060ff801983541691151516179055565b610745575b501561071f577fe366c1c0452ed8eec96861e9e54141ebff23c9ec89fe27b996b45f5ec38849875f80a2005b7fe1434e25d6611e0db941968fdc97811c982ac1602e951637d206f5fdda9dd8f15f80a2005b6040516301ffc9a760e01b81526325ba90dd60e11b6004820152909190602081602481875afa5f91816107be575b5061079457630d1689bb60e31b5f526001600160a01b03831660045260245ffd5b919091156107a357505f6106f3565b630d1689bb60e31b5f526001600160a01b031660045260245ffd5b6107e191925060203d6020116107e8575b6107d98183610261565b81019061137e565b905f610773565b503d6107cf565b63bc3a81bd60e01b5f5260045ffd5b3461018c57606036600319011261018c5760043561081b8161022f565b602435906108288261022f565b604435610833611c01565b6001600160a01b0383169283156108dc576001600160a01b03831692836108a857505f80808481945af16108656112d8565b50156108995760207f6120f899b040ab0e38c9059d176d2dfb56273e93b7aa559635f65c5ec84ec913915b604051908152a3005b6316b452f760e01b5f5260045ffd5b916108d7816020937f6120f899b040ab0e38c9059d176d2dfb56273e93b7aa559635f65c5ec84ec91395611c54565b610890565b635435b28960e11b5f5260045ffd5b3461018c57604036600319011261018c576004356109088161022f565b6024356001600160c01b0381169081900361018c57604051631aab3f0d60e11b8152306004820152602481019190915290602090829060449082906001600160a01b03165afa801561099e57610434915f9161096f57506040519081529081906020820190565b610991915060203d602011610997575b6109898183610261565b810190611675565b5f610423565b503d61097f565b611105565b5f91031261018c57565b3461018c575f36600319011261018c576020604051734337084d9e255ff0702461cf8895ce9e3b5ff1088152f35b3461018c575f36600319011261018c576020604051735ff137d4b0fdcd49dca30c7cf57e578a026d27898152f35b3461018c57602036600319011261018c57602060ff610a32600435610a2d8161022f565b61107e565b54166040519015158152f35b3461018c57602036600319011261018c576020610a65600435610a608161022f565b611684565b6040519015158152f35b3461018c575f36600319011261018c5760206040516f71727de22e5e9d8baf0edac6f37da0328152f35b3461018c57606036600319011261018c57600435610ab68161022f565b602435906044356001600160401b03811161018c57610ad9903690600401610341565b610ae4929192611c01565b6001600160a01b03821692831561061a57610aff8282611c2c565b926001600160e01b0319841663558a729760e01b8114908115610b90575b506105fa575f92868493610b36604051809481936112cb565b03925af192610b436112d8565b9315610b8857604080519182526001600160e01b03199290921660208201527f02cb5483f9c6b769c7b58fd7bd050427ea566bd357c86f6f5c724c42c0e1745a9190a2005b835160208501fd5b63426a849360e01b1490505f610b1d565b9080601f8301121561018c578135916001600160401b038311610282578260051b9060405193610bd46020840186610261565b845260208085019282010192831161018c57602001905b828210610bf85750505090565b8135815260209182019101610beb565b3461018c5760a036600319011261018c57610c2460043561022f565b610c2f60243561022f565b6044356001600160401b03811161018c57610c4e903690600401610ba1565b506064356001600160401b03811161018c57610c6e903690600401610ba1565b506084356001600160401b03811161018c57610c8e9036906004016102a2565b5060405163bc197c8160e01b8152602090f35b60c4359060ff8216820361018c57565b3461018c5761012036600319011261018c57600435610ccf8161022f565b60243590610cdc8261022f565b60443591606435916084359160a43591610cf4610ca1565b9460e4356101043592610d05611c01565b6001600160a01b03169687158015610db4575b6108dc57873b1561018c575f938992610d4992886040519a8b9788976377aadf6360e11b8952308c60048b016116ea565b038183885af192831561099e577f2e3e88ccc3a3c06646b59ed5964ac16a95a871e0fffcf1156f52a26a8a6626f993610d9a575b506040805195865260208601929092526001600160a01b031693a3005b80610da85f610dae93610261565b806109a3565b5f610d7d565b506001600160a01b03851615610d18565b3461018c57606036600319011261018c576004356001600160401b03811161018c57610df5903690600401610450565b60243591610e028361022f565b6044356001600160401b03811161018c57610e21903690600401610450565b610e2c949194611c01565b6001600160a01b0382169283158015610ed8575b6108dc575f5b858110610e4f57005b610e62610e5d82888561173b565b61128f565b6001600160a01b038116919082156108dc576001927f6120f899b040ab0e38c9059d176d2dfb56273e93b7aa559635f65c5ec84ec913610ecf610ebe85898e610eb98e988e610eb286868661173b565b3591611c54565b61173b565b604051903581529081906020820190565b0390a301610e46565b5081851415610e40565b3461018c575f36600319011261018c57604051631aab3f0d60e11b81523060048201525f60248201526020816044816f71727de22e5e9d8baf0edac6f37da0325afa801561099e57610434915f9161096f57506040519081529081906020820190565b3461018c5761014036600319011261018c57600435610f638161022f565b60243590610f708261022f565b60443591610f7d8361022f565b60e43560c43560a43560843560643560ff8516850361018c5761000e976101043596610124359861174b565b3461018c5760a036600319011261018c57610fc560043561022f565b610fd060243561022f565b6084356001600160401b03811161018c57610fef9036906004016102a2565b5060405163f23a6e6160e01b8152602090f35b9060141161018c5790601490565b909291928360141161018c57831161018c57601401916013190190565b9060401161018c5760200190602090565b356bffffffffffffffffffffffff1981169291906014821061105e575050565b6bffffffffffffffffffffffff1960149290920360031b82901b16169150565b6001600160a01b03165f9081527f1b4e5d4a454a55d6403a95493733c095f0f7bd469e7096e579d67272204a37326020526040902090565b9081602091031261018c57516110cb8161017a565b90565b908060209392818452848401375f828201840152601f01601f1916010190565b6040906110cb9492815281602082015201916110ce565b6040513d5f823e3d90fd5b91909161111c82611836565b61123a576014821161113857506001600160e01b031992915050565b6111576104dc61115161114b8587611002565b9061103e565b60601c90565b61117161116d6111668361107e565b5460ff1690565b1590565b61122957611185836020946111a396611010565b604051630b135d3f60e11b81529586948593849391600485016110ee565b03916001600160a01b03165afa5f91816111f8575b506111ca57506001600160e01b031990565b6001600160e01b031916630b135d3f60e11b036111ec57630b135d3f60e11b90565b6001600160e01b031990565b61121b91925060203d602011611222575b6112138183610261565b8101906110b6565b905f6111b8565b503d611209565b506001600160e01b03199392505050565b9161124492611869565b156111ec57630b135d3f60e11b90565b634e487b7160e01b5f52603260045260245ffd5b919081101561128a5760051b81013590605e198136030182121561018c570190565b611254565b356110cb8161022f565b903590601e198136030182121561018c57018035906001600160401b03821161018c5760200191813603831361018c57565b908092918237015f815290565b3d15611302573d906112e982610287565b916112f76040519384610261565b82523d5f602084013e565b606090565b9060609260209183526040828401528051918291826040860152018484015e5f828201840152601f01601f1916010190565b919081101561128a5760c0020190565b6002111561135357565b634e487b7160e01b5f52602160045260245ffd5b35600281101561018c5790565b356110cb81610684565b9081602091031261018c57516110cb81610684565b919061139d611c01565b5f5b8181106113ac5750509050565b6113b7818386611339565b9060208201916113c96104dc8461128f565b15801561165f575b6108dc576113de81611367565b6113e781611349565b61151b576113f760a08201611374565b61150c576080905f61146760206114136104dc6104dc8961128f565b9360408101946114228661128f565b6040516304ade6db60e11b81526001600160a01b0390911660048201526060830135602482018190529790920135604483018190529491938492839182906064820190565b03925af190811561099e575f916114ee575b50156114df577f4a94f89e131699ed3416670c011ce64d62e5a581a4ebb4603bf6c4a5d06a06ce6114d56114b76114b160019861128f565b9461128f565b60405193845260a088901b8890039081169416929081906020820190565b0390a45b0161139f565b635e2e42c560e01b5f5260045ffd5b611506915060203d81116107e8576107d98183610261565b5f611479565b63b026d5a360e01b5f5260045ffd5b606081013515801590611652575b6116435761153c6104dc6104dc8561128f565b6115936020604084019360a06115518661128f565b91019361155d85611374565b60405163558a729760e01b81526001600160a01b03909316600484015215156024830152909283919082905f9082906044820190565b03925af190811561099e575f91611625575b5015611616577f9c8e17fa114d24cfc8f67c3d6ce6bc2e24067dbe41256640bc48fd6d1066562f61160e6115ec6115e66115e060019861128f565b9561128f565b93611374565b604051901515815260a087901b87900393841694909316929081906020820190565b0390a36114d9565b6342ac345d60e11b5f5260045ffd5b61163d915060203d81116107e8576107d98183610261565b5f6115a5565b6341f521f960e11b5f5260045ffd5b5060808101351515611529565b5061166f6104dc6040830161128f565b156113d1565b9081602091031261018c575190565b60018060a01b0316735ff137d4b0fdcd49dca30c7cf57e578a026d278981149081156116d0575b81156116b5575090565b734337084d9e255ff0702461cf8895ce9e3b5ff10891501490565b6f71727de22e5e9d8baf0edac6f37da032811491506116ab565b959193610100979360ff959b9a9996929b61012089019c60018060a01b0316895260018060a01b0316602089015260408801526060870152608086015260a08501521660c083015260e08201520152565b919081101561128a5760051b0190565b989594969193909897929761175e611c01565b6001600160a01b03169687158015611825575b8015611814575b6108dc57873b1561018c5788965f948694886117ab948e966040519c8d998a996371f70b0760e11b8b5260048b016116ea565b038183885af192831561099e577f728fcd896ed8764f00b06c3ea4847b5011b53fa28be451d6a2acebef60827da093611800575b506040805195865260208601929092526001600160a01b03908116951693a4565b80610da85f61180e93610261565b5f6117df565b506001600160a01b03851615611778565b506001600160a01b038a1615611771565b60408114908115611845575090565b604191501490565b35906020811061185b575090565b5f199060200360031b1b1690565b90604183036119345761188561187f848361102d565b9061184d565b6fa2a8918ca85bafe22016d0b997e4df60600160ff1b031061192d575b6040519280604014611905576041146118c757505050505b638baa579f5f526004601cfd5b806040809201355f1a60205281375b5f526020600160805f825afa51905f6060526040523d6118f75750506118ba565b6001600160a01b0316301490565b5060208181013560ff81901c601b0190915290356040526001600160ff1b03166060526118d6565b5050505f90565b6040830361192d57611958611949848361102d565b6001600160ff1b03929161184d565b166fa2a8918ca85bafe22016d0b997e4df60600160ff1b0310156118a2575050505f90565b15611986575050565b63fe34a6d360e01b5f9081526001600160a01b0391821660045291166024526f71727de22e5e9d8baf0edac6f37da032604452606490fd5b9035601e198236030181121561018c5701602081359101916001600160401b03821161018c57813603831361018c57565b611ace6110cb9593949260608352611a1a60608401611a0d83610240565b6001600160a01b03169052565b60208101356080840152611abb611aaf611a6f611a50611a3d60408601866119be565b61012060a08a01526101808901916110ce565b611a5d60608601866119be565b888303605f190160c08a0152906110ce565b608084013560e087015260a084013561010087015260c0840135610120870152611a9c60e08501856119be565b878303605f1901610140890152906110ce565b916101008101906119be565b848303605f1901610160860152906110ce565b93602082015260408185039101526110ce565b90611af0610100830183611299565b611afc81949294611836565b611be75760138111611b1757631a59f0eb60e11b5f5260045ffd5b611b2a6104dc61115161114b8488611002565b93611b3a61116d6111668761107e565b611bcb5791611b5082611b6e9593602095611010565b604051635cac526360e01b81529586948594919391600486016119ef565b03816001600160a01b0386165afa5f9181611baa575b50611ba557632dcc2a2760e11b5f526001600160a01b03821660045260245ffd5b905090565b611bc491925060203d602011610997576109898183610261565b905f611b84565b6330e3a03960e21b5f526001600160a01b03851660045260245ffd5b91611bf3939150611869565b15611bfc575f90565b600190565b3033148015611c18575b61024b903090339061197d565b5061024b611c2533611684565b9050611c0b565b5f929160038111611c3b575050565b9091925060041161018c57356001600160e01b03191690565b919060145260345263a9059cbb60601b5f5260205f6044601082855af1908160015f51141615611c87575b50505f603452565b3b153d171015611c98575f80611c7f565b6390b8ec185f526004601cfdfea164736f6c6343000822000a
    /// ```
    #[rustfmt::skip]
    #[allow(clippy::all)]
    pub static BYTECODE: alloy_sol_types::private::Bytes = alloy_sol_types::private::Bytes::from_static(
        b"`\x80\x80`@R4`\x15Wa\x1C\xB2\x90\x81a\0\x1A\x829\xF3[_\x80\xFD\xFE`\x80`@R`\x046\x10\x15a\0\x10W[\0[_5`\xE0\x1C\x80c\x01\xFF\xC9\xA7\x14a\x01uW\x80c\x15\x0Bz\x02\x14a\x01pW\x80c\x16&\xBA~\x14a\x01kW\x80c\x19\x82/|\x14a\x01fW\x80c4\xFC\xD5\xBE\x14a\x01aW\x80cB\x03\xA94\x14a\x01\\W\x80cF#\xC9\x1D\x14a\x01WW\x80cP6\x90\xD1\x14a\x01RW\x80cY++r\x14a\x01MW\x80cY\xCD3\0\x14a\x01HW\x80ckP\xBC\x91\x14a\x01CW\x80cu\xF2Qg\x14a\x01>W\x80czs\x85E\x14a\x019W\x80c\x91\x95\xF86\x14a\x014W\x80c\xA14\xF6N\x14a\x014W\x80c\xB0\xD6\x91\xFE\x14a\x014W\x80c\xB6\x1D'\xF6\x14a\x01/W\x80c\xBC\x19|\x81\x14a\x01*W\x80c\xBD7\x10\xB0\x14a\x01%W\x80c\xC7\x0B[\xAF\x14a\x01 W\x80c\xD0\x87\xD2\x88\x14a\x01\x1BW\x80c\xE9P\x05)\x14a\x01\x16Wc\xF2:na\x03a\0\x0EWa\x0F\xA9V[a\x0FEV[a\x0E\xE2V[a\r\xC5V[a\x0C\xB1V[a\x0C\x08V[a\n\x99V[a\noV[a\n>V[a\n\tV[a\t\xDBV[a\t\xADV[a\x08\xEBV[a\x07\xFEV[a\x06\x8EV[a\x06)V[a\x04\x80V[a\x03\xC0V[a\x03nV[a\x02\xE8V[a\x01\x90V[`\x01`\x01`\xE0\x1B\x03\x19\x81\x16\x03a\x01\x8CWV[_\x80\xFD[4a\x01\x8CW` 6`\x03\x19\x01\x12a\x01\x8CW`\x045a\x01\xAD\x81a\x01zV[c\xFF\xFF\xFF\xFF`\xE0\x1B\x16c\x01\xFF\xC9\xA7`\xE0\x1B\x81\x14\x90\x81\x15a\x02\x1EW[\x81\x15a\x02\rW[\x81\x15a\x01\xFCW[\x81\x15a\x01\xEBW[P`@Q\x90\x15\x15\x81R` \x90\xF3[c\n\x85\xBD\x01`\xE1\x1B\x14\x90P_a\x01\xDDV[c\x02q\x18\x97`\xE5\x1B\x81\x14\x91Pa\x01\xD6V[c\x0B\x13]?`\xE1\x1B\x81\x14\x91Pa\x01\xCFV[c\x06`\x8B\xDF`\xE2\x1B\x81\x14\x91Pa\x01\xC8V[`\x01`\x01`\xA0\x1B\x03\x81\x16\x03a\x01\x8CWV[5\x90a\x02K\x82a\x02/V[V[cNH{q`\xE0\x1B_R`A`\x04R`$_\xFD[\x90`\x1F\x80\x19\x91\x01\x16\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x02\x82W`@RV[a\x02MV[`\x01`\x01`@\x1B\x03\x81\x11a\x02\x82W`\x1F\x01`\x1F\x19\x16` \x01\x90V[\x81`\x1F\x82\x01\x12\x15a\x01\x8CW\x805\x90a\x02\xB9\x82a\x02\x87V[\x92a\x02\xC7`@Q\x94\x85a\x02aV[\x82\x84R` \x83\x83\x01\x01\x11a\x01\x8CW\x81_\x92` \x80\x93\x01\x83\x86\x017\x83\x01\x01R\x90V[4a\x01\x8CW`\x806`\x03\x19\x01\x12a\x01\x8CWa\x03\x04`\x045a\x02/V[a\x03\x0F`$5a\x02/V[`d5`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\x03.\x906\x90`\x04\x01a\x02\xA2V[P`@Qc\n\x85\xBD\x01`\xE1\x1B\x81R` \x90\xF3[\x91\x81`\x1F\x84\x01\x12\x15a\x01\x8CW\x825\x91`\x01`\x01`@\x1B\x03\x83\x11a\x01\x8CW` \x83\x81\x86\x01\x95\x01\x01\x11a\x01\x8CWV[4a\x01\x8CW`@6`\x03\x19\x01\x12a\x01\x8CW`\x045`$5`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CW` \x91a\x03\xA7a\x03\xAD\x926\x90`\x04\x01a\x03AV[\x91a\x11\x10V[`@Q`\x01`\x01`\xE0\x1B\x03\x19\x90\x91\x16\x81R\xF3[4a\x01\x8CW``6`\x03\x19\x01\x12a\x01\x8CW`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\x01 `\x03\x19\x826\x03\x01\x12a\x01\x8CWa\x044\x90a\x04\x1C`$5`D5\x92a\x04\x14a\x04\x0B3a\x16\x84V[0\x903\x90a\x19}V[`\x04\x01a\x1A\xE1V[\x90\x80a\x048W[P`@Q\x90\x81R\x90\x81\x90` \x82\x01\x90V[\x03\x90\xF3[_\x80\x80\x80\x933Z\xF1Pa\x04Ia\x12\xD8V[P_a\x04#V[\x91\x81`\x1F\x84\x01\x12\x15a\x01\x8CW\x825\x91`\x01`\x01`@\x1B\x03\x83\x11a\x01\x8CW` \x80\x85\x01\x94\x84`\x05\x1B\x01\x01\x11a\x01\x8CWV[4a\x01\x8CW` 6`\x03\x19\x01\x12a\x01\x8CW`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\x04\xB0\x906\x90`\x04\x01a\x04PV[a\x04\xB8a\x1C\x01V[_\x91[\x81\x83\x10a\x04\xC4W\0[a\x04\xCF\x83\x83\x83a\x12hV[\x92a\x04\xE8a\x04\xDC\x85a\x12\x8FV[`\x01`\x01`\xA0\x1B\x03\x16\x90V[\x15a\x06\x1AW`@\x84\x01a\x05\x04a\x04\xFE\x82\x87a\x12\x99V[\x90a\x1C,V[\x94`\x01`\x01`\xE0\x1B\x03\x19\x86\x16cU\x8Ar\x97`\xE0\x1B\x81\x14\x90\x81\x15a\x06\tW[Pa\x05\xFAW_\x80a\x05E\x93a\x056\x84a\x12\x8FV[\x90` \x85\x015\x95\x86\x91\x86a\x12\x99V[\x91\x90a\x05V`@Q\x80\x94\x81\x93a\x12\xCBV[\x03\x92Z\xF1a\x05ba\x12\xD8V[\x90\x15a\x05\xC8WP\x93\x94\x92\x93`\x01\x93\x7F\x02\xCBT\x83\xF9\xC6\xB7i\xC7\xB5\x8F\xD7\xBD\x05\x04'\xEAVk\xD3W\xC8oo\\rLB\xC0\xE1tZ\x91`\x01`\x01`\xA0\x1B\x03\x90a\x05\xA4\x90a\x12\x8FV[`@\x80Q\x95\x86R`\x01`\x01`\xE0\x1B\x03\x19\x93\x90\x93\x16` \x86\x01R\x16\x92\xA2\x01\x91\x90a\x04\xBBV[\x83`\x01\x87\x14a\x05\xF1Wa\x05\xED`@Q\x92\x83\x92cZ\x15Fu`\xE0\x1B\x84R`\x04\x84\x01a\x13\x07V[\x03\x90\xFD[P` \x81Q\x91\x01\xFD[c\x1F\xB7\xCC\xA5`\xE0\x1B_R`\x04_\xFD[cBj\x84\x93`\xE0\x1B\x14\x90P_a\x05\"V[c\x0C\xA6\"\xC7`\xE2\x1B_R`\x04_\xFD[4a\x01\x8CW` 6`\x03\x19\x01\x12a\x01\x8CW`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CW6`#\x82\x01\x12\x15a\x01\x8CW\x80`\x04\x015`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CW6`$`\xC0\x83\x02\x84\x01\x01\x11a\x01\x8CW`$a\0\x0E\x92\x01a\x13\x93V[\x80\x15\x15\x03a\x01\x8CWV[4a\x01\x8CW`@6`\x03\x19\x01\x12a\x01\x8CW`\x045a\x06\xAB\x81a\x02/V[`$5a\x06\xB7\x81a\x06\x84V[03\x03a\x07\xEFW`\x01`\x01`\xA0\x1B\x03\x82\x16\x91\x82\x15a\x07\xA3W\x81a\x06\xEE\x81a\x06\xDD\x84a\x10~V[\x90`\xFF\x80\x19\x83T\x16\x91\x15\x15\x16\x17\x90UV[a\x07EW[P\x15a\x07\x1FW\x7F\xE3f\xC1\xC0E.\xD8\xEE\xC9ha\xE9\xE5AA\xEB\xFF#\xC9\xEC\x89\xFE'\xB9\x96\xB4_^\xC3\x88I\x87_\x80\xA2\0[\x7F\xE1CN%\xD6a\x1E\r\xB9A\x96\x8F\xDC\x97\x81\x1C\x98*\xC1`.\x95\x167\xD2\x06\xF5\xFD\xDA\x9D\xD8\xF1_\x80\xA2\0[`@Qc\x01\xFF\xC9\xA7`\xE0\x1B\x81Rc%\xBA\x90\xDD`\xE1\x1B`\x04\x82\x01R\x90\x91\x90` \x81`$\x81\x87Z\xFA_\x91\x81a\x07\xBEW[Pa\x07\x94Wc\r\x16\x89\xBB`\xE3\x1B_R`\x01`\x01`\xA0\x1B\x03\x83\x16`\x04R`$_\xFD[\x91\x90\x91\x15a\x07\xA3WP_a\x06\xF3V[c\r\x16\x89\xBB`\xE3\x1B_R`\x01`\x01`\xA0\x1B\x03\x16`\x04R`$_\xFD[a\x07\xE1\x91\x92P` =` \x11a\x07\xE8W[a\x07\xD9\x81\x83a\x02aV[\x81\x01\x90a\x13~V[\x90_a\x07sV[P=a\x07\xCFV[c\xBC:\x81\xBD`\xE0\x1B_R`\x04_\xFD[4a\x01\x8CW``6`\x03\x19\x01\x12a\x01\x8CW`\x045a\x08\x1B\x81a\x02/V[`$5\x90a\x08(\x82a\x02/V[`D5a\x083a\x1C\x01V[`\x01`\x01`\xA0\x1B\x03\x83\x16\x92\x83\x15a\x08\xDCW`\x01`\x01`\xA0\x1B\x03\x83\x16\x92\x83a\x08\xA8WP_\x80\x80\x84\x81\x94Z\xF1a\x08ea\x12\xD8V[P\x15a\x08\x99W` \x7Fa \xF8\x99\xB0@\xAB\x0E8\xC9\x05\x9D\x17m-\xFBV'>\x93\xB7\xAAU\x965\xF6\\^\xC8N\xC9\x13\x91[`@Q\x90\x81R\xA3\0[c\x16\xB4R\xF7`\xE0\x1B_R`\x04_\xFD[\x91a\x08\xD7\x81` \x93\x7Fa \xF8\x99\xB0@\xAB\x0E8\xC9\x05\x9D\x17m-\xFBV'>\x93\xB7\xAAU\x965\xF6\\^\xC8N\xC9\x13\x95a\x1CTV[a\x08\x90V[cT5\xB2\x89`\xE1\x1B_R`\x04_\xFD[4a\x01\x8CW`@6`\x03\x19\x01\x12a\x01\x8CW`\x045a\t\x08\x81a\x02/V[`$5`\x01`\x01`\xC0\x1B\x03\x81\x16\x90\x81\x90\x03a\x01\x8CW`@Qc\x1A\xAB?\r`\xE1\x1B\x81R0`\x04\x82\x01R`$\x81\x01\x91\x90\x91R\x90` \x90\x82\x90`D\x90\x82\x90`\x01`\x01`\xA0\x1B\x03\x16Z\xFA\x80\x15a\t\x9EWa\x044\x91_\x91a\toWP`@Q\x90\x81R\x90\x81\x90` \x82\x01\x90V[a\t\x91\x91P` =` \x11a\t\x97W[a\t\x89\x81\x83a\x02aV[\x81\x01\x90a\x16uV[_a\x04#V[P=a\t\x7FV[a\x11\x05V[_\x91\x03\x12a\x01\x8CWV[4a\x01\x8CW_6`\x03\x19\x01\x12a\x01\x8CW` `@QsC7\x08M\x9E%_\xF0p$a\xCF\x88\x95\xCE\x9E;_\xF1\x08\x81R\xF3[4a\x01\x8CW_6`\x03\x19\x01\x12a\x01\x8CW` `@Qs_\xF17\xD4\xB0\xFD\xCDI\xDC\xA3\x0C|\xF5~W\x8A\x02m'\x89\x81R\xF3[4a\x01\x8CW` 6`\x03\x19\x01\x12a\x01\x8CW` `\xFFa\n2`\x045a\n-\x81a\x02/V[a\x10~V[T\x16`@Q\x90\x15\x15\x81R\xF3[4a\x01\x8CW` 6`\x03\x19\x01\x12a\x01\x8CW` a\ne`\x045a\n`\x81a\x02/V[a\x16\x84V[`@Q\x90\x15\x15\x81R\xF3[4a\x01\x8CW_6`\x03\x19\x01\x12a\x01\x8CW` `@Qoqr}\xE2.^\x9D\x8B\xAF\x0E\xDA\xC6\xF3}\xA02\x81R\xF3[4a\x01\x8CW``6`\x03\x19\x01\x12a\x01\x8CW`\x045a\n\xB6\x81a\x02/V[`$5\x90`D5`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\n\xD9\x906\x90`\x04\x01a\x03AV[a\n\xE4\x92\x91\x92a\x1C\x01V[`\x01`\x01`\xA0\x1B\x03\x82\x16\x92\x83\x15a\x06\x1AWa\n\xFF\x82\x82a\x1C,V[\x92`\x01`\x01`\xE0\x1B\x03\x19\x84\x16cU\x8Ar\x97`\xE0\x1B\x81\x14\x90\x81\x15a\x0B\x90W[Pa\x05\xFAW_\x92\x86\x84\x93a\x0B6`@Q\x80\x94\x81\x93a\x12\xCBV[\x03\x92Z\xF1\x92a\x0BCa\x12\xD8V[\x93\x15a\x0B\x88W`@\x80Q\x91\x82R`\x01`\x01`\xE0\x1B\x03\x19\x92\x90\x92\x16` \x82\x01R\x7F\x02\xCBT\x83\xF9\xC6\xB7i\xC7\xB5\x8F\xD7\xBD\x05\x04'\xEAVk\xD3W\xC8oo\\rLB\xC0\xE1tZ\x91\x90\xA2\0[\x83Q` \x85\x01\xFD[cBj\x84\x93`\xE0\x1B\x14\x90P_a\x0B\x1DV[\x90\x80`\x1F\x83\x01\x12\x15a\x01\x8CW\x815\x91`\x01`\x01`@\x1B\x03\x83\x11a\x02\x82W\x82`\x05\x1B\x90`@Q\x93a\x0B\xD4` \x84\x01\x86a\x02aV[\x84R` \x80\x85\x01\x92\x82\x01\x01\x92\x83\x11a\x01\x8CW` \x01\x90[\x82\x82\x10a\x0B\xF8WPPP\x90V[\x815\x81R` \x91\x82\x01\x91\x01a\x0B\xEBV[4a\x01\x8CW`\xA06`\x03\x19\x01\x12a\x01\x8CWa\x0C$`\x045a\x02/V[a\x0C/`$5a\x02/V[`D5`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\x0CN\x906\x90`\x04\x01a\x0B\xA1V[P`d5`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\x0Cn\x906\x90`\x04\x01a\x0B\xA1V[P`\x845`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\x0C\x8E\x906\x90`\x04\x01a\x02\xA2V[P`@Qc\xBC\x19|\x81`\xE0\x1B\x81R` \x90\xF3[`\xC45\x90`\xFF\x82\x16\x82\x03a\x01\x8CWV[4a\x01\x8CWa\x01 6`\x03\x19\x01\x12a\x01\x8CW`\x045a\x0C\xCF\x81a\x02/V[`$5\x90a\x0C\xDC\x82a\x02/V[`D5\x91`d5\x91`\x845\x91`\xA45\x91a\x0C\xF4a\x0C\xA1V[\x94`\xE45a\x01\x045\x92a\r\x05a\x1C\x01V[`\x01`\x01`\xA0\x1B\x03\x16\x96\x87\x15\x80\x15a\r\xB4W[a\x08\xDCW\x87;\x15a\x01\x8CW_\x93\x89\x92a\rI\x92\x88`@Q\x9A\x8B\x97\x88\x97cw\xAA\xDFc`\xE1\x1B\x89R0\x8C`\x04\x8B\x01a\x16\xEAV[\x03\x81\x83\x88Z\xF1\x92\x83\x15a\t\x9EW\x7F.>\x88\xCC\xC3\xA3\xC0fF\xB5\x9E\xD5\x96J\xC1j\x95\xA8q\xE0\xFF\xFC\xF1\x15oR\xA2j\x8Af&\xF9\x93a\r\x9AW[P`@\x80Q\x95\x86R` \x86\x01\x92\x90\x92R`\x01`\x01`\xA0\x1B\x03\x16\x93\xA3\0[\x80a\r\xA8_a\r\xAE\x93a\x02aV[\x80a\t\xA3V[_a\r}V[P`\x01`\x01`\xA0\x1B\x03\x85\x16\x15a\r\x18V[4a\x01\x8CW``6`\x03\x19\x01\x12a\x01\x8CW`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\r\xF5\x906\x90`\x04\x01a\x04PV[`$5\x91a\x0E\x02\x83a\x02/V[`D5`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\x0E!\x906\x90`\x04\x01a\x04PV[a\x0E,\x94\x91\x94a\x1C\x01V[`\x01`\x01`\xA0\x1B\x03\x82\x16\x92\x83\x15\x80\x15a\x0E\xD8W[a\x08\xDCW_[\x85\x81\x10a\x0EOW\0[a\x0Eba\x0E]\x82\x88\x85a\x17;V[a\x12\x8FV[`\x01`\x01`\xA0\x1B\x03\x81\x16\x91\x90\x82\x15a\x08\xDCW`\x01\x92\x7Fa \xF8\x99\xB0@\xAB\x0E8\xC9\x05\x9D\x17m-\xFBV'>\x93\xB7\xAAU\x965\xF6\\^\xC8N\xC9\x13a\x0E\xCFa\x0E\xBE\x85\x89\x8Ea\x0E\xB9\x8E\x98\x8Ea\x0E\xB2\x86\x86\x86a\x17;V[5\x91a\x1CTV[a\x17;V[`@Q\x905\x81R\x90\x81\x90` \x82\x01\x90V[\x03\x90\xA3\x01a\x0EFV[P\x81\x85\x14\x15a\x0E@V[4a\x01\x8CW_6`\x03\x19\x01\x12a\x01\x8CW`@Qc\x1A\xAB?\r`\xE1\x1B\x81R0`\x04\x82\x01R_`$\x82\x01R` \x81`D\x81oqr}\xE2.^\x9D\x8B\xAF\x0E\xDA\xC6\xF3}\xA02Z\xFA\x80\x15a\t\x9EWa\x044\x91_\x91a\toWP`@Q\x90\x81R\x90\x81\x90` \x82\x01\x90V[4a\x01\x8CWa\x01@6`\x03\x19\x01\x12a\x01\x8CW`\x045a\x0Fc\x81a\x02/V[`$5\x90a\x0Fp\x82a\x02/V[`D5\x91a\x0F}\x83a\x02/V[`\xE45`\xC45`\xA45`\x845`d5`\xFF\x85\x16\x85\x03a\x01\x8CWa\0\x0E\x97a\x01\x045\x96a\x01$5\x98a\x17KV[4a\x01\x8CW`\xA06`\x03\x19\x01\x12a\x01\x8CWa\x0F\xC5`\x045a\x02/V[a\x0F\xD0`$5a\x02/V[`\x845`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\x0F\xEF\x906\x90`\x04\x01a\x02\xA2V[P`@Qc\xF2:na`\xE0\x1B\x81R` \x90\xF3[\x90`\x14\x11a\x01\x8CW\x90`\x14\x90V[\x90\x92\x91\x92\x83`\x14\x11a\x01\x8CW\x83\x11a\x01\x8CW`\x14\x01\x91`\x13\x19\x01\x90V[\x90`@\x11a\x01\x8CW` \x01\x90` \x90V[5k\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\x19\x81\x16\x92\x91\x90`\x14\x82\x10a\x10^WPPV[k\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\x19`\x14\x92\x90\x92\x03`\x03\x1B\x82\x90\x1B\x16\x16\x91PV[`\x01`\x01`\xA0\x1B\x03\x16_\x90\x81R\x7F\x1BN]JEJU\xD6@:\x95I73\xC0\x95\xF0\xF7\xBDF\x9Ep\x96\xE5y\xD6rr J72` R`@\x90 \x90V[\x90\x81` \x91\x03\x12a\x01\x8CWQa\x10\xCB\x81a\x01zV[\x90V[\x90\x80` \x93\x92\x81\x84R\x84\x84\x017_\x82\x82\x01\x84\x01R`\x1F\x01`\x1F\x19\x16\x01\x01\x90V[`@\x90a\x10\xCB\x94\x92\x81R\x81` \x82\x01R\x01\x91a\x10\xCEV[`@Q=_\x82>=\x90\xFD[\x91\x90\x91a\x11\x1C\x82a\x186V[a\x12:W`\x14\x82\x11a\x118WP`\x01`\x01`\xE0\x1B\x03\x19\x92\x91PPV[a\x11Wa\x04\xDCa\x11Qa\x11K\x85\x87a\x10\x02V[\x90a\x10>V[``\x1C\x90V[a\x11qa\x11ma\x11f\x83a\x10~V[T`\xFF\x16\x90V[\x15\x90V[a\x12)Wa\x11\x85\x83` \x94a\x11\xA3\x96a\x10\x10V[`@Qc\x0B\x13]?`\xE1\x1B\x81R\x95\x86\x94\x85\x93\x84\x93\x91`\x04\x85\x01a\x10\xEEV[\x03\x91`\x01`\x01`\xA0\x1B\x03\x16Z\xFA_\x91\x81a\x11\xF8W[Pa\x11\xCAWP`\x01`\x01`\xE0\x1B\x03\x19\x90V[`\x01`\x01`\xE0\x1B\x03\x19\x16c\x0B\x13]?`\xE1\x1B\x03a\x11\xECWc\x0B\x13]?`\xE1\x1B\x90V[`\x01`\x01`\xE0\x1B\x03\x19\x90V[a\x12\x1B\x91\x92P` =` \x11a\x12\"W[a\x12\x13\x81\x83a\x02aV[\x81\x01\x90a\x10\xB6V[\x90_a\x11\xB8V[P=a\x12\tV[P`\x01`\x01`\xE0\x1B\x03\x19\x93\x92PPPV[\x91a\x12D\x92a\x18iV[\x15a\x11\xECWc\x0B\x13]?`\xE1\x1B\x90V[cNH{q`\xE0\x1B_R`2`\x04R`$_\xFD[\x91\x90\x81\x10\x15a\x12\x8AW`\x05\x1B\x81\x015\x90`^\x19\x816\x03\x01\x82\x12\x15a\x01\x8CW\x01\x90V[a\x12TV[5a\x10\xCB\x81a\x02/V[\x905\x90`\x1E\x19\x816\x03\x01\x82\x12\x15a\x01\x8CW\x01\x805\x90`\x01`\x01`@\x1B\x03\x82\x11a\x01\x8CW` \x01\x91\x816\x03\x83\x13a\x01\x8CWV[\x90\x80\x92\x91\x827\x01_\x81R\x90V[=\x15a\x13\x02W=\x90a\x12\xE9\x82a\x02\x87V[\x91a\x12\xF7`@Q\x93\x84a\x02aV[\x82R=_` \x84\x01>V[``\x90V[\x90``\x92` \x91\x83R`@\x82\x84\x01R\x80Q\x91\x82\x91\x82`@\x86\x01R\x01\x84\x84\x01^_\x82\x82\x01\x84\x01R`\x1F\x01`\x1F\x19\x16\x01\x01\x90V[\x91\x90\x81\x10\x15a\x12\x8AW`\xC0\x02\x01\x90V[`\x02\x11\x15a\x13SWV[cNH{q`\xE0\x1B_R`!`\x04R`$_\xFD[5`\x02\x81\x10\x15a\x01\x8CW\x90V[5a\x10\xCB\x81a\x06\x84V[\x90\x81` \x91\x03\x12a\x01\x8CWQa\x10\xCB\x81a\x06\x84V[\x91\x90a\x13\x9Da\x1C\x01V[_[\x81\x81\x10a\x13\xACWPP\x90PV[a\x13\xB7\x81\x83\x86a\x139V[\x90` \x82\x01\x91a\x13\xC9a\x04\xDC\x84a\x12\x8FV[\x15\x80\x15a\x16_W[a\x08\xDCWa\x13\xDE\x81a\x13gV[a\x13\xE7\x81a\x13IV[a\x15\x1BWa\x13\xF7`\xA0\x82\x01a\x13tV[a\x15\x0CW`\x80\x90_a\x14g` a\x14\x13a\x04\xDCa\x04\xDC\x89a\x12\x8FV[\x93`@\x81\x01\x94a\x14\"\x86a\x12\x8FV[`@Qc\x04\xAD\xE6\xDB`\xE1\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x90\x91\x16`\x04\x82\x01R``\x83\x015`$\x82\x01\x81\x90R\x97\x90\x92\x015`D\x83\x01\x81\x90R\x94\x91\x93\x84\x92\x83\x91\x82\x90`d\x82\x01\x90V[\x03\x92Z\xF1\x90\x81\x15a\t\x9EW_\x91a\x14\xEEW[P\x15a\x14\xDFW\x7FJ\x94\xF8\x9E\x13\x16\x99\xED4\x16g\x0C\x01\x1C\xE6Mb\xE5\xA5\x81\xA4\xEB\xB4`;\xF6\xC4\xA5\xD0j\x06\xCEa\x14\xD5a\x14\xB7a\x14\xB1`\x01\x98a\x12\x8FV[\x94a\x12\x8FV[`@Q\x93\x84R`\xA0\x88\x90\x1B\x88\x90\x03\x90\x81\x16\x94\x16\x92\x90\x81\x90` \x82\x01\x90V[\x03\x90\xA4[\x01a\x13\x9FV[c^.B\xC5`\xE0\x1B_R`\x04_\xFD[a\x15\x06\x91P` =\x81\x11a\x07\xE8Wa\x07\xD9\x81\x83a\x02aV[_a\x14yV[c\xB0&\xD5\xA3`\xE0\x1B_R`\x04_\xFD[``\x81\x015\x15\x80\x15\x90a\x16RW[a\x16CWa\x15<a\x04\xDCa\x04\xDC\x85a\x12\x8FV[a\x15\x93` `@\x84\x01\x93`\xA0a\x15Q\x86a\x12\x8FV[\x91\x01\x93a\x15]\x85a\x13tV[`@QcU\x8Ar\x97`\xE0\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x90\x93\x16`\x04\x84\x01R\x15\x15`$\x83\x01R\x90\x92\x83\x91\x90\x82\x90_\x90\x82\x90`D\x82\x01\x90V[\x03\x92Z\xF1\x90\x81\x15a\t\x9EW_\x91a\x16%W[P\x15a\x16\x16W\x7F\x9C\x8E\x17\xFA\x11M$\xCF\xC8\xF6|=l\xE6\xBC.$\x06}\xBEA%f@\xBCH\xFDm\x10fV/a\x16\x0Ea\x15\xECa\x15\xE6a\x15\xE0`\x01\x98a\x12\x8FV[\x95a\x12\x8FV[\x93a\x13tV[`@Q\x90\x15\x15\x81R`\xA0\x87\x90\x1B\x87\x90\x03\x93\x84\x16\x94\x90\x93\x16\x92\x90\x81\x90` \x82\x01\x90V[\x03\x90\xA3a\x14\xD9V[cB\xAC4]`\xE1\x1B_R`\x04_\xFD[a\x16=\x91P` =\x81\x11a\x07\xE8Wa\x07\xD9\x81\x83a\x02aV[_a\x15\xA5V[cA\xF5!\xF9`\xE1\x1B_R`\x04_\xFD[P`\x80\x81\x015\x15\x15a\x15)V[Pa\x16oa\x04\xDC`@\x83\x01a\x12\x8FV[\x15a\x13\xD1V[\x90\x81` \x91\x03\x12a\x01\x8CWQ\x90V[`\x01\x80`\xA0\x1B\x03\x16s_\xF17\xD4\xB0\xFD\xCDI\xDC\xA3\x0C|\xF5~W\x8A\x02m'\x89\x81\x14\x90\x81\x15a\x16\xD0W[\x81\x15a\x16\xB5WP\x90V[sC7\x08M\x9E%_\xF0p$a\xCF\x88\x95\xCE\x9E;_\xF1\x08\x91P\x14\x90V[oqr}\xE2.^\x9D\x8B\xAF\x0E\xDA\xC6\xF3}\xA02\x81\x14\x91Pa\x16\xABV[\x95\x91\x93a\x01\0\x97\x93`\xFF\x95\x9B\x9A\x99\x96\x92\x9Ba\x01 \x89\x01\x9C`\x01\x80`\xA0\x1B\x03\x16\x89R`\x01\x80`\xA0\x1B\x03\x16` \x89\x01R`@\x88\x01R``\x87\x01R`\x80\x86\x01R`\xA0\x85\x01R\x16`\xC0\x83\x01R`\xE0\x82\x01R\x01RV[\x91\x90\x81\x10\x15a\x12\x8AW`\x05\x1B\x01\x90V[\x98\x95\x94\x96\x91\x93\x90\x98\x97\x92\x97a\x17^a\x1C\x01V[`\x01`\x01`\xA0\x1B\x03\x16\x96\x87\x15\x80\x15a\x18%W[\x80\x15a\x18\x14W[a\x08\xDCW\x87;\x15a\x01\x8CW\x88\x96_\x94\x86\x94\x88a\x17\xAB\x94\x8E\x96`@Q\x9C\x8D\x99\x8A\x99cq\xF7\x0B\x07`\xE1\x1B\x8BR`\x04\x8B\x01a\x16\xEAV[\x03\x81\x83\x88Z\xF1\x92\x83\x15a\t\x9EW\x7Fr\x8F\xCD\x89n\xD8vO\0\xB0l>\xA4\x84{P\x11\xB5?\xA2\x8B\xE4Q\xD6\xA2\xAC\xEB\xEF`\x82}\xA0\x93a\x18\0W[P`@\x80Q\x95\x86R` \x86\x01\x92\x90\x92R`\x01`\x01`\xA0\x1B\x03\x90\x81\x16\x95\x16\x93\xA4V[\x80a\r\xA8_a\x18\x0E\x93a\x02aV[_a\x17\xDFV[P`\x01`\x01`\xA0\x1B\x03\x85\x16\x15a\x17xV[P`\x01`\x01`\xA0\x1B\x03\x8A\x16\x15a\x17qV[`@\x81\x14\x90\x81\x15a\x18EWP\x90V[`A\x91P\x14\x90V[5\x90` \x81\x10a\x18[WP\x90V[_\x19\x90` \x03`\x03\x1B\x1B\x16\x90V[\x90`A\x83\x03a\x194Wa\x18\x85a\x18\x7F\x84\x83a\x10-V[\x90a\x18MV[o\xA2\xA8\x91\x8C\xA8[\xAF\xE2 \x16\xD0\xB9\x97\xE4\xDF``\x01`\xFF\x1B\x03\x10a\x19-W[`@Q\x92\x80`@\x14a\x19\x05W`A\x14a\x18\xC7WPPPP[c\x8B\xAAW\x9F_R`\x04`\x1C\xFD[\x80`@\x80\x92\x015_\x1A` R\x817[_R` `\x01`\x80_\x82Z\xFAQ\x90_``R`@R=a\x18\xF7WPPa\x18\xBAV[`\x01`\x01`\xA0\x1B\x03\x160\x14\x90V[P` \x81\x81\x015`\xFF\x81\x90\x1C`\x1B\x01\x90\x91R\x905`@R`\x01`\x01`\xFF\x1B\x03\x16``Ra\x18\xD6V[PPP_\x90V[`@\x83\x03a\x19-Wa\x19Xa\x19I\x84\x83a\x10-V[`\x01`\x01`\xFF\x1B\x03\x92\x91a\x18MV[\x16o\xA2\xA8\x91\x8C\xA8[\xAF\xE2 \x16\xD0\xB9\x97\xE4\xDF``\x01`\xFF\x1B\x03\x10\x15a\x18\xA2WPPP_\x90V[\x15a\x19\x86WPPV[c\xFE4\xA6\xD3`\xE0\x1B_\x90\x81R`\x01`\x01`\xA0\x1B\x03\x91\x82\x16`\x04R\x91\x16`$Roqr}\xE2.^\x9D\x8B\xAF\x0E\xDA\xC6\xF3}\xA02`DR`d\x90\xFD[\x905`\x1E\x19\x826\x03\x01\x81\x12\x15a\x01\x8CW\x01` \x815\x91\x01\x91`\x01`\x01`@\x1B\x03\x82\x11a\x01\x8CW\x816\x03\x83\x13a\x01\x8CWV[a\x1A\xCEa\x10\xCB\x95\x93\x94\x92``\x83Ra\x1A\x1A``\x84\x01a\x1A\r\x83a\x02@V[`\x01`\x01`\xA0\x1B\x03\x16\x90RV[` \x81\x015`\x80\x84\x01Ra\x1A\xBBa\x1A\xAFa\x1Aoa\x1APa\x1A=`@\x86\x01\x86a\x19\xBEV[a\x01 `\xA0\x8A\x01Ra\x01\x80\x89\x01\x91a\x10\xCEV[a\x1A]``\x86\x01\x86a\x19\xBEV[\x88\x83\x03`_\x19\x01`\xC0\x8A\x01R\x90a\x10\xCEV[`\x80\x84\x015`\xE0\x87\x01R`\xA0\x84\x015a\x01\0\x87\x01R`\xC0\x84\x015a\x01 \x87\x01Ra\x1A\x9C`\xE0\x85\x01\x85a\x19\xBEV[\x87\x83\x03`_\x19\x01a\x01@\x89\x01R\x90a\x10\xCEV[\x91a\x01\0\x81\x01\x90a\x19\xBEV[\x84\x83\x03`_\x19\x01a\x01`\x86\x01R\x90a\x10\xCEV[\x93` \x82\x01R`@\x81\x85\x03\x91\x01Ra\x10\xCEV[\x90a\x1A\xF0a\x01\0\x83\x01\x83a\x12\x99V[a\x1A\xFC\x81\x94\x92\x94a\x186V[a\x1B\xE7W`\x13\x81\x11a\x1B\x17Wc\x1AY\xF0\xEB`\xE1\x1B_R`\x04_\xFD[a\x1B*a\x04\xDCa\x11Qa\x11K\x84\x88a\x10\x02V[\x93a\x1B:a\x11ma\x11f\x87a\x10~V[a\x1B\xCBW\x91a\x1BP\x82a\x1Bn\x95\x93` \x95a\x10\x10V[`@Qc\\\xACRc`\xE0\x1B\x81R\x95\x86\x94\x85\x94\x91\x93\x91`\x04\x86\x01a\x19\xEFV[\x03\x81`\x01`\x01`\xA0\x1B\x03\x86\x16Z\xFA_\x91\x81a\x1B\xAAW[Pa\x1B\xA5Wc-\xCC*'`\xE1\x1B_R`\x01`\x01`\xA0\x1B\x03\x82\x16`\x04R`$_\xFD[\x90P\x90V[a\x1B\xC4\x91\x92P` =` \x11a\t\x97Wa\t\x89\x81\x83a\x02aV[\x90_a\x1B\x84V[c0\xE3\xA09`\xE2\x1B_R`\x01`\x01`\xA0\x1B\x03\x85\x16`\x04R`$_\xFD[\x91a\x1B\xF3\x93\x91Pa\x18iV[\x15a\x1B\xFCW_\x90V[`\x01\x90V[03\x14\x80\x15a\x1C\x18W[a\x02K\x900\x903\x90a\x19}V[Pa\x02Ka\x1C%3a\x16\x84V[\x90Pa\x1C\x0BV[_\x92\x91`\x03\x81\x11a\x1C;WPPV[\x90\x91\x92P`\x04\x11a\x01\x8CW5`\x01`\x01`\xE0\x1B\x03\x19\x16\x90V[\x91\x90`\x14R`4Rc\xA9\x05\x9C\xBB``\x1B_R` _`D`\x10\x82\x85Z\xF1\x90\x81`\x01_Q\x14\x16\x15a\x1C\x87W[PP_`4RV[;\x15=\x17\x10\x15a\x1C\x98W_\x80a\x1C\x7FV[c\x90\xB8\xEC\x18_R`\x04`\x1C\xFD\xFE\xA1dsolcC\0\x08\"\0\n",
    );
    /// The runtime bytecode of the contract, as deployed on the network.
    ///
    /// ```text
    ///0x60806040526004361015610010575b005b5f3560e01c806301ffc9a714610175578063150b7a02146101705780631626ba7e1461016b57806319822f7c1461016657806334fcd5be146101615780634203a9341461015c5780634623c91d14610157578063503690d114610152578063592b2b721461014d57806359cd3300146101485780636b50bc911461014357806375f251671461013e5780637a738545146101395780639195f83614610134578063a134f64e14610134578063b0d691fe14610134578063b61d27f61461012f578063bc197c811461012a578063bd3710b014610125578063c70b5baf14610120578063d087d2881461011b578063e9500529146101165763f23a6e610361000e57610fa9565b610f45565b610ee2565b610dc5565b610cb1565b610c08565b610a99565b610a6f565b610a3e565b610a09565b6109db565b6109ad565b6108eb565b6107fe565b61068e565b610629565b610480565b6103c0565b61036e565b6102e8565b610190565b6001600160e01b031981160361018c57565b5f80fd5b3461018c57602036600319011261018c576004356101ad8161017a565b63ffffffff60e01b166301ffc9a760e01b811490811561021e575b811561020d575b81156101fc575b81156101eb575b506040519015158152602090f35b630a85bd0160e11b1490505f6101dd565b630271189760e51b811491506101d6565b630b135d3f60e11b811491506101cf565b6306608bdf60e21b811491506101c8565b6001600160a01b0381160361018c57565b359061024b8261022f565b565b634e487b7160e01b5f52604160045260245ffd5b90601f801991011681019081106001600160401b0382111761028257604052565b61024d565b6001600160401b03811161028257601f01601f191660200190565b81601f8201121561018c578035906102b982610287565b926102c76040519485610261565b8284526020838301011161018c57815f926020809301838601378301015290565b3461018c57608036600319011261018c5761030460043561022f565b61030f60243561022f565b6064356001600160401b03811161018c5761032e9036906004016102a2565b50604051630a85bd0160e11b8152602090f35b9181601f8401121561018c578235916001600160401b03831161018c576020838186019501011161018c57565b3461018c57604036600319011261018c576004356024356001600160401b03811161018c576020916103a76103ad923690600401610341565b91611110565b6040516001600160e01b03199091168152f35b3461018c57606036600319011261018c576004356001600160401b03811161018c57610120600319823603011261018c576104349061041c6024356044359261041461040b33611684565b3090339061197d565b600401611ae1565b9080610438575b506040519081529081906020820190565b0390f35b5f80808093335af1506104496112d8565b505f610423565b9181601f8401121561018c578235916001600160401b03831161018c576020808501948460051b01011161018c57565b3461018c57602036600319011261018c576004356001600160401b03811161018c576104b0903690600401610450565b6104b8611c01565b5f915b8183106104c457005b6104cf838383611268565b926104e86104dc8561128f565b6001600160a01b031690565b1561061a57604084016105046104fe8287611299565b90611c2c565b946001600160e01b0319861663558a729760e01b8114908115610609575b506105fa575f80610545936105368461128f565b90602085013595869186611299565b9190610556604051809481936112cb565b03925af16105626112d8565b90156105c85750939492936001937f02cb5483f9c6b769c7b58fd7bd050427ea566bd357c86f6f5c724c42c0e1745a916001600160a01b03906105a49061128f565b604080519586526001600160e01b03199390931660208601521692a20191906104bb565b83600187146105f1576105ed604051928392635a15467560e01b845260048401611307565b0390fd5b50602081519101fd5b631fb7cca560e01b5f5260045ffd5b63426a849360e01b1490505f610522565b630ca622c760e21b5f5260045ffd5b3461018c57602036600319011261018c576004356001600160401b03811161018c573660238201121561018c5780600401356001600160401b03811161018c5736602460c083028401011161018c57602461000e9201611393565b8015150361018c57565b3461018c57604036600319011261018c576004356106ab8161022f565b6024356106b781610684565b3033036107ef576001600160a01b0382169182156107a357816106ee816106dd8461107e565b9060ff801983541691151516179055565b610745575b501561071f577fe366c1c0452ed8eec96861e9e54141ebff23c9ec89fe27b996b45f5ec38849875f80a2005b7fe1434e25d6611e0db941968fdc97811c982ac1602e951637d206f5fdda9dd8f15f80a2005b6040516301ffc9a760e01b81526325ba90dd60e11b6004820152909190602081602481875afa5f91816107be575b5061079457630d1689bb60e31b5f526001600160a01b03831660045260245ffd5b919091156107a357505f6106f3565b630d1689bb60e31b5f526001600160a01b031660045260245ffd5b6107e191925060203d6020116107e8575b6107d98183610261565b81019061137e565b905f610773565b503d6107cf565b63bc3a81bd60e01b5f5260045ffd5b3461018c57606036600319011261018c5760043561081b8161022f565b602435906108288261022f565b604435610833611c01565b6001600160a01b0383169283156108dc576001600160a01b03831692836108a857505f80808481945af16108656112d8565b50156108995760207f6120f899b040ab0e38c9059d176d2dfb56273e93b7aa559635f65c5ec84ec913915b604051908152a3005b6316b452f760e01b5f5260045ffd5b916108d7816020937f6120f899b040ab0e38c9059d176d2dfb56273e93b7aa559635f65c5ec84ec91395611c54565b610890565b635435b28960e11b5f5260045ffd5b3461018c57604036600319011261018c576004356109088161022f565b6024356001600160c01b0381169081900361018c57604051631aab3f0d60e11b8152306004820152602481019190915290602090829060449082906001600160a01b03165afa801561099e57610434915f9161096f57506040519081529081906020820190565b610991915060203d602011610997575b6109898183610261565b810190611675565b5f610423565b503d61097f565b611105565b5f91031261018c57565b3461018c575f36600319011261018c576020604051734337084d9e255ff0702461cf8895ce9e3b5ff1088152f35b3461018c575f36600319011261018c576020604051735ff137d4b0fdcd49dca30c7cf57e578a026d27898152f35b3461018c57602036600319011261018c57602060ff610a32600435610a2d8161022f565b61107e565b54166040519015158152f35b3461018c57602036600319011261018c576020610a65600435610a608161022f565b611684565b6040519015158152f35b3461018c575f36600319011261018c5760206040516f71727de22e5e9d8baf0edac6f37da0328152f35b3461018c57606036600319011261018c57600435610ab68161022f565b602435906044356001600160401b03811161018c57610ad9903690600401610341565b610ae4929192611c01565b6001600160a01b03821692831561061a57610aff8282611c2c565b926001600160e01b0319841663558a729760e01b8114908115610b90575b506105fa575f92868493610b36604051809481936112cb565b03925af192610b436112d8565b9315610b8857604080519182526001600160e01b03199290921660208201527f02cb5483f9c6b769c7b58fd7bd050427ea566bd357c86f6f5c724c42c0e1745a9190a2005b835160208501fd5b63426a849360e01b1490505f610b1d565b9080601f8301121561018c578135916001600160401b038311610282578260051b9060405193610bd46020840186610261565b845260208085019282010192831161018c57602001905b828210610bf85750505090565b8135815260209182019101610beb565b3461018c5760a036600319011261018c57610c2460043561022f565b610c2f60243561022f565b6044356001600160401b03811161018c57610c4e903690600401610ba1565b506064356001600160401b03811161018c57610c6e903690600401610ba1565b506084356001600160401b03811161018c57610c8e9036906004016102a2565b5060405163bc197c8160e01b8152602090f35b60c4359060ff8216820361018c57565b3461018c5761012036600319011261018c57600435610ccf8161022f565b60243590610cdc8261022f565b60443591606435916084359160a43591610cf4610ca1565b9460e4356101043592610d05611c01565b6001600160a01b03169687158015610db4575b6108dc57873b1561018c575f938992610d4992886040519a8b9788976377aadf6360e11b8952308c60048b016116ea565b038183885af192831561099e577f2e3e88ccc3a3c06646b59ed5964ac16a95a871e0fffcf1156f52a26a8a6626f993610d9a575b506040805195865260208601929092526001600160a01b031693a3005b80610da85f610dae93610261565b806109a3565b5f610d7d565b506001600160a01b03851615610d18565b3461018c57606036600319011261018c576004356001600160401b03811161018c57610df5903690600401610450565b60243591610e028361022f565b6044356001600160401b03811161018c57610e21903690600401610450565b610e2c949194611c01565b6001600160a01b0382169283158015610ed8575b6108dc575f5b858110610e4f57005b610e62610e5d82888561173b565b61128f565b6001600160a01b038116919082156108dc576001927f6120f899b040ab0e38c9059d176d2dfb56273e93b7aa559635f65c5ec84ec913610ecf610ebe85898e610eb98e988e610eb286868661173b565b3591611c54565b61173b565b604051903581529081906020820190565b0390a301610e46565b5081851415610e40565b3461018c575f36600319011261018c57604051631aab3f0d60e11b81523060048201525f60248201526020816044816f71727de22e5e9d8baf0edac6f37da0325afa801561099e57610434915f9161096f57506040519081529081906020820190565b3461018c5761014036600319011261018c57600435610f638161022f565b60243590610f708261022f565b60443591610f7d8361022f565b60e43560c43560a43560843560643560ff8516850361018c5761000e976101043596610124359861174b565b3461018c5760a036600319011261018c57610fc560043561022f565b610fd060243561022f565b6084356001600160401b03811161018c57610fef9036906004016102a2565b5060405163f23a6e6160e01b8152602090f35b9060141161018c5790601490565b909291928360141161018c57831161018c57601401916013190190565b9060401161018c5760200190602090565b356bffffffffffffffffffffffff1981169291906014821061105e575050565b6bffffffffffffffffffffffff1960149290920360031b82901b16169150565b6001600160a01b03165f9081527f1b4e5d4a454a55d6403a95493733c095f0f7bd469e7096e579d67272204a37326020526040902090565b9081602091031261018c57516110cb8161017a565b90565b908060209392818452848401375f828201840152601f01601f1916010190565b6040906110cb9492815281602082015201916110ce565b6040513d5f823e3d90fd5b91909161111c82611836565b61123a576014821161113857506001600160e01b031992915050565b6111576104dc61115161114b8587611002565b9061103e565b60601c90565b61117161116d6111668361107e565b5460ff1690565b1590565b61122957611185836020946111a396611010565b604051630b135d3f60e11b81529586948593849391600485016110ee565b03916001600160a01b03165afa5f91816111f8575b506111ca57506001600160e01b031990565b6001600160e01b031916630b135d3f60e11b036111ec57630b135d3f60e11b90565b6001600160e01b031990565b61121b91925060203d602011611222575b6112138183610261565b8101906110b6565b905f6111b8565b503d611209565b506001600160e01b03199392505050565b9161124492611869565b156111ec57630b135d3f60e11b90565b634e487b7160e01b5f52603260045260245ffd5b919081101561128a5760051b81013590605e198136030182121561018c570190565b611254565b356110cb8161022f565b903590601e198136030182121561018c57018035906001600160401b03821161018c5760200191813603831361018c57565b908092918237015f815290565b3d15611302573d906112e982610287565b916112f76040519384610261565b82523d5f602084013e565b606090565b9060609260209183526040828401528051918291826040860152018484015e5f828201840152601f01601f1916010190565b919081101561128a5760c0020190565b6002111561135357565b634e487b7160e01b5f52602160045260245ffd5b35600281101561018c5790565b356110cb81610684565b9081602091031261018c57516110cb81610684565b919061139d611c01565b5f5b8181106113ac5750509050565b6113b7818386611339565b9060208201916113c96104dc8461128f565b15801561165f575b6108dc576113de81611367565b6113e781611349565b61151b576113f760a08201611374565b61150c576080905f61146760206114136104dc6104dc8961128f565b9360408101946114228661128f565b6040516304ade6db60e11b81526001600160a01b0390911660048201526060830135602482018190529790920135604483018190529491938492839182906064820190565b03925af190811561099e575f916114ee575b50156114df577f4a94f89e131699ed3416670c011ce64d62e5a581a4ebb4603bf6c4a5d06a06ce6114d56114b76114b160019861128f565b9461128f565b60405193845260a088901b8890039081169416929081906020820190565b0390a45b0161139f565b635e2e42c560e01b5f5260045ffd5b611506915060203d81116107e8576107d98183610261565b5f611479565b63b026d5a360e01b5f5260045ffd5b606081013515801590611652575b6116435761153c6104dc6104dc8561128f565b6115936020604084019360a06115518661128f565b91019361155d85611374565b60405163558a729760e01b81526001600160a01b03909316600484015215156024830152909283919082905f9082906044820190565b03925af190811561099e575f91611625575b5015611616577f9c8e17fa114d24cfc8f67c3d6ce6bc2e24067dbe41256640bc48fd6d1066562f61160e6115ec6115e66115e060019861128f565b9561128f565b93611374565b604051901515815260a087901b87900393841694909316929081906020820190565b0390a36114d9565b6342ac345d60e11b5f5260045ffd5b61163d915060203d81116107e8576107d98183610261565b5f6115a5565b6341f521f960e11b5f5260045ffd5b5060808101351515611529565b5061166f6104dc6040830161128f565b156113d1565b9081602091031261018c575190565b60018060a01b0316735ff137d4b0fdcd49dca30c7cf57e578a026d278981149081156116d0575b81156116b5575090565b734337084d9e255ff0702461cf8895ce9e3b5ff10891501490565b6f71727de22e5e9d8baf0edac6f37da032811491506116ab565b959193610100979360ff959b9a9996929b61012089019c60018060a01b0316895260018060a01b0316602089015260408801526060870152608086015260a08501521660c083015260e08201520152565b919081101561128a5760051b0190565b989594969193909897929761175e611c01565b6001600160a01b03169687158015611825575b8015611814575b6108dc57873b1561018c5788965f948694886117ab948e966040519c8d998a996371f70b0760e11b8b5260048b016116ea565b038183885af192831561099e577f728fcd896ed8764f00b06c3ea4847b5011b53fa28be451d6a2acebef60827da093611800575b506040805195865260208601929092526001600160a01b03908116951693a4565b80610da85f61180e93610261565b5f6117df565b506001600160a01b03851615611778565b506001600160a01b038a1615611771565b60408114908115611845575090565b604191501490565b35906020811061185b575090565b5f199060200360031b1b1690565b90604183036119345761188561187f848361102d565b9061184d565b6fa2a8918ca85bafe22016d0b997e4df60600160ff1b031061192d575b6040519280604014611905576041146118c757505050505b638baa579f5f526004601cfd5b806040809201355f1a60205281375b5f526020600160805f825afa51905f6060526040523d6118f75750506118ba565b6001600160a01b0316301490565b5060208181013560ff81901c601b0190915290356040526001600160ff1b03166060526118d6565b5050505f90565b6040830361192d57611958611949848361102d565b6001600160ff1b03929161184d565b166fa2a8918ca85bafe22016d0b997e4df60600160ff1b0310156118a2575050505f90565b15611986575050565b63fe34a6d360e01b5f9081526001600160a01b0391821660045291166024526f71727de22e5e9d8baf0edac6f37da032604452606490fd5b9035601e198236030181121561018c5701602081359101916001600160401b03821161018c57813603831361018c57565b611ace6110cb9593949260608352611a1a60608401611a0d83610240565b6001600160a01b03169052565b60208101356080840152611abb611aaf611a6f611a50611a3d60408601866119be565b61012060a08a01526101808901916110ce565b611a5d60608601866119be565b888303605f190160c08a0152906110ce565b608084013560e087015260a084013561010087015260c0840135610120870152611a9c60e08501856119be565b878303605f1901610140890152906110ce565b916101008101906119be565b848303605f1901610160860152906110ce565b93602082015260408185039101526110ce565b90611af0610100830183611299565b611afc81949294611836565b611be75760138111611b1757631a59f0eb60e11b5f5260045ffd5b611b2a6104dc61115161114b8488611002565b93611b3a61116d6111668761107e565b611bcb5791611b5082611b6e9593602095611010565b604051635cac526360e01b81529586948594919391600486016119ef565b03816001600160a01b0386165afa5f9181611baa575b50611ba557632dcc2a2760e11b5f526001600160a01b03821660045260245ffd5b905090565b611bc491925060203d602011610997576109898183610261565b905f611b84565b6330e3a03960e21b5f526001600160a01b03851660045260245ffd5b91611bf3939150611869565b15611bfc575f90565b600190565b3033148015611c18575b61024b903090339061197d565b5061024b611c2533611684565b9050611c0b565b5f929160038111611c3b575050565b9091925060041161018c57356001600160e01b03191690565b919060145260345263a9059cbb60601b5f5260205f6044601082855af1908160015f51141615611c87575b50505f603452565b3b153d171015611c98575f80611c7f565b6390b8ec185f526004601cfdfea164736f6c6343000822000a
    /// ```
    #[rustfmt::skip]
    #[allow(clippy::all)]
    pub static DEPLOYED_BYTECODE: alloy_sol_types::private::Bytes = alloy_sol_types::private::Bytes::from_static(
        b"`\x80`@R`\x046\x10\x15a\0\x10W[\0[_5`\xE0\x1C\x80c\x01\xFF\xC9\xA7\x14a\x01uW\x80c\x15\x0Bz\x02\x14a\x01pW\x80c\x16&\xBA~\x14a\x01kW\x80c\x19\x82/|\x14a\x01fW\x80c4\xFC\xD5\xBE\x14a\x01aW\x80cB\x03\xA94\x14a\x01\\W\x80cF#\xC9\x1D\x14a\x01WW\x80cP6\x90\xD1\x14a\x01RW\x80cY++r\x14a\x01MW\x80cY\xCD3\0\x14a\x01HW\x80ckP\xBC\x91\x14a\x01CW\x80cu\xF2Qg\x14a\x01>W\x80czs\x85E\x14a\x019W\x80c\x91\x95\xF86\x14a\x014W\x80c\xA14\xF6N\x14a\x014W\x80c\xB0\xD6\x91\xFE\x14a\x014W\x80c\xB6\x1D'\xF6\x14a\x01/W\x80c\xBC\x19|\x81\x14a\x01*W\x80c\xBD7\x10\xB0\x14a\x01%W\x80c\xC7\x0B[\xAF\x14a\x01 W\x80c\xD0\x87\xD2\x88\x14a\x01\x1BW\x80c\xE9P\x05)\x14a\x01\x16Wc\xF2:na\x03a\0\x0EWa\x0F\xA9V[a\x0FEV[a\x0E\xE2V[a\r\xC5V[a\x0C\xB1V[a\x0C\x08V[a\n\x99V[a\noV[a\n>V[a\n\tV[a\t\xDBV[a\t\xADV[a\x08\xEBV[a\x07\xFEV[a\x06\x8EV[a\x06)V[a\x04\x80V[a\x03\xC0V[a\x03nV[a\x02\xE8V[a\x01\x90V[`\x01`\x01`\xE0\x1B\x03\x19\x81\x16\x03a\x01\x8CWV[_\x80\xFD[4a\x01\x8CW` 6`\x03\x19\x01\x12a\x01\x8CW`\x045a\x01\xAD\x81a\x01zV[c\xFF\xFF\xFF\xFF`\xE0\x1B\x16c\x01\xFF\xC9\xA7`\xE0\x1B\x81\x14\x90\x81\x15a\x02\x1EW[\x81\x15a\x02\rW[\x81\x15a\x01\xFCW[\x81\x15a\x01\xEBW[P`@Q\x90\x15\x15\x81R` \x90\xF3[c\n\x85\xBD\x01`\xE1\x1B\x14\x90P_a\x01\xDDV[c\x02q\x18\x97`\xE5\x1B\x81\x14\x91Pa\x01\xD6V[c\x0B\x13]?`\xE1\x1B\x81\x14\x91Pa\x01\xCFV[c\x06`\x8B\xDF`\xE2\x1B\x81\x14\x91Pa\x01\xC8V[`\x01`\x01`\xA0\x1B\x03\x81\x16\x03a\x01\x8CWV[5\x90a\x02K\x82a\x02/V[V[cNH{q`\xE0\x1B_R`A`\x04R`$_\xFD[\x90`\x1F\x80\x19\x91\x01\x16\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x02\x82W`@RV[a\x02MV[`\x01`\x01`@\x1B\x03\x81\x11a\x02\x82W`\x1F\x01`\x1F\x19\x16` \x01\x90V[\x81`\x1F\x82\x01\x12\x15a\x01\x8CW\x805\x90a\x02\xB9\x82a\x02\x87V[\x92a\x02\xC7`@Q\x94\x85a\x02aV[\x82\x84R` \x83\x83\x01\x01\x11a\x01\x8CW\x81_\x92` \x80\x93\x01\x83\x86\x017\x83\x01\x01R\x90V[4a\x01\x8CW`\x806`\x03\x19\x01\x12a\x01\x8CWa\x03\x04`\x045a\x02/V[a\x03\x0F`$5a\x02/V[`d5`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\x03.\x906\x90`\x04\x01a\x02\xA2V[P`@Qc\n\x85\xBD\x01`\xE1\x1B\x81R` \x90\xF3[\x91\x81`\x1F\x84\x01\x12\x15a\x01\x8CW\x825\x91`\x01`\x01`@\x1B\x03\x83\x11a\x01\x8CW` \x83\x81\x86\x01\x95\x01\x01\x11a\x01\x8CWV[4a\x01\x8CW`@6`\x03\x19\x01\x12a\x01\x8CW`\x045`$5`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CW` \x91a\x03\xA7a\x03\xAD\x926\x90`\x04\x01a\x03AV[\x91a\x11\x10V[`@Q`\x01`\x01`\xE0\x1B\x03\x19\x90\x91\x16\x81R\xF3[4a\x01\x8CW``6`\x03\x19\x01\x12a\x01\x8CW`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\x01 `\x03\x19\x826\x03\x01\x12a\x01\x8CWa\x044\x90a\x04\x1C`$5`D5\x92a\x04\x14a\x04\x0B3a\x16\x84V[0\x903\x90a\x19}V[`\x04\x01a\x1A\xE1V[\x90\x80a\x048W[P`@Q\x90\x81R\x90\x81\x90` \x82\x01\x90V[\x03\x90\xF3[_\x80\x80\x80\x933Z\xF1Pa\x04Ia\x12\xD8V[P_a\x04#V[\x91\x81`\x1F\x84\x01\x12\x15a\x01\x8CW\x825\x91`\x01`\x01`@\x1B\x03\x83\x11a\x01\x8CW` \x80\x85\x01\x94\x84`\x05\x1B\x01\x01\x11a\x01\x8CWV[4a\x01\x8CW` 6`\x03\x19\x01\x12a\x01\x8CW`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\x04\xB0\x906\x90`\x04\x01a\x04PV[a\x04\xB8a\x1C\x01V[_\x91[\x81\x83\x10a\x04\xC4W\0[a\x04\xCF\x83\x83\x83a\x12hV[\x92a\x04\xE8a\x04\xDC\x85a\x12\x8FV[`\x01`\x01`\xA0\x1B\x03\x16\x90V[\x15a\x06\x1AW`@\x84\x01a\x05\x04a\x04\xFE\x82\x87a\x12\x99V[\x90a\x1C,V[\x94`\x01`\x01`\xE0\x1B\x03\x19\x86\x16cU\x8Ar\x97`\xE0\x1B\x81\x14\x90\x81\x15a\x06\tW[Pa\x05\xFAW_\x80a\x05E\x93a\x056\x84a\x12\x8FV[\x90` \x85\x015\x95\x86\x91\x86a\x12\x99V[\x91\x90a\x05V`@Q\x80\x94\x81\x93a\x12\xCBV[\x03\x92Z\xF1a\x05ba\x12\xD8V[\x90\x15a\x05\xC8WP\x93\x94\x92\x93`\x01\x93\x7F\x02\xCBT\x83\xF9\xC6\xB7i\xC7\xB5\x8F\xD7\xBD\x05\x04'\xEAVk\xD3W\xC8oo\\rLB\xC0\xE1tZ\x91`\x01`\x01`\xA0\x1B\x03\x90a\x05\xA4\x90a\x12\x8FV[`@\x80Q\x95\x86R`\x01`\x01`\xE0\x1B\x03\x19\x93\x90\x93\x16` \x86\x01R\x16\x92\xA2\x01\x91\x90a\x04\xBBV[\x83`\x01\x87\x14a\x05\xF1Wa\x05\xED`@Q\x92\x83\x92cZ\x15Fu`\xE0\x1B\x84R`\x04\x84\x01a\x13\x07V[\x03\x90\xFD[P` \x81Q\x91\x01\xFD[c\x1F\xB7\xCC\xA5`\xE0\x1B_R`\x04_\xFD[cBj\x84\x93`\xE0\x1B\x14\x90P_a\x05\"V[c\x0C\xA6\"\xC7`\xE2\x1B_R`\x04_\xFD[4a\x01\x8CW` 6`\x03\x19\x01\x12a\x01\x8CW`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CW6`#\x82\x01\x12\x15a\x01\x8CW\x80`\x04\x015`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CW6`$`\xC0\x83\x02\x84\x01\x01\x11a\x01\x8CW`$a\0\x0E\x92\x01a\x13\x93V[\x80\x15\x15\x03a\x01\x8CWV[4a\x01\x8CW`@6`\x03\x19\x01\x12a\x01\x8CW`\x045a\x06\xAB\x81a\x02/V[`$5a\x06\xB7\x81a\x06\x84V[03\x03a\x07\xEFW`\x01`\x01`\xA0\x1B\x03\x82\x16\x91\x82\x15a\x07\xA3W\x81a\x06\xEE\x81a\x06\xDD\x84a\x10~V[\x90`\xFF\x80\x19\x83T\x16\x91\x15\x15\x16\x17\x90UV[a\x07EW[P\x15a\x07\x1FW\x7F\xE3f\xC1\xC0E.\xD8\xEE\xC9ha\xE9\xE5AA\xEB\xFF#\xC9\xEC\x89\xFE'\xB9\x96\xB4_^\xC3\x88I\x87_\x80\xA2\0[\x7F\xE1CN%\xD6a\x1E\r\xB9A\x96\x8F\xDC\x97\x81\x1C\x98*\xC1`.\x95\x167\xD2\x06\xF5\xFD\xDA\x9D\xD8\xF1_\x80\xA2\0[`@Qc\x01\xFF\xC9\xA7`\xE0\x1B\x81Rc%\xBA\x90\xDD`\xE1\x1B`\x04\x82\x01R\x90\x91\x90` \x81`$\x81\x87Z\xFA_\x91\x81a\x07\xBEW[Pa\x07\x94Wc\r\x16\x89\xBB`\xE3\x1B_R`\x01`\x01`\xA0\x1B\x03\x83\x16`\x04R`$_\xFD[\x91\x90\x91\x15a\x07\xA3WP_a\x06\xF3V[c\r\x16\x89\xBB`\xE3\x1B_R`\x01`\x01`\xA0\x1B\x03\x16`\x04R`$_\xFD[a\x07\xE1\x91\x92P` =` \x11a\x07\xE8W[a\x07\xD9\x81\x83a\x02aV[\x81\x01\x90a\x13~V[\x90_a\x07sV[P=a\x07\xCFV[c\xBC:\x81\xBD`\xE0\x1B_R`\x04_\xFD[4a\x01\x8CW``6`\x03\x19\x01\x12a\x01\x8CW`\x045a\x08\x1B\x81a\x02/V[`$5\x90a\x08(\x82a\x02/V[`D5a\x083a\x1C\x01V[`\x01`\x01`\xA0\x1B\x03\x83\x16\x92\x83\x15a\x08\xDCW`\x01`\x01`\xA0\x1B\x03\x83\x16\x92\x83a\x08\xA8WP_\x80\x80\x84\x81\x94Z\xF1a\x08ea\x12\xD8V[P\x15a\x08\x99W` \x7Fa \xF8\x99\xB0@\xAB\x0E8\xC9\x05\x9D\x17m-\xFBV'>\x93\xB7\xAAU\x965\xF6\\^\xC8N\xC9\x13\x91[`@Q\x90\x81R\xA3\0[c\x16\xB4R\xF7`\xE0\x1B_R`\x04_\xFD[\x91a\x08\xD7\x81` \x93\x7Fa \xF8\x99\xB0@\xAB\x0E8\xC9\x05\x9D\x17m-\xFBV'>\x93\xB7\xAAU\x965\xF6\\^\xC8N\xC9\x13\x95a\x1CTV[a\x08\x90V[cT5\xB2\x89`\xE1\x1B_R`\x04_\xFD[4a\x01\x8CW`@6`\x03\x19\x01\x12a\x01\x8CW`\x045a\t\x08\x81a\x02/V[`$5`\x01`\x01`\xC0\x1B\x03\x81\x16\x90\x81\x90\x03a\x01\x8CW`@Qc\x1A\xAB?\r`\xE1\x1B\x81R0`\x04\x82\x01R`$\x81\x01\x91\x90\x91R\x90` \x90\x82\x90`D\x90\x82\x90`\x01`\x01`\xA0\x1B\x03\x16Z\xFA\x80\x15a\t\x9EWa\x044\x91_\x91a\toWP`@Q\x90\x81R\x90\x81\x90` \x82\x01\x90V[a\t\x91\x91P` =` \x11a\t\x97W[a\t\x89\x81\x83a\x02aV[\x81\x01\x90a\x16uV[_a\x04#V[P=a\t\x7FV[a\x11\x05V[_\x91\x03\x12a\x01\x8CWV[4a\x01\x8CW_6`\x03\x19\x01\x12a\x01\x8CW` `@QsC7\x08M\x9E%_\xF0p$a\xCF\x88\x95\xCE\x9E;_\xF1\x08\x81R\xF3[4a\x01\x8CW_6`\x03\x19\x01\x12a\x01\x8CW` `@Qs_\xF17\xD4\xB0\xFD\xCDI\xDC\xA3\x0C|\xF5~W\x8A\x02m'\x89\x81R\xF3[4a\x01\x8CW` 6`\x03\x19\x01\x12a\x01\x8CW` `\xFFa\n2`\x045a\n-\x81a\x02/V[a\x10~V[T\x16`@Q\x90\x15\x15\x81R\xF3[4a\x01\x8CW` 6`\x03\x19\x01\x12a\x01\x8CW` a\ne`\x045a\n`\x81a\x02/V[a\x16\x84V[`@Q\x90\x15\x15\x81R\xF3[4a\x01\x8CW_6`\x03\x19\x01\x12a\x01\x8CW` `@Qoqr}\xE2.^\x9D\x8B\xAF\x0E\xDA\xC6\xF3}\xA02\x81R\xF3[4a\x01\x8CW``6`\x03\x19\x01\x12a\x01\x8CW`\x045a\n\xB6\x81a\x02/V[`$5\x90`D5`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\n\xD9\x906\x90`\x04\x01a\x03AV[a\n\xE4\x92\x91\x92a\x1C\x01V[`\x01`\x01`\xA0\x1B\x03\x82\x16\x92\x83\x15a\x06\x1AWa\n\xFF\x82\x82a\x1C,V[\x92`\x01`\x01`\xE0\x1B\x03\x19\x84\x16cU\x8Ar\x97`\xE0\x1B\x81\x14\x90\x81\x15a\x0B\x90W[Pa\x05\xFAW_\x92\x86\x84\x93a\x0B6`@Q\x80\x94\x81\x93a\x12\xCBV[\x03\x92Z\xF1\x92a\x0BCa\x12\xD8V[\x93\x15a\x0B\x88W`@\x80Q\x91\x82R`\x01`\x01`\xE0\x1B\x03\x19\x92\x90\x92\x16` \x82\x01R\x7F\x02\xCBT\x83\xF9\xC6\xB7i\xC7\xB5\x8F\xD7\xBD\x05\x04'\xEAVk\xD3W\xC8oo\\rLB\xC0\xE1tZ\x91\x90\xA2\0[\x83Q` \x85\x01\xFD[cBj\x84\x93`\xE0\x1B\x14\x90P_a\x0B\x1DV[\x90\x80`\x1F\x83\x01\x12\x15a\x01\x8CW\x815\x91`\x01`\x01`@\x1B\x03\x83\x11a\x02\x82W\x82`\x05\x1B\x90`@Q\x93a\x0B\xD4` \x84\x01\x86a\x02aV[\x84R` \x80\x85\x01\x92\x82\x01\x01\x92\x83\x11a\x01\x8CW` \x01\x90[\x82\x82\x10a\x0B\xF8WPPP\x90V[\x815\x81R` \x91\x82\x01\x91\x01a\x0B\xEBV[4a\x01\x8CW`\xA06`\x03\x19\x01\x12a\x01\x8CWa\x0C$`\x045a\x02/V[a\x0C/`$5a\x02/V[`D5`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\x0CN\x906\x90`\x04\x01a\x0B\xA1V[P`d5`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\x0Cn\x906\x90`\x04\x01a\x0B\xA1V[P`\x845`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\x0C\x8E\x906\x90`\x04\x01a\x02\xA2V[P`@Qc\xBC\x19|\x81`\xE0\x1B\x81R` \x90\xF3[`\xC45\x90`\xFF\x82\x16\x82\x03a\x01\x8CWV[4a\x01\x8CWa\x01 6`\x03\x19\x01\x12a\x01\x8CW`\x045a\x0C\xCF\x81a\x02/V[`$5\x90a\x0C\xDC\x82a\x02/V[`D5\x91`d5\x91`\x845\x91`\xA45\x91a\x0C\xF4a\x0C\xA1V[\x94`\xE45a\x01\x045\x92a\r\x05a\x1C\x01V[`\x01`\x01`\xA0\x1B\x03\x16\x96\x87\x15\x80\x15a\r\xB4W[a\x08\xDCW\x87;\x15a\x01\x8CW_\x93\x89\x92a\rI\x92\x88`@Q\x9A\x8B\x97\x88\x97cw\xAA\xDFc`\xE1\x1B\x89R0\x8C`\x04\x8B\x01a\x16\xEAV[\x03\x81\x83\x88Z\xF1\x92\x83\x15a\t\x9EW\x7F.>\x88\xCC\xC3\xA3\xC0fF\xB5\x9E\xD5\x96J\xC1j\x95\xA8q\xE0\xFF\xFC\xF1\x15oR\xA2j\x8Af&\xF9\x93a\r\x9AW[P`@\x80Q\x95\x86R` \x86\x01\x92\x90\x92R`\x01`\x01`\xA0\x1B\x03\x16\x93\xA3\0[\x80a\r\xA8_a\r\xAE\x93a\x02aV[\x80a\t\xA3V[_a\r}V[P`\x01`\x01`\xA0\x1B\x03\x85\x16\x15a\r\x18V[4a\x01\x8CW``6`\x03\x19\x01\x12a\x01\x8CW`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\r\xF5\x906\x90`\x04\x01a\x04PV[`$5\x91a\x0E\x02\x83a\x02/V[`D5`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\x0E!\x906\x90`\x04\x01a\x04PV[a\x0E,\x94\x91\x94a\x1C\x01V[`\x01`\x01`\xA0\x1B\x03\x82\x16\x92\x83\x15\x80\x15a\x0E\xD8W[a\x08\xDCW_[\x85\x81\x10a\x0EOW\0[a\x0Eba\x0E]\x82\x88\x85a\x17;V[a\x12\x8FV[`\x01`\x01`\xA0\x1B\x03\x81\x16\x91\x90\x82\x15a\x08\xDCW`\x01\x92\x7Fa \xF8\x99\xB0@\xAB\x0E8\xC9\x05\x9D\x17m-\xFBV'>\x93\xB7\xAAU\x965\xF6\\^\xC8N\xC9\x13a\x0E\xCFa\x0E\xBE\x85\x89\x8Ea\x0E\xB9\x8E\x98\x8Ea\x0E\xB2\x86\x86\x86a\x17;V[5\x91a\x1CTV[a\x17;V[`@Q\x905\x81R\x90\x81\x90` \x82\x01\x90V[\x03\x90\xA3\x01a\x0EFV[P\x81\x85\x14\x15a\x0E@V[4a\x01\x8CW_6`\x03\x19\x01\x12a\x01\x8CW`@Qc\x1A\xAB?\r`\xE1\x1B\x81R0`\x04\x82\x01R_`$\x82\x01R` \x81`D\x81oqr}\xE2.^\x9D\x8B\xAF\x0E\xDA\xC6\xF3}\xA02Z\xFA\x80\x15a\t\x9EWa\x044\x91_\x91a\toWP`@Q\x90\x81R\x90\x81\x90` \x82\x01\x90V[4a\x01\x8CWa\x01@6`\x03\x19\x01\x12a\x01\x8CW`\x045a\x0Fc\x81a\x02/V[`$5\x90a\x0Fp\x82a\x02/V[`D5\x91a\x0F}\x83a\x02/V[`\xE45`\xC45`\xA45`\x845`d5`\xFF\x85\x16\x85\x03a\x01\x8CWa\0\x0E\x97a\x01\x045\x96a\x01$5\x98a\x17KV[4a\x01\x8CW`\xA06`\x03\x19\x01\x12a\x01\x8CWa\x0F\xC5`\x045a\x02/V[a\x0F\xD0`$5a\x02/V[`\x845`\x01`\x01`@\x1B\x03\x81\x11a\x01\x8CWa\x0F\xEF\x906\x90`\x04\x01a\x02\xA2V[P`@Qc\xF2:na`\xE0\x1B\x81R` \x90\xF3[\x90`\x14\x11a\x01\x8CW\x90`\x14\x90V[\x90\x92\x91\x92\x83`\x14\x11a\x01\x8CW\x83\x11a\x01\x8CW`\x14\x01\x91`\x13\x19\x01\x90V[\x90`@\x11a\x01\x8CW` \x01\x90` \x90V[5k\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\x19\x81\x16\x92\x91\x90`\x14\x82\x10a\x10^WPPV[k\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\x19`\x14\x92\x90\x92\x03`\x03\x1B\x82\x90\x1B\x16\x16\x91PV[`\x01`\x01`\xA0\x1B\x03\x16_\x90\x81R\x7F\x1BN]JEJU\xD6@:\x95I73\xC0\x95\xF0\xF7\xBDF\x9Ep\x96\xE5y\xD6rr J72` R`@\x90 \x90V[\x90\x81` \x91\x03\x12a\x01\x8CWQa\x10\xCB\x81a\x01zV[\x90V[\x90\x80` \x93\x92\x81\x84R\x84\x84\x017_\x82\x82\x01\x84\x01R`\x1F\x01`\x1F\x19\x16\x01\x01\x90V[`@\x90a\x10\xCB\x94\x92\x81R\x81` \x82\x01R\x01\x91a\x10\xCEV[`@Q=_\x82>=\x90\xFD[\x91\x90\x91a\x11\x1C\x82a\x186V[a\x12:W`\x14\x82\x11a\x118WP`\x01`\x01`\xE0\x1B\x03\x19\x92\x91PPV[a\x11Wa\x04\xDCa\x11Qa\x11K\x85\x87a\x10\x02V[\x90a\x10>V[``\x1C\x90V[a\x11qa\x11ma\x11f\x83a\x10~V[T`\xFF\x16\x90V[\x15\x90V[a\x12)Wa\x11\x85\x83` \x94a\x11\xA3\x96a\x10\x10V[`@Qc\x0B\x13]?`\xE1\x1B\x81R\x95\x86\x94\x85\x93\x84\x93\x91`\x04\x85\x01a\x10\xEEV[\x03\x91`\x01`\x01`\xA0\x1B\x03\x16Z\xFA_\x91\x81a\x11\xF8W[Pa\x11\xCAWP`\x01`\x01`\xE0\x1B\x03\x19\x90V[`\x01`\x01`\xE0\x1B\x03\x19\x16c\x0B\x13]?`\xE1\x1B\x03a\x11\xECWc\x0B\x13]?`\xE1\x1B\x90V[`\x01`\x01`\xE0\x1B\x03\x19\x90V[a\x12\x1B\x91\x92P` =` \x11a\x12\"W[a\x12\x13\x81\x83a\x02aV[\x81\x01\x90a\x10\xB6V[\x90_a\x11\xB8V[P=a\x12\tV[P`\x01`\x01`\xE0\x1B\x03\x19\x93\x92PPPV[\x91a\x12D\x92a\x18iV[\x15a\x11\xECWc\x0B\x13]?`\xE1\x1B\x90V[cNH{q`\xE0\x1B_R`2`\x04R`$_\xFD[\x91\x90\x81\x10\x15a\x12\x8AW`\x05\x1B\x81\x015\x90`^\x19\x816\x03\x01\x82\x12\x15a\x01\x8CW\x01\x90V[a\x12TV[5a\x10\xCB\x81a\x02/V[\x905\x90`\x1E\x19\x816\x03\x01\x82\x12\x15a\x01\x8CW\x01\x805\x90`\x01`\x01`@\x1B\x03\x82\x11a\x01\x8CW` \x01\x91\x816\x03\x83\x13a\x01\x8CWV[\x90\x80\x92\x91\x827\x01_\x81R\x90V[=\x15a\x13\x02W=\x90a\x12\xE9\x82a\x02\x87V[\x91a\x12\xF7`@Q\x93\x84a\x02aV[\x82R=_` \x84\x01>V[``\x90V[\x90``\x92` \x91\x83R`@\x82\x84\x01R\x80Q\x91\x82\x91\x82`@\x86\x01R\x01\x84\x84\x01^_\x82\x82\x01\x84\x01R`\x1F\x01`\x1F\x19\x16\x01\x01\x90V[\x91\x90\x81\x10\x15a\x12\x8AW`\xC0\x02\x01\x90V[`\x02\x11\x15a\x13SWV[cNH{q`\xE0\x1B_R`!`\x04R`$_\xFD[5`\x02\x81\x10\x15a\x01\x8CW\x90V[5a\x10\xCB\x81a\x06\x84V[\x90\x81` \x91\x03\x12a\x01\x8CWQa\x10\xCB\x81a\x06\x84V[\x91\x90a\x13\x9Da\x1C\x01V[_[\x81\x81\x10a\x13\xACWPP\x90PV[a\x13\xB7\x81\x83\x86a\x139V[\x90` \x82\x01\x91a\x13\xC9a\x04\xDC\x84a\x12\x8FV[\x15\x80\x15a\x16_W[a\x08\xDCWa\x13\xDE\x81a\x13gV[a\x13\xE7\x81a\x13IV[a\x15\x1BWa\x13\xF7`\xA0\x82\x01a\x13tV[a\x15\x0CW`\x80\x90_a\x14g` a\x14\x13a\x04\xDCa\x04\xDC\x89a\x12\x8FV[\x93`@\x81\x01\x94a\x14\"\x86a\x12\x8FV[`@Qc\x04\xAD\xE6\xDB`\xE1\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x90\x91\x16`\x04\x82\x01R``\x83\x015`$\x82\x01\x81\x90R\x97\x90\x92\x015`D\x83\x01\x81\x90R\x94\x91\x93\x84\x92\x83\x91\x82\x90`d\x82\x01\x90V[\x03\x92Z\xF1\x90\x81\x15a\t\x9EW_\x91a\x14\xEEW[P\x15a\x14\xDFW\x7FJ\x94\xF8\x9E\x13\x16\x99\xED4\x16g\x0C\x01\x1C\xE6Mb\xE5\xA5\x81\xA4\xEB\xB4`;\xF6\xC4\xA5\xD0j\x06\xCEa\x14\xD5a\x14\xB7a\x14\xB1`\x01\x98a\x12\x8FV[\x94a\x12\x8FV[`@Q\x93\x84R`\xA0\x88\x90\x1B\x88\x90\x03\x90\x81\x16\x94\x16\x92\x90\x81\x90` \x82\x01\x90V[\x03\x90\xA4[\x01a\x13\x9FV[c^.B\xC5`\xE0\x1B_R`\x04_\xFD[a\x15\x06\x91P` =\x81\x11a\x07\xE8Wa\x07\xD9\x81\x83a\x02aV[_a\x14yV[c\xB0&\xD5\xA3`\xE0\x1B_R`\x04_\xFD[``\x81\x015\x15\x80\x15\x90a\x16RW[a\x16CWa\x15<a\x04\xDCa\x04\xDC\x85a\x12\x8FV[a\x15\x93` `@\x84\x01\x93`\xA0a\x15Q\x86a\x12\x8FV[\x91\x01\x93a\x15]\x85a\x13tV[`@QcU\x8Ar\x97`\xE0\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x90\x93\x16`\x04\x84\x01R\x15\x15`$\x83\x01R\x90\x92\x83\x91\x90\x82\x90_\x90\x82\x90`D\x82\x01\x90V[\x03\x92Z\xF1\x90\x81\x15a\t\x9EW_\x91a\x16%W[P\x15a\x16\x16W\x7F\x9C\x8E\x17\xFA\x11M$\xCF\xC8\xF6|=l\xE6\xBC.$\x06}\xBEA%f@\xBCH\xFDm\x10fV/a\x16\x0Ea\x15\xECa\x15\xE6a\x15\xE0`\x01\x98a\x12\x8FV[\x95a\x12\x8FV[\x93a\x13tV[`@Q\x90\x15\x15\x81R`\xA0\x87\x90\x1B\x87\x90\x03\x93\x84\x16\x94\x90\x93\x16\x92\x90\x81\x90` \x82\x01\x90V[\x03\x90\xA3a\x14\xD9V[cB\xAC4]`\xE1\x1B_R`\x04_\xFD[a\x16=\x91P` =\x81\x11a\x07\xE8Wa\x07\xD9\x81\x83a\x02aV[_a\x15\xA5V[cA\xF5!\xF9`\xE1\x1B_R`\x04_\xFD[P`\x80\x81\x015\x15\x15a\x15)V[Pa\x16oa\x04\xDC`@\x83\x01a\x12\x8FV[\x15a\x13\xD1V[\x90\x81` \x91\x03\x12a\x01\x8CWQ\x90V[`\x01\x80`\xA0\x1B\x03\x16s_\xF17\xD4\xB0\xFD\xCDI\xDC\xA3\x0C|\xF5~W\x8A\x02m'\x89\x81\x14\x90\x81\x15a\x16\xD0W[\x81\x15a\x16\xB5WP\x90V[sC7\x08M\x9E%_\xF0p$a\xCF\x88\x95\xCE\x9E;_\xF1\x08\x91P\x14\x90V[oqr}\xE2.^\x9D\x8B\xAF\x0E\xDA\xC6\xF3}\xA02\x81\x14\x91Pa\x16\xABV[\x95\x91\x93a\x01\0\x97\x93`\xFF\x95\x9B\x9A\x99\x96\x92\x9Ba\x01 \x89\x01\x9C`\x01\x80`\xA0\x1B\x03\x16\x89R`\x01\x80`\xA0\x1B\x03\x16` \x89\x01R`@\x88\x01R``\x87\x01R`\x80\x86\x01R`\xA0\x85\x01R\x16`\xC0\x83\x01R`\xE0\x82\x01R\x01RV[\x91\x90\x81\x10\x15a\x12\x8AW`\x05\x1B\x01\x90V[\x98\x95\x94\x96\x91\x93\x90\x98\x97\x92\x97a\x17^a\x1C\x01V[`\x01`\x01`\xA0\x1B\x03\x16\x96\x87\x15\x80\x15a\x18%W[\x80\x15a\x18\x14W[a\x08\xDCW\x87;\x15a\x01\x8CW\x88\x96_\x94\x86\x94\x88a\x17\xAB\x94\x8E\x96`@Q\x9C\x8D\x99\x8A\x99cq\xF7\x0B\x07`\xE1\x1B\x8BR`\x04\x8B\x01a\x16\xEAV[\x03\x81\x83\x88Z\xF1\x92\x83\x15a\t\x9EW\x7Fr\x8F\xCD\x89n\xD8vO\0\xB0l>\xA4\x84{P\x11\xB5?\xA2\x8B\xE4Q\xD6\xA2\xAC\xEB\xEF`\x82}\xA0\x93a\x18\0W[P`@\x80Q\x95\x86R` \x86\x01\x92\x90\x92R`\x01`\x01`\xA0\x1B\x03\x90\x81\x16\x95\x16\x93\xA4V[\x80a\r\xA8_a\x18\x0E\x93a\x02aV[_a\x17\xDFV[P`\x01`\x01`\xA0\x1B\x03\x85\x16\x15a\x17xV[P`\x01`\x01`\xA0\x1B\x03\x8A\x16\x15a\x17qV[`@\x81\x14\x90\x81\x15a\x18EWP\x90V[`A\x91P\x14\x90V[5\x90` \x81\x10a\x18[WP\x90V[_\x19\x90` \x03`\x03\x1B\x1B\x16\x90V[\x90`A\x83\x03a\x194Wa\x18\x85a\x18\x7F\x84\x83a\x10-V[\x90a\x18MV[o\xA2\xA8\x91\x8C\xA8[\xAF\xE2 \x16\xD0\xB9\x97\xE4\xDF``\x01`\xFF\x1B\x03\x10a\x19-W[`@Q\x92\x80`@\x14a\x19\x05W`A\x14a\x18\xC7WPPPP[c\x8B\xAAW\x9F_R`\x04`\x1C\xFD[\x80`@\x80\x92\x015_\x1A` R\x817[_R` `\x01`\x80_\x82Z\xFAQ\x90_``R`@R=a\x18\xF7WPPa\x18\xBAV[`\x01`\x01`\xA0\x1B\x03\x160\x14\x90V[P` \x81\x81\x015`\xFF\x81\x90\x1C`\x1B\x01\x90\x91R\x905`@R`\x01`\x01`\xFF\x1B\x03\x16``Ra\x18\xD6V[PPP_\x90V[`@\x83\x03a\x19-Wa\x19Xa\x19I\x84\x83a\x10-V[`\x01`\x01`\xFF\x1B\x03\x92\x91a\x18MV[\x16o\xA2\xA8\x91\x8C\xA8[\xAF\xE2 \x16\xD0\xB9\x97\xE4\xDF``\x01`\xFF\x1B\x03\x10\x15a\x18\xA2WPPP_\x90V[\x15a\x19\x86WPPV[c\xFE4\xA6\xD3`\xE0\x1B_\x90\x81R`\x01`\x01`\xA0\x1B\x03\x91\x82\x16`\x04R\x91\x16`$Roqr}\xE2.^\x9D\x8B\xAF\x0E\xDA\xC6\xF3}\xA02`DR`d\x90\xFD[\x905`\x1E\x19\x826\x03\x01\x81\x12\x15a\x01\x8CW\x01` \x815\x91\x01\x91`\x01`\x01`@\x1B\x03\x82\x11a\x01\x8CW\x816\x03\x83\x13a\x01\x8CWV[a\x1A\xCEa\x10\xCB\x95\x93\x94\x92``\x83Ra\x1A\x1A``\x84\x01a\x1A\r\x83a\x02@V[`\x01`\x01`\xA0\x1B\x03\x16\x90RV[` \x81\x015`\x80\x84\x01Ra\x1A\xBBa\x1A\xAFa\x1Aoa\x1APa\x1A=`@\x86\x01\x86a\x19\xBEV[a\x01 `\xA0\x8A\x01Ra\x01\x80\x89\x01\x91a\x10\xCEV[a\x1A]``\x86\x01\x86a\x19\xBEV[\x88\x83\x03`_\x19\x01`\xC0\x8A\x01R\x90a\x10\xCEV[`\x80\x84\x015`\xE0\x87\x01R`\xA0\x84\x015a\x01\0\x87\x01R`\xC0\x84\x015a\x01 \x87\x01Ra\x1A\x9C`\xE0\x85\x01\x85a\x19\xBEV[\x87\x83\x03`_\x19\x01a\x01@\x89\x01R\x90a\x10\xCEV[\x91a\x01\0\x81\x01\x90a\x19\xBEV[\x84\x83\x03`_\x19\x01a\x01`\x86\x01R\x90a\x10\xCEV[\x93` \x82\x01R`@\x81\x85\x03\x91\x01Ra\x10\xCEV[\x90a\x1A\xF0a\x01\0\x83\x01\x83a\x12\x99V[a\x1A\xFC\x81\x94\x92\x94a\x186V[a\x1B\xE7W`\x13\x81\x11a\x1B\x17Wc\x1AY\xF0\xEB`\xE1\x1B_R`\x04_\xFD[a\x1B*a\x04\xDCa\x11Qa\x11K\x84\x88a\x10\x02V[\x93a\x1B:a\x11ma\x11f\x87a\x10~V[a\x1B\xCBW\x91a\x1BP\x82a\x1Bn\x95\x93` \x95a\x10\x10V[`@Qc\\\xACRc`\xE0\x1B\x81R\x95\x86\x94\x85\x94\x91\x93\x91`\x04\x86\x01a\x19\xEFV[\x03\x81`\x01`\x01`\xA0\x1B\x03\x86\x16Z\xFA_\x91\x81a\x1B\xAAW[Pa\x1B\xA5Wc-\xCC*'`\xE1\x1B_R`\x01`\x01`\xA0\x1B\x03\x82\x16`\x04R`$_\xFD[\x90P\x90V[a\x1B\xC4\x91\x92P` =` \x11a\t\x97Wa\t\x89\x81\x83a\x02aV[\x90_a\x1B\x84V[c0\xE3\xA09`\xE2\x1B_R`\x01`\x01`\xA0\x1B\x03\x85\x16`\x04R`$_\xFD[\x91a\x1B\xF3\x93\x91Pa\x18iV[\x15a\x1B\xFCW_\x90V[`\x01\x90V[03\x14\x80\x15a\x1C\x18W[a\x02K\x900\x903\x90a\x19}V[Pa\x02Ka\x1C%3a\x16\x84V[\x90Pa\x1C\x0BV[_\x92\x91`\x03\x81\x11a\x1C;WPPV[\x90\x91\x92P`\x04\x11a\x01\x8CW5`\x01`\x01`\xE0\x1B\x03\x19\x16\x90V[\x91\x90`\x14R`4Rc\xA9\x05\x9C\xBB``\x1B_R` _`D`\x10\x82\x85Z\xF1\x90\x81`\x01_Q\x14\x16\x15a\x1C\x87W[PP_`4RV[;\x15=\x17\x10\x15a\x1C\x98W_\x80a\x1C\x7FV[c\x90\xB8\xEC\x18_R`\x04`\x1C\xFD\xFE\xA1dsolcC\0\x08\"\0\n",
    );
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Erc6909Op(u8);
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[automatically_derived]
        impl alloy_sol_types::private::SolTypeValue<Erc6909Op> for u8 {
            #[inline]
            fn stv_to_tokens(
                &self,
            ) -> <alloy::sol_types::sol_data::Uint<
                8,
            > as alloy_sol_types::SolType>::Token<'_> {
                alloy_sol_types::private::SolTypeValue::<
                    alloy::sol_types::sol_data::Uint<8>,
                >::stv_to_tokens(self)
            }
            #[inline]
            fn stv_eip712_data_word(&self) -> alloy_sol_types::Word {
                <alloy::sol_types::sol_data::Uint<
                    8,
                > as alloy_sol_types::SolType>::tokenize(self)
                    .0
            }
            #[inline]
            fn stv_abi_encode_packed_to(
                &self,
                out: &mut alloy_sol_types::private::Vec<u8>,
            ) {
                <alloy::sol_types::sol_data::Uint<
                    8,
                > as alloy_sol_types::SolType>::abi_encode_packed_to(self, out)
            }
            #[inline]
            fn stv_abi_packed_encoded_size(&self) -> usize {
                <alloy::sol_types::sol_data::Uint<
                    8,
                > as alloy_sol_types::SolType>::abi_encoded_size(self)
            }
        }
        impl Erc6909Op {
            /// The Solidity type name.
            pub const NAME: &'static str = stringify!(@ name);
            /// Convert from the underlying value type.
            #[inline]
            pub const fn from_underlying(value: u8) -> Self {
                Self(value)
            }
            /// Return the underlying value.
            #[inline]
            pub const fn into_underlying(self) -> u8 {
                self.0
            }
            /// Return the single encoding of this value, delegating to the
            /// underlying type.
            #[inline]
            pub fn abi_encode(&self) -> alloy_sol_types::private::Vec<u8> {
                <Self as alloy_sol_types::SolType>::abi_encode(&self.0)
            }
            /// Return the packed encoding of this value, delegating to the
            /// underlying type.
            #[inline]
            pub fn abi_encode_packed(&self) -> alloy_sol_types::private::Vec<u8> {
                <Self as alloy_sol_types::SolType>::abi_encode_packed(&self.0)
            }
        }
        #[automatically_derived]
        impl From<u8> for Erc6909Op {
            fn from(value: u8) -> Self {
                Self::from_underlying(value)
            }
        }
        #[automatically_derived]
        impl From<Erc6909Op> for u8 {
            fn from(value: Erc6909Op) -> Self {
                value.into_underlying()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolType for Erc6909Op {
            type RustType = u8;
            type Token<'a> = <alloy::sol_types::sol_data::Uint<
                8,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SOL_NAME: &'static str = Self::NAME;
            const ENCODED_SIZE: Option<usize> = <alloy::sol_types::sol_data::Uint<
                8,
            > as alloy_sol_types::SolType>::ENCODED_SIZE;
            const PACKED_ENCODED_SIZE: Option<usize> = <alloy::sol_types::sol_data::Uint<
                8,
            > as alloy_sol_types::SolType>::PACKED_ENCODED_SIZE;
            #[inline]
            fn valid_token(token: &Self::Token<'_>) -> bool {
                Self::type_check(token).is_ok()
            }
            #[inline]
            fn type_check(token: &Self::Token<'_>) -> alloy_sol_types::Result<()> {
                <alloy::sol_types::sol_data::Uint<
                    8,
                > as alloy_sol_types::SolType>::type_check(token)
            }
            #[inline]
            fn detokenize(token: Self::Token<'_>) -> Self::RustType {
                <alloy::sol_types::sol_data::Uint<
                    8,
                > as alloy_sol_types::SolType>::detokenize(token)
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::EventTopic for Erc6909Op {
            #[inline]
            fn topic_preimage_length(rust: &Self::RustType) -> usize {
                <alloy::sol_types::sol_data::Uint<
                    8,
                > as alloy_sol_types::EventTopic>::topic_preimage_length(rust)
            }
            #[inline]
            fn encode_topic_preimage(
                rust: &Self::RustType,
                out: &mut alloy_sol_types::private::Vec<u8>,
            ) {
                <alloy::sol_types::sol_data::Uint<
                    8,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(rust, out)
            }
            #[inline]
            fn encode_topic(
                rust: &Self::RustType,
            ) -> alloy_sol_types::abi::token::WordToken {
                <alloy::sol_types::sol_data::Uint<
                    8,
                > as alloy_sol_types::EventTopic>::encode_topic(rust)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**```solidity
struct Erc6909Call { Erc6909Op op; address token; address counterparty; uint256 id; uint256 amount; bool approved; }
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Erc6909Call {
        #[allow(missing_docs)]
        pub op: <Erc6909Op as alloy::sol_types::SolType>::RustType,
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub counterparty: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub id: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub amount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub approved: bool,
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
            Erc6909Op,
            alloy::sol_types::sol_data::Address,
            alloy::sol_types::sol_data::Address,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Bool,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
            <Erc6909Op as alloy::sol_types::SolType>::RustType,
            alloy::sol_types::private::Address,
            alloy::sol_types::private::Address,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::primitives::aliases::U256,
            bool,
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
        impl ::core::convert::From<Erc6909Call> for UnderlyingRustTuple<'_> {
            fn from(value: Erc6909Call) -> Self {
                (
                    value.op,
                    value.token,
                    value.counterparty,
                    value.id,
                    value.amount,
                    value.approved,
                )
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for Erc6909Call {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    op: tuple.0,
                    token: tuple.1,
                    counterparty: tuple.2,
                    id: tuple.3,
                    amount: tuple.4,
                    approved: tuple.5,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolValue for Erc6909Call {
            type SolType = Self;
        }
        #[automatically_derived]
        impl alloy_sol_types::private::SolTypeValue<Self> for Erc6909Call {
            #[inline]
            fn stv_to_tokens(&self) -> <Self as alloy_sol_types::SolType>::Token<'_> {
                (
                    <Erc6909Op as alloy_sol_types::SolType>::tokenize(&self.op),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.token,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.counterparty,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.id),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amount),
                    <alloy::sol_types::sol_data::Bool as alloy_sol_types::SolType>::tokenize(
                        &self.approved,
                    ),
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
        impl alloy_sol_types::SolType for Erc6909Call {
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
        impl alloy_sol_types::SolStruct for Erc6909Call {
            const NAME: &'static str = "Erc6909Call";
            #[inline]
            fn eip712_root_type() -> alloy_sol_types::private::Cow<'static, str> {
                alloy_sol_types::private::Cow::Borrowed(
                    "Erc6909Call(uint8 op,address token,address counterparty,uint256 id,uint256 amount,bool approved)",
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
                    <Erc6909Op as alloy_sol_types::SolType>::eip712_data_word(&self.op)
                        .0,
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::eip712_data_word(
                            &self.token,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::eip712_data_word(
                            &self.counterparty,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.id)
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.amount)
                        .0,
                    <alloy::sol_types::sol_data::Bool as alloy_sol_types::SolType>::eip712_data_word(
                            &self.approved,
                        )
                        .0,
                ]
                    .concat()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::EventTopic for Erc6909Call {
            #[inline]
            fn topic_preimage_length(rust: &Self::RustType) -> usize {
                0usize
                    + <Erc6909Op as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.op,
                    )
                    + <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.token,
                    )
                    + <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.counterparty,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(&rust.id)
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.amount,
                    )
                    + <alloy::sol_types::sol_data::Bool as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.approved,
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
                <Erc6909Op as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.op,
                    out,
                );
                <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.token,
                    out,
                );
                <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.counterparty,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(&rust.id, out);
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.amount,
                    out,
                );
                <alloy::sol_types::sol_data::Bool as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.approved,
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
struct PackedUserOperation { address sender; uint256 nonce; bytes initCode; bytes callData; bytes32 accountGasLimits; uint256 preVerificationGas; bytes32 gasFees; bytes paymasterAndData; bytes signature; }
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct PackedUserOperation {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub nonce: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub initCode: alloy::sol_types::private::Bytes,
        #[allow(missing_docs)]
        pub callData: alloy::sol_types::private::Bytes,
        #[allow(missing_docs)]
        pub accountGasLimits: alloy::sol_types::private::FixedBytes<32>,
        #[allow(missing_docs)]
        pub preVerificationGas: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub gasFees: alloy::sol_types::private::FixedBytes<32>,
        #[allow(missing_docs)]
        pub paymasterAndData: alloy::sol_types::private::Bytes,
        #[allow(missing_docs)]
        pub signature: alloy::sol_types::private::Bytes,
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
            alloy::sol_types::sol_data::Address,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Bytes,
            alloy::sol_types::sol_data::Bytes,
            alloy::sol_types::sol_data::FixedBytes<32>,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::FixedBytes<32>,
            alloy::sol_types::sol_data::Bytes,
            alloy::sol_types::sol_data::Bytes,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
            alloy::sol_types::private::Address,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::Bytes,
            alloy::sol_types::private::Bytes,
            alloy::sol_types::private::FixedBytes<32>,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::FixedBytes<32>,
            alloy::sol_types::private::Bytes,
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
        impl ::core::convert::From<PackedUserOperation> for UnderlyingRustTuple<'_> {
            fn from(value: PackedUserOperation) -> Self {
                (
                    value.sender,
                    value.nonce,
                    value.initCode,
                    value.callData,
                    value.accountGasLimits,
                    value.preVerificationGas,
                    value.gasFees,
                    value.paymasterAndData,
                    value.signature,
                )
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for PackedUserOperation {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    sender: tuple.0,
                    nonce: tuple.1,
                    initCode: tuple.2,
                    callData: tuple.3,
                    accountGasLimits: tuple.4,
                    preVerificationGas: tuple.5,
                    gasFees: tuple.6,
                    paymasterAndData: tuple.7,
                    signature: tuple.8,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolValue for PackedUserOperation {
            type SolType = Self;
        }
        #[automatically_derived]
        impl alloy_sol_types::private::SolTypeValue<Self> for PackedUserOperation {
            #[inline]
            fn stv_to_tokens(&self) -> <Self as alloy_sol_types::SolType>::Token<'_> {
                (
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.sender,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.nonce),
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self.initCode,
                    ),
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self.callData,
                    ),
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.accountGasLimits),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.preVerificationGas),
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.gasFees),
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self.paymasterAndData,
                    ),
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self.signature,
                    ),
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
        impl alloy_sol_types::SolType for PackedUserOperation {
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
        impl alloy_sol_types::SolStruct for PackedUserOperation {
            const NAME: &'static str = "PackedUserOperation";
            #[inline]
            fn eip712_root_type() -> alloy_sol_types::private::Cow<'static, str> {
                alloy_sol_types::private::Cow::Borrowed(
                    "PackedUserOperation(address sender,uint256 nonce,bytes initCode,bytes callData,bytes32 accountGasLimits,uint256 preVerificationGas,bytes32 gasFees,bytes paymasterAndData,bytes signature)",
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
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::eip712_data_word(
                            &self.sender,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.nonce)
                        .0,
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::eip712_data_word(
                            &self.initCode,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::eip712_data_word(
                            &self.callData,
                        )
                        .0,
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::eip712_data_word(
                            &self.accountGasLimits,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(
                            &self.preVerificationGas,
                        )
                        .0,
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.gasFees)
                        .0,
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::eip712_data_word(
                            &self.paymasterAndData,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::eip712_data_word(
                            &self.signature,
                        )
                        .0,
                ]
                    .concat()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::EventTopic for PackedUserOperation {
            #[inline]
            fn topic_preimage_length(rust: &Self::RustType) -> usize {
                0usize
                    + <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.sender,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(&rust.nonce)
                    + <alloy::sol_types::sol_data::Bytes as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.initCode,
                    )
                    + <alloy::sol_types::sol_data::Bytes as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.callData,
                    )
                    + <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.accountGasLimits,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.preVerificationGas,
                    )
                    + <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.gasFees,
                    )
                    + <alloy::sol_types::sol_data::Bytes as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.paymasterAndData,
                    )
                    + <alloy::sol_types::sol_data::Bytes as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.signature,
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
                <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.sender,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.nonce,
                    out,
                );
                <alloy::sol_types::sol_data::Bytes as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.initCode,
                    out,
                );
                <alloy::sol_types::sol_data::Bytes as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.callData,
                    out,
                );
                <alloy::sol_types::sol_data::FixedBytes<
                    32,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.accountGasLimits,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.preVerificationGas,
                    out,
                );
                <alloy::sol_types::sol_data::FixedBytes<
                    32,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.gasFees,
                    out,
                );
                <alloy::sol_types::sol_data::Bytes as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.paymasterAndData,
                    out,
                );
                <alloy::sol_types::sol_data::Bytes as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.signature,
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
    /**Custom error with signature `Erc6909Op_SetOperatorRequiresZeroIdAndAmount()` and selector `0x83ea43f2`.
```solidity
error Erc6909Op_SetOperatorRequiresZeroIdAndAmount();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Erc6909Op_SetOperatorRequiresZeroIdAndAmount;
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
        impl ::core::convert::From<Erc6909Op_SetOperatorRequiresZeroIdAndAmount>
        for UnderlyingRustTuple<'_> {
            fn from(value: Erc6909Op_SetOperatorRequiresZeroIdAndAmount) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for Erc6909Op_SetOperatorRequiresZeroIdAndAmount {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for Erc6909Op_SetOperatorRequiresZeroIdAndAmount {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "Erc6909Op_SetOperatorRequiresZeroIdAndAmount()";
            const SELECTOR: [u8; 4] = [131u8, 234u8, 67u8, 242u8];
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
    /**Custom error with signature `Erc6909Op_TransferRequiresZeroApproved()` and selector `0xb026d5a3`.
```solidity
error Erc6909Op_TransferRequiresZeroApproved();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Erc6909Op_TransferRequiresZeroApproved;
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
        impl ::core::convert::From<Erc6909Op_TransferRequiresZeroApproved>
        for UnderlyingRustTuple<'_> {
            fn from(value: Erc6909Op_TransferRequiresZeroApproved) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for Erc6909Op_TransferRequiresZeroApproved {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for Erc6909Op_TransferRequiresZeroApproved {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "Erc6909Op_TransferRequiresZeroApproved()";
            const SELECTOR: [u8; 4] = [176u8, 38u8, 213u8, 163u8];
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
    /**Custom error with signature `Erc6909SetOperatorBlocked()` and selector `0x1fb7cca5`.
```solidity
error Erc6909SetOperatorBlocked();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Erc6909SetOperatorBlocked;
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
        impl ::core::convert::From<Erc6909SetOperatorBlocked>
        for UnderlyingRustTuple<'_> {
            fn from(value: Erc6909SetOperatorBlocked) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for Erc6909SetOperatorBlocked {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for Erc6909SetOperatorBlocked {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "Erc6909SetOperatorBlocked()";
            const SELECTOR: [u8; 4] = [31u8, 183u8, 204u8, 165u8];
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
    /**Custom error with signature `ExecuteError(uint256,bytes)` and selector `0x5a154675`.
```solidity
error ExecuteError(uint256 index, bytes error);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ExecuteError {
        #[allow(missing_docs)]
        pub index: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub error: alloy::sol_types::private::Bytes,
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
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Bytes,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
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
        impl ::core::convert::From<ExecuteError> for UnderlyingRustTuple<'_> {
            fn from(value: ExecuteError) -> Self {
                (value.index, value.error)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for ExecuteError {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    index: tuple.0,
                    error: tuple.1,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for ExecuteError {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "ExecuteError(uint256,bytes)";
            const SELECTOR: [u8; 4] = [90u8, 21u8, 70u8, 117u8];
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
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.index),
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self.error,
                    ),
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
    /**Custom error with signature `InvalidParams()` and selector `0xa86b6512`.
```solidity
error InvalidParams();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct InvalidParams;
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
        impl ::core::convert::From<InvalidParams> for UnderlyingRustTuple<'_> {
            fn from(value: InvalidParams) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for InvalidParams {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for InvalidParams {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "InvalidParams()";
            const SELECTOR: [u8; 4] = [168u8, 107u8, 101u8, 18u8];
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
    /**Custom error with signature `MevBotDelegate__Erc6909SetOperatorFailed()` and selector `0x855868ba`.
```solidity
error MevBotDelegate__Erc6909SetOperatorFailed();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MevBotDelegate__Erc6909SetOperatorFailed;
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
        impl ::core::convert::From<MevBotDelegate__Erc6909SetOperatorFailed>
        for UnderlyingRustTuple<'_> {
            fn from(value: MevBotDelegate__Erc6909SetOperatorFailed) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for MevBotDelegate__Erc6909SetOperatorFailed {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for MevBotDelegate__Erc6909SetOperatorFailed {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "MevBotDelegate__Erc6909SetOperatorFailed()";
            const SELECTOR: [u8; 4] = [133u8, 88u8, 104u8, 186u8];
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
    /**Custom error with signature `MevBotDelegate__Erc6909TransferFailed()` and selector `0x5e2e42c5`.
```solidity
error MevBotDelegate__Erc6909TransferFailed();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MevBotDelegate__Erc6909TransferFailed;
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
        impl ::core::convert::From<MevBotDelegate__Erc6909TransferFailed>
        for UnderlyingRustTuple<'_> {
            fn from(value: MevBotDelegate__Erc6909TransferFailed) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for MevBotDelegate__Erc6909TransferFailed {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for MevBotDelegate__Erc6909TransferFailed {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "MevBotDelegate__Erc6909TransferFailed()";
            const SELECTOR: [u8; 4] = [94u8, 46u8, 66u8, 197u8];
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
    /**Custom error with signature `MevBotDelegate__InvalidSignatureLength()` and selector `0x34b3e1d6`.
```solidity
error MevBotDelegate__InvalidSignatureLength();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MevBotDelegate__InvalidSignatureLength;
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
        impl ::core::convert::From<MevBotDelegate__InvalidSignatureLength>
        for UnderlyingRustTuple<'_> {
            fn from(value: MevBotDelegate__InvalidSignatureLength) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for MevBotDelegate__InvalidSignatureLength {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for MevBotDelegate__InvalidSignatureLength {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "MevBotDelegate__InvalidSignatureLength()";
            const SELECTOR: [u8; 4] = [52u8, 179u8, 225u8, 214u8];
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
    /**Custom error with signature `MevBotDelegate__InvalidValidator(address)` and selector `0x68b44dd8`.
```solidity
error MevBotDelegate__InvalidValidator(address validator);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MevBotDelegate__InvalidValidator {
        #[allow(missing_docs)]
        pub validator: alloy::sol_types::private::Address,
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
        type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Address,);
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (alloy::sol_types::private::Address,);
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
        impl ::core::convert::From<MevBotDelegate__InvalidValidator>
        for UnderlyingRustTuple<'_> {
            fn from(value: MevBotDelegate__InvalidValidator) -> Self {
                (value.validator,)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for MevBotDelegate__InvalidValidator {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self { validator: tuple.0 }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for MevBotDelegate__InvalidValidator {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "MevBotDelegate__InvalidValidator(address)";
            const SELECTOR: [u8; 4] = [104u8, 180u8, 77u8, 216u8];
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
                        &self.validator,
                    ),
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
    /**Custom error with signature `MevBotDelegate__Unauthorized()` and selector `0xbc3a81bd`.
```solidity
error MevBotDelegate__Unauthorized();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MevBotDelegate__Unauthorized;
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
        impl ::core::convert::From<MevBotDelegate__Unauthorized>
        for UnderlyingRustTuple<'_> {
            fn from(value: MevBotDelegate__Unauthorized) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for MevBotDelegate__Unauthorized {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for MevBotDelegate__Unauthorized {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "MevBotDelegate__Unauthorized()";
            const SELECTOR: [u8; 4] = [188u8, 58u8, 129u8, 189u8];
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
    /**Custom error with signature `MevBotDelegate__ValidatorNotEnabled(address)` and selector `0xc38e80e4`.
```solidity
error MevBotDelegate__ValidatorNotEnabled(address validator);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MevBotDelegate__ValidatorNotEnabled {
        #[allow(missing_docs)]
        pub validator: alloy::sol_types::private::Address,
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
        type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Address,);
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (alloy::sol_types::private::Address,);
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
        impl ::core::convert::From<MevBotDelegate__ValidatorNotEnabled>
        for UnderlyingRustTuple<'_> {
            fn from(value: MevBotDelegate__ValidatorNotEnabled) -> Self {
                (value.validator,)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for MevBotDelegate__ValidatorNotEnabled {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self { validator: tuple.0 }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for MevBotDelegate__ValidatorNotEnabled {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "MevBotDelegate__ValidatorNotEnabled(address)";
            const SELECTOR: [u8; 4] = [195u8, 142u8, 128u8, 228u8];
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
                        &self.validator,
                    ),
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
    /**Custom error with signature `MevBotDelegate__ValidatorReverted(address)` and selector `0x5b98544e`.
```solidity
error MevBotDelegate__ValidatorReverted(address validator);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MevBotDelegate__ValidatorReverted {
        #[allow(missing_docs)]
        pub validator: alloy::sol_types::private::Address,
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
        type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Address,);
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (alloy::sol_types::private::Address,);
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
        impl ::core::convert::From<MevBotDelegate__ValidatorReverted>
        for UnderlyingRustTuple<'_> {
            fn from(value: MevBotDelegate__ValidatorReverted) -> Self {
                (value.validator,)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for MevBotDelegate__ValidatorReverted {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self { validator: tuple.0 }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for MevBotDelegate__ValidatorReverted {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "MevBotDelegate__ValidatorReverted(address)";
            const SELECTOR: [u8; 4] = [91u8, 152u8, 84u8, 78u8];
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
                        &self.validator,
                    ),
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
    /**Custom error with signature `MevBotDelegate__ZeroTarget()` and selector `0x32988b1c`.
```solidity
error MevBotDelegate__ZeroTarget();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MevBotDelegate__ZeroTarget;
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
        impl ::core::convert::From<MevBotDelegate__ZeroTarget>
        for UnderlyingRustTuple<'_> {
            fn from(value: MevBotDelegate__ZeroTarget) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for MevBotDelegate__ZeroTarget {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for MevBotDelegate__ZeroTarget {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "MevBotDelegate__ZeroTarget()";
            const SELECTOR: [u8; 4] = [50u8, 152u8, 139u8, 28u8];
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
    /**Custom error with signature `NativeSweepFailed()` and selector `0x16b452f7`.
```solidity
error NativeSweepFailed();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct NativeSweepFailed;
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
        impl ::core::convert::From<NativeSweepFailed> for UnderlyingRustTuple<'_> {
            fn from(value: NativeSweepFailed) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for NativeSweepFailed {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for NativeSweepFailed {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "NativeSweepFailed()";
            const SELECTOR: [u8; 4] = [22u8, 180u8, 82u8, 247u8];
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
    /**Custom error with signature `NotFromEntryPoint(address,address,address)` and selector `0xfe34a6d3`.
```solidity
error NotFromEntryPoint(address msgSender, address entity, address entryPoint);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct NotFromEntryPoint {
        #[allow(missing_docs)]
        pub msgSender: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub entity: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub entryPoint: alloy::sol_types::private::Address,
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
            alloy::sol_types::sol_data::Address,
            alloy::sol_types::sol_data::Address,
            alloy::sol_types::sol_data::Address,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
            alloy::sol_types::private::Address,
            alloy::sol_types::private::Address,
            alloy::sol_types::private::Address,
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
        impl ::core::convert::From<NotFromEntryPoint> for UnderlyingRustTuple<'_> {
            fn from(value: NotFromEntryPoint) -> Self {
                (value.msgSender, value.entity, value.entryPoint)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for NotFromEntryPoint {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    msgSender: tuple.0,
                    entity: tuple.1,
                    entryPoint: tuple.2,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for NotFromEntryPoint {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "NotFromEntryPoint(address,address,address)";
            const SELECTOR: [u8; 4] = [254u8, 52u8, 166u8, 211u8];
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
                        &self.msgSender,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.entity,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.entryPoint,
                    ),
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
    /**Event with signature `BotDelegateActivated(address,uint256,bytes4)` and selector `0x02cb5483f9c6b769c7b58fd7bd050427ea566bd357c86f6f5c724c42c0e1745a`.
```solidity
event BotDelegateActivated(address indexed target, uint256 value, bytes4 selector);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct BotDelegateActivated {
        #[allow(missing_docs)]
        pub target: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub value: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub selector: alloy::sol_types::private::FixedBytes<4>,
    }
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[automatically_derived]
        impl alloy_sol_types::SolEvent for BotDelegateActivated {
            type DataTuple<'a> = (
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::FixedBytes<4>,
            );
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "BotDelegateActivated(address,uint256,bytes4)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                2u8, 203u8, 84u8, 131u8, 249u8, 198u8, 183u8, 105u8, 199u8, 181u8, 143u8,
                215u8, 189u8, 5u8, 4u8, 39u8, 234u8, 86u8, 107u8, 211u8, 87u8, 200u8,
                111u8, 111u8, 92u8, 114u8, 76u8, 66u8, 192u8, 225u8, 116u8, 90u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    target: topics.1,
                    value: data.0,
                    selector: data.1,
                }
            }
            #[inline]
            fn check_signature(
                topics: &<Self::TopicList as alloy_sol_types::SolType>::RustType,
            ) -> alloy_sol_types::Result<()> {
                if topics.0 != Self::SIGNATURE_HASH {
                    return Err(
                        alloy_sol_types::Error::invalid_event_signature_hash(
                            Self::SIGNATURE,
                            topics.0,
                            Self::SIGNATURE_HASH,
                        ),
                    );
                }
                Ok(())
            }
            #[inline]
            fn tokenize_body(&self) -> Self::DataToken<'_> {
                (
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.value),
                    <alloy::sol_types::sol_data::FixedBytes<
                        4,
                    > as alloy_sol_types::SolType>::tokenize(&self.selector),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(), self.target.clone())
            }
            #[inline]
            fn encode_topics_raw(
                &self,
                out: &mut [alloy_sol_types::abi::token::WordToken],
            ) -> alloy_sol_types::Result<()> {
                if out.len() < <Self::TopicList as alloy_sol_types::TopicList>::COUNT {
                    return Err(alloy_sol_types::Error::Overrun);
                }
                out[0usize] = alloy_sol_types::abi::token::WordToken(
                    Self::SIGNATURE_HASH,
                );
                out[1usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.target,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for BotDelegateActivated {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&BotDelegateActivated> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &BotDelegateActivated) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `Erc20Swept(address,address,uint256)` and selector `0x6120f899b040ab0e38c9059d176d2dfb56273e93b7aa559635f65c5ec84ec913`.
```solidity
event Erc20Swept(address indexed token, address indexed to, uint256 amount);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct Erc20Swept {
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub to: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub amount: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[automatically_derived]
        impl alloy_sol_types::SolEvent for Erc20Swept {
            type DataTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "Erc20Swept(address,address,uint256)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                97u8, 32u8, 248u8, 153u8, 176u8, 64u8, 171u8, 14u8, 56u8, 201u8, 5u8,
                157u8, 23u8, 109u8, 45u8, 251u8, 86u8, 39u8, 62u8, 147u8, 183u8, 170u8,
                85u8, 150u8, 53u8, 246u8, 92u8, 94u8, 200u8, 78u8, 201u8, 19u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    token: topics.1,
                    to: topics.2,
                    amount: data.0,
                }
            }
            #[inline]
            fn check_signature(
                topics: &<Self::TopicList as alloy_sol_types::SolType>::RustType,
            ) -> alloy_sol_types::Result<()> {
                if topics.0 != Self::SIGNATURE_HASH {
                    return Err(
                        alloy_sol_types::Error::invalid_event_signature_hash(
                            Self::SIGNATURE,
                            topics.0,
                            Self::SIGNATURE_HASH,
                        ),
                    );
                }
                Ok(())
            }
            #[inline]
            fn tokenize_body(&self) -> Self::DataToken<'_> {
                (
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amount),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(), self.token.clone(), self.to.clone())
            }
            #[inline]
            fn encode_topics_raw(
                &self,
                out: &mut [alloy_sol_types::abi::token::WordToken],
            ) -> alloy_sol_types::Result<()> {
                if out.len() < <Self::TopicList as alloy_sol_types::TopicList>::COUNT {
                    return Err(alloy_sol_types::Error::Overrun);
                }
                out[0usize] = alloy_sol_types::abi::token::WordToken(
                    Self::SIGNATURE_HASH,
                );
                out[1usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.token,
                );
                out[2usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.to,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for Erc20Swept {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&Erc20Swept> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &Erc20Swept) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `Erc3009Received(address,address,uint256,bytes32)` and selector `0x2e3e88ccc3a3c06646b59ed5964ac16a95a871e0fffcf1156f52a26a8a6626f9`.
```solidity
event Erc3009Received(address indexed token, address indexed from, uint256 value, bytes32 nonce);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct Erc3009Received {
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub from: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub value: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub nonce: alloy::sol_types::private::FixedBytes<32>,
    }
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[automatically_derived]
        impl alloy_sol_types::SolEvent for Erc3009Received {
            type DataTuple<'a> = (
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::FixedBytes<32>,
            );
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "Erc3009Received(address,address,uint256,bytes32)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                46u8, 62u8, 136u8, 204u8, 195u8, 163u8, 192u8, 102u8, 70u8, 181u8, 158u8,
                213u8, 150u8, 74u8, 193u8, 106u8, 149u8, 168u8, 113u8, 224u8, 255u8,
                252u8, 241u8, 21u8, 111u8, 82u8, 162u8, 106u8, 138u8, 102u8, 38u8, 249u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    token: topics.1,
                    from: topics.2,
                    value: data.0,
                    nonce: data.1,
                }
            }
            #[inline]
            fn check_signature(
                topics: &<Self::TopicList as alloy_sol_types::SolType>::RustType,
            ) -> alloy_sol_types::Result<()> {
                if topics.0 != Self::SIGNATURE_HASH {
                    return Err(
                        alloy_sol_types::Error::invalid_event_signature_hash(
                            Self::SIGNATURE,
                            topics.0,
                            Self::SIGNATURE_HASH,
                        ),
                    );
                }
                Ok(())
            }
            #[inline]
            fn tokenize_body(&self) -> Self::DataToken<'_> {
                (
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.value),
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.nonce),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(), self.token.clone(), self.from.clone())
            }
            #[inline]
            fn encode_topics_raw(
                &self,
                out: &mut [alloy_sol_types::abi::token::WordToken],
            ) -> alloy_sol_types::Result<()> {
                if out.len() < <Self::TopicList as alloy_sol_types::TopicList>::COUNT {
                    return Err(alloy_sol_types::Error::Overrun);
                }
                out[0usize] = alloy_sol_types::abi::token::WordToken(
                    Self::SIGNATURE_HASH,
                );
                out[1usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.token,
                );
                out[2usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.from,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for Erc3009Received {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&Erc3009Received> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &Erc3009Received) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `Erc3009Relayed(address,address,address,uint256,bytes32)` and selector `0x728fcd896ed8764f00b06c3ea4847b5011b53fa28be451d6a2acebef60827da0`.
```solidity
event Erc3009Relayed(address indexed token, address indexed from, address indexed to, uint256 value, bytes32 nonce);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct Erc3009Relayed {
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub from: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub to: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub value: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub nonce: alloy::sol_types::private::FixedBytes<32>,
    }
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[automatically_derived]
        impl alloy_sol_types::SolEvent for Erc3009Relayed {
            type DataTuple<'a> = (
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::FixedBytes<32>,
            );
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "Erc3009Relayed(address,address,address,uint256,bytes32)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                114u8, 143u8, 205u8, 137u8, 110u8, 216u8, 118u8, 79u8, 0u8, 176u8, 108u8,
                62u8, 164u8, 132u8, 123u8, 80u8, 17u8, 181u8, 63u8, 162u8, 139u8, 228u8,
                81u8, 214u8, 162u8, 172u8, 235u8, 239u8, 96u8, 130u8, 125u8, 160u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    token: topics.1,
                    from: topics.2,
                    to: topics.3,
                    value: data.0,
                    nonce: data.1,
                }
            }
            #[inline]
            fn check_signature(
                topics: &<Self::TopicList as alloy_sol_types::SolType>::RustType,
            ) -> alloy_sol_types::Result<()> {
                if topics.0 != Self::SIGNATURE_HASH {
                    return Err(
                        alloy_sol_types::Error::invalid_event_signature_hash(
                            Self::SIGNATURE,
                            topics.0,
                            Self::SIGNATURE_HASH,
                        ),
                    );
                }
                Ok(())
            }
            #[inline]
            fn tokenize_body(&self) -> Self::DataToken<'_> {
                (
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.value),
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.nonce),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (
                    Self::SIGNATURE_HASH.into(),
                    self.token.clone(),
                    self.from.clone(),
                    self.to.clone(),
                )
            }
            #[inline]
            fn encode_topics_raw(
                &self,
                out: &mut [alloy_sol_types::abi::token::WordToken],
            ) -> alloy_sol_types::Result<()> {
                if out.len() < <Self::TopicList as alloy_sol_types::TopicList>::COUNT {
                    return Err(alloy_sol_types::Error::Overrun);
                }
                out[0usize] = alloy_sol_types::abi::token::WordToken(
                    Self::SIGNATURE_HASH,
                );
                out[1usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.token,
                );
                out[2usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.from,
                );
                out[3usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.to,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for Erc3009Relayed {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&Erc3009Relayed> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &Erc3009Relayed) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `Erc6909OperatorSet(address,address,bool)` and selector `0x9c8e17fa114d24cfc8f67c3d6ce6bc2e24067dbe41256640bc48fd6d1066562f`.
```solidity
event Erc6909OperatorSet(address indexed token, address indexed operator, bool approved);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct Erc6909OperatorSet {
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub operator: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub approved: bool,
    }
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[automatically_derived]
        impl alloy_sol_types::SolEvent for Erc6909OperatorSet {
            type DataTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "Erc6909OperatorSet(address,address,bool)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                156u8, 142u8, 23u8, 250u8, 17u8, 77u8, 36u8, 207u8, 200u8, 246u8, 124u8,
                61u8, 108u8, 230u8, 188u8, 46u8, 36u8, 6u8, 125u8, 190u8, 65u8, 37u8,
                102u8, 64u8, 188u8, 72u8, 253u8, 109u8, 16u8, 102u8, 86u8, 47u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    token: topics.1,
                    operator: topics.2,
                    approved: data.0,
                }
            }
            #[inline]
            fn check_signature(
                topics: &<Self::TopicList as alloy_sol_types::SolType>::RustType,
            ) -> alloy_sol_types::Result<()> {
                if topics.0 != Self::SIGNATURE_HASH {
                    return Err(
                        alloy_sol_types::Error::invalid_event_signature_hash(
                            Self::SIGNATURE,
                            topics.0,
                            Self::SIGNATURE_HASH,
                        ),
                    );
                }
                Ok(())
            }
            #[inline]
            fn tokenize_body(&self) -> Self::DataToken<'_> {
                (
                    <alloy::sol_types::sol_data::Bool as alloy_sol_types::SolType>::tokenize(
                        &self.approved,
                    ),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(), self.token.clone(), self.operator.clone())
            }
            #[inline]
            fn encode_topics_raw(
                &self,
                out: &mut [alloy_sol_types::abi::token::WordToken],
            ) -> alloy_sol_types::Result<()> {
                if out.len() < <Self::TopicList as alloy_sol_types::TopicList>::COUNT {
                    return Err(alloy_sol_types::Error::Overrun);
                }
                out[0usize] = alloy_sol_types::abi::token::WordToken(
                    Self::SIGNATURE_HASH,
                );
                out[1usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.token,
                );
                out[2usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.operator,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for Erc6909OperatorSet {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&Erc6909OperatorSet> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &Erc6909OperatorSet) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `Erc6909Transferred(address,address,uint256,uint256)` and selector `0x4a94f89e131699ed3416670c011ce64d62e5a581a4ebb4603bf6c4a5d06a06ce`.
```solidity
event Erc6909Transferred(address indexed token, address indexed to, uint256 indexed id, uint256 amount);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct Erc6909Transferred {
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub to: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub id: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub amount: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[automatically_derived]
        impl alloy_sol_types::SolEvent for Erc6909Transferred {
            type DataTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
            );
            const SIGNATURE: &'static str = "Erc6909Transferred(address,address,uint256,uint256)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                74u8, 148u8, 248u8, 158u8, 19u8, 22u8, 153u8, 237u8, 52u8, 22u8, 103u8,
                12u8, 1u8, 28u8, 230u8, 77u8, 98u8, 229u8, 165u8, 129u8, 164u8, 235u8,
                180u8, 96u8, 59u8, 246u8, 196u8, 165u8, 208u8, 106u8, 6u8, 206u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    token: topics.1,
                    to: topics.2,
                    id: topics.3,
                    amount: data.0,
                }
            }
            #[inline]
            fn check_signature(
                topics: &<Self::TopicList as alloy_sol_types::SolType>::RustType,
            ) -> alloy_sol_types::Result<()> {
                if topics.0 != Self::SIGNATURE_HASH {
                    return Err(
                        alloy_sol_types::Error::invalid_event_signature_hash(
                            Self::SIGNATURE,
                            topics.0,
                            Self::SIGNATURE_HASH,
                        ),
                    );
                }
                Ok(())
            }
            #[inline]
            fn tokenize_body(&self) -> Self::DataToken<'_> {
                (
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amount),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (
                    Self::SIGNATURE_HASH.into(),
                    self.token.clone(),
                    self.to.clone(),
                    self.id.clone(),
                )
            }
            #[inline]
            fn encode_topics_raw(
                &self,
                out: &mut [alloy_sol_types::abi::token::WordToken],
            ) -> alloy_sol_types::Result<()> {
                if out.len() < <Self::TopicList as alloy_sol_types::TopicList>::COUNT {
                    return Err(alloy_sol_types::Error::Overrun);
                }
                out[0usize] = alloy_sol_types::abi::token::WordToken(
                    Self::SIGNATURE_HASH,
                );
                out[1usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.token,
                );
                out[2usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.to,
                );
                out[3usize] = <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic(&self.id);
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for Erc6909Transferred {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&Erc6909Transferred> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &Erc6909Transferred) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `ValidatorAdded(address)` and selector `0xe366c1c0452ed8eec96861e9e54141ebff23c9ec89fe27b996b45f5ec3884987`.
```solidity
event ValidatorAdded(address indexed validator);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct ValidatorAdded {
        #[allow(missing_docs)]
        pub validator: alloy::sol_types::private::Address,
    }
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[automatically_derived]
        impl alloy_sol_types::SolEvent for ValidatorAdded {
            type DataTuple<'a> = ();
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "ValidatorAdded(address)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                227u8, 102u8, 193u8, 192u8, 69u8, 46u8, 216u8, 238u8, 201u8, 104u8, 97u8,
                233u8, 229u8, 65u8, 65u8, 235u8, 255u8, 35u8, 201u8, 236u8, 137u8, 254u8,
                39u8, 185u8, 150u8, 180u8, 95u8, 94u8, 195u8, 136u8, 73u8, 135u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self { validator: topics.1 }
            }
            #[inline]
            fn check_signature(
                topics: &<Self::TopicList as alloy_sol_types::SolType>::RustType,
            ) -> alloy_sol_types::Result<()> {
                if topics.0 != Self::SIGNATURE_HASH {
                    return Err(
                        alloy_sol_types::Error::invalid_event_signature_hash(
                            Self::SIGNATURE,
                            topics.0,
                            Self::SIGNATURE_HASH,
                        ),
                    );
                }
                Ok(())
            }
            #[inline]
            fn tokenize_body(&self) -> Self::DataToken<'_> {
                ()
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(), self.validator.clone())
            }
            #[inline]
            fn encode_topics_raw(
                &self,
                out: &mut [alloy_sol_types::abi::token::WordToken],
            ) -> alloy_sol_types::Result<()> {
                if out.len() < <Self::TopicList as alloy_sol_types::TopicList>::COUNT {
                    return Err(alloy_sol_types::Error::Overrun);
                }
                out[0usize] = alloy_sol_types::abi::token::WordToken(
                    Self::SIGNATURE_HASH,
                );
                out[1usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.validator,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for ValidatorAdded {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&ValidatorAdded> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &ValidatorAdded) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `ValidatorRemoved(address)` and selector `0xe1434e25d6611e0db941968fdc97811c982ac1602e951637d206f5fdda9dd8f1`.
```solidity
event ValidatorRemoved(address indexed validator);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct ValidatorRemoved {
        #[allow(missing_docs)]
        pub validator: alloy::sol_types::private::Address,
    }
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[automatically_derived]
        impl alloy_sol_types::SolEvent for ValidatorRemoved {
            type DataTuple<'a> = ();
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "ValidatorRemoved(address)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                225u8, 67u8, 78u8, 37u8, 214u8, 97u8, 30u8, 13u8, 185u8, 65u8, 150u8,
                143u8, 220u8, 151u8, 129u8, 28u8, 152u8, 42u8, 193u8, 96u8, 46u8, 149u8,
                22u8, 55u8, 210u8, 6u8, 245u8, 253u8, 218u8, 157u8, 216u8, 241u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self { validator: topics.1 }
            }
            #[inline]
            fn check_signature(
                topics: &<Self::TopicList as alloy_sol_types::SolType>::RustType,
            ) -> alloy_sol_types::Result<()> {
                if topics.0 != Self::SIGNATURE_HASH {
                    return Err(
                        alloy_sol_types::Error::invalid_event_signature_hash(
                            Self::SIGNATURE,
                            topics.0,
                            Self::SIGNATURE_HASH,
                        ),
                    );
                }
                Ok(())
            }
            #[inline]
            fn tokenize_body(&self) -> Self::DataToken<'_> {
                ()
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(), self.validator.clone())
            }
            #[inline]
            fn encode_topics_raw(
                &self,
                out: &mut [alloy_sol_types::abi::token::WordToken],
            ) -> alloy_sol_types::Result<()> {
                if out.len() < <Self::TopicList as alloy_sol_types::TopicList>::COUNT {
                    return Err(alloy_sol_types::Error::Overrun);
                }
                out[0usize] = alloy_sol_types::abi::token::WordToken(
                    Self::SIGNATURE_HASH,
                );
                out[1usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.validator,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for ValidatorRemoved {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&ValidatorRemoved> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &ValidatorRemoved) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `ENTRY_POINT_ADDR()` and selector `0xa134f64e`.
```solidity
function ENTRY_POINT_ADDR() external view returns (address);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ENTRY_POINT_ADDRCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`ENTRY_POINT_ADDR()`](ENTRY_POINT_ADDRCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ENTRY_POINT_ADDRReturn {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::Address,
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
            impl ::core::convert::From<ENTRY_POINT_ADDRCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: ENTRY_POINT_ADDRCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for ENTRY_POINT_ADDRCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Address,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (alloy::sol_types::private::Address,);
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
            impl ::core::convert::From<ENTRY_POINT_ADDRReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: ENTRY_POINT_ADDRReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for ENTRY_POINT_ADDRReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for ENTRY_POINT_ADDRCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::Address;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Address,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "ENTRY_POINT_ADDR()";
            const SELECTOR: [u8; 4] = [161u8, 52u8, 246u8, 78u8];
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
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        ret,
                    ),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: ENTRY_POINT_ADDRReturn = r.into();
                        r._0
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
                        let r: ENTRY_POINT_ADDRReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `ENTRY_POINT_V06()` and selector `0x6b50bc91`.
```solidity
function ENTRY_POINT_V06() external view returns (address);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ENTRY_POINT_V06Call;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`ENTRY_POINT_V06()`](ENTRY_POINT_V06Call) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ENTRY_POINT_V06Return {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::Address,
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
            impl ::core::convert::From<ENTRY_POINT_V06Call> for UnderlyingRustTuple<'_> {
                fn from(value: ENTRY_POINT_V06Call) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for ENTRY_POINT_V06Call {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Address,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (alloy::sol_types::private::Address,);
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
            impl ::core::convert::From<ENTRY_POINT_V06Return>
            for UnderlyingRustTuple<'_> {
                fn from(value: ENTRY_POINT_V06Return) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for ENTRY_POINT_V06Return {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for ENTRY_POINT_V06Call {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::Address;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Address,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "ENTRY_POINT_V06()";
            const SELECTOR: [u8; 4] = [107u8, 80u8, 188u8, 145u8];
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
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        ret,
                    ),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: ENTRY_POINT_V06Return = r.into();
                        r._0
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
                        let r: ENTRY_POINT_V06Return = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `ENTRY_POINT_V07()` and selector `0x9195f836`.
```solidity
function ENTRY_POINT_V07() external view returns (address);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ENTRY_POINT_V07Call;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`ENTRY_POINT_V07()`](ENTRY_POINT_V07Call) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ENTRY_POINT_V07Return {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::Address,
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
            impl ::core::convert::From<ENTRY_POINT_V07Call> for UnderlyingRustTuple<'_> {
                fn from(value: ENTRY_POINT_V07Call) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for ENTRY_POINT_V07Call {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Address,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (alloy::sol_types::private::Address,);
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
            impl ::core::convert::From<ENTRY_POINT_V07Return>
            for UnderlyingRustTuple<'_> {
                fn from(value: ENTRY_POINT_V07Return) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for ENTRY_POINT_V07Return {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for ENTRY_POINT_V07Call {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::Address;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Address,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "ENTRY_POINT_V07()";
            const SELECTOR: [u8; 4] = [145u8, 149u8, 248u8, 54u8];
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
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        ret,
                    ),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: ENTRY_POINT_V07Return = r.into();
                        r._0
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
                        let r: ENTRY_POINT_V07Return = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `ENTRY_POINT_V08()` and selector `0x59cd3300`.
```solidity
function ENTRY_POINT_V08() external view returns (address);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ENTRY_POINT_V08Call;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`ENTRY_POINT_V08()`](ENTRY_POINT_V08Call) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ENTRY_POINT_V08Return {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::Address,
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
            impl ::core::convert::From<ENTRY_POINT_V08Call> for UnderlyingRustTuple<'_> {
                fn from(value: ENTRY_POINT_V08Call) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for ENTRY_POINT_V08Call {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Address,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (alloy::sol_types::private::Address,);
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
            impl ::core::convert::From<ENTRY_POINT_V08Return>
            for UnderlyingRustTuple<'_> {
                fn from(value: ENTRY_POINT_V08Return) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for ENTRY_POINT_V08Return {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for ENTRY_POINT_V08Call {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::Address;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Address,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "ENTRY_POINT_V08()";
            const SELECTOR: [u8; 4] = [89u8, 205u8, 51u8, 0u8];
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
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        ret,
                    ),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: ENTRY_POINT_V08Return = r.into();
                        r._0
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
                        let r: ENTRY_POINT_V08Return = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `entryPoint()` and selector `0xb0d691fe`.
```solidity
function entryPoint() external pure returns (address);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct entryPointCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`entryPoint()`](entryPointCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct entryPointReturn {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::Address,
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
            impl ::core::convert::From<entryPointCall> for UnderlyingRustTuple<'_> {
                fn from(value: entryPointCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for entryPointCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Address,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (alloy::sol_types::private::Address,);
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
            impl ::core::convert::From<entryPointReturn> for UnderlyingRustTuple<'_> {
                fn from(value: entryPointReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for entryPointReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for entryPointCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::Address;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Address,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "entryPoint()";
            const SELECTOR: [u8; 4] = [176u8, 214u8, 145u8, 254u8];
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
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        ret,
                    ),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: entryPointReturn = r.into();
                        r._0
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
                        let r: entryPointReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `execute(address,uint256,bytes)` and selector `0xb61d27f6`.
```solidity
function execute(address target, uint256 value, bytes memory data) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct executeCall {
        #[allow(missing_docs)]
        pub target: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub value: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub data: alloy::sol_types::private::Bytes,
    }
    ///Container type for the return parameters of the [`execute(address,uint256,bytes)`](executeCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct executeReturn {}
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
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Bytes,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Address,
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
            impl ::core::convert::From<executeCall> for UnderlyingRustTuple<'_> {
                fn from(value: executeCall) -> Self {
                    (value.target, value.value, value.data)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for executeCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        target: tuple.0,
                        value: tuple.1,
                        data: tuple.2,
                    }
                }
            }
        }
        {
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
            impl ::core::convert::From<executeReturn> for UnderlyingRustTuple<'_> {
                fn from(value: executeReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for executeReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl executeReturn {
            fn _tokenize(
                &self,
            ) -> <executeCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for executeCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Bytes,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = executeReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "execute(address,uint256,bytes)";
            const SELECTOR: [u8; 4] = [182u8, 29u8, 39u8, 246u8];
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
                        &self.target,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.value),
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self.data,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                executeReturn::_tokenize(ret)
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
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `executeBatch((address,uint256,bytes)[])` and selector `0x34fcd5be`.
```solidity
function executeBatch(BaseAccount.Call[] memory calls) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct executeBatchCall {
        #[allow(missing_docs)]
        pub calls: alloy::sol_types::private::Vec<
            <BaseAccount::Call as alloy::sol_types::SolType>::RustType,
        >,
    }
    ///Container type for the return parameters of the [`executeBatch((address,uint256,bytes)[])`](executeBatchCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct executeBatchReturn {}
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
                alloy::sol_types::sol_data::Array<BaseAccount::Call>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Vec<
                    <BaseAccount::Call as alloy::sol_types::SolType>::RustType,
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
            impl ::core::convert::From<executeBatchCall> for UnderlyingRustTuple<'_> {
                fn from(value: executeBatchCall) -> Self {
                    (value.calls,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for executeBatchCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { calls: tuple.0 }
                }
            }
        }
        {
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
            impl ::core::convert::From<executeBatchReturn> for UnderlyingRustTuple<'_> {
                fn from(value: executeBatchReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for executeBatchReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl executeBatchReturn {
            fn _tokenize(
                &self,
            ) -> <executeBatchCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for executeBatchCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Array<BaseAccount::Call>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = executeBatchReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "executeBatch((address,uint256,bytes)[])";
            const SELECTOR: [u8; 4] = [52u8, 252u8, 213u8, 190u8];
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
                        BaseAccount::Call,
                    > as alloy_sol_types::SolType>::tokenize(&self.calls),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                executeBatchReturn::_tokenize(ret)
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
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `executeErc6909Batch((uint8,address,address,uint256,uint256,bool)[])` and selector `0x4203a934`.
```solidity
function executeErc6909Batch(Erc6909Call[] memory calls) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct executeErc6909BatchCall {
        #[allow(missing_docs)]
        pub calls: alloy::sol_types::private::Vec<
            <Erc6909Call as alloy::sol_types::SolType>::RustType,
        >,
    }
    ///Container type for the return parameters of the [`executeErc6909Batch((uint8,address,address,uint256,uint256,bool)[])`](executeErc6909BatchCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct executeErc6909BatchReturn {}
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
                alloy::sol_types::sol_data::Array<Erc6909Call>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Vec<
                    <Erc6909Call as alloy::sol_types::SolType>::RustType,
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
            impl ::core::convert::From<executeErc6909BatchCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: executeErc6909BatchCall) -> Self {
                    (value.calls,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for executeErc6909BatchCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { calls: tuple.0 }
                }
            }
        }
        {
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
            impl ::core::convert::From<executeErc6909BatchReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: executeErc6909BatchReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for executeErc6909BatchReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl executeErc6909BatchReturn {
            fn _tokenize(
                &self,
            ) -> <executeErc6909BatchCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for executeErc6909BatchCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Array<Erc6909Call>,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = executeErc6909BatchReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "executeErc6909Batch((uint8,address,address,uint256,uint256,bool)[])";
            const SELECTOR: [u8; 4] = [66u8, 3u8, 169u8, 52u8];
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
                        Erc6909Call,
                    > as alloy_sol_types::SolType>::tokenize(&self.calls),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                executeErc6909BatchReturn::_tokenize(ret)
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
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `getNonce()` and selector `0xd087d288`.
```solidity
function getNonce() external view returns (uint256);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct getNonceCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`getNonce()`](getNonceCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct getNonceReturn {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::primitives::aliases::U256,
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
            impl ::core::convert::From<getNonceCall> for UnderlyingRustTuple<'_> {
                fn from(value: getNonceCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for getNonceCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
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
            impl ::core::convert::From<getNonceReturn> for UnderlyingRustTuple<'_> {
                fn from(value: getNonceReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for getNonceReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for getNonceCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::primitives::aliases::U256;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "getNonce()";
            const SELECTOR: [u8; 4] = [208u8, 135u8, 210u8, 136u8];
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
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(ret),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: getNonceReturn = r.into();
                        r._0
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
                        let r: getNonceReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `isEntryPoint(address)` and selector `0x7a738545`.
```solidity
function isEntryPoint(address caller) external pure returns (bool);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct isEntryPointCall {
        #[allow(missing_docs)]
        pub caller: alloy::sol_types::private::Address,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`isEntryPoint(address)`](isEntryPointCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct isEntryPointReturn {
        #[allow(missing_docs)]
        pub _0: bool,
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
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Address,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (alloy::sol_types::private::Address,);
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
            impl ::core::convert::From<isEntryPointCall> for UnderlyingRustTuple<'_> {
                fn from(value: isEntryPointCall) -> Self {
                    (value.caller,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for isEntryPointCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { caller: tuple.0 }
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (bool,);
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
            impl ::core::convert::From<isEntryPointReturn> for UnderlyingRustTuple<'_> {
                fn from(value: isEntryPointReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for isEntryPointReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for isEntryPointCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Address,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = bool;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "isEntryPoint(address)";
            const SELECTOR: [u8; 4] = [122u8, 115u8, 133u8, 69u8];
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
                        &self.caller,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::Bool as alloy_sol_types::SolType>::tokenize(
                        ret,
                    ),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: isEntryPointReturn = r.into();
                        r._0
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
                        let r: isEntryPointReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `isValidSignature(bytes32,bytes)` and selector `0x1626ba7e`.
```solidity
function isValidSignature(bytes32 hash, bytes memory signature) external view returns (bytes4 magicValue);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct isValidSignatureCall {
        #[allow(missing_docs)]
        pub hash: alloy::sol_types::private::FixedBytes<32>,
        #[allow(missing_docs)]
        pub signature: alloy::sol_types::private::Bytes,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`isValidSignature(bytes32,bytes)`](isValidSignatureCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct isValidSignatureReturn {
        #[allow(missing_docs)]
        pub magicValue: alloy::sol_types::private::FixedBytes<4>,
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
                alloy::sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Bytes,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::FixedBytes<32>,
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
            impl ::core::convert::From<isValidSignatureCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: isValidSignatureCall) -> Self {
                    (value.hash, value.signature)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for isValidSignatureCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        hash: tuple.0,
                        signature: tuple.1,
                    }
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::FixedBytes<4>,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (alloy::sol_types::private::FixedBytes<4>,);
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
            impl ::core::convert::From<isValidSignatureReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: isValidSignatureReturn) -> Self {
                    (value.magicValue,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for isValidSignatureReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { magicValue: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for isValidSignatureCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Bytes,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::FixedBytes<4>;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::FixedBytes<4>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "isValidSignature(bytes32,bytes)";
            const SELECTOR: [u8; 4] = [22u8, 38u8, 186u8, 126u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                (
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.hash),
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self.signature,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::FixedBytes<
                        4,
                    > as alloy_sol_types::SolType>::tokenize(ret),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: isValidSignatureReturn = r.into();
                        r.magicValue
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
                        let r: isValidSignatureReturn = r.into();
                        r.magicValue
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `isValidatorEnabled(address)` and selector `0x75f25167`.
```solidity
function isValidatorEnabled(address validator) external view returns (bool enabled);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct isValidatorEnabledCall {
        #[allow(missing_docs)]
        pub validator: alloy::sol_types::private::Address,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`isValidatorEnabled(address)`](isValidatorEnabledCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct isValidatorEnabledReturn {
        #[allow(missing_docs)]
        pub enabled: bool,
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
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Address,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (alloy::sol_types::private::Address,);
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
            impl ::core::convert::From<isValidatorEnabledCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: isValidatorEnabledCall) -> Self {
                    (value.validator,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for isValidatorEnabledCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { validator: tuple.0 }
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (bool,);
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
            impl ::core::convert::From<isValidatorEnabledReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: isValidatorEnabledReturn) -> Self {
                    (value.enabled,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for isValidatorEnabledReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { enabled: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for isValidatorEnabledCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Address,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = bool;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "isValidatorEnabled(address)";
            const SELECTOR: [u8; 4] = [117u8, 242u8, 81u8, 103u8];
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
                        &self.validator,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::Bool as alloy_sol_types::SolType>::tokenize(
                        ret,
                    ),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: isValidatorEnabledReturn = r.into();
                        r.enabled
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
                        let r: isValidatorEnabledReturn = r.into();
                        r.enabled
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `nonceFor(address,uint192)` and selector `0x592b2b72`.
```solidity
function nonceFor(address ep, uint192 key) external view returns (uint256);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct nonceForCall {
        #[allow(missing_docs)]
        pub ep: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub key: alloy::sol_types::private::primitives::aliases::U192,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`nonceFor(address,uint192)`](nonceForCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct nonceForReturn {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::primitives::aliases::U256,
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
                alloy::sol_types::sol_data::Uint<192>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Address,
                alloy::sol_types::private::primitives::aliases::U192,
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
            impl ::core::convert::From<nonceForCall> for UnderlyingRustTuple<'_> {
                fn from(value: nonceForCall) -> Self {
                    (value.ep, value.key)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for nonceForCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { ep: tuple.0, key: tuple.1 }
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
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
            impl ::core::convert::From<nonceForReturn> for UnderlyingRustTuple<'_> {
                fn from(value: nonceForReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for nonceForReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for nonceForCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<192>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::primitives::aliases::U256;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "nonceFor(address,uint192)";
            const SELECTOR: [u8; 4] = [89u8, 43u8, 43u8, 114u8];
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
                        &self.ep,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        192,
                    > as alloy_sol_types::SolType>::tokenize(&self.key),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(ret),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: nonceForReturn = r.into();
                        r._0
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
                        let r: nonceForReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `onERC1155BatchReceived(address,address,uint256[],uint256[],bytes)` and selector `0xbc197c81`.
```solidity
function onERC1155BatchReceived(address, address, uint256[] memory, uint256[] memory, bytes memory) external returns (bytes4);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct onERC1155BatchReceivedCall {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub _1: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub _2: alloy::sol_types::private::Vec<
            alloy::sol_types::private::primitives::aliases::U256,
        >,
        #[allow(missing_docs)]
        pub _3: alloy::sol_types::private::Vec<
            alloy::sol_types::private::primitives::aliases::U256,
        >,
        #[allow(missing_docs)]
        pub _4: alloy::sol_types::private::Bytes,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`onERC1155BatchReceived(address,address,uint256[],uint256[],bytes)`](onERC1155BatchReceivedCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct onERC1155BatchReceivedReturn {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::FixedBytes<4>,
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
                alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Uint<256>>,
                alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Uint<256>>,
                alloy::sol_types::sol_data::Bytes,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Address,
                alloy::sol_types::private::Address,
                alloy::sol_types::private::Vec<
                    alloy::sol_types::private::primitives::aliases::U256,
                >,
                alloy::sol_types::private::Vec<
                    alloy::sol_types::private::primitives::aliases::U256,
                >,
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
            impl ::core::convert::From<onERC1155BatchReceivedCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: onERC1155BatchReceivedCall) -> Self {
                    (value._0, value._1, value._2, value._3, value._4)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for onERC1155BatchReceivedCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        _0: tuple.0,
                        _1: tuple.1,
                        _2: tuple.2,
                        _3: tuple.3,
                        _4: tuple.4,
                    }
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::FixedBytes<4>,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (alloy::sol_types::private::FixedBytes<4>,);
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
            impl ::core::convert::From<onERC1155BatchReceivedReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: onERC1155BatchReceivedReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for onERC1155BatchReceivedReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for onERC1155BatchReceivedCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Uint<256>>,
                alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Uint<256>>,
                alloy::sol_types::sol_data::Bytes,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::FixedBytes<4>;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::FixedBytes<4>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "onERC1155BatchReceived(address,address,uint256[],uint256[],bytes)";
            const SELECTOR: [u8; 4] = [188u8, 25u8, 124u8, 129u8];
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
                        &self._0,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self._1,
                    ),
                    <alloy::sol_types::sol_data::Array<
                        alloy::sol_types::sol_data::Uint<256>,
                    > as alloy_sol_types::SolType>::tokenize(&self._2),
                    <alloy::sol_types::sol_data::Array<
                        alloy::sol_types::sol_data::Uint<256>,
                    > as alloy_sol_types::SolType>::tokenize(&self._3),
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self._4,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::FixedBytes<
                        4,
                    > as alloy_sol_types::SolType>::tokenize(ret),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: onERC1155BatchReceivedReturn = r.into();
                        r._0
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
                        let r: onERC1155BatchReceivedReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `onERC1155Received(address,address,uint256,uint256,bytes)` and selector `0xf23a6e61`.
```solidity
function onERC1155Received(address, address, uint256, uint256, bytes memory) external returns (bytes4);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct onERC1155ReceivedCall {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub _1: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub _2: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub _3: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub _4: alloy::sol_types::private::Bytes,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`onERC1155Received(address,address,uint256,uint256,bytes)`](onERC1155ReceivedCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct onERC1155ReceivedReturn {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::FixedBytes<4>,
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
            impl ::core::convert::From<onERC1155ReceivedCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: onERC1155ReceivedCall) -> Self {
                    (value._0, value._1, value._2, value._3, value._4)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for onERC1155ReceivedCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        _0: tuple.0,
                        _1: tuple.1,
                        _2: tuple.2,
                        _3: tuple.3,
                        _4: tuple.4,
                    }
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::FixedBytes<4>,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (alloy::sol_types::private::FixedBytes<4>,);
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
            impl ::core::convert::From<onERC1155ReceivedReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: onERC1155ReceivedReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for onERC1155ReceivedReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for onERC1155ReceivedCall {
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
            type Return = alloy::sol_types::private::FixedBytes<4>;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::FixedBytes<4>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "onERC1155Received(address,address,uint256,uint256,bytes)";
            const SELECTOR: [u8; 4] = [242u8, 58u8, 110u8, 97u8];
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
                        &self._0,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self._1,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self._2),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self._3),
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self._4,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::FixedBytes<
                        4,
                    > as alloy_sol_types::SolType>::tokenize(ret),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: onERC1155ReceivedReturn = r.into();
                        r._0
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
                        let r: onERC1155ReceivedReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `onERC721Received(address,address,uint256,bytes)` and selector `0x150b7a02`.
```solidity
function onERC721Received(address, address, uint256, bytes memory) external returns (bytes4);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct onERC721ReceivedCall {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub _1: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub _2: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub _3: alloy::sol_types::private::Bytes,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`onERC721Received(address,address,uint256,bytes)`](onERC721ReceivedCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct onERC721ReceivedReturn {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::FixedBytes<4>,
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
                alloy::sol_types::sol_data::Bytes,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Address,
                alloy::sol_types::private::Address,
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
            impl ::core::convert::From<onERC721ReceivedCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: onERC721ReceivedCall) -> Self {
                    (value._0, value._1, value._2, value._3)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for onERC721ReceivedCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        _0: tuple.0,
                        _1: tuple.1,
                        _2: tuple.2,
                        _3: tuple.3,
                    }
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::FixedBytes<4>,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (alloy::sol_types::private::FixedBytes<4>,);
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
            impl ::core::convert::From<onERC721ReceivedReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: onERC721ReceivedReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for onERC721ReceivedReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for onERC721ReceivedCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Bytes,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::FixedBytes<4>;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::FixedBytes<4>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "onERC721Received(address,address,uint256,bytes)";
            const SELECTOR: [u8; 4] = [21u8, 11u8, 122u8, 2u8];
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
                        &self._0,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self._1,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self._2),
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self._3,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::FixedBytes<
                        4,
                    > as alloy_sol_types::SolType>::tokenize(ret),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: onERC721ReceivedReturn = r.into();
                        r._0
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
                        let r: onERC721ReceivedReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `receiveErc3009(address,address,uint256,uint256,uint256,bytes32,uint8,bytes32,bytes32)` and selector `0xbd3710b0`.
```solidity
function receiveErc3009(address token, address from, uint256 value, uint256 validAfter, uint256 validBefore, bytes32 nonce, uint8 v, bytes32 r, bytes32 s) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct receiveErc3009Call {
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub from: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub value: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub validAfter: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub validBefore: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub nonce: alloy::sol_types::private::FixedBytes<32>,
        #[allow(missing_docs)]
        pub v: u8,
        #[allow(missing_docs)]
        pub r: alloy::sol_types::private::FixedBytes<32>,
        #[allow(missing_docs)]
        pub s: alloy::sol_types::private::FixedBytes<32>,
    }
    ///Container type for the return parameters of the [`receiveErc3009(address,address,uint256,uint256,uint256,bytes32,uint8,bytes32,bytes32)`](receiveErc3009Call) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct receiveErc3009Return {}
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
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Uint<8>,
                alloy::sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::FixedBytes<32>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Address,
                alloy::sol_types::private::Address,
                alloy::sol_types::private::primitives::aliases::U256,
                alloy::sol_types::private::primitives::aliases::U256,
                alloy::sol_types::private::primitives::aliases::U256,
                alloy::sol_types::private::FixedBytes<32>,
                u8,
                alloy::sol_types::private::FixedBytes<32>,
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
            impl ::core::convert::From<receiveErc3009Call> for UnderlyingRustTuple<'_> {
                fn from(value: receiveErc3009Call) -> Self {
                    (
                        value.token,
                        value.from,
                        value.value,
                        value.validAfter,
                        value.validBefore,
                        value.nonce,
                        value.v,
                        value.r,
                        value.s,
                    )
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for receiveErc3009Call {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        token: tuple.0,
                        from: tuple.1,
                        value: tuple.2,
                        validAfter: tuple.3,
                        validBefore: tuple.4,
                        nonce: tuple.5,
                        v: tuple.6,
                        r: tuple.7,
                        s: tuple.8,
                    }
                }
            }
        }
        {
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
            impl ::core::convert::From<receiveErc3009Return>
            for UnderlyingRustTuple<'_> {
                fn from(value: receiveErc3009Return) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for receiveErc3009Return {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl receiveErc3009Return {
            fn _tokenize(
                &self,
            ) -> <receiveErc3009Call as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for receiveErc3009Call {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Uint<8>,
                alloy::sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::FixedBytes<32>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = receiveErc3009Return;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "receiveErc3009(address,address,uint256,uint256,uint256,bytes32,uint8,bytes32,bytes32)";
            const SELECTOR: [u8; 4] = [189u8, 55u8, 16u8, 176u8];
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
                        &self.token,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.from,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.value),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.validAfter),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.validBefore),
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.nonce),
                    <alloy::sol_types::sol_data::Uint<
                        8,
                    > as alloy_sol_types::SolType>::tokenize(&self.v),
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.r),
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.s),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                receiveErc3009Return::_tokenize(ret)
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
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `setValidator(address,bool)` and selector `0x4623c91d`.
```solidity
function setValidator(address validator, bool enabled) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setValidatorCall {
        #[allow(missing_docs)]
        pub validator: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub enabled: bool,
    }
    ///Container type for the return parameters of the [`setValidator(address,bool)`](setValidatorCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setValidatorReturn {}
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
                alloy::sol_types::sol_data::Bool,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (alloy::sol_types::private::Address, bool);
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
            impl ::core::convert::From<setValidatorCall> for UnderlyingRustTuple<'_> {
                fn from(value: setValidatorCall) -> Self {
                    (value.validator, value.enabled)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setValidatorCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        validator: tuple.0,
                        enabled: tuple.1,
                    }
                }
            }
        }
        {
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
            impl ::core::convert::From<setValidatorReturn> for UnderlyingRustTuple<'_> {
                fn from(value: setValidatorReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setValidatorReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl setValidatorReturn {
            fn _tokenize(
                &self,
            ) -> <setValidatorCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for setValidatorCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Bool,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = setValidatorReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "setValidator(address,bool)";
            const SELECTOR: [u8; 4] = [70u8, 35u8, 201u8, 29u8];
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
                        &self.validator,
                    ),
                    <alloy::sol_types::sol_data::Bool as alloy_sol_types::SolType>::tokenize(
                        &self.enabled,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                setValidatorReturn::_tokenize(ret)
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
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `supportsInterface(bytes4)` and selector `0x01ffc9a7`.
```solidity
function supportsInterface(bytes4 id) external pure returns (bool);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct supportsInterfaceCall {
        #[allow(missing_docs)]
        pub id: alloy::sol_types::private::FixedBytes<4>,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`supportsInterface(bytes4)`](supportsInterfaceCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct supportsInterfaceReturn {
        #[allow(missing_docs)]
        pub _0: bool,
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
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::FixedBytes<4>,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (alloy::sol_types::private::FixedBytes<4>,);
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
            impl ::core::convert::From<supportsInterfaceCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: supportsInterfaceCall) -> Self {
                    (value.id,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for supportsInterfaceCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { id: tuple.0 }
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (bool,);
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
            impl ::core::convert::From<supportsInterfaceReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: supportsInterfaceReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for supportsInterfaceReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for supportsInterfaceCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::FixedBytes<4>,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = bool;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "supportsInterface(bytes4)";
            const SELECTOR: [u8; 4] = [1u8, 255u8, 201u8, 167u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                (
                    <alloy::sol_types::sol_data::FixedBytes<
                        4,
                    > as alloy_sol_types::SolType>::tokenize(&self.id),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::Bool as alloy_sol_types::SolType>::tokenize(
                        ret,
                    ),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: supportsInterfaceReturn = r.into();
                        r._0
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
                        let r: supportsInterfaceReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `sweepERC20(address,address,uint256)` and selector `0x503690d1`.
```solidity
function sweepERC20(address token, address to, uint256 amount) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct sweepERC20Call {
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub to: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub amount: alloy::sol_types::private::primitives::aliases::U256,
    }
    ///Container type for the return parameters of the [`sweepERC20(address,address,uint256)`](sweepERC20Call) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct sweepERC20Return {}
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
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Address,
                alloy::sol_types::private::Address,
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
            impl ::core::convert::From<sweepERC20Call> for UnderlyingRustTuple<'_> {
                fn from(value: sweepERC20Call) -> Self {
                    (value.token, value.to, value.amount)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for sweepERC20Call {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        token: tuple.0,
                        to: tuple.1,
                        amount: tuple.2,
                    }
                }
            }
        }
        {
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
            impl ::core::convert::From<sweepERC20Return> for UnderlyingRustTuple<'_> {
                fn from(value: sweepERC20Return) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for sweepERC20Return {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl sweepERC20Return {
            fn _tokenize(
                &self,
            ) -> <sweepERC20Call as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for sweepERC20Call {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = sweepERC20Return;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "sweepERC20(address,address,uint256)";
            const SELECTOR: [u8; 4] = [80u8, 54u8, 144u8, 209u8];
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
                        &self.token,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.to,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amount),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                sweepERC20Return::_tokenize(ret)
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
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `sweepERC20Batch(address[],address,uint256[])` and selector `0xc70b5baf`.
```solidity
function sweepERC20Batch(address[] memory tokens, address to, uint256[] memory amounts) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct sweepERC20BatchCall {
        #[allow(missing_docs)]
        pub tokens: alloy::sol_types::private::Vec<alloy::sol_types::private::Address>,
        #[allow(missing_docs)]
        pub to: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub amounts: alloy::sol_types::private::Vec<
            alloy::sol_types::private::primitives::aliases::U256,
        >,
    }
    ///Container type for the return parameters of the [`sweepERC20Batch(address[],address,uint256[])`](sweepERC20BatchCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct sweepERC20BatchReturn {}
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
                alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Address>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Uint<256>>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Vec<alloy::sol_types::private::Address>,
                alloy::sol_types::private::Address,
                alloy::sol_types::private::Vec<
                    alloy::sol_types::private::primitives::aliases::U256,
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
            impl ::core::convert::From<sweepERC20BatchCall> for UnderlyingRustTuple<'_> {
                fn from(value: sweepERC20BatchCall) -> Self {
                    (value.tokens, value.to, value.amounts)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for sweepERC20BatchCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        tokens: tuple.0,
                        to: tuple.1,
                        amounts: tuple.2,
                    }
                }
            }
        }
        {
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
            impl ::core::convert::From<sweepERC20BatchReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: sweepERC20BatchReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for sweepERC20BatchReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl sweepERC20BatchReturn {
            fn _tokenize(
                &self,
            ) -> <sweepERC20BatchCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for sweepERC20BatchCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Address>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Uint<256>>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = sweepERC20BatchReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "sweepERC20Batch(address[],address,uint256[])";
            const SELECTOR: [u8; 4] = [199u8, 11u8, 91u8, 175u8];
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
                        alloy::sol_types::sol_data::Address,
                    > as alloy_sol_types::SolType>::tokenize(&self.tokens),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.to,
                    ),
                    <alloy::sol_types::sol_data::Array<
                        alloy::sol_types::sol_data::Uint<256>,
                    > as alloy_sol_types::SolType>::tokenize(&self.amounts),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                sweepERC20BatchReturn::_tokenize(ret)
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
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `transferErc3009(address,address,address,uint256,uint256,uint256,bytes32,uint8,bytes32,bytes32)` and selector `0xe9500529`.
```solidity
function transferErc3009(address token, address from, address to, uint256 value, uint256 validAfter, uint256 validBefore, bytes32 nonce, uint8 v, bytes32 r, bytes32 s) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct transferErc3009Call {
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub from: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub to: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub value: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub validAfter: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub validBefore: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub nonce: alloy::sol_types::private::FixedBytes<32>,
        #[allow(missing_docs)]
        pub v: u8,
        #[allow(missing_docs)]
        pub r: alloy::sol_types::private::FixedBytes<32>,
        #[allow(missing_docs)]
        pub s: alloy::sol_types::private::FixedBytes<32>,
    }
    ///Container type for the return parameters of the [`transferErc3009(address,address,address,uint256,uint256,uint256,bytes32,uint8,bytes32,bytes32)`](transferErc3009Call) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct transferErc3009Return {}
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
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Uint<8>,
                alloy::sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::FixedBytes<32>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Address,
                alloy::sol_types::private::Address,
                alloy::sol_types::private::Address,
                alloy::sol_types::private::primitives::aliases::U256,
                alloy::sol_types::private::primitives::aliases::U256,
                alloy::sol_types::private::primitives::aliases::U256,
                alloy::sol_types::private::FixedBytes<32>,
                u8,
                alloy::sol_types::private::FixedBytes<32>,
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
            impl ::core::convert::From<transferErc3009Call> for UnderlyingRustTuple<'_> {
                fn from(value: transferErc3009Call) -> Self {
                    (
                        value.token,
                        value.from,
                        value.to,
                        value.value,
                        value.validAfter,
                        value.validBefore,
                        value.nonce,
                        value.v,
                        value.r,
                        value.s,
                    )
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for transferErc3009Call {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        token: tuple.0,
                        from: tuple.1,
                        to: tuple.2,
                        value: tuple.3,
                        validAfter: tuple.4,
                        validBefore: tuple.5,
                        nonce: tuple.6,
                        v: tuple.7,
                        r: tuple.8,
                        s: tuple.9,
                    }
                }
            }
        }
        {
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
            impl ::core::convert::From<transferErc3009Return>
            for UnderlyingRustTuple<'_> {
                fn from(value: transferErc3009Return) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for transferErc3009Return {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl transferErc3009Return {
            fn _tokenize(
                &self,
            ) -> <transferErc3009Call as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for transferErc3009Call {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Uint<8>,
                alloy::sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::FixedBytes<32>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = transferErc3009Return;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "transferErc3009(address,address,address,uint256,uint256,uint256,bytes32,uint8,bytes32,bytes32)";
            const SELECTOR: [u8; 4] = [233u8, 80u8, 5u8, 41u8];
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
                        &self.token,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.from,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.to,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.value),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.validAfter),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.validBefore),
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.nonce),
                    <alloy::sol_types::sol_data::Uint<
                        8,
                    > as alloy_sol_types::SolType>::tokenize(&self.v),
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.r),
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.s),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                transferErc3009Return::_tokenize(ret)
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
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `validateUserOp((address,uint256,bytes,bytes,bytes32,uint256,bytes32,bytes,bytes),bytes32,uint256)` and selector `0x19822f7c`.
```solidity
function validateUserOp(PackedUserOperation memory userOp, bytes32 userOpHash, uint256 missingAccountFunds) external returns (uint256 validationData);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct validateUserOpCall {
        #[allow(missing_docs)]
        pub userOp: <PackedUserOperation as alloy::sol_types::SolType>::RustType,
        #[allow(missing_docs)]
        pub userOpHash: alloy::sol_types::private::FixedBytes<32>,
        #[allow(missing_docs)]
        pub missingAccountFunds: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`validateUserOp((address,uint256,bytes,bytes,bytes32,uint256,bytes32,bytes,bytes),bytes32,uint256)`](validateUserOpCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct validateUserOpReturn {
        #[allow(missing_docs)]
        pub validationData: alloy::sol_types::private::primitives::aliases::U256,
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
                PackedUserOperation,
                alloy::sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                <PackedUserOperation as alloy::sol_types::SolType>::RustType,
                alloy::sol_types::private::FixedBytes<32>,
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
            impl ::core::convert::From<validateUserOpCall> for UnderlyingRustTuple<'_> {
                fn from(value: validateUserOpCall) -> Self {
                    (value.userOp, value.userOpHash, value.missingAccountFunds)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for validateUserOpCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        userOp: tuple.0,
                        userOpHash: tuple.1,
                        missingAccountFunds: tuple.2,
                    }
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
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
            impl ::core::convert::From<validateUserOpReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: validateUserOpReturn) -> Self {
                    (value.validationData,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for validateUserOpReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { validationData: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for validateUserOpCall {
            type Parameters<'a> = (
                PackedUserOperation,
                alloy::sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::primitives::aliases::U256;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "validateUserOp((address,uint256,bytes,bytes,bytes32,uint256,bytes32,bytes,bytes),bytes32,uint256)";
            const SELECTOR: [u8; 4] = [25u8, 130u8, 47u8, 124u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                (
                    <PackedUserOperation as alloy_sol_types::SolType>::tokenize(
                        &self.userOp,
                    ),
                    <alloy::sol_types::sol_data::FixedBytes<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.userOpHash),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.missingAccountFunds),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(ret),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: validateUserOpReturn = r.into();
                        r.validationData
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
                        let r: validateUserOpReturn = r.into();
                        r.validationData
                    })
            }
        }
    };
    ///Container for all the [`MevBotDelegate`](self) function calls.
    #[derive(Clone)]
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive()]
    pub enum MevBotDelegateCalls {
        #[allow(missing_docs)]
        ENTRY_POINT_ADDR(ENTRY_POINT_ADDRCall),
        #[allow(missing_docs)]
        ENTRY_POINT_V06(ENTRY_POINT_V06Call),
        #[allow(missing_docs)]
        ENTRY_POINT_V07(ENTRY_POINT_V07Call),
        #[allow(missing_docs)]
        ENTRY_POINT_V08(ENTRY_POINT_V08Call),
        #[allow(missing_docs)]
        entryPoint(entryPointCall),
        #[allow(missing_docs)]
        execute(executeCall),
        #[allow(missing_docs)]
        executeBatch(executeBatchCall),
        #[allow(missing_docs)]
        executeErc6909Batch(executeErc6909BatchCall),
        #[allow(missing_docs)]
        getNonce(getNonceCall),
        #[allow(missing_docs)]
        isEntryPoint(isEntryPointCall),
        #[allow(missing_docs)]
        isValidSignature(isValidSignatureCall),
        #[allow(missing_docs)]
        isValidatorEnabled(isValidatorEnabledCall),
        #[allow(missing_docs)]
        nonceFor(nonceForCall),
        #[allow(missing_docs)]
        onERC1155BatchReceived(onERC1155BatchReceivedCall),
        #[allow(missing_docs)]
        onERC1155Received(onERC1155ReceivedCall),
        #[allow(missing_docs)]
        onERC721Received(onERC721ReceivedCall),
        #[allow(missing_docs)]
        receiveErc3009(receiveErc3009Call),
        #[allow(missing_docs)]
        setValidator(setValidatorCall),
        #[allow(missing_docs)]
        supportsInterface(supportsInterfaceCall),
        #[allow(missing_docs)]
        sweepERC20(sweepERC20Call),
        #[allow(missing_docs)]
        sweepERC20Batch(sweepERC20BatchCall),
        #[allow(missing_docs)]
        transferErc3009(transferErc3009Call),
        #[allow(missing_docs)]
        validateUserOp(validateUserOpCall),
    }
    impl MevBotDelegateCalls {
        /// All the selectors of this enum.
        ///
        /// Note that the selectors might not be in the same order as the variants.
        /// No guarantees are made about the order of the selectors.
        ///
        /// Prefer using `SolInterface` methods instead.
        pub const SELECTORS: &'static [[u8; 4usize]] = &[
            [1u8, 255u8, 201u8, 167u8],
            [21u8, 11u8, 122u8, 2u8],
            [22u8, 38u8, 186u8, 126u8],
            [25u8, 130u8, 47u8, 124u8],
            [52u8, 252u8, 213u8, 190u8],
            [66u8, 3u8, 169u8, 52u8],
            [70u8, 35u8, 201u8, 29u8],
            [80u8, 54u8, 144u8, 209u8],
            [89u8, 43u8, 43u8, 114u8],
            [89u8, 205u8, 51u8, 0u8],
            [107u8, 80u8, 188u8, 145u8],
            [117u8, 242u8, 81u8, 103u8],
            [122u8, 115u8, 133u8, 69u8],
            [145u8, 149u8, 248u8, 54u8],
            [161u8, 52u8, 246u8, 78u8],
            [176u8, 214u8, 145u8, 254u8],
            [182u8, 29u8, 39u8, 246u8],
            [188u8, 25u8, 124u8, 129u8],
            [189u8, 55u8, 16u8, 176u8],
            [199u8, 11u8, 91u8, 175u8],
            [208u8, 135u8, 210u8, 136u8],
            [233u8, 80u8, 5u8, 41u8],
            [242u8, 58u8, 110u8, 97u8],
        ];
        /// The names of the variants in the same order as `SELECTORS`.
        pub const VARIANT_NAMES: &'static [&'static str] = &[
            ::core::stringify!(supportsInterface),
            ::core::stringify!(onERC721Received),
            ::core::stringify!(isValidSignature),
            ::core::stringify!(validateUserOp),
            ::core::stringify!(executeBatch),
            ::core::stringify!(executeErc6909Batch),
            ::core::stringify!(setValidator),
            ::core::stringify!(sweepERC20),
            ::core::stringify!(nonceFor),
            ::core::stringify!(ENTRY_POINT_V08),
            ::core::stringify!(ENTRY_POINT_V06),
            ::core::stringify!(isValidatorEnabled),
            ::core::stringify!(isEntryPoint),
            ::core::stringify!(ENTRY_POINT_V07),
            ::core::stringify!(ENTRY_POINT_ADDR),
            ::core::stringify!(entryPoint),
            ::core::stringify!(execute),
            ::core::stringify!(onERC1155BatchReceived),
            ::core::stringify!(receiveErc3009),
            ::core::stringify!(sweepERC20Batch),
            ::core::stringify!(getNonce),
            ::core::stringify!(transferErc3009),
            ::core::stringify!(onERC1155Received),
        ];
        /// The signatures in the same order as `SELECTORS`.
        pub const SIGNATURES: &'static [&'static str] = &[
            <supportsInterfaceCall as alloy_sol_types::SolCall>::SIGNATURE,
            <onERC721ReceivedCall as alloy_sol_types::SolCall>::SIGNATURE,
            <isValidSignatureCall as alloy_sol_types::SolCall>::SIGNATURE,
            <validateUserOpCall as alloy_sol_types::SolCall>::SIGNATURE,
            <executeBatchCall as alloy_sol_types::SolCall>::SIGNATURE,
            <executeErc6909BatchCall as alloy_sol_types::SolCall>::SIGNATURE,
            <setValidatorCall as alloy_sol_types::SolCall>::SIGNATURE,
            <sweepERC20Call as alloy_sol_types::SolCall>::SIGNATURE,
            <nonceForCall as alloy_sol_types::SolCall>::SIGNATURE,
            <ENTRY_POINT_V08Call as alloy_sol_types::SolCall>::SIGNATURE,
            <ENTRY_POINT_V06Call as alloy_sol_types::SolCall>::SIGNATURE,
            <isValidatorEnabledCall as alloy_sol_types::SolCall>::SIGNATURE,
            <isEntryPointCall as alloy_sol_types::SolCall>::SIGNATURE,
            <ENTRY_POINT_V07Call as alloy_sol_types::SolCall>::SIGNATURE,
            <ENTRY_POINT_ADDRCall as alloy_sol_types::SolCall>::SIGNATURE,
            <entryPointCall as alloy_sol_types::SolCall>::SIGNATURE,
            <executeCall as alloy_sol_types::SolCall>::SIGNATURE,
            <onERC1155BatchReceivedCall as alloy_sol_types::SolCall>::SIGNATURE,
            <receiveErc3009Call as alloy_sol_types::SolCall>::SIGNATURE,
            <sweepERC20BatchCall as alloy_sol_types::SolCall>::SIGNATURE,
            <getNonceCall as alloy_sol_types::SolCall>::SIGNATURE,
            <transferErc3009Call as alloy_sol_types::SolCall>::SIGNATURE,
            <onERC1155ReceivedCall as alloy_sol_types::SolCall>::SIGNATURE,
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
    impl alloy_sol_types::SolInterface for MevBotDelegateCalls {
        const NAME: &'static str = "MevBotDelegateCalls";
        const MIN_DATA_LENGTH: usize = 0usize;
        const COUNT: usize = 23usize;
        #[inline]
        fn selector(&self) -> [u8; 4] {
            match self {
                Self::ENTRY_POINT_ADDR(_) => {
                    <ENTRY_POINT_ADDRCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::ENTRY_POINT_V06(_) => {
                    <ENTRY_POINT_V06Call as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::ENTRY_POINT_V07(_) => {
                    <ENTRY_POINT_V07Call as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::ENTRY_POINT_V08(_) => {
                    <ENTRY_POINT_V08Call as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::entryPoint(_) => {
                    <entryPointCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::execute(_) => <executeCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::executeBatch(_) => {
                    <executeBatchCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::executeErc6909Batch(_) => {
                    <executeErc6909BatchCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::getNonce(_) => <getNonceCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::isEntryPoint(_) => {
                    <isEntryPointCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::isValidSignature(_) => {
                    <isValidSignatureCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::isValidatorEnabled(_) => {
                    <isValidatorEnabledCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::nonceFor(_) => <nonceForCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::onERC1155BatchReceived(_) => {
                    <onERC1155BatchReceivedCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::onERC1155Received(_) => {
                    <onERC1155ReceivedCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::onERC721Received(_) => {
                    <onERC721ReceivedCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::receiveErc3009(_) => {
                    <receiveErc3009Call as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::setValidator(_) => {
                    <setValidatorCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::supportsInterface(_) => {
                    <supportsInterfaceCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::sweepERC20(_) => {
                    <sweepERC20Call as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::sweepERC20Batch(_) => {
                    <sweepERC20BatchCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::transferErc3009(_) => {
                    <transferErc3009Call as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::validateUserOp(_) => {
                    <validateUserOpCall as alloy_sol_types::SolCall>::SELECTOR
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
            ) -> alloy_sol_types::Result<MevBotDelegateCalls>] = &[
                {
                    fn supportsInterface(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <supportsInterfaceCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::supportsInterface)
                    }
                    supportsInterface
                },
                {
                    fn onERC721Received(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <onERC721ReceivedCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::onERC721Received)
                    }
                    onERC721Received
                },
                {
                    fn isValidSignature(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <isValidSignatureCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::isValidSignature)
                    }
                    isValidSignature
                },
                {
                    fn validateUserOp(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <validateUserOpCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::validateUserOp)
                    }
                    validateUserOp
                },
                {
                    fn executeBatch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <executeBatchCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::executeBatch)
                    }
                    executeBatch
                },
                {
                    fn executeErc6909Batch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <executeErc6909BatchCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::executeErc6909Batch)
                    }
                    executeErc6909Batch
                },
                {
                    fn setValidator(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <setValidatorCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::setValidator)
                    }
                    setValidator
                },
                {
                    fn sweepERC20(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <sweepERC20Call as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::sweepERC20)
                    }
                    sweepERC20
                },
                {
                    fn nonceFor(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <nonceForCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevBotDelegateCalls::nonceFor)
                    }
                    nonceFor
                },
                {
                    fn ENTRY_POINT_V08(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <ENTRY_POINT_V08Call as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::ENTRY_POINT_V08)
                    }
                    ENTRY_POINT_V08
                },
                {
                    fn ENTRY_POINT_V06(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <ENTRY_POINT_V06Call as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::ENTRY_POINT_V06)
                    }
                    ENTRY_POINT_V06
                },
                {
                    fn isValidatorEnabled(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <isValidatorEnabledCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::isValidatorEnabled)
                    }
                    isValidatorEnabled
                },
                {
                    fn isEntryPoint(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <isEntryPointCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::isEntryPoint)
                    }
                    isEntryPoint
                },
                {
                    fn ENTRY_POINT_V07(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <ENTRY_POINT_V07Call as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::ENTRY_POINT_V07)
                    }
                    ENTRY_POINT_V07
                },
                {
                    fn ENTRY_POINT_ADDR(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <ENTRY_POINT_ADDRCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::ENTRY_POINT_ADDR)
                    }
                    ENTRY_POINT_ADDR
                },
                {
                    fn entryPoint(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <entryPointCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::entryPoint)
                    }
                    entryPoint
                },
                {
                    fn execute(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <executeCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevBotDelegateCalls::execute)
                    }
                    execute
                },
                {
                    fn onERC1155BatchReceived(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <onERC1155BatchReceivedCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::onERC1155BatchReceived)
                    }
                    onERC1155BatchReceived
                },
                {
                    fn receiveErc3009(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <receiveErc3009Call as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::receiveErc3009)
                    }
                    receiveErc3009
                },
                {
                    fn sweepERC20Batch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <sweepERC20BatchCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::sweepERC20Batch)
                    }
                    sweepERC20Batch
                },
                {
                    fn getNonce(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <getNonceCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevBotDelegateCalls::getNonce)
                    }
                    getNonce
                },
                {
                    fn transferErc3009(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <transferErc3009Call as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::transferErc3009)
                    }
                    transferErc3009
                },
                {
                    fn onERC1155Received(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <onERC1155ReceivedCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateCalls::onERC1155Received)
                    }
                    onERC1155Received
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
            ) -> alloy_sol_types::Result<MevBotDelegateCalls>] = &[
                {
                    fn supportsInterface(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <supportsInterfaceCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::supportsInterface)
                    }
                    supportsInterface
                },
                {
                    fn onERC721Received(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <onERC721ReceivedCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::onERC721Received)
                    }
                    onERC721Received
                },
                {
                    fn isValidSignature(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <isValidSignatureCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::isValidSignature)
                    }
                    isValidSignature
                },
                {
                    fn validateUserOp(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <validateUserOpCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::validateUserOp)
                    }
                    validateUserOp
                },
                {
                    fn executeBatch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <executeBatchCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::executeBatch)
                    }
                    executeBatch
                },
                {
                    fn executeErc6909Batch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <executeErc6909BatchCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::executeErc6909Batch)
                    }
                    executeErc6909Batch
                },
                {
                    fn setValidator(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <setValidatorCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::setValidator)
                    }
                    setValidator
                },
                {
                    fn sweepERC20(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <sweepERC20Call as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::sweepERC20)
                    }
                    sweepERC20
                },
                {
                    fn nonceFor(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <nonceForCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::nonceFor)
                    }
                    nonceFor
                },
                {
                    fn ENTRY_POINT_V08(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <ENTRY_POINT_V08Call as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::ENTRY_POINT_V08)
                    }
                    ENTRY_POINT_V08
                },
                {
                    fn ENTRY_POINT_V06(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <ENTRY_POINT_V06Call as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::ENTRY_POINT_V06)
                    }
                    ENTRY_POINT_V06
                },
                {
                    fn isValidatorEnabled(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <isValidatorEnabledCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::isValidatorEnabled)
                    }
                    isValidatorEnabled
                },
                {
                    fn isEntryPoint(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <isEntryPointCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::isEntryPoint)
                    }
                    isEntryPoint
                },
                {
                    fn ENTRY_POINT_V07(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <ENTRY_POINT_V07Call as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::ENTRY_POINT_V07)
                    }
                    ENTRY_POINT_V07
                },
                {
                    fn ENTRY_POINT_ADDR(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <ENTRY_POINT_ADDRCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::ENTRY_POINT_ADDR)
                    }
                    ENTRY_POINT_ADDR
                },
                {
                    fn entryPoint(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <entryPointCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::entryPoint)
                    }
                    entryPoint
                },
                {
                    fn execute(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <executeCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::execute)
                    }
                    execute
                },
                {
                    fn onERC1155BatchReceived(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <onERC1155BatchReceivedCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::onERC1155BatchReceived)
                    }
                    onERC1155BatchReceived
                },
                {
                    fn receiveErc3009(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <receiveErc3009Call as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::receiveErc3009)
                    }
                    receiveErc3009
                },
                {
                    fn sweepERC20Batch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <sweepERC20BatchCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::sweepERC20Batch)
                    }
                    sweepERC20Batch
                },
                {
                    fn getNonce(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <getNonceCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::getNonce)
                    }
                    getNonce
                },
                {
                    fn transferErc3009(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <transferErc3009Call as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::transferErc3009)
                    }
                    transferErc3009
                },
                {
                    fn onERC1155Received(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateCalls> {
                        <onERC1155ReceivedCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateCalls::onERC1155Received)
                    }
                    onERC1155Received
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
                Self::ENTRY_POINT_ADDR(inner) => {
                    <ENTRY_POINT_ADDRCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::ENTRY_POINT_V06(inner) => {
                    <ENTRY_POINT_V06Call as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::ENTRY_POINT_V07(inner) => {
                    <ENTRY_POINT_V07Call as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::ENTRY_POINT_V08(inner) => {
                    <ENTRY_POINT_V08Call as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::entryPoint(inner) => {
                    <entryPointCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::execute(inner) => {
                    <executeCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::executeBatch(inner) => {
                    <executeBatchCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::executeErc6909Batch(inner) => {
                    <executeErc6909BatchCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::getNonce(inner) => {
                    <getNonceCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::isEntryPoint(inner) => {
                    <isEntryPointCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::isValidSignature(inner) => {
                    <isValidSignatureCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::isValidatorEnabled(inner) => {
                    <isValidatorEnabledCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::nonceFor(inner) => {
                    <nonceForCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::onERC1155BatchReceived(inner) => {
                    <onERC1155BatchReceivedCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::onERC1155Received(inner) => {
                    <onERC1155ReceivedCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::onERC721Received(inner) => {
                    <onERC721ReceivedCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::receiveErc3009(inner) => {
                    <receiveErc3009Call as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::setValidator(inner) => {
                    <setValidatorCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::supportsInterface(inner) => {
                    <supportsInterfaceCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::sweepERC20(inner) => {
                    <sweepERC20Call as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::sweepERC20Batch(inner) => {
                    <sweepERC20BatchCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::transferErc3009(inner) => {
                    <transferErc3009Call as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::validateUserOp(inner) => {
                    <validateUserOpCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
            }
        }
        #[inline]
        fn abi_encode_raw(&self, out: &mut alloy_sol_types::private::Vec<u8>) {
            match self {
                Self::ENTRY_POINT_ADDR(inner) => {
                    <ENTRY_POINT_ADDRCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::ENTRY_POINT_V06(inner) => {
                    <ENTRY_POINT_V06Call as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::ENTRY_POINT_V07(inner) => {
                    <ENTRY_POINT_V07Call as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::ENTRY_POINT_V08(inner) => {
                    <ENTRY_POINT_V08Call as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::entryPoint(inner) => {
                    <entryPointCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::execute(inner) => {
                    <executeCall as alloy_sol_types::SolCall>::abi_encode_raw(inner, out)
                }
                Self::executeBatch(inner) => {
                    <executeBatchCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::executeErc6909Batch(inner) => {
                    <executeErc6909BatchCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::getNonce(inner) => {
                    <getNonceCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::isEntryPoint(inner) => {
                    <isEntryPointCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::isValidSignature(inner) => {
                    <isValidSignatureCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::isValidatorEnabled(inner) => {
                    <isValidatorEnabledCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::nonceFor(inner) => {
                    <nonceForCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::onERC1155BatchReceived(inner) => {
                    <onERC1155BatchReceivedCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::onERC1155Received(inner) => {
                    <onERC1155ReceivedCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::onERC721Received(inner) => {
                    <onERC721ReceivedCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::receiveErc3009(inner) => {
                    <receiveErc3009Call as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::setValidator(inner) => {
                    <setValidatorCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::supportsInterface(inner) => {
                    <supportsInterfaceCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::sweepERC20(inner) => {
                    <sweepERC20Call as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::sweepERC20Batch(inner) => {
                    <sweepERC20BatchCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::transferErc3009(inner) => {
                    <transferErc3009Call as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::validateUserOp(inner) => {
                    <validateUserOpCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
            }
        }
    }
    ///Container for all the [`MevBotDelegate`](self) custom errors.
    #[derive(Clone)]
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub enum MevBotDelegateErrors {
        #[allow(missing_docs)]
        Erc6909Op_SetOperatorRequiresZeroIdAndAmount(
            Erc6909Op_SetOperatorRequiresZeroIdAndAmount,
        ),
        #[allow(missing_docs)]
        Erc6909Op_TransferRequiresZeroApproved(Erc6909Op_TransferRequiresZeroApproved),
        #[allow(missing_docs)]
        Erc6909SetOperatorBlocked(Erc6909SetOperatorBlocked),
        #[allow(missing_docs)]
        ExecuteError(ExecuteError),
        #[allow(missing_docs)]
        InvalidParams(InvalidParams),
        #[allow(missing_docs)]
        MevBotDelegate__Erc6909SetOperatorFailed(
            MevBotDelegate__Erc6909SetOperatorFailed,
        ),
        #[allow(missing_docs)]
        MevBotDelegate__Erc6909TransferFailed(MevBotDelegate__Erc6909TransferFailed),
        #[allow(missing_docs)]
        MevBotDelegate__InvalidSignatureLength(MevBotDelegate__InvalidSignatureLength),
        #[allow(missing_docs)]
        MevBotDelegate__InvalidValidator(MevBotDelegate__InvalidValidator),
        #[allow(missing_docs)]
        MevBotDelegate__Unauthorized(MevBotDelegate__Unauthorized),
        #[allow(missing_docs)]
        MevBotDelegate__ValidatorNotEnabled(MevBotDelegate__ValidatorNotEnabled),
        #[allow(missing_docs)]
        MevBotDelegate__ValidatorReverted(MevBotDelegate__ValidatorReverted),
        #[allow(missing_docs)]
        MevBotDelegate__ZeroTarget(MevBotDelegate__ZeroTarget),
        #[allow(missing_docs)]
        NativeSweepFailed(NativeSweepFailed),
        #[allow(missing_docs)]
        NotFromEntryPoint(NotFromEntryPoint),
    }
    impl MevBotDelegateErrors {
        /// All the selectors of this enum.
        ///
        /// Note that the selectors might not be in the same order as the variants.
        /// No guarantees are made about the order of the selectors.
        ///
        /// Prefer using `SolInterface` methods instead.
        pub const SELECTORS: &'static [[u8; 4usize]] = &[
            [22u8, 180u8, 82u8, 247u8],
            [31u8, 183u8, 204u8, 165u8],
            [50u8, 152u8, 139u8, 28u8],
            [52u8, 179u8, 225u8, 214u8],
            [90u8, 21u8, 70u8, 117u8],
            [91u8, 152u8, 84u8, 78u8],
            [94u8, 46u8, 66u8, 197u8],
            [104u8, 180u8, 77u8, 216u8],
            [131u8, 234u8, 67u8, 242u8],
            [133u8, 88u8, 104u8, 186u8],
            [168u8, 107u8, 101u8, 18u8],
            [176u8, 38u8, 213u8, 163u8],
            [188u8, 58u8, 129u8, 189u8],
            [195u8, 142u8, 128u8, 228u8],
            [254u8, 52u8, 166u8, 211u8],
        ];
        /// The names of the variants in the same order as `SELECTORS`.
        pub const VARIANT_NAMES: &'static [&'static str] = &[
            ::core::stringify!(NativeSweepFailed),
            ::core::stringify!(Erc6909SetOperatorBlocked),
            ::core::stringify!(MevBotDelegate__ZeroTarget),
            ::core::stringify!(MevBotDelegate__InvalidSignatureLength),
            ::core::stringify!(ExecuteError),
            ::core::stringify!(MevBotDelegate__ValidatorReverted),
            ::core::stringify!(MevBotDelegate__Erc6909TransferFailed),
            ::core::stringify!(MevBotDelegate__InvalidValidator),
            ::core::stringify!(Erc6909Op_SetOperatorRequiresZeroIdAndAmount),
            ::core::stringify!(MevBotDelegate__Erc6909SetOperatorFailed),
            ::core::stringify!(InvalidParams),
            ::core::stringify!(Erc6909Op_TransferRequiresZeroApproved),
            ::core::stringify!(MevBotDelegate__Unauthorized),
            ::core::stringify!(MevBotDelegate__ValidatorNotEnabled),
            ::core::stringify!(NotFromEntryPoint),
        ];
        /// The signatures in the same order as `SELECTORS`.
        pub const SIGNATURES: &'static [&'static str] = &[
            <NativeSweepFailed as alloy_sol_types::SolError>::SIGNATURE,
            <Erc6909SetOperatorBlocked as alloy_sol_types::SolError>::SIGNATURE,
            <MevBotDelegate__ZeroTarget as alloy_sol_types::SolError>::SIGNATURE,
            <MevBotDelegate__InvalidSignatureLength as alloy_sol_types::SolError>::SIGNATURE,
            <ExecuteError as alloy_sol_types::SolError>::SIGNATURE,
            <MevBotDelegate__ValidatorReverted as alloy_sol_types::SolError>::SIGNATURE,
            <MevBotDelegate__Erc6909TransferFailed as alloy_sol_types::SolError>::SIGNATURE,
            <MevBotDelegate__InvalidValidator as alloy_sol_types::SolError>::SIGNATURE,
            <Erc6909Op_SetOperatorRequiresZeroIdAndAmount as alloy_sol_types::SolError>::SIGNATURE,
            <MevBotDelegate__Erc6909SetOperatorFailed as alloy_sol_types::SolError>::SIGNATURE,
            <InvalidParams as alloy_sol_types::SolError>::SIGNATURE,
            <Erc6909Op_TransferRequiresZeroApproved as alloy_sol_types::SolError>::SIGNATURE,
            <MevBotDelegate__Unauthorized as alloy_sol_types::SolError>::SIGNATURE,
            <MevBotDelegate__ValidatorNotEnabled as alloy_sol_types::SolError>::SIGNATURE,
            <NotFromEntryPoint as alloy_sol_types::SolError>::SIGNATURE,
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
    impl alloy_sol_types::SolInterface for MevBotDelegateErrors {
        const NAME: &'static str = "MevBotDelegateErrors";
        const MIN_DATA_LENGTH: usize = 0usize;
        const COUNT: usize = 15usize;
        #[inline]
        fn selector(&self) -> [u8; 4] {
            match self {
                Self::Erc6909Op_SetOperatorRequiresZeroIdAndAmount(_) => {
                    <Erc6909Op_SetOperatorRequiresZeroIdAndAmount as alloy_sol_types::SolError>::SELECTOR
                }
                Self::Erc6909Op_TransferRequiresZeroApproved(_) => {
                    <Erc6909Op_TransferRequiresZeroApproved as alloy_sol_types::SolError>::SELECTOR
                }
                Self::Erc6909SetOperatorBlocked(_) => {
                    <Erc6909SetOperatorBlocked as alloy_sol_types::SolError>::SELECTOR
                }
                Self::ExecuteError(_) => {
                    <ExecuteError as alloy_sol_types::SolError>::SELECTOR
                }
                Self::InvalidParams(_) => {
                    <InvalidParams as alloy_sol_types::SolError>::SELECTOR
                }
                Self::MevBotDelegate__Erc6909SetOperatorFailed(_) => {
                    <MevBotDelegate__Erc6909SetOperatorFailed as alloy_sol_types::SolError>::SELECTOR
                }
                Self::MevBotDelegate__Erc6909TransferFailed(_) => {
                    <MevBotDelegate__Erc6909TransferFailed as alloy_sol_types::SolError>::SELECTOR
                }
                Self::MevBotDelegate__InvalidSignatureLength(_) => {
                    <MevBotDelegate__InvalidSignatureLength as alloy_sol_types::SolError>::SELECTOR
                }
                Self::MevBotDelegate__InvalidValidator(_) => {
                    <MevBotDelegate__InvalidValidator as alloy_sol_types::SolError>::SELECTOR
                }
                Self::MevBotDelegate__Unauthorized(_) => {
                    <MevBotDelegate__Unauthorized as alloy_sol_types::SolError>::SELECTOR
                }
                Self::MevBotDelegate__ValidatorNotEnabled(_) => {
                    <MevBotDelegate__ValidatorNotEnabled as alloy_sol_types::SolError>::SELECTOR
                }
                Self::MevBotDelegate__ValidatorReverted(_) => {
                    <MevBotDelegate__ValidatorReverted as alloy_sol_types::SolError>::SELECTOR
                }
                Self::MevBotDelegate__ZeroTarget(_) => {
                    <MevBotDelegate__ZeroTarget as alloy_sol_types::SolError>::SELECTOR
                }
                Self::NativeSweepFailed(_) => {
                    <NativeSweepFailed as alloy_sol_types::SolError>::SELECTOR
                }
                Self::NotFromEntryPoint(_) => {
                    <NotFromEntryPoint as alloy_sol_types::SolError>::SELECTOR
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
            ) -> alloy_sol_types::Result<MevBotDelegateErrors>] = &[
                {
                    fn NativeSweepFailed(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <NativeSweepFailed as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateErrors::NativeSweepFailed)
                    }
                    NativeSweepFailed
                },
                {
                    fn Erc6909SetOperatorBlocked(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <Erc6909SetOperatorBlocked as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateErrors::Erc6909SetOperatorBlocked)
                    }
                    Erc6909SetOperatorBlocked
                },
                {
                    fn MevBotDelegate__ZeroTarget(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <MevBotDelegate__ZeroTarget as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateErrors::MevBotDelegate__ZeroTarget)
                    }
                    MevBotDelegate__ZeroTarget
                },
                {
                    fn MevBotDelegate__InvalidSignatureLength(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <MevBotDelegate__InvalidSignatureLength as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(
                                MevBotDelegateErrors::MevBotDelegate__InvalidSignatureLength,
                            )
                    }
                    MevBotDelegate__InvalidSignatureLength
                },
                {
                    fn ExecuteError(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <ExecuteError as alloy_sol_types::SolError>::abi_decode_raw(data)
                            .map(MevBotDelegateErrors::ExecuteError)
                    }
                    ExecuteError
                },
                {
                    fn MevBotDelegate__ValidatorReverted(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <MevBotDelegate__ValidatorReverted as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateErrors::MevBotDelegate__ValidatorReverted)
                    }
                    MevBotDelegate__ValidatorReverted
                },
                {
                    fn MevBotDelegate__Erc6909TransferFailed(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <MevBotDelegate__Erc6909TransferFailed as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(
                                MevBotDelegateErrors::MevBotDelegate__Erc6909TransferFailed,
                            )
                    }
                    MevBotDelegate__Erc6909TransferFailed
                },
                {
                    fn MevBotDelegate__InvalidValidator(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <MevBotDelegate__InvalidValidator as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateErrors::MevBotDelegate__InvalidValidator)
                    }
                    MevBotDelegate__InvalidValidator
                },
                {
                    fn Erc6909Op_SetOperatorRequiresZeroIdAndAmount(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <Erc6909Op_SetOperatorRequiresZeroIdAndAmount as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(
                                MevBotDelegateErrors::Erc6909Op_SetOperatorRequiresZeroIdAndAmount,
                            )
                    }
                    Erc6909Op_SetOperatorRequiresZeroIdAndAmount
                },
                {
                    fn MevBotDelegate__Erc6909SetOperatorFailed(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <MevBotDelegate__Erc6909SetOperatorFailed as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(
                                MevBotDelegateErrors::MevBotDelegate__Erc6909SetOperatorFailed,
                            )
                    }
                    MevBotDelegate__Erc6909SetOperatorFailed
                },
                {
                    fn InvalidParams(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <InvalidParams as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateErrors::InvalidParams)
                    }
                    InvalidParams
                },
                {
                    fn Erc6909Op_TransferRequiresZeroApproved(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <Erc6909Op_TransferRequiresZeroApproved as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(
                                MevBotDelegateErrors::Erc6909Op_TransferRequiresZeroApproved,
                            )
                    }
                    Erc6909Op_TransferRequiresZeroApproved
                },
                {
                    fn MevBotDelegate__Unauthorized(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <MevBotDelegate__Unauthorized as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateErrors::MevBotDelegate__Unauthorized)
                    }
                    MevBotDelegate__Unauthorized
                },
                {
                    fn MevBotDelegate__ValidatorNotEnabled(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <MevBotDelegate__ValidatorNotEnabled as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(
                                MevBotDelegateErrors::MevBotDelegate__ValidatorNotEnabled,
                            )
                    }
                    MevBotDelegate__ValidatorNotEnabled
                },
                {
                    fn NotFromEntryPoint(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <NotFromEntryPoint as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevBotDelegateErrors::NotFromEntryPoint)
                    }
                    NotFromEntryPoint
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
            ) -> alloy_sol_types::Result<MevBotDelegateErrors>] = &[
                {
                    fn NativeSweepFailed(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <NativeSweepFailed as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateErrors::NativeSweepFailed)
                    }
                    NativeSweepFailed
                },
                {
                    fn Erc6909SetOperatorBlocked(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <Erc6909SetOperatorBlocked as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateErrors::Erc6909SetOperatorBlocked)
                    }
                    Erc6909SetOperatorBlocked
                },
                {
                    fn MevBotDelegate__ZeroTarget(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <MevBotDelegate__ZeroTarget as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateErrors::MevBotDelegate__ZeroTarget)
                    }
                    MevBotDelegate__ZeroTarget
                },
                {
                    fn MevBotDelegate__InvalidSignatureLength(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <MevBotDelegate__InvalidSignatureLength as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(
                                MevBotDelegateErrors::MevBotDelegate__InvalidSignatureLength,
                            )
                    }
                    MevBotDelegate__InvalidSignatureLength
                },
                {
                    fn ExecuteError(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <ExecuteError as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateErrors::ExecuteError)
                    }
                    ExecuteError
                },
                {
                    fn MevBotDelegate__ValidatorReverted(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <MevBotDelegate__ValidatorReverted as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateErrors::MevBotDelegate__ValidatorReverted)
                    }
                    MevBotDelegate__ValidatorReverted
                },
                {
                    fn MevBotDelegate__Erc6909TransferFailed(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <MevBotDelegate__Erc6909TransferFailed as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(
                                MevBotDelegateErrors::MevBotDelegate__Erc6909TransferFailed,
                            )
                    }
                    MevBotDelegate__Erc6909TransferFailed
                },
                {
                    fn MevBotDelegate__InvalidValidator(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <MevBotDelegate__InvalidValidator as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateErrors::MevBotDelegate__InvalidValidator)
                    }
                    MevBotDelegate__InvalidValidator
                },
                {
                    fn Erc6909Op_SetOperatorRequiresZeroIdAndAmount(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <Erc6909Op_SetOperatorRequiresZeroIdAndAmount as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(
                                MevBotDelegateErrors::Erc6909Op_SetOperatorRequiresZeroIdAndAmount,
                            )
                    }
                    Erc6909Op_SetOperatorRequiresZeroIdAndAmount
                },
                {
                    fn MevBotDelegate__Erc6909SetOperatorFailed(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <MevBotDelegate__Erc6909SetOperatorFailed as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(
                                MevBotDelegateErrors::MevBotDelegate__Erc6909SetOperatorFailed,
                            )
                    }
                    MevBotDelegate__Erc6909SetOperatorFailed
                },
                {
                    fn InvalidParams(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <InvalidParams as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateErrors::InvalidParams)
                    }
                    InvalidParams
                },
                {
                    fn Erc6909Op_TransferRequiresZeroApproved(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <Erc6909Op_TransferRequiresZeroApproved as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(
                                MevBotDelegateErrors::Erc6909Op_TransferRequiresZeroApproved,
                            )
                    }
                    Erc6909Op_TransferRequiresZeroApproved
                },
                {
                    fn MevBotDelegate__Unauthorized(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <MevBotDelegate__Unauthorized as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateErrors::MevBotDelegate__Unauthorized)
                    }
                    MevBotDelegate__Unauthorized
                },
                {
                    fn MevBotDelegate__ValidatorNotEnabled(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <MevBotDelegate__ValidatorNotEnabled as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(
                                MevBotDelegateErrors::MevBotDelegate__ValidatorNotEnabled,
                            )
                    }
                    MevBotDelegate__ValidatorNotEnabled
                },
                {
                    fn NotFromEntryPoint(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevBotDelegateErrors> {
                        <NotFromEntryPoint as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevBotDelegateErrors::NotFromEntryPoint)
                    }
                    NotFromEntryPoint
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
                Self::Erc6909Op_SetOperatorRequiresZeroIdAndAmount(inner) => {
                    <Erc6909Op_SetOperatorRequiresZeroIdAndAmount as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::Erc6909Op_TransferRequiresZeroApproved(inner) => {
                    <Erc6909Op_TransferRequiresZeroApproved as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::Erc6909SetOperatorBlocked(inner) => {
                    <Erc6909SetOperatorBlocked as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::ExecuteError(inner) => {
                    <ExecuteError as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
                Self::InvalidParams(inner) => {
                    <InvalidParams as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
                Self::MevBotDelegate__Erc6909SetOperatorFailed(inner) => {
                    <MevBotDelegate__Erc6909SetOperatorFailed as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::MevBotDelegate__Erc6909TransferFailed(inner) => {
                    <MevBotDelegate__Erc6909TransferFailed as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::MevBotDelegate__InvalidSignatureLength(inner) => {
                    <MevBotDelegate__InvalidSignatureLength as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::MevBotDelegate__InvalidValidator(inner) => {
                    <MevBotDelegate__InvalidValidator as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::MevBotDelegate__Unauthorized(inner) => {
                    <MevBotDelegate__Unauthorized as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::MevBotDelegate__ValidatorNotEnabled(inner) => {
                    <MevBotDelegate__ValidatorNotEnabled as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::MevBotDelegate__ValidatorReverted(inner) => {
                    <MevBotDelegate__ValidatorReverted as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::MevBotDelegate__ZeroTarget(inner) => {
                    <MevBotDelegate__ZeroTarget as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::NativeSweepFailed(inner) => {
                    <NativeSweepFailed as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::NotFromEntryPoint(inner) => {
                    <NotFromEntryPoint as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
            }
        }
        #[inline]
        fn abi_encode_raw(&self, out: &mut alloy_sol_types::private::Vec<u8>) {
            match self {
                Self::Erc6909Op_SetOperatorRequiresZeroIdAndAmount(inner) => {
                    <Erc6909Op_SetOperatorRequiresZeroIdAndAmount as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::Erc6909Op_TransferRequiresZeroApproved(inner) => {
                    <Erc6909Op_TransferRequiresZeroApproved as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::Erc6909SetOperatorBlocked(inner) => {
                    <Erc6909SetOperatorBlocked as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::ExecuteError(inner) => {
                    <ExecuteError as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::InvalidParams(inner) => {
                    <InvalidParams as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::MevBotDelegate__Erc6909SetOperatorFailed(inner) => {
                    <MevBotDelegate__Erc6909SetOperatorFailed as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::MevBotDelegate__Erc6909TransferFailed(inner) => {
                    <MevBotDelegate__Erc6909TransferFailed as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::MevBotDelegate__InvalidSignatureLength(inner) => {
                    <MevBotDelegate__InvalidSignatureLength as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::MevBotDelegate__InvalidValidator(inner) => {
                    <MevBotDelegate__InvalidValidator as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::MevBotDelegate__Unauthorized(inner) => {
                    <MevBotDelegate__Unauthorized as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::MevBotDelegate__ValidatorNotEnabled(inner) => {
                    <MevBotDelegate__ValidatorNotEnabled as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::MevBotDelegate__ValidatorReverted(inner) => {
                    <MevBotDelegate__ValidatorReverted as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::MevBotDelegate__ZeroTarget(inner) => {
                    <MevBotDelegate__ZeroTarget as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::NativeSweepFailed(inner) => {
                    <NativeSweepFailed as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::NotFromEntryPoint(inner) => {
                    <NotFromEntryPoint as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
            }
        }
    }
    #[automatically_derived]
    impl MevBotDelegateErrors {
        /**Creates a [`Erc6909Op_SetOperatorRequiresZeroIdAndAmount`] error.

```solidity
error Erc6909Op_SetOperatorRequiresZeroIdAndAmount()
```*/
        #[inline]
        pub fn erc_6909_op_set_operator_requires_zero_id_and_amount() -> Self {
            Self::Erc6909Op_SetOperatorRequiresZeroIdAndAmount(
                Erc6909Op_SetOperatorRequiresZeroIdAndAmount,
            )
        }
        /**Creates a [`Erc6909Op_TransferRequiresZeroApproved`] error.

```solidity
error Erc6909Op_TransferRequiresZeroApproved()
```*/
        #[inline]
        pub fn erc_6909_op_transfer_requires_zero_approved() -> Self {
            Self::Erc6909Op_TransferRequiresZeroApproved(
                Erc6909Op_TransferRequiresZeroApproved,
            )
        }
        /**Creates a [`Erc6909SetOperatorBlocked`] error.

```solidity
error Erc6909SetOperatorBlocked()
```*/
        #[inline]
        pub fn erc_6909_set_operator_blocked() -> Self {
            Self::Erc6909SetOperatorBlocked(Erc6909SetOperatorBlocked)
        }
        /**Creates a [`ExecuteError`] error.

```solidity
error ExecuteError(uint256,bytes)
```*/
        #[inline]
        pub fn execute_error(
            index: alloy::sol_types::private::primitives::aliases::U256,
            error: alloy::sol_types::private::Bytes,
        ) -> Self {
            Self::ExecuteError(ExecuteError {
                index: index,
                error: error,
            })
        }
        /**Creates a [`InvalidParams`] error.

```solidity
error InvalidParams()
```*/
        #[inline]
        pub fn invalid_params() -> Self {
            Self::InvalidParams(InvalidParams)
        }
        /**Creates a [`MevBotDelegate__Erc6909SetOperatorFailed`] error.

```solidity
error MevBotDelegate__Erc6909SetOperatorFailed()
```*/
        #[inline]
        pub fn mev_bot_delegate_erc_6909_set_operator_failed() -> Self {
            Self::MevBotDelegate__Erc6909SetOperatorFailed(
                MevBotDelegate__Erc6909SetOperatorFailed,
            )
        }
        /**Creates a [`MevBotDelegate__Erc6909TransferFailed`] error.

```solidity
error MevBotDelegate__Erc6909TransferFailed()
```*/
        #[inline]
        pub fn mev_bot_delegate_erc_6909_transfer_failed() -> Self {
            Self::MevBotDelegate__Erc6909TransferFailed(
                MevBotDelegate__Erc6909TransferFailed,
            )
        }
        /**Creates a [`MevBotDelegate__InvalidSignatureLength`] error.

```solidity
error MevBotDelegate__InvalidSignatureLength()
```*/
        #[inline]
        pub fn mev_bot_delegate_invalid_signature_length() -> Self {
            Self::MevBotDelegate__InvalidSignatureLength(
                MevBotDelegate__InvalidSignatureLength,
            )
        }
        /**Creates a [`MevBotDelegate__InvalidValidator`] error.

```solidity
error MevBotDelegate__InvalidValidator(address)
```*/
        #[inline]
        pub fn mev_bot_delegate_invalid_validator(
            validator: alloy::sol_types::private::Address,
        ) -> Self {
            Self::MevBotDelegate__InvalidValidator(MevBotDelegate__InvalidValidator {
                validator: validator,
            })
        }
        /**Creates a [`MevBotDelegate__Unauthorized`] error.

```solidity
error MevBotDelegate__Unauthorized()
```*/
        #[inline]
        pub fn mev_bot_delegate_unauthorized() -> Self {
            Self::MevBotDelegate__Unauthorized(MevBotDelegate__Unauthorized)
        }
        /**Creates a [`MevBotDelegate__ValidatorNotEnabled`] error.

```solidity
error MevBotDelegate__ValidatorNotEnabled(address)
```*/
        #[inline]
        pub fn mev_bot_delegate_validator_not_enabled(
            validator: alloy::sol_types::private::Address,
        ) -> Self {
            Self::MevBotDelegate__ValidatorNotEnabled(MevBotDelegate__ValidatorNotEnabled {
                validator: validator,
            })
        }
        /**Creates a [`MevBotDelegate__ValidatorReverted`] error.

```solidity
error MevBotDelegate__ValidatorReverted(address)
```*/
        #[inline]
        pub fn mev_bot_delegate_validator_reverted(
            validator: alloy::sol_types::private::Address,
        ) -> Self {
            Self::MevBotDelegate__ValidatorReverted(MevBotDelegate__ValidatorReverted {
                validator: validator,
            })
        }
        /**Creates a [`MevBotDelegate__ZeroTarget`] error.

```solidity
error MevBotDelegate__ZeroTarget()
```*/
        #[inline]
        pub fn mev_bot_delegate_zero_target() -> Self {
            Self::MevBotDelegate__ZeroTarget(MevBotDelegate__ZeroTarget)
        }
        /**Creates a [`NativeSweepFailed`] error.

```solidity
error NativeSweepFailed()
```*/
        #[inline]
        pub fn native_sweep_failed() -> Self {
            Self::NativeSweepFailed(NativeSweepFailed)
        }
        /**Creates a [`NotFromEntryPoint`] error.

```solidity
error NotFromEntryPoint(address,address,address)
```*/
        #[inline]
        pub fn not_from_entry_point(
            msg_sender: alloy::sol_types::private::Address,
            entity: alloy::sol_types::private::Address,
            entry_point: alloy::sol_types::private::Address,
        ) -> Self {
            Self::NotFromEntryPoint(NotFromEntryPoint {
                msgSender: msg_sender,
                entity: entity,
                entryPoint: entry_point,
            })
        }
    }
    ///Container for all the [`MevBotDelegate`](self) events.
    #[derive(Clone)]
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub enum MevBotDelegateEvents {
        #[allow(missing_docs)]
        BotDelegateActivated(BotDelegateActivated),
        #[allow(missing_docs)]
        Erc20Swept(Erc20Swept),
        #[allow(missing_docs)]
        Erc3009Received(Erc3009Received),
        #[allow(missing_docs)]
        Erc3009Relayed(Erc3009Relayed),
        #[allow(missing_docs)]
        Erc6909OperatorSet(Erc6909OperatorSet),
        #[allow(missing_docs)]
        Erc6909Transferred(Erc6909Transferred),
        #[allow(missing_docs)]
        ValidatorAdded(ValidatorAdded),
        #[allow(missing_docs)]
        ValidatorRemoved(ValidatorRemoved),
    }
    impl MevBotDelegateEvents {
        /// All the selectors of this enum.
        ///
        /// Note that the selectors might not be in the same order as the variants.
        /// No guarantees are made about the order of the selectors.
        ///
        /// Prefer using `SolInterface` methods instead.
        pub const SELECTORS: &'static [[u8; 32usize]] = &[
            [
                2u8, 203u8, 84u8, 131u8, 249u8, 198u8, 183u8, 105u8, 199u8, 181u8, 143u8,
                215u8, 189u8, 5u8, 4u8, 39u8, 234u8, 86u8, 107u8, 211u8, 87u8, 200u8,
                111u8, 111u8, 92u8, 114u8, 76u8, 66u8, 192u8, 225u8, 116u8, 90u8,
            ],
            [
                46u8, 62u8, 136u8, 204u8, 195u8, 163u8, 192u8, 102u8, 70u8, 181u8, 158u8,
                213u8, 150u8, 74u8, 193u8, 106u8, 149u8, 168u8, 113u8, 224u8, 255u8,
                252u8, 241u8, 21u8, 111u8, 82u8, 162u8, 106u8, 138u8, 102u8, 38u8, 249u8,
            ],
            [
                74u8, 148u8, 248u8, 158u8, 19u8, 22u8, 153u8, 237u8, 52u8, 22u8, 103u8,
                12u8, 1u8, 28u8, 230u8, 77u8, 98u8, 229u8, 165u8, 129u8, 164u8, 235u8,
                180u8, 96u8, 59u8, 246u8, 196u8, 165u8, 208u8, 106u8, 6u8, 206u8,
            ],
            [
                97u8, 32u8, 248u8, 153u8, 176u8, 64u8, 171u8, 14u8, 56u8, 201u8, 5u8,
                157u8, 23u8, 109u8, 45u8, 251u8, 86u8, 39u8, 62u8, 147u8, 183u8, 170u8,
                85u8, 150u8, 53u8, 246u8, 92u8, 94u8, 200u8, 78u8, 201u8, 19u8,
            ],
            [
                114u8, 143u8, 205u8, 137u8, 110u8, 216u8, 118u8, 79u8, 0u8, 176u8, 108u8,
                62u8, 164u8, 132u8, 123u8, 80u8, 17u8, 181u8, 63u8, 162u8, 139u8, 228u8,
                81u8, 214u8, 162u8, 172u8, 235u8, 239u8, 96u8, 130u8, 125u8, 160u8,
            ],
            [
                156u8, 142u8, 23u8, 250u8, 17u8, 77u8, 36u8, 207u8, 200u8, 246u8, 124u8,
                61u8, 108u8, 230u8, 188u8, 46u8, 36u8, 6u8, 125u8, 190u8, 65u8, 37u8,
                102u8, 64u8, 188u8, 72u8, 253u8, 109u8, 16u8, 102u8, 86u8, 47u8,
            ],
            [
                225u8, 67u8, 78u8, 37u8, 214u8, 97u8, 30u8, 13u8, 185u8, 65u8, 150u8,
                143u8, 220u8, 151u8, 129u8, 28u8, 152u8, 42u8, 193u8, 96u8, 46u8, 149u8,
                22u8, 55u8, 210u8, 6u8, 245u8, 253u8, 218u8, 157u8, 216u8, 241u8,
            ],
            [
                227u8, 102u8, 193u8, 192u8, 69u8, 46u8, 216u8, 238u8, 201u8, 104u8, 97u8,
                233u8, 229u8, 65u8, 65u8, 235u8, 255u8, 35u8, 201u8, 236u8, 137u8, 254u8,
                39u8, 185u8, 150u8, 180u8, 95u8, 94u8, 195u8, 136u8, 73u8, 135u8,
            ],
        ];
        /// The names of the variants in the same order as `SELECTORS`.
        pub const VARIANT_NAMES: &'static [&'static str] = &[
            ::core::stringify!(BotDelegateActivated),
            ::core::stringify!(Erc3009Received),
            ::core::stringify!(Erc6909Transferred),
            ::core::stringify!(Erc20Swept),
            ::core::stringify!(Erc3009Relayed),
            ::core::stringify!(Erc6909OperatorSet),
            ::core::stringify!(ValidatorRemoved),
            ::core::stringify!(ValidatorAdded),
        ];
        /// The signatures in the same order as `SELECTORS`.
        pub const SIGNATURES: &'static [&'static str] = &[
            <BotDelegateActivated as alloy_sol_types::SolEvent>::SIGNATURE,
            <Erc3009Received as alloy_sol_types::SolEvent>::SIGNATURE,
            <Erc6909Transferred as alloy_sol_types::SolEvent>::SIGNATURE,
            <Erc20Swept as alloy_sol_types::SolEvent>::SIGNATURE,
            <Erc3009Relayed as alloy_sol_types::SolEvent>::SIGNATURE,
            <Erc6909OperatorSet as alloy_sol_types::SolEvent>::SIGNATURE,
            <ValidatorRemoved as alloy_sol_types::SolEvent>::SIGNATURE,
            <ValidatorAdded as alloy_sol_types::SolEvent>::SIGNATURE,
        ];
        /// Returns the signature for the given selector, if known.
        #[inline]
        pub fn signature_by_selector(
            selector: [u8; 32usize],
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
            selector: [u8; 32usize],
        ) -> ::core::option::Option<&'static str> {
            let sig = Self::signature_by_selector(selector)?;
            sig.split_once('(').map(|(name, _)| name)
        }
    }
    #[automatically_derived]
    impl alloy_sol_types::SolEventInterface for MevBotDelegateEvents {
        const NAME: &'static str = "MevBotDelegateEvents";
        const COUNT: usize = 8usize;
        fn decode_raw_log(
            topics: &[alloy_sol_types::Word],
            data: &[u8],
        ) -> alloy_sol_types::Result<Self> {
            match topics.first().copied() {
                Some(
                    <BotDelegateActivated as alloy_sol_types::SolEvent>::SIGNATURE_HASH,
                ) => {
                    <BotDelegateActivated as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::BotDelegateActivated)
                }
                Some(<Erc20Swept as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <Erc20Swept as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::Erc20Swept)
                }
                Some(<Erc3009Received as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <Erc3009Received as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::Erc3009Received)
                }
                Some(<Erc3009Relayed as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <Erc3009Relayed as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::Erc3009Relayed)
                }
                Some(
                    <Erc6909OperatorSet as alloy_sol_types::SolEvent>::SIGNATURE_HASH,
                ) => {
                    <Erc6909OperatorSet as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::Erc6909OperatorSet)
                }
                Some(
                    <Erc6909Transferred as alloy_sol_types::SolEvent>::SIGNATURE_HASH,
                ) => {
                    <Erc6909Transferred as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::Erc6909Transferred)
                }
                Some(<ValidatorAdded as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <ValidatorAdded as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::ValidatorAdded)
                }
                Some(<ValidatorRemoved as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <ValidatorRemoved as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::ValidatorRemoved)
                }
                _ => {
                    alloy_sol_types::private::Err(alloy_sol_types::Error::InvalidLog {
                        name: <Self as alloy_sol_types::SolEventInterface>::NAME,
                        log: alloy_sol_types::private::Box::new(
                            alloy_sol_types::private::LogData::new_unchecked(
                                topics.to_vec(),
                                data.to_vec().into(),
                            ),
                        ),
                    })
                }
            }
        }
    }
    #[automatically_derived]
    impl alloy_sol_types::private::IntoLogData for MevBotDelegateEvents {
        fn to_log_data(&self) -> alloy_sol_types::private::LogData {
            match self {
                Self::BotDelegateActivated(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Erc20Swept(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Erc3009Received(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Erc3009Relayed(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Erc6909OperatorSet(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Erc6909Transferred(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::ValidatorAdded(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::ValidatorRemoved(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
            }
        }
        fn into_log_data(self) -> alloy_sol_types::private::LogData {
            match self {
                Self::BotDelegateActivated(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Erc20Swept(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Erc3009Received(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Erc3009Relayed(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Erc6909OperatorSet(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Erc6909Transferred(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::ValidatorAdded(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::ValidatorRemoved(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
            }
        }
    }
    #[automatically_derived]
    impl MevBotDelegateEvents {
        /**Creates a [`BotDelegateActivated`] event.

```solidity
event BotDelegateActivated(address,uint256,bytes4)
```*/
        #[inline]
        pub fn bot_delegate_activated(
            target: alloy::sol_types::private::Address,
            value: alloy::sol_types::private::primitives::aliases::U256,
            selector: alloy::sol_types::private::FixedBytes<4>,
        ) -> Self {
            Self::BotDelegateActivated(BotDelegateActivated {
                target: target,
                value: value,
                selector: selector,
            })
        }
        /**Creates a [`Erc20Swept`] event.

```solidity
event Erc20Swept(address,address,uint256)
```*/
        #[inline]
        pub fn erc_20_swept(
            token: alloy::sol_types::private::Address,
            to: alloy::sol_types::private::Address,
            amount: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::Erc20Swept(Erc20Swept {
                token: token,
                to: to,
                amount: amount,
            })
        }
        /**Creates a [`Erc3009Received`] event.

```solidity
event Erc3009Received(address,address,uint256,bytes32)
```*/
        #[inline]
        pub fn erc_3009_received(
            token: alloy::sol_types::private::Address,
            from: alloy::sol_types::private::Address,
            value: alloy::sol_types::private::primitives::aliases::U256,
            nonce: alloy::sol_types::private::FixedBytes<32>,
        ) -> Self {
            Self::Erc3009Received(Erc3009Received {
                token: token,
                from: from,
                value: value,
                nonce: nonce,
            })
        }
        /**Creates a [`Erc3009Relayed`] event.

```solidity
event Erc3009Relayed(address,address,address,uint256,bytes32)
```*/
        #[inline]
        pub fn erc_3009_relayed(
            token: alloy::sol_types::private::Address,
            from: alloy::sol_types::private::Address,
            to: alloy::sol_types::private::Address,
            value: alloy::sol_types::private::primitives::aliases::U256,
            nonce: alloy::sol_types::private::FixedBytes<32>,
        ) -> Self {
            Self::Erc3009Relayed(Erc3009Relayed {
                token: token,
                from: from,
                to: to,
                value: value,
                nonce: nonce,
            })
        }
        /**Creates a [`Erc6909OperatorSet`] event.

```solidity
event Erc6909OperatorSet(address,address,bool)
```*/
        #[inline]
        pub fn erc_6909_operator_set(
            token: alloy::sol_types::private::Address,
            operator: alloy::sol_types::private::Address,
            approved: bool,
        ) -> Self {
            Self::Erc6909OperatorSet(Erc6909OperatorSet {
                token: token,
                operator: operator,
                approved: approved,
            })
        }
        /**Creates a [`Erc6909Transferred`] event.

```solidity
event Erc6909Transferred(address,address,uint256,uint256)
```*/
        #[inline]
        pub fn erc_6909_transferred(
            token: alloy::sol_types::private::Address,
            to: alloy::sol_types::private::Address,
            id: alloy::sol_types::private::primitives::aliases::U256,
            amount: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::Erc6909Transferred(Erc6909Transferred {
                token: token,
                to: to,
                id: id,
                amount: amount,
            })
        }
        /**Creates a [`ValidatorAdded`] event.

```solidity
event ValidatorAdded(address)
```*/
        #[inline]
        pub fn validator_added(validator: alloy::sol_types::private::Address) -> Self {
            Self::ValidatorAdded(ValidatorAdded {
                validator: validator,
            })
        }
        /**Creates a [`ValidatorRemoved`] event.

```solidity
event ValidatorRemoved(address)
```*/
        #[inline]
        pub fn validator_removed(validator: alloy::sol_types::private::Address) -> Self {
            Self::ValidatorRemoved(ValidatorRemoved {
                validator: validator,
            })
        }
    }
    use alloy::contract as alloy_contract;
    /**Creates a new wrapper around an on-chain [`MevBotDelegate`](self) contract instance.

See the [wrapper's documentation](`MevBotDelegateInstance`) for more details.*/
    #[inline]
    pub const fn new<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    >(
        address: alloy_sol_types::private::Address,
        __provider: P,
    ) -> MevBotDelegateInstance<P, N> {
        MevBotDelegateInstance::<P, N>::new(address, __provider)
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
        Output = alloy_contract::Result<MevBotDelegateInstance<P, N>>,
    > {
        MevBotDelegateInstance::<P, N>::deploy(__provider)
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
        MevBotDelegateInstance::<P, N>::deploy_builder(__provider)
    }
    /**A [`MevBotDelegate`](self) instance.

Contains type-safe methods for interacting with an on-chain instance of the
[`MevBotDelegate`](self) contract located at a given `address`, using a given
provider `P`.

If the contract bytecode is available (see the [`sol!`](alloy_sol_types::sol!)
documentation on how to provide it), the `deploy` and `deploy_builder` methods can
be used to deploy a new instance of the contract.

See the [module-level documentation](self) for all the available methods.*/
    #[derive(Clone)]
    pub struct MevBotDelegateInstance<P, N = alloy_contract::private::Ethereum> {
        address: alloy_sol_types::private::Address,
        provider: P,
        _network: ::core::marker::PhantomData<N>,
    }
    #[automatically_derived]
    impl<P, N> ::core::fmt::Debug for MevBotDelegateInstance<P, N> {
        #[inline]
        fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
            f.debug_tuple("MevBotDelegateInstance").field(&self.address).finish()
        }
    }
    /// Instantiation and getters/setters.
    impl<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    > MevBotDelegateInstance<P, N> {
        /**Creates a new wrapper around an on-chain [`MevBotDelegate`](self) contract instance.

See the [wrapper's documentation](`MevBotDelegateInstance`) for more details.*/
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
        ) -> alloy_contract::Result<MevBotDelegateInstance<P, N>> {
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
    impl<P: ::core::clone::Clone, N> MevBotDelegateInstance<&P, N> {
        /// Clones the provider and returns a new instance with the cloned provider.
        #[inline]
        pub fn with_cloned_provider(self) -> MevBotDelegateInstance<P, N> {
            MevBotDelegateInstance {
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
    > MevBotDelegateInstance<P, N> {
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
        ///Creates a new call builder for the [`ENTRY_POINT_ADDR`] function.
        pub fn ENTRY_POINT_ADDR(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, ENTRY_POINT_ADDRCall, N> {
            self.call_builder(&ENTRY_POINT_ADDRCall)
        }
        ///Creates a new call builder for the [`ENTRY_POINT_V06`] function.
        pub fn ENTRY_POINT_V06(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, ENTRY_POINT_V06Call, N> {
            self.call_builder(&ENTRY_POINT_V06Call)
        }
        ///Creates a new call builder for the [`ENTRY_POINT_V07`] function.
        pub fn ENTRY_POINT_V07(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, ENTRY_POINT_V07Call, N> {
            self.call_builder(&ENTRY_POINT_V07Call)
        }
        ///Creates a new call builder for the [`ENTRY_POINT_V08`] function.
        pub fn ENTRY_POINT_V08(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, ENTRY_POINT_V08Call, N> {
            self.call_builder(&ENTRY_POINT_V08Call)
        }
        ///Creates a new call builder for the [`entryPoint`] function.
        pub fn entryPoint(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, entryPointCall, N> {
            self.call_builder(&entryPointCall)
        }
        ///Creates a new call builder for the [`execute`] function.
        pub fn execute(
            &self,
            target: alloy::sol_types::private::Address,
            value: alloy::sol_types::private::primitives::aliases::U256,
            data: alloy::sol_types::private::Bytes,
        ) -> alloy_contract::SolCallBuilder<&P, executeCall, N> {
            self.call_builder(&executeCall { target, value, data })
        }
        ///Creates a new call builder for the [`executeBatch`] function.
        pub fn executeBatch(
            &self,
            calls: alloy::sol_types::private::Vec<
                <BaseAccount::Call as alloy::sol_types::SolType>::RustType,
            >,
        ) -> alloy_contract::SolCallBuilder<&P, executeBatchCall, N> {
            self.call_builder(&executeBatchCall { calls })
        }
        ///Creates a new call builder for the [`executeErc6909Batch`] function.
        pub fn executeErc6909Batch(
            &self,
            calls: alloy::sol_types::private::Vec<
                <Erc6909Call as alloy::sol_types::SolType>::RustType,
            >,
        ) -> alloy_contract::SolCallBuilder<&P, executeErc6909BatchCall, N> {
            self.call_builder(&executeErc6909BatchCall { calls })
        }
        ///Creates a new call builder for the [`getNonce`] function.
        pub fn getNonce(&self) -> alloy_contract::SolCallBuilder<&P, getNonceCall, N> {
            self.call_builder(&getNonceCall)
        }
        ///Creates a new call builder for the [`isEntryPoint`] function.
        pub fn isEntryPoint(
            &self,
            caller: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, isEntryPointCall, N> {
            self.call_builder(&isEntryPointCall { caller })
        }
        ///Creates a new call builder for the [`isValidSignature`] function.
        pub fn isValidSignature(
            &self,
            hash: alloy::sol_types::private::FixedBytes<32>,
            signature: alloy::sol_types::private::Bytes,
        ) -> alloy_contract::SolCallBuilder<&P, isValidSignatureCall, N> {
            self.call_builder(
                &isValidSignatureCall {
                    hash,
                    signature,
                },
            )
        }
        ///Creates a new call builder for the [`isValidatorEnabled`] function.
        pub fn isValidatorEnabled(
            &self,
            validator: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, isValidatorEnabledCall, N> {
            self.call_builder(
                &isValidatorEnabledCall {
                    validator,
                },
            )
        }
        ///Creates a new call builder for the [`nonceFor`] function.
        pub fn nonceFor(
            &self,
            ep: alloy::sol_types::private::Address,
            key: alloy::sol_types::private::primitives::aliases::U192,
        ) -> alloy_contract::SolCallBuilder<&P, nonceForCall, N> {
            self.call_builder(&nonceForCall { ep, key })
        }
        ///Creates a new call builder for the [`onERC1155BatchReceived`] function.
        pub fn onERC1155BatchReceived(
            &self,
            _0: alloy::sol_types::private::Address,
            _1: alloy::sol_types::private::Address,
            _2: alloy::sol_types::private::Vec<
                alloy::sol_types::private::primitives::aliases::U256,
            >,
            _3: alloy::sol_types::private::Vec<
                alloy::sol_types::private::primitives::aliases::U256,
            >,
            _4: alloy::sol_types::private::Bytes,
        ) -> alloy_contract::SolCallBuilder<&P, onERC1155BatchReceivedCall, N> {
            self.call_builder(
                &onERC1155BatchReceivedCall {
                    _0,
                    _1,
                    _2,
                    _3,
                    _4,
                },
            )
        }
        ///Creates a new call builder for the [`onERC1155Received`] function.
        pub fn onERC1155Received(
            &self,
            _0: alloy::sol_types::private::Address,
            _1: alloy::sol_types::private::Address,
            _2: alloy::sol_types::private::primitives::aliases::U256,
            _3: alloy::sol_types::private::primitives::aliases::U256,
            _4: alloy::sol_types::private::Bytes,
        ) -> alloy_contract::SolCallBuilder<&P, onERC1155ReceivedCall, N> {
            self.call_builder(
                &onERC1155ReceivedCall {
                    _0,
                    _1,
                    _2,
                    _3,
                    _4,
                },
            )
        }
        ///Creates a new call builder for the [`onERC721Received`] function.
        pub fn onERC721Received(
            &self,
            _0: alloy::sol_types::private::Address,
            _1: alloy::sol_types::private::Address,
            _2: alloy::sol_types::private::primitives::aliases::U256,
            _3: alloy::sol_types::private::Bytes,
        ) -> alloy_contract::SolCallBuilder<&P, onERC721ReceivedCall, N> {
            self.call_builder(
                &onERC721ReceivedCall {
                    _0,
                    _1,
                    _2,
                    _3,
                },
            )
        }
        ///Creates a new call builder for the [`receiveErc3009`] function.
        pub fn receiveErc3009(
            &self,
            token: alloy::sol_types::private::Address,
            from: alloy::sol_types::private::Address,
            value: alloy::sol_types::private::primitives::aliases::U256,
            validAfter: alloy::sol_types::private::primitives::aliases::U256,
            validBefore: alloy::sol_types::private::primitives::aliases::U256,
            nonce: alloy::sol_types::private::FixedBytes<32>,
            v: u8,
            r: alloy::sol_types::private::FixedBytes<32>,
            s: alloy::sol_types::private::FixedBytes<32>,
        ) -> alloy_contract::SolCallBuilder<&P, receiveErc3009Call, N> {
            self.call_builder(
                &receiveErc3009Call {
                    token,
                    from,
                    value,
                    validAfter,
                    validBefore,
                    nonce,
                    v,
                    r,
                    s,
                },
            )
        }
        ///Creates a new call builder for the [`setValidator`] function.
        pub fn setValidator(
            &self,
            validator: alloy::sol_types::private::Address,
            enabled: bool,
        ) -> alloy_contract::SolCallBuilder<&P, setValidatorCall, N> {
            self.call_builder(
                &setValidatorCall {
                    validator,
                    enabled,
                },
            )
        }
        ///Creates a new call builder for the [`supportsInterface`] function.
        pub fn supportsInterface(
            &self,
            id: alloy::sol_types::private::FixedBytes<4>,
        ) -> alloy_contract::SolCallBuilder<&P, supportsInterfaceCall, N> {
            self.call_builder(&supportsInterfaceCall { id })
        }
        ///Creates a new call builder for the [`sweepERC20`] function.
        pub fn sweepERC20(
            &self,
            token: alloy::sol_types::private::Address,
            to: alloy::sol_types::private::Address,
            amount: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, sweepERC20Call, N> {
            self.call_builder(
                &sweepERC20Call {
                    token,
                    to,
                    amount,
                },
            )
        }
        ///Creates a new call builder for the [`sweepERC20Batch`] function.
        pub fn sweepERC20Batch(
            &self,
            tokens: alloy::sol_types::private::Vec<alloy::sol_types::private::Address>,
            to: alloy::sol_types::private::Address,
            amounts: alloy::sol_types::private::Vec<
                alloy::sol_types::private::primitives::aliases::U256,
            >,
        ) -> alloy_contract::SolCallBuilder<&P, sweepERC20BatchCall, N> {
            self.call_builder(
                &sweepERC20BatchCall {
                    tokens,
                    to,
                    amounts,
                },
            )
        }
        ///Creates a new call builder for the [`transferErc3009`] function.
        pub fn transferErc3009(
            &self,
            token: alloy::sol_types::private::Address,
            from: alloy::sol_types::private::Address,
            to: alloy::sol_types::private::Address,
            value: alloy::sol_types::private::primitives::aliases::U256,
            validAfter: alloy::sol_types::private::primitives::aliases::U256,
            validBefore: alloy::sol_types::private::primitives::aliases::U256,
            nonce: alloy::sol_types::private::FixedBytes<32>,
            v: u8,
            r: alloy::sol_types::private::FixedBytes<32>,
            s: alloy::sol_types::private::FixedBytes<32>,
        ) -> alloy_contract::SolCallBuilder<&P, transferErc3009Call, N> {
            self.call_builder(
                &transferErc3009Call {
                    token,
                    from,
                    to,
                    value,
                    validAfter,
                    validBefore,
                    nonce,
                    v,
                    r,
                    s,
                },
            )
        }
        ///Creates a new call builder for the [`validateUserOp`] function.
        pub fn validateUserOp(
            &self,
            userOp: <PackedUserOperation as alloy::sol_types::SolType>::RustType,
            userOpHash: alloy::sol_types::private::FixedBytes<32>,
            missingAccountFunds: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, validateUserOpCall, N> {
            self.call_builder(
                &validateUserOpCall {
                    userOp,
                    userOpHash,
                    missingAccountFunds,
                },
            )
        }
    }
    /// Event filters.
    impl<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    > MevBotDelegateInstance<P, N> {
        /// Creates a new event filter using this contract instance's provider and address.
        ///
        /// Note that the type can be any event, not just those defined in this contract.
        /// Prefer using the other methods for building type-safe event filters.
        pub fn event_filter<E: alloy_sol_types::SolEvent>(
            &self,
        ) -> alloy_contract::Event<&P, E, N> {
            alloy_contract::Event::new_sol(&self.provider, &self.address)
        }
        ///Creates a new event filter for the [`BotDelegateActivated`] event.
        pub fn BotDelegateActivated_filter(
            &self,
        ) -> alloy_contract::Event<&P, BotDelegateActivated, N> {
            self.event_filter::<BotDelegateActivated>()
        }
        ///Creates a new event filter for the [`Erc20Swept`] event.
        pub fn Erc20Swept_filter(&self) -> alloy_contract::Event<&P, Erc20Swept, N> {
            self.event_filter::<Erc20Swept>()
        }
        ///Creates a new event filter for the [`Erc3009Received`] event.
        pub fn Erc3009Received_filter(
            &self,
        ) -> alloy_contract::Event<&P, Erc3009Received, N> {
            self.event_filter::<Erc3009Received>()
        }
        ///Creates a new event filter for the [`Erc3009Relayed`] event.
        pub fn Erc3009Relayed_filter(
            &self,
        ) -> alloy_contract::Event<&P, Erc3009Relayed, N> {
            self.event_filter::<Erc3009Relayed>()
        }
        ///Creates a new event filter for the [`Erc6909OperatorSet`] event.
        pub fn Erc6909OperatorSet_filter(
            &self,
        ) -> alloy_contract::Event<&P, Erc6909OperatorSet, N> {
            self.event_filter::<Erc6909OperatorSet>()
        }
        ///Creates a new event filter for the [`Erc6909Transferred`] event.
        pub fn Erc6909Transferred_filter(
            &self,
        ) -> alloy_contract::Event<&P, Erc6909Transferred, N> {
            self.event_filter::<Erc6909Transferred>()
        }
        ///Creates a new event filter for the [`ValidatorAdded`] event.
        pub fn ValidatorAdded_filter(
            &self,
        ) -> alloy_contract::Event<&P, ValidatorAdded, N> {
            self.event_filter::<ValidatorAdded>()
        }
        ///Creates a new event filter for the [`ValidatorRemoved`] event.
        pub fn ValidatorRemoved_filter(
            &self,
        ) -> alloy_contract::Event<&P, ValidatorRemoved, N> {
            self.event_filter::<ValidatorRemoved>()
        }
    }
}
