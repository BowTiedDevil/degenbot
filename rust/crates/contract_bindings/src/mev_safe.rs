///Module containing a contract's types and functions.
/**

```solidity
library LpTransferLib {
    type LpKind is uint8;
}
```*/
#[allow(
    non_camel_case_types,
    non_snake_case,
    clippy::pub_underscore_fields,
    clippy::style,
    clippy::empty_structs_with_brackets
)]
pub mod LpTransferLib {
    use super::*;
    use alloy::sol_types as alloy_sol_types;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct LpKind(u8);
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[automatically_derived]
        impl alloy_sol_types::private::SolTypeValue<LpKind> for u8 {
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
        impl LpKind {
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
        impl From<u8> for LpKind {
            fn from(value: u8) -> Self {
                Self::from_underlying(value)
            }
        }
        #[automatically_derived]
        impl From<LpKind> for u8 {
            fn from(value: LpKind) -> Self {
                value.into_underlying()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolType for LpKind {
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
        impl alloy_sol_types::EventTopic for LpKind {
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
    use alloy::contract as alloy_contract;
    /**Creates a new wrapper around an on-chain [`LpTransferLib`](self) contract instance.

See the [wrapper's documentation](`LpTransferLibInstance`) for more details.*/
    #[inline]
    pub const fn new<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    >(
        address: alloy_sol_types::private::Address,
        __provider: P,
    ) -> LpTransferLibInstance<P, N> {
        LpTransferLibInstance::<P, N>::new(address, __provider)
    }
    /**A [`LpTransferLib`](self) instance.

Contains type-safe methods for interacting with an on-chain instance of the
[`LpTransferLib`](self) contract located at a given `address`, using a given
provider `P`.

If the contract bytecode is available (see the [`sol!`](alloy_sol_types::sol!)
documentation on how to provide it), the `deploy` and `deploy_builder` methods can
be used to deploy a new instance of the contract.

See the [module-level documentation](self) for all the available methods.*/
    #[derive(Clone)]
    pub struct LpTransferLibInstance<P, N = alloy_contract::private::Ethereum> {
        address: alloy_sol_types::private::Address,
        provider: P,
        _network: ::core::marker::PhantomData<N>,
    }
    #[automatically_derived]
    impl<P, N> ::core::fmt::Debug for LpTransferLibInstance<P, N> {
        #[inline]
        fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
            f.debug_tuple("LpTransferLibInstance").field(&self.address).finish()
        }
    }
    /// Instantiation and getters/setters.
    impl<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    > LpTransferLibInstance<P, N> {
        /**Creates a new wrapper around an on-chain [`LpTransferLib`](self) contract instance.

See the [wrapper's documentation](`LpTransferLibInstance`) for more details.*/
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
    impl<P: ::core::clone::Clone, N> LpTransferLibInstance<&P, N> {
        /// Clones the provider and returns a new instance with the cloned provider.
        #[inline]
        pub fn with_cloned_provider(self) -> LpTransferLibInstance<P, N> {
            LpTransferLibInstance {
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
    > LpTransferLibInstance<P, N> {
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
    > LpTransferLibInstance<P, N> {
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
library LpTransferLib {
    type LpKind is uint8;
}

interface MevSafe {
    type Erc6909Op is uint8;
    struct Call {
        address target;
        uint256 value;
        bytes data;
    }
    struct Erc6909Call {
        Erc6909Op op;
        address token;
        address counterparty;
        uint256 id;
        uint256 amount;
        bool approved;
    }
    struct FinancePlan {
        address flashLender;
        address flashAsset;
        uint256 flashAmount;
        Call[] preActions;
        address aavePool;
        address collateralAsset;
        uint256 supplyAmount;
        address debtAsset;
        uint256 borrowAmount;
        uint256 interestRateMode;
        Call[] postActions;
        int256 minDeltaFlashAsset;
    }
    struct FinancePlanV3 {
        address flashAsset;
        uint256 flashAmount;
        Call[] preActions;
        address aavePool;
        address collateralAsset;
        uint256 supplyAmount;
        address debtAsset;
        uint256 borrowAmount;
        uint256 interestRateMode;
        Call[] postActions;
        int256 minDeltaFlashAsset;
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

    error CallFailed(uint256 index, bytes returnData);
    error EnforcedPause();
    error Erc6909Op_SetOperatorRequiresZeroIdAndAmount();
    error Erc6909Op_TransferRequiresZeroApproved();
    error Erc6909SetOperatorBlocked();
    error ExpectedPause();
    error FinanceUnprofitable(int256 delta, int256 minDelta);
    error FlashAmountMismatch();
    error InvalidParams();
    error LpTransferLib__V4SetOperatorFailed();
    error LpTransferLib__V4TransferFailed();
    error MevSafe__NativeSweepFailed();
    error MevSafe__UnsupportedFlashLender(address lender);
    error NoActiveFlash();
    error NotAuthorized();
    error NotEntryPoint();
    error NotFlashLender();
    error OnlyV3Vault();
    error OwnableInvalidOwner(address owner);
    error OwnableUnauthorizedAccount(address account);
    error PlanHashMismatch();
    error Reentrancy();
    error V3SettleShortfall(uint256 expected, uint256 credit);

    event CollateralizationFinanced(bytes32 indexed opId, address indexed flashAsset, uint256 flashAmount, address collateralAsset, uint256 supplyAmount, address indexed debtAsset, uint256 borrowAmount, uint256 flashRepaid, int256 netDeltaFlash);
    event Erc6909OperatorSet(address indexed token, address indexed operator, bool approved);
    event Erc6909Transferred(address indexed token, address indexed to, uint256 indexed id, uint256 amount);
    event Executed(address indexed target, uint256 value, bytes4 selector);
    event LegacyDelegateeSet(address indexed account, bool allowed);
    event LpMoved(LpTransferLib.LpKind indexed kind, address indexed pool, uint256 idOrTokenId, uint256 amount, address indexed to);
    event OwnershipTransferStarted(address indexed previousOwner, address indexed newOwner);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);
    event Paused(address account);
    event Received(bytes4 indexed standard, address indexed sender, uint256 indexed tokenId, uint256 value);
    event SignerSet(address indexed signer, bool allowed);
    event ThresholdSet(uint256 newThreshold);
    event Unpaused(address account);
    event V4OperatorSet(address indexed poolManager, address indexed operator, bool approved);

    constructor(address initialOwner, address permissions);

    receive() external payable;

    function AAVE_V3_POOL() external view returns (address);
    function BALANCER_V2_VAULT() external view returns (address);
    function BALANCER_V3_VAULT() external view returns (address);
    function ENTRY_POINT() external view returns (address);
    function MORPHO_BLUE() external view returns (address);
    function PERMISSIONS() external view returns (address);
    function aaveBorrow(address pool, address asset, uint256 amount, uint256 interestRateMode) external;
    function aaveRepay(address pool, address asset, uint256 amount, uint256 interestRateMode) external returns (uint256 repaid);
    function aaveSupply(address pool, address asset, uint256 amount) external;
    function aaveWithdraw(address pool, address asset, uint256 amount) external returns (uint256 withdrawn);
    function acceptOwnership() external;
    function execute(Call memory c) external payable;
    function executeBatch(Call[] memory calls) external payable;
    function executeErc6909Batch(Erc6909Call[] memory calls) external;
    function executeFinanceUnlock(FinancePlanV3 memory plan) external;
    function flashCollateralize(FinancePlan memory plan) external;
    function flashCollateralizeV3(FinancePlanV3 memory plan) external;
    function isAuthorized(address account, address target, bytes4 selector) external view returns (bool);
    function isSigner(address) external view returns (bool);
    function isValidSignature(bytes32 hash, bytes memory signature) external view returns (bytes4);
    function legacyDelegatees(address) external view returns (bool);
    function moveLpV3Nft(address positionManager, uint256 tokenId, address to) external;
    function moveLpV4(address poolManager, uint256 id, uint256 amount, address to) external;
    function onERC1155BatchReceived(address operator, address from, uint256[] memory ids, uint256[] memory values, bytes memory data) external returns (bytes4);
    function onERC1155Received(address operator, address from, uint256 id, uint256 value, bytes memory data) external returns (bytes4);
    function onERC721Received(address operator, address from, uint256 tokenId, bytes memory data) external returns (bytes4);
    function owner() external view returns (address);
    function pause() external;
    function paused() external view returns (bool);
    function pendingOwner() external view returns (address);
    function receiveFlashLoan(address[] memory tokens, uint256[] memory amounts, uint256[] memory feeAmounts, bytes memory userData) external;
    function renounceOwnership() external;
    function setLegacyDelegatee(address account, bool allowed) external;
    function setSigner(address signer, bool allowed) external;
    function setThreshold(uint256 t) external;
    function setV4Operator(address poolManager, address operator, bool approved) external;
    function signerCount() external view returns (uint256);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
    function sweepBatch(address[] memory tokens, address to, uint256[] memory amounts) external;
    function sweepERC20(address token, address to, uint256 amount) external;
    function threshold() external view returns (uint256);
    function transferOwnership(address newOwner) external;
    function unpause() external;
    function validateUserOp(PackedUserOperation memory userOp, bytes32 userOpHash, uint256 missingAccountFunds) external returns (uint256 validationData);
}
```

...which was generated by the following JSON ABI:
```json
[
  {
    "type": "constructor",
    "inputs": [
      {
        "name": "initialOwner",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "permissions",
        "type": "address",
        "internalType": "contract PermissionToken"
      }
    ],
    "stateMutability": "nonpayable"
  },
  {
    "type": "receive",
    "stateMutability": "payable"
  },
  {
    "type": "function",
    "name": "AAVE_V3_POOL",
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
    "name": "BALANCER_V2_VAULT",
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
    "name": "BALANCER_V3_VAULT",
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
    "name": "ENTRY_POINT",
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
    "name": "MORPHO_BLUE",
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
    "name": "PERMISSIONS",
    "inputs": [],
    "outputs": [
      {
        "name": "",
        "type": "address",
        "internalType": "contract PermissionToken"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "aaveBorrow",
    "inputs": [
      {
        "name": "pool",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "asset",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "amount",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "interestRateMode",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "aaveRepay",
    "inputs": [
      {
        "name": "pool",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "asset",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "amount",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "interestRateMode",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "outputs": [
      {
        "name": "repaid",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "aaveSupply",
    "inputs": [
      {
        "name": "pool",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "asset",
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
    "name": "aaveWithdraw",
    "inputs": [
      {
        "name": "pool",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "asset",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "amount",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "outputs": [
      {
        "name": "withdrawn",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "acceptOwnership",
    "inputs": [],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "execute",
    "inputs": [
      {
        "name": "c",
        "type": "tuple",
        "internalType": "struct Call",
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
    "stateMutability": "payable"
  },
  {
    "type": "function",
    "name": "executeBatch",
    "inputs": [
      {
        "name": "calls",
        "type": "tuple[]",
        "internalType": "struct Call[]",
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
    "stateMutability": "payable"
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
    "name": "executeFinanceUnlock",
    "inputs": [
      {
        "name": "plan",
        "type": "tuple",
        "internalType": "struct FinancePlanV3",
        "components": [
          {
            "name": "flashAsset",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "flashAmount",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "preActions",
            "type": "tuple[]",
            "internalType": "struct Call[]",
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
          },
          {
            "name": "aavePool",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "collateralAsset",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "supplyAmount",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "debtAsset",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "borrowAmount",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "interestRateMode",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "postActions",
            "type": "tuple[]",
            "internalType": "struct Call[]",
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
          },
          {
            "name": "minDeltaFlashAsset",
            "type": "int256",
            "internalType": "int256"
          }
        ]
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "flashCollateralize",
    "inputs": [
      {
        "name": "plan",
        "type": "tuple",
        "internalType": "struct FinancePlan",
        "components": [
          {
            "name": "flashLender",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "flashAsset",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "flashAmount",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "preActions",
            "type": "tuple[]",
            "internalType": "struct Call[]",
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
          },
          {
            "name": "aavePool",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "collateralAsset",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "supplyAmount",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "debtAsset",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "borrowAmount",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "interestRateMode",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "postActions",
            "type": "tuple[]",
            "internalType": "struct Call[]",
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
          },
          {
            "name": "minDeltaFlashAsset",
            "type": "int256",
            "internalType": "int256"
          }
        ]
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "flashCollateralizeV3",
    "inputs": [
      {
        "name": "plan",
        "type": "tuple",
        "internalType": "struct FinancePlanV3",
        "components": [
          {
            "name": "flashAsset",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "flashAmount",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "preActions",
            "type": "tuple[]",
            "internalType": "struct Call[]",
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
          },
          {
            "name": "aavePool",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "collateralAsset",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "supplyAmount",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "debtAsset",
            "type": "address",
            "internalType": "address"
          },
          {
            "name": "borrowAmount",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "interestRateMode",
            "type": "uint256",
            "internalType": "uint256"
          },
          {
            "name": "postActions",
            "type": "tuple[]",
            "internalType": "struct Call[]",
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
          },
          {
            "name": "minDeltaFlashAsset",
            "type": "int256",
            "internalType": "int256"
          }
        ]
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "isAuthorized",
    "inputs": [
      {
        "name": "account",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "target",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "selector",
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
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "isSigner",
    "inputs": [
      {
        "name": "",
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
    "stateMutability": "view"
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
        "name": "",
        "type": "bytes4",
        "internalType": "bytes4"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "legacyDelegatees",
    "inputs": [
      {
        "name": "",
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
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "moveLpV3Nft",
    "inputs": [
      {
        "name": "positionManager",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "tokenId",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "to",
        "type": "address",
        "internalType": "address"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "moveLpV4",
    "inputs": [
      {
        "name": "poolManager",
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
        "name": "to",
        "type": "address",
        "internalType": "address"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "onERC1155BatchReceived",
    "inputs": [
      {
        "name": "operator",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "from",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "ids",
        "type": "uint256[]",
        "internalType": "uint256[]"
      },
      {
        "name": "values",
        "type": "uint256[]",
        "internalType": "uint256[]"
      },
      {
        "name": "data",
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
        "name": "operator",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "from",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "id",
        "type": "uint256",
        "internalType": "uint256"
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
        "name": "operator",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "from",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "tokenId",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "data",
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
    "name": "owner",
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
    "name": "pause",
    "inputs": [],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "paused",
    "inputs": [],
    "outputs": [
      {
        "name": "",
        "type": "bool",
        "internalType": "bool"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "pendingOwner",
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
    "name": "receiveFlashLoan",
    "inputs": [
      {
        "name": "tokens",
        "type": "address[]",
        "internalType": "address[]"
      },
      {
        "name": "amounts",
        "type": "uint256[]",
        "internalType": "uint256[]"
      },
      {
        "name": "feeAmounts",
        "type": "uint256[]",
        "internalType": "uint256[]"
      },
      {
        "name": "userData",
        "type": "bytes",
        "internalType": "bytes"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "renounceOwnership",
    "inputs": [],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "setLegacyDelegatee",
    "inputs": [
      {
        "name": "account",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "allowed",
        "type": "bool",
        "internalType": "bool"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "setSigner",
    "inputs": [
      {
        "name": "signer",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "allowed",
        "type": "bool",
        "internalType": "bool"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "setThreshold",
    "inputs": [
      {
        "name": "t",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "setV4Operator",
    "inputs": [
      {
        "name": "poolManager",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "operator",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "approved",
        "type": "bool",
        "internalType": "bool"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "signerCount",
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
    "name": "supportsInterface",
    "inputs": [
      {
        "name": "interfaceId",
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
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "sweepBatch",
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
    "name": "threshold",
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
    "name": "transferOwnership",
    "inputs": [
      {
        "name": "newOwner",
        "type": "address",
        "internalType": "address"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "unpause",
    "inputs": [],
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
    "name": "CollateralizationFinanced",
    "inputs": [
      {
        "name": "opId",
        "type": "bytes32",
        "indexed": true,
        "internalType": "bytes32"
      },
      {
        "name": "flashAsset",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "flashAmount",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      },
      {
        "name": "collateralAsset",
        "type": "address",
        "indexed": false,
        "internalType": "address"
      },
      {
        "name": "supplyAmount",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      },
      {
        "name": "debtAsset",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "borrowAmount",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      },
      {
        "name": "flashRepaid",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      },
      {
        "name": "netDeltaFlash",
        "type": "int256",
        "indexed": false,
        "internalType": "int256"
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
    "name": "Executed",
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
    "name": "LegacyDelegateeSet",
    "inputs": [
      {
        "name": "account",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "allowed",
        "type": "bool",
        "indexed": false,
        "internalType": "bool"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "LpMoved",
    "inputs": [
      {
        "name": "kind",
        "type": "uint8",
        "indexed": true,
        "internalType": "enum LpTransferLib.LpKind"
      },
      {
        "name": "pool",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "idOrTokenId",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      },
      {
        "name": "amount",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      },
      {
        "name": "to",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "OwnershipTransferStarted",
    "inputs": [
      {
        "name": "previousOwner",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "newOwner",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "OwnershipTransferred",
    "inputs": [
      {
        "name": "previousOwner",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "newOwner",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "Paused",
    "inputs": [
      {
        "name": "account",
        "type": "address",
        "indexed": false,
        "internalType": "address"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "Received",
    "inputs": [
      {
        "name": "standard",
        "type": "bytes4",
        "indexed": true,
        "internalType": "bytes4"
      },
      {
        "name": "sender",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "tokenId",
        "type": "uint256",
        "indexed": true,
        "internalType": "uint256"
      },
      {
        "name": "value",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "SignerSet",
    "inputs": [
      {
        "name": "signer",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "allowed",
        "type": "bool",
        "indexed": false,
        "internalType": "bool"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "ThresholdSet",
    "inputs": [
      {
        "name": "newThreshold",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "Unpaused",
    "inputs": [
      {
        "name": "account",
        "type": "address",
        "indexed": false,
        "internalType": "address"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "V4OperatorSet",
    "inputs": [
      {
        "name": "poolManager",
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
    "type": "error",
    "name": "CallFailed",
    "inputs": [
      {
        "name": "index",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "returnData",
        "type": "bytes",
        "internalType": "bytes"
      }
    ]
  },
  {
    "type": "error",
    "name": "EnforcedPause",
    "inputs": []
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
    "name": "ExpectedPause",
    "inputs": []
  },
  {
    "type": "error",
    "name": "FinanceUnprofitable",
    "inputs": [
      {
        "name": "delta",
        "type": "int256",
        "internalType": "int256"
      },
      {
        "name": "minDelta",
        "type": "int256",
        "internalType": "int256"
      }
    ]
  },
  {
    "type": "error",
    "name": "FlashAmountMismatch",
    "inputs": []
  },
  {
    "type": "error",
    "name": "InvalidParams",
    "inputs": []
  },
  {
    "type": "error",
    "name": "LpTransferLib__V4SetOperatorFailed",
    "inputs": []
  },
  {
    "type": "error",
    "name": "LpTransferLib__V4TransferFailed",
    "inputs": []
  },
  {
    "type": "error",
    "name": "MevSafe__NativeSweepFailed",
    "inputs": []
  },
  {
    "type": "error",
    "name": "MevSafe__UnsupportedFlashLender",
    "inputs": [
      {
        "name": "lender",
        "type": "address",
        "internalType": "address"
      }
    ]
  },
  {
    "type": "error",
    "name": "NoActiveFlash",
    "inputs": []
  },
  {
    "type": "error",
    "name": "NotAuthorized",
    "inputs": []
  },
  {
    "type": "error",
    "name": "NotEntryPoint",
    "inputs": []
  },
  {
    "type": "error",
    "name": "NotFlashLender",
    "inputs": []
  },
  {
    "type": "error",
    "name": "OnlyV3Vault",
    "inputs": []
  },
  {
    "type": "error",
    "name": "OwnableInvalidOwner",
    "inputs": [
      {
        "name": "owner",
        "type": "address",
        "internalType": "address"
      }
    ]
  },
  {
    "type": "error",
    "name": "OwnableUnauthorizedAccount",
    "inputs": [
      {
        "name": "account",
        "type": "address",
        "internalType": "address"
      }
    ]
  },
  {
    "type": "error",
    "name": "PlanHashMismatch",
    "inputs": []
  },
  {
    "type": "error",
    "name": "Reentrancy",
    "inputs": []
  },
  {
    "type": "error",
    "name": "V3SettleShortfall",
    "inputs": [
      {
        "name": "expected",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "credit",
        "type": "uint256",
        "internalType": "uint256"
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
pub mod MevSafe {
    use super::*;
    use alloy::sol_types as alloy_sol_types;
    /// The creation / init bytecode of the contract.
    ///
    /// ```text
    ///0x60a03461012957601f613b7238819003918201601f19168301916001600160401b0383118484101761012d5780849260409485528339810103126101295780516001600160a01b038116919082900361012957602001516001600160a01b038116919082810361012957811561011657600180546001600160a01b03199081169091555f805491821684178155604051949184916001600160a01b03909116907f8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e09080a315610107576080525f52600360205260405f20600160ff1982541617905560016004556001600555613a309081610142823960805181818161218701526138de0152f35b635435b28960e11b5f5260045ffd5b631e4fbdf760e01b5f525f60045260245ffd5b5f80fd5b634e487b7160e01b5f52604160045260245ffdfe6080604052600436101561001a575b3615610018575f80fd5b005b5f3560e01c806301ffc9a7146102d9578063150b7a02146102d45780631626ba7e146102cf57806319822f7c146102ca578063213c5033146102c557806331cb6105146102c057806334fcd5be146102bb578063376794d8146102b65780633b303705146102b15780633f4ba83a146102ac5780634203a934146102a757806342cde4e8146102a2578063503690d11461029d5780635303ad28146102985780635589e272146102935780635c1c6dcd1461028e5780635c975abb1461028957806366579be814610284578063715018a61461027f57806379ba50971461027a5780637ca548c6146102755780637d281caa146102705780637df73e271461026b578063804a0566146102665780638456cb59146102615780638da5cb5b1461025c5780638f205a1d1461025757806394430fa514610252578063960bfe041461024d57806399fec7a014610248578063a4c01bbb14610243578063b06c944a1461023e578063bc197c8114610239578063d41e5d3f14610234578063d475c0981461022f578063e24d8c4c1461022a578063e30c397814610225578063e99f5b1614610220578063f04f27071461021b578063f23a6e6114610216578063f2fde38b14610211578063f434c9141461020c578063f6eb79c7146102075763fdb020980361000e57612234565b6121b6565b612172565b6120fc565b612068565b611fcb565b611f77565b611f4f565b611ee4565b611e48565b6117ae565b6116fd565b61156d565b61153f565b611511565b6114ac565b611482565b61135d565b611302565b6112a1565b61122b565b6111bb565b61117b565b61115e565b6110d9565b611076565b610ff1565b610fcc565b610e6a565b610da7565b610b7e565b610b18565b610acf565b610a74565b610a06565b6109d8565b610934565b61078f565b610740565b6106e3565b610620565b6105b2565b6104f3565b6102f9565b600435906001600160e01b0319821682036102f557565b5f80fd5b346102f55760203660031901126102f5576103636001600160e01b031961031e6102de565b166301ffc9a760e01b811490819082156103bc575b82156103ab575b821561039a575b8215610389575b8215610367575b505060405190151581529081906020820190565b0390f35b630271189760e51b1491508115610381575b505f8061034f565b90505f610379565b6306608bdf60e21b81149250610348565b630271189760e51b81149250610341565b630a85bd0160e11b8114925061033a565b630b135d3f60e11b81149250610333565b6001600160a01b038116036102f557565b35906103e9826103cd565b565b634e487b7160e01b5f52604160045260245ffd5b606081019081106001600160401b0382111761041a57604052565b6103eb565b90601f801991011681019081106001600160401b0382111761041a57604052565b604051906103e96101808361041f565b6001600160401b03811161041a57601f01601f191660200190565b91909161047781610450565b610484604051918261041f565b809382825282116102f55781815f9384602080950137010152565b9291926104ab82610450565b916104b9604051938461041f565b8294818452818301116102f5578281602093845f960137010152565b9080601f830112156102f5578160206104f09335910161049f565b90565b346102f55760803660031901126102f55761050f6004356103cd565b61051a6024356103cd565b6044356064356001600160401b0381116102f55761053c9036906004016104d5565b506040516001815233907fd1ec030da0f99fccad1de2774dc98277b6e7693a549f4751c319cf87379bc30c6020630a85bd0160e11b92a4604051630a85bd0160e11b8152602090f35b9181601f840112156102f5578235916001600160401b0383116102f557602083818601950101116102f557565b346102f55760403660031901126102f5576024356004356001600160401b0382116102f5576105e86105ee923690600401610585565b91613402565b1561061257630b135d3f60e11b5b6040516001600160e01b03199091168152602090f35b6001600160e01b03196105fc565b346102f55760603660031901126102f5576004356001600160401b0381116102f55761012060031982360301126102f55760243590604435906f71727de22e5e9d8baf0edac6f37da03233036106ca5760ff926105e8826101046106889401906004016122c1565b156106c2575f905b806106a3575b5060405191168152602090f35b5f808080936f71727de22e5e9d8baf0edac6f37da0325af1505f610696565b600190610690565b636b31ba1560e11b5f5260045ffd5b5f9103126102f557565b346102f5575f3660031901126102f557602060405173ba12222222228d8ba445958a75a0704d566bf2c88152f35b801515036102f557565b60409060031901126102f557600435610733816103cd565b906024356104f081610711565b346102f5576100186107513661071b565b9061075a613513565b6122f3565b9181601f840112156102f5578235916001600160401b0383116102f5576020808501948460051b0101116102f557565b60203660031901126102f5576004356001600160401b0381116102f5576107ba90369060040161075f565b6107c2613526565b6107ca613513565b6107d261355e565b5f5b8181106107e9575f688000000000ab143c065d005b6107f48183856123cf565b6001600160a01b03610805826125f4565b161561092557604081019061082361081d83836122c1565b906139bb565b906001600160e01b0319821663558a729760e01b8114908115610914575b50610905575f8061086494610855846125f4565b906020850135968791866122c1565b919061087560405180948193612bda565b03925af1610881612633565b90156108e3575060019392917fe08f8925f45c337d514b07af2526e14449bcf90afd92efd8b611f17ebf419db0916001600160a01b03906108c1906125f4565b604080519586526001600160e01b03199390931660208601521692a2016107d4565b604051635c0dee5d60e01b8152908190610901908760048401612be7565b0390fd5b631fb7cca560e01b5f5260045ffd5b63426a849360e01b1490505f610841565b635435b28960e11b5f5260045ffd5b346102f55760803660031901126102f557600435610951816103cd565b60243560027f35e259c1a781dc2649e76298b5a4d548c7905287ca3d7c4337480ce3fd05eb69606435604435610986826103cd565b61098e613526565b610996613513565b61099e61355e565b6109aa82828789613591565b6040805195865260208601919091526001600160a01b039182169590911693a45f688000000000ab143c065d005b346102f5575f3660031901126102f557602060405173794a61358d6845594f94dc1db02a252b5b4814ad8152f35b346102f5575f3660031901126102f557610a1e613513565b60015460ff8160a01c1615610a655760ff60a01b19166001556040513381527f5db9ee0a495bf2e6ff9c91a7834c1ba4fdd244a5e8aa4e537bd38aeae4b073aa90602090a1005b638dfc202b60e01b5f5260045ffd5b346102f55760203660031901126102f5576004356001600160401b0381116102f557366023820112156102f55780600401356001600160401b0381116102f55736602460c08302840101116102f557602461001892016123f6565b346102f5575f3660031901126102f5576020600554604051908152f35b60609060031901126102f557600435610b04816103cd565b90602435610b11816103cd565b9060443590565b346102f557610b2636610aec565b90610b2f613513565b6001600160a01b03811615610925576001600160a01b038316610b75575f809350809281925af1610b5e612633565b5015610b6657005b63d1d8760360e01b5f5260045ffd5b6100189261374f565b346102f55760203660031901126102f5576004356001600160401b0381116102f5578060040161018060031983360301126102f557610bbb613526565b610bc3613513565b610bcb61355e565b6001600160a01b03610bdc826125f4565b16158015610d91575b8015610d85575b8015610d6f575b6109255773ba12222222228d8ba445958a75a0704d566bf2c86001600160a01b03610c1d836125f4565b1603610d4557610c34610c2f826125f4565b613781565b60405160208101610c5782610c498584612778565b03601f19810184528361041f565b815190205f5160206139e45f395f51905f525d610ccb610cbf610cbf610c7b612894565b946044610c86612894565b97610cae610c96602483016125f4565b610c9f8a6128b6565b6001600160a01b039091169052565b0135610cb9886128b6565b526125f4565b6001600160a01b031690565b803b156102f557610cf7935f809460405196879586948593632e1c224f60e11b855230600486016128fb565b03925af18015610d4057610d26575b610d0e61379b565b5f5f5160206139e45f395f51905f525d610018613550565b80610d345f610d3a9361041f565b806106d9565b5f610d06565b612999565b610d51610d6c916125f4565b63a0aad8bb60e01b5f526001600160a01b0316600452602490565b5ffd5b50610d7f610cbf608484016125f4565b15610bf3565b50604482013515610bec565b50610da1610cbf602484016125f4565b15610be5565b346102f55760603660031901126102f5576004356001600160401b0381116102f557610dd790369060040161075f565b602435610de3816103cd565b6044356001600160401b0381116102f557610e0290369060040161075f565b91610e0b613513565b6001600160a01b038116158015610e60575b610925575f5b848110610e2c57005b80610e5a610e3d600193888a6129ad565b35610e47816103cd565b84610e538489896129ad565b359161374f565b01610e23565b5082841415610e1d565b60203660031901126102f5576004356001600160401b0381116102f5578060040190606060031982360301126102f557610ea2613526565b610eaa613513565b610eb261355e565b6001600160a01b03610ec3836125f4565b1615610925576044810190610edb61081d83856122c1565b906001600160e01b0319821663558a729760e01b8114908115610fbb575b50610905575f8091610f1c946024610f10886125f4565b920135958691886122c1565b9190610f2d60405180948193612bda565b03925af192610f3a612633565b9315610f9f577fe08f8925f45c337d514b07af2526e14449bcf90afd92efd8b611f17ebf419db091906001600160a01b0390610f75906125f4565b604080519586526001600160e01b03199390931660208601521692a25f688000000000ab143c065d005b604051635c0dee5d60e01b815280610901865f60048401612be7565b63426a849360e01b1490505f610ef9565b346102f5575f3660031901126102f557602060ff60015460a01c166040519015158152f35b346102f55760603660031901126102f55760043561100e816103cd565b60243561101a816103cd565b7f82e43140fc41dbcab6163bc2bd7ddf40d7477286fb7c95c37c1dbc957756a9ba60206044359261104a84610711565b611052613513565b61105d84828761364a565b60405193151584526001600160a01b03908116941692a3005b346102f5575f3660031901126102f55761108e613513565b600180546001600160a01b03199081169091555f80549182168155906001600160a01b03167f8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e08280a3005b346102f5575f3660031901126102f557600154336001600160a01b039091160361114b57600180546001600160a01b03199081169091555f805433928116831782556001600160a01b0316907f8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e09080a3005b63118cdaa760e01b5f523360045260245ffd5b346102f5575f3660031901126102f5576020600454604051908152f35b346102f55760203660031901126102f557600435611198816103cd565b60018060a01b03165f526002602052602060ff60405f2054166040519015158152f35b346102f55760203660031901126102f5576004356111d8816103cd565b60018060a01b03165f526003602052602060ff60405f2054166040519015158152f35b60809060031901126102f557600435611213816103cd565b90602435611220816103cd565b906044359060643590565b346102f557611239366111fb565b9092611243613513565b61124b61355e565b6001600160a01b031691823b156102f557611283925f928360405180968195829463a415bcad60e01b8452849a3092600486016129bd565b03925af18015610d4057611295575080f35b61001891505f9061041f565b346102f5575f3660031901126102f5576112b9613513565b6112c161355e565b6001805460ff60a01b1916600160a01b1790556040513381527f62e78cea01bee320cd4e420270b5ea74000d11b0c9f74754ebdbfc544b05a25890602090a1005b346102f5575f3660031901126102f5575f546040516001600160a01b039091168152602090f35b60206003198201126102f557600435906001600160401b0382116102f5576101609082900360031901126102f55760040190565b346102f55761136b36611329565b611373613526565b61137b613513565b61138361355e565b6001600160a01b03611394826125f4565b16158015611476575b8015611460575b610925575f61140591604051906113d082610c49602082019363d41e5d3f60e01b8552602483016129ed565b6113d86137ad565b815190205f5160206139e45f395f51905f525d604051809381926348c8949160e01b835260048301612b13565b03818373ba1333333333a1ba1108e8412f11850a5c319ba95af1908115610d40575f9161143e575b50515f191461092557610d0e61379b565b61145a91503d805f833e611452818361041f565b810190612ab1565b5f61142d565b50611470610cbf606083016125f4565b156113a4565b5060208101351561139d565b346102f5575f3660031901126102f55760206040516f71727de22e5e9d8baf0edac6f37da0328152f35b346102f55760203660031901126102f5576004356114c8613513565b80158015611506575b610925576020817f6e8a187d7944998085dbd1f16b84c51c903bb727536cdba86962439aded2cfd792600555604051908152a1005b5060045481116114d1565b346102f5575f3660031901126102f5576020604051736c247b1f6182318877311737bac0844baa518f5e8152f35b346102f5575f3660031901126102f557602060405173ba1333333333a1ba1108e8412f11850a5c319ba98152f35b346102f55760603660031901126102f55760043561158a816103cd565b602435906044359061159b826103cd565b6115a3613526565b6115ab613513565b6115b361355e565b6001600160a01b03169081158015611678575b61092557813b156102f557604051632142170760e11b81523060048201526001600160a01b0382166024820152604481018490525f8160648183875af18015610d40576001927f35e259c1a781dc2649e76298b5a4d548c7905287ca3d7c4337480ce3fd05eb699261165992611664575b50604051918291858060a01b0316968260205f91939293604081019481520152565b0390a4610018613550565b80610d345f6116729361041f565b5f611637565b506001600160a01b038116156115c6565b6001600160401b03811161041a5760051b60200190565b9080601f830112156102f55781356116b781611689565b926116c5604051948561041f565b81845260208085019260051b8201019283116102f557602001905b8282106116ed5750505090565b81358152602091820191016116e0565b346102f55760a03660031901126102f55760043561171a816103cd565b60243590611727826103cd565b6044356001600160401b0381116102f5576117469036906004016116a0565b906064356001600160401b0381116102f5576117669036906004016116a0565b608435926001600160401b0384116102f5576103639461178d6117939536906004016104d5565b93612b24565b6040516001600160e01b031990911681529081906020820190565b346102f5576117bc36611329565b73ba1333333333a1ba1108e8412f11850a5c319ba93303611e39575f5160206139e45f395f51905f525c8015611e2a576117f6363661046b565b6020815191012003611e1b57611811610cbf610cbf836125f4565b6040516370a0823160e01b815230600482015290602090829060249082905afa908115610d40575f91611dfc575b50611849826125f4565b9160208101359273ba1333333333a1ba1108e8412f11850a5c319ba93b156102f55760405163ae63932960e01b81526001600160a01b03919091166004820152306024820152604481018490525f816064818373ba1333333333a1ba1108e8412f11850a5c319ba95af18015610d4057611de8575b5090916040820191905f5b6118d38484612ba5565b9050811015611970575f806118fa6118f5846118ef8989612ba5565b906123cf565b6125f4565b602061190a856118ef8a8a612ba5565b013561192761191d866118ef8b8b612ba5565b60408101906122c1565b919061193860405180948193612bda565b03925af1611944612633565b901561195357506001016118c9565b604051635c0dee5d60e01b81529182916109019160048401612be7565b5092909160a08301359081611d45575b60e08401359081611cca575b61012085019361199c8587612ba5565b90505f5b818110611c345750506119b8610cbf610cbf886125f4565b6040516370a0823160e01b81523060048201529190602090839060249082905afa8015610d4057611a01896119fb611a0693611a0c965f91611c15575b5061386b565b93612c25565b61386b565b90612c32565b93610140860135808612611bfe5750611a2d87611a28886125f4565b6136ea565b611a6d602088611a3c896125f4565b6040516315afd40960e01b81526001600160a01b039091166004820152602481019190915291829081906044820190565b03815f73ba1333333333a1ba1108e8412f11850a5c319ba95af1908115610d40575f91611bcf575b50878110611bb857507f53f2133355063b0787be9b73f9f2c3d6e14670e2a37dd462368b5d1a94e86b06939291611bb391611b4c611ae8611adf611ad88b6125f4565b948b612ba5565b9390508a612ba5565b60408051306020820190815273ba1333333333a1ba1108e8412f11850a5c319ba9928201929092526001600160a01b039096166060870152608086018d905260a086019490945260c0850152509091908160e081015b03601f19810183528261041f565b51902094611b59876125f4565b97611b7260c0611b6b60808b016125f4565b99016125f4565b604080518381526001600160a01b039a8b166020820152908101969096526060860194909452608085015260a08401529085169590941693819060c0820190565b0390a4005b633e6339e360e11b5f52600488905260245260445ffd5b611bf1915060203d602011611bf7575b611be9818361041f565b810190612b96565b5f611a95565b503d611bdf565b630a3b09a160e01b5f52600486905260245260445ffd5b611c2e915060203d602011611bf757611be9818361041f565b5f6119f5565b5f80888a611c6c61191d866118ef611c536118f5836118ef8989612ba5565b956020611c64846118ef848a612ba5565b013595612ba5565b9190611c7d60405180948193612bda565b03925af1611c89612633565b9015611c9857506001016119a0565b611cae82611ca6878c612ba5565b919050612c25565b610901604051928392635c0dee5d60e01b845260048401612be7565b611cdc610cbf610cbf606088016125f4565b611ce860c087016125f4565b61010087013590823b156102f557611d1c925f928360405180968195829463a415bcad60e01b84528b3092600486016129bd565b03925af18015610d4057611d31575b5061198c565b80610d345f611d3f9361041f565b5f611d2b565b60808401611d7c611d76610cbf610cbf611d5e856125f4565b6118f58860608c0192611d70846125f4565b906137d3565b916125f4565b90803b156102f55760405163617ba03760e01b81526001600160a01b03929092166004830152602482018490523060448301525f60648301819052908290608490829084905af18015610d4057611dd4575b50611980565b80610d345f611de29361041f565b5f611dce565b80610d345f611df69361041f565b5f6118be565b611e15915060203d602011611bf757611be9818361041f565b5f61183f565b63620be62360e01b5f5260045ffd5b6378fcf80b60e11b5f5260045ffd5b638e5e503d60e01b5f5260045ffd5b346102f55760646020611e5a36610aec565b919390611e65613513565b611e6d61355e565b604051631a4ca37b60e21b81526001600160a01b03918216600482015260248101939093523060448401529193849283915f91165af18015610d4057610363915f91611ec5575b506040519081529081906020820190565b611ede915060203d602011611bf757611be9818361041f565b5f611eb4565b346102f55760207f0435416a5f48d41d5e5ede2d05c0e1ff6b1f71cb57176b1011ea7fbd8c725a83611f153661071b565b9290611f1f613513565b6001600160a01b03165f8181526002835260409020805460ff191660ff86151516179055926040519015158152a2005b346102f5575f3660031901126102f5576001546040516001600160a01b039091168152602090f35b346102f55760603660031901126102f557600435611f94816103cd565b60243590611fa1826103cd565b6044356001600160e01b0319811681036102f557602092611fc192613880565b6040519015158152f35b346102f55760803660031901126102f5576004356001600160401b0381116102f557611ffb90369060040161075f565b6024356001600160401b0381116102f55761201a90369060040161075f565b6044939193356001600160401b0381116102f55761203c90369060040161075f565b91606435956001600160401b0387116102f557612060610018973690600401610585565b969095612e2b565b346102f55760a03660031901126102f5576120846004356103cd565b61208f6024356103cd565b6044356064356084356001600160401b0381116102f5576120b49036906004016104d5565b5060405190815233907fd1ec030da0f99fccad1de2774dc98277b6e7693a549f4751c319cf87379bc30c602063f23a6e6160e01b92a460405163f23a6e6160e01b8152602090f35b346102f55760203660031901126102f557600435612119816103cd565b612121613513565b60018060a01b0316806bffffffffffffffffffffffff60a01b600154161760015560018060a01b035f54167f38d16b8cac22d99fc7c124b9cd0de2d3fa1faef420bfe791d8c362d765e227005f80a3005b346102f5575f3660031901126102f5576040517f00000000000000000000000000000000000000000000000000000000000000006001600160a01b03168152602090f35b346102f5576121c436610aec565b916121cd613513565b6121d561355e565b6121e08382846137d3565b6001600160a01b0316803b156102f55760405163617ba03760e01b81526001600160a01b03909216600483015260248201929092523060448201525f60648201819052918290829081838160848101611283565b346102f557608460205f612247366111fb565b9592612254949194613513565b61225c61355e565b6122678582856137d3565b604051968795869463573ade8160e01b865260018060a01b031660048601526024850152604484015230606484015260018060a01b03165af18015610d4057610363915f91611ec557506040519081529081906020820190565b903590601e19813603018212156102f557018035906001600160401b0382116102f5576020019181360383136102f557565b6001600160a01b03811691908215610925575f8381526003602052604090205460ff161515821515146123b6576001600160a01b03165f9081526003602052604090207ffc4acb499491cd850a8a21ab98c7f128850c0f0e5f1a875a62b7fa055c2ecf199161239d91612371908260ff801983541691151516179055565b80156123a25761238b61238660045460010190565b600455565b60405190151581529081906020820190565b0390a2565b6004546123b1905f1901600455565b61238b565b505050565b634e487b7160e01b5f52603260045260245ffd5b91908110156123f15760051b81013590605e19813603018212156102f5570190565b6123bb565b9190612400613526565b612408613513565b61241061355e565b5f5b81811061242557505090506103e9613550565b6124308183866125e4565b9060208201612441610cbf826125f4565b1580156125ce575b610925576124568361261c565b61245f816125fe565b61250b5761246f60a08401612629565b6124fc5760019261247f826125f4565b907f4a94f89e131699ed3416670c011ce64d62e5a581a4ebb4603bf6c4a5d06a06ce6124f26124d46124ce6060850135966118f560406080880135970197878a6124c88b6125f4565b92613591565b946125f4565b60405193845260a088901b8890039081169416929081906020820190565b0390a45b01612412565b63b026d5a360e01b5f5260045ffd5b6060830135158015906125c1575b6125b2576001927f9c8e17fa114d24cfc8f67c3d6ce6bc2e24067dbe41256640bc48fd6d1066562f6125aa61258861258261257c612556876125f4565b966118f5604088019860a061256a8b6125f4565b9901986125768a612629565b9161364a565b956125f4565b93612629565b604051901515815260a087901b87900393841694909316929081906020820190565b0390a36124f6565b6341f521f960e11b5f5260045ffd5b5060808301351515612519565b506125de610cbf604085016125f4565b15612449565b91908110156123f15760c0020190565b356104f0816103cd565b6002111561260857565b634e487b7160e01b5f52602160045260245ffd5b3560028110156102f55790565b356104f081610711565b3d1561265d573d9061264482610450565b91612652604051938461041f565b82523d5f602084013e565b606090565b9035601e19823603018112156102f55701602081359101916001600160401b0382116102f5578160051b360383136102f557565b908060209392818452848401375f828201840152601f01601f1916010190565b906020838281520160208260051b85010193835f915b8483106126dc5750505050505090565b909192939495601f198282030185528635605e19843603018112156102f55783018035612708816103cd565b6001600160a01b0316825260208181013590830152604081013536829003601e19018112156102f55701602081359101906001600160401b0381116102f55780360382136102f55761276a602092839260608681604060019901520191612696565b9801969501930191906126cc565b602081526127996020820161278c846103de565b6001600160a01b03169052565b6127b86127a8602084016103de565b6001600160a01b03166040830152565b6040820135606082015261018061016061288b6127ec6127db6060870187612662565b8560808801526101a08701916126b6565b61280b6127fb608088016103de565b6001600160a01b031660a0870152565b61282a61281a60a088016103de565b6001600160a01b031660c0870152565b60c086013560e086015261285461284360e088016103de565b6001600160a01b0316610100870152565b61010086013561012086015261012086013561014086015261287a610140870187612662565b868303601f190185880152906126b6565b93013591015290565b604080519091906128a5838261041f565b6001815291601f1901366020840137565b8051156123f15760200190565b80518210156123f15760209160051b010190565b805180835260209291819084018484015e5f828201840152601f01601f1916010190565b6001600160a01b03909116815260806020808301829052835191830182905260a0830196959301905f5b81811061297a5750505080850360408201526020808451968781520193015f955b8087106129625750506104f093945060608184039101526128d7565b90936020806001928751815201950196019590612946565b82516001600160a01b0316885260209788019790920191600101612925565b6040513d5f823e3d90fd5b90156123f15790565b91908110156123f15760051b0190565b6001600160a01b039182168152602081019290925260408201929092525f60608201529116608082015260a00190565b60208152612a016020820161278c846103de565b6020820135604082015261016061014061288b612a35612a246040870187612662565b8560608801526101808701916126b6565b612a54612a44606088016103de565b6001600160a01b03166080870152565b612a636127fb608088016103de565b60a086013560c0860152612a8c612a7c60c088016103de565b6001600160a01b031660e0870152565b60e086013561010086015261010086013561012086015261287a610120870187612662565b6020818303126102f5578051906001600160401b0382116102f5570181601f820112156102f557805190612ae482610450565b92612af2604051948561041f565b828452602083830101116102f557815f9260208093018386015e8301015290565b9060206104f09281815201906128d7565b50509291505f5b8351811015612b885780612b41600192866128c3565b51612b4c82856128c3565b5160405190815233907fd1ec030da0f99fccad1de2774dc98277b6e7693a549f4751c319cf87379bc30c602063bc197c8160e01b92a401612b2b565b5063bc197c8160e01b925050565b908160209103126102f5575190565b903590601e19813603018212156102f557018035906001600160401b0382116102f557602001918160051b360383136102f557565b908092918237015f815290565b6040906104f09392815281602082015201906128d7565b634e487b7160e01b5f52601160045260245ffd5b9060418201809211612c2057565b612bfe565b91908201809211612c2057565b81810392915f138015828513169184121617612c2057565b9080601f830112156102f557813591612c6283611689565b92612c70604051948561041f565b80845260208085019160051b830101918383116102f55760208101915b838310612c9c57505050505090565b82356001600160401b0381116102f5578201906060828703601f1901126102f55760405190612cca826103ff565b6020830135612cd8816103cd565b8252604083013560208301526060830135916001600160401b0383116102f557612d0a886020809695819601016104d5565b6040820152815201920191612c8d565b6020818303126102f5578035906001600160401b0382116102f5570190610180828203126102f557612d4a610440565b91612d54816103de565b8352612d62602082016103de565b60208401526040810135604084015260608101356001600160401b0381116102f55782612d90918301612c4a565b6060840152612da1608082016103de565b6080840152612db260a082016103de565b60a084015260c081013560c0840152612dcd60e082016103de565b60e08401526101008101356101008401526101208101356101208401526101408101356001600160401b0381116102f55761016092612e0d918301612c4a565b610140840152013561016082015290565b91908203918211612c2057565b92975f516020613a045f395f51905f525c97949690959294916001600160a01b0389168015611e2a5733036133b457600181148015906133a9575b801561339e575b613377575f5160206139e45f395f51905f525c612e8b36858561049f565b6020815191012003611e1b57612eaa82612eb0946118f5940190612d1a565b946129a4565b60208301805190929190612ecc906001600160a01b0316610cbf565b6001600160a01b0390911614801590613386575b613377578151612efa90610cbf906001600160a01b031681565b6040516370a0823160e01b815230600482015290602090829060249082905afa8015610d4057612f40915f91613358575b50612f398a879b9a9b6129a4565b3590612e1e565b6060840197905f5b89518051821015612fb2575f8b612f71612f638584956128c3565b51516001600160a01b031690565b6040612f8e866020612f848287516128c3565b51015194516128c3565b51015191602083519301915af1612fa3612633565b90156119535750600101612f48565b505091949792959890939660c08801958651806132a4575b5061010089019586518a8161321e575b6101409150019a8b51515f5b8d8282106131c9575050509161300361300a9261301195946129a4565b35926129a4565b3590612c25565b865190929061302a90610cbf906001600160a01b031681565b6040516370a0823160e01b81523060048201529190602090839060249082905afa8015610d4057611a01856119fb611a069361306c965f91611c15575061386b565b926101608801518085126131b2575091613142826131ad946131318b611b3e60406130dd8e6130d07f53f2133355063b0787be9b73f9f2c3d6e14670e2a37dd462368b5d1a94e86b069f9e9d9b8b906130cb845160018060a01b031690565b61374f565b516001600160a01b031690565b920180519451519f51516040805130602082019081526001600160a01b03998a1692820192909252979094166060880152608087019590955260a086019f909f5260c08501939093529291829060e0820190565b51902097516001600160a01b031690565b985160a089015190989061316d9060e0906001600160a01b031697519201516001600160a01b031690565b9451604080519a8b526001600160a01b0397881660208c01528a01919091526060890152608088015260a0870152908216959091169390819060c0820190565b0390a4565b630a3b09a160e01b5f52600485905260245260445ffd5b5f816131da612f63858495516128c3565b60406131ed866020612f848287516128c3565b51015191602083519301915af1613202612633565b90156132115750600101612fe6565b611cae828b515190612c25565b6080015161323690610cbf906001600160a01b031681565b60e08c01519091906001600160a01b03166101208d015192803b156102f55761327b935f80946040519687958694859363a415bcad60e01b85523092600486016129bd565b03925af18015610d4057613290575b8a612fda565b80610d345f61329e9361041f565b5f61328a565b6132ee6132e0610cbf610cbf8d6130d060a082019660806132cb895160018060a01b031690565b930180519093906001600160a01b0316611d70565b91516001600160a01b031690565b885190823b156102f55760405163617ba03760e01b81526001600160a01b0391909116600482015260248101919091523060448201525f6064820181905290918290608490829084905af18015610d405715612fca5780610d345f6133529361041f565b5f612fca565b613371915060203d602011611bf757611be9818361041f565b5f612f2b565b630415b9db60e11b5f5260045ffd5b5061339188856129a4565b3560408401511415612ee0565b506001841415612e6d565b5060018a1415612e66565b632500c52560e11b5f5260045ffd5b9081604102916041830403612c2057565b90604182029180830460411490151715612c2057565b909392938483116102f55784116102f5578101920390565b909160055491613411836133c3565b821061350b576041820661350b575f93849260418104929190845b84861061344e5750505050505080821191821561344857505090565b14919050565b613480613479613460889a97986133d4565b61347161346c8c6133d4565b612c12565b9085876133ea565b908661393a565b906001600160a01b03821680156134ff576001600160a01b0382168082109182156134f5575b50506134e8576001600160a01b0382165f9081526003602052604090205460ff16156134dd5750600180919501975b01949361342c565b9497600191506134d5565b5050505050505050505f90565b1490505f806134a6565b509497600191506134d5565b505050505f90565b5f546001600160a01b0316330361114b57565b688000000000ab143c065c6135435730688000000000ab143c065d565b63ab143c065f526004601cfd5b5f688000000000ab143c065d565b60ff60015460a01c1661356d57565b63d93c066560e01b5f5260045ffd5b908160209103126102f557516104f081610711565b909291906001600160a01b031680158015613639575b610925576040516304ade6db60e11b81526001600160a01b039390931660048401526024830193909352604482015290602090829060649082905f905af1908115610d40575f9161360a575b50156135fb57565b6331146f1560e01b5f5260045ffd5b61362c915060203d602011613632575b613624818361041f565b81019061357c565b5f6135f3565b503d61361a565b506001600160a01b038316156135a7565b6001600160a01b031691821580156136d9575b6109255760405163558a729760e01b81526001600160a01b039290921660048301521515602482015290602090829060449082905f905af1908115610d40575f916136ba575b50156136ab57565b632db30e4760e11b5f5260045ffd5b6136d3915060203d60201161363257613624818361041f565b5f6136a3565b506001600160a01b0382161561365d565b9073ba1333333333a1ba1108e8412f11850a5c319ba960145260345263a9059cbb60601b5f5260205f6044601082855af1908160015f51141615613731575b50505f603452565b3b153d171015613742575f80613729565b6390b8ec185f526004601cfd5b919060145260345263a9059cbb60601b5f5260205f6044601082855af1908160015f511416156137315750505f603452565b6001600160a01b03165f516020613a045f395f51905f525d565b5f5f516020613a045f395f51905f525d565b73ba1333333333a1ba1108e8412f11850a5c319ba95f516020613a045f395f51905f525d565b91906014528060345263095ea7b360601b5f5260205f6044601082865af18060015f51141615613807575b5050505f603452565b3d833b15171015613819575b806137fe565b5f603481905263095ea7b360601b8152386044601083865af15060345260205f6044601082855af1908160015f511416613813573b153d17101561385e575f80613813565b633e3f8f735f526004601cfd5b5f8112156104f0576335278d125f526004601cfd5b9060018060a01b0382165f52600260205260ff60405f20541661393257604051631577184560e01b81526001600160a01b039283166004820152911660248201526001600160e01b03199091166044820152602081806064810103817f00000000000000000000000000000000000000000000000000000000000000006001600160a01b03165afa908115610d40575f91613919575090565b6104f0915060203d60201161363257613624818361041f565b505050600190565b9092919260405193806040146139935760411461396357505050505b638baa579f5f526004601cfd5b806040809201355f1a60205281375b5f526020600160805f825afa51915f6060526040523d6103e9575050613956565b5060208181013560ff81901c601b0190915290356040526001600160ff1b0316606052613972565b5f9291600381116139ca575050565b909192506004116102f557356001600160e01b0319169056feadc7f65bddb36fdfcf34db7845a6b352d0b82e988cd19f14617aa0970baded73b72bb2dbdbbe818012abb21a937ec70b2e6d68f45c1643fcf3f13b60215179dfa164736f6c6343000822000a
    /// ```
    #[rustfmt::skip]
    #[allow(clippy::all)]
    pub static BYTECODE: alloy_sol_types::private::Bytes = alloy_sol_types::private::Bytes::from_static(
        b"`\xA04a\x01)W`\x1Fa;r8\x81\x90\x03\x91\x82\x01`\x1F\x19\x16\x83\x01\x91`\x01`\x01`@\x1B\x03\x83\x11\x84\x84\x10\x17a\x01-W\x80\x84\x92`@\x94\x85R\x839\x81\x01\x03\x12a\x01)W\x80Q`\x01`\x01`\xA0\x1B\x03\x81\x16\x91\x90\x82\x90\x03a\x01)W` \x01Q`\x01`\x01`\xA0\x1B\x03\x81\x16\x91\x90\x82\x81\x03a\x01)W\x81\x15a\x01\x16W`\x01\x80T`\x01`\x01`\xA0\x1B\x03\x19\x90\x81\x16\x90\x91U_\x80T\x91\x82\x16\x84\x17\x81U`@Q\x94\x91\x84\x91`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x90\x7F\x8B\xE0\x07\x9CS\x16Y\x14\x13D\xCD\x1F\xD0\xA4\xF2\x84\x19I\x7F\x97\"\xA3\xDA\xAF\xE3\xB4\x18okdW\xE0\x90\x80\xA3\x15a\x01\x07W`\x80R_R`\x03` R`@_ `\x01`\xFF\x19\x82T\x16\x17\x90U`\x01`\x04U`\x01`\x05Ua:0\x90\x81a\x01B\x829`\x80Q\x81\x81\x81a!\x87\x01Ra8\xDE\x01R\xF3[cT5\xB2\x89`\xE1\x1B_R`\x04_\xFD[c\x1EO\xBD\xF7`\xE0\x1B_R_`\x04R`$_\xFD[_\x80\xFD[cNH{q`\xE0\x1B_R`A`\x04R`$_\xFD\xFE`\x80`@R`\x046\x10\x15a\0\x1AW[6\x15a\0\x18W_\x80\xFD[\0[_5`\xE0\x1C\x80c\x01\xFF\xC9\xA7\x14a\x02\xD9W\x80c\x15\x0Bz\x02\x14a\x02\xD4W\x80c\x16&\xBA~\x14a\x02\xCFW\x80c\x19\x82/|\x14a\x02\xCAW\x80c!<P3\x14a\x02\xC5W\x80c1\xCBa\x05\x14a\x02\xC0W\x80c4\xFC\xD5\xBE\x14a\x02\xBBW\x80c7g\x94\xD8\x14a\x02\xB6W\x80c;07\x05\x14a\x02\xB1W\x80c?K\xA8:\x14a\x02\xACW\x80cB\x03\xA94\x14a\x02\xA7W\x80cB\xCD\xE4\xE8\x14a\x02\xA2W\x80cP6\x90\xD1\x14a\x02\x9DW\x80cS\x03\xAD(\x14a\x02\x98W\x80cU\x89\xE2r\x14a\x02\x93W\x80c\\\x1Cm\xCD\x14a\x02\x8EW\x80c\\\x97Z\xBB\x14a\x02\x89W\x80cfW\x9B\xE8\x14a\x02\x84W\x80cqP\x18\xA6\x14a\x02\x7FW\x80cy\xBAP\x97\x14a\x02zW\x80c|\xA5H\xC6\x14a\x02uW\x80c}(\x1C\xAA\x14a\x02pW\x80c}\xF7>'\x14a\x02kW\x80c\x80J\x05f\x14a\x02fW\x80c\x84V\xCBY\x14a\x02aW\x80c\x8D\xA5\xCB[\x14a\x02\\W\x80c\x8F Z\x1D\x14a\x02WW\x80c\x94C\x0F\xA5\x14a\x02RW\x80c\x96\x0B\xFE\x04\x14a\x02MW\x80c\x99\xFE\xC7\xA0\x14a\x02HW\x80c\xA4\xC0\x1B\xBB\x14a\x02CW\x80c\xB0l\x94J\x14a\x02>W\x80c\xBC\x19|\x81\x14a\x029W\x80c\xD4\x1E]?\x14a\x024W\x80c\xD4u\xC0\x98\x14a\x02/W\x80c\xE2M\x8CL\x14a\x02*W\x80c\xE3\x0C9x\x14a\x02%W\x80c\xE9\x9F[\x16\x14a\x02 W\x80c\xF0O'\x07\x14a\x02\x1BW\x80c\xF2:na\x14a\x02\x16W\x80c\xF2\xFD\xE3\x8B\x14a\x02\x11W\x80c\xF44\xC9\x14\x14a\x02\x0CW\x80c\xF6\xEBy\xC7\x14a\x02\x07Wc\xFD\xB0 \x98\x03a\0\x0EWa\"4V[a!\xB6V[a!rV[a \xFCV[a hV[a\x1F\xCBV[a\x1FwV[a\x1FOV[a\x1E\xE4V[a\x1EHV[a\x17\xAEV[a\x16\xFDV[a\x15mV[a\x15?V[a\x15\x11V[a\x14\xACV[a\x14\x82V[a\x13]V[a\x13\x02V[a\x12\xA1V[a\x12+V[a\x11\xBBV[a\x11{V[a\x11^V[a\x10\xD9V[a\x10vV[a\x0F\xF1V[a\x0F\xCCV[a\x0EjV[a\r\xA7V[a\x0B~V[a\x0B\x18V[a\n\xCFV[a\ntV[a\n\x06V[a\t\xD8V[a\t4V[a\x07\x8FV[a\x07@V[a\x06\xE3V[a\x06 V[a\x05\xB2V[a\x04\xF3V[a\x02\xF9V[`\x045\x90`\x01`\x01`\xE0\x1B\x03\x19\x82\x16\x82\x03a\x02\xF5WV[_\x80\xFD[4a\x02\xF5W` 6`\x03\x19\x01\x12a\x02\xF5Wa\x03c`\x01`\x01`\xE0\x1B\x03\x19a\x03\x1Ea\x02\xDEV[\x16c\x01\xFF\xC9\xA7`\xE0\x1B\x81\x14\x90\x81\x90\x82\x15a\x03\xBCW[\x82\x15a\x03\xABW[\x82\x15a\x03\x9AW[\x82\x15a\x03\x89W[\x82\x15a\x03gW[PP`@Q\x90\x15\x15\x81R\x90\x81\x90` \x82\x01\x90V[\x03\x90\xF3[c\x02q\x18\x97`\xE5\x1B\x14\x91P\x81\x15a\x03\x81W[P_\x80a\x03OV[\x90P_a\x03yV[c\x06`\x8B\xDF`\xE2\x1B\x81\x14\x92Pa\x03HV[c\x02q\x18\x97`\xE5\x1B\x81\x14\x92Pa\x03AV[c\n\x85\xBD\x01`\xE1\x1B\x81\x14\x92Pa\x03:V[c\x0B\x13]?`\xE1\x1B\x81\x14\x92Pa\x033V[`\x01`\x01`\xA0\x1B\x03\x81\x16\x03a\x02\xF5WV[5\x90a\x03\xE9\x82a\x03\xCDV[V[cNH{q`\xE0\x1B_R`A`\x04R`$_\xFD[``\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x04\x1AW`@RV[a\x03\xEBV[\x90`\x1F\x80\x19\x91\x01\x16\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x04\x1AW`@RV[`@Q\x90a\x03\xE9a\x01\x80\x83a\x04\x1FV[`\x01`\x01`@\x1B\x03\x81\x11a\x04\x1AW`\x1F\x01`\x1F\x19\x16` \x01\x90V[\x91\x90\x91a\x04w\x81a\x04PV[a\x04\x84`@Q\x91\x82a\x04\x1FV[\x80\x93\x82\x82R\x82\x11a\x02\xF5W\x81\x81_\x93\x84` \x80\x95\x017\x01\x01RV[\x92\x91\x92a\x04\xAB\x82a\x04PV[\x91a\x04\xB9`@Q\x93\x84a\x04\x1FV[\x82\x94\x81\x84R\x81\x83\x01\x11a\x02\xF5W\x82\x81` \x93\x84_\x96\x017\x01\x01RV[\x90\x80`\x1F\x83\x01\x12\x15a\x02\xF5W\x81` a\x04\xF0\x935\x91\x01a\x04\x9FV[\x90V[4a\x02\xF5W`\x806`\x03\x19\x01\x12a\x02\xF5Wa\x05\x0F`\x045a\x03\xCDV[a\x05\x1A`$5a\x03\xCDV[`D5`d5`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\x05<\x906\x90`\x04\x01a\x04\xD5V[P`@Q`\x01\x81R3\x90\x7F\xD1\xEC\x03\r\xA0\xF9\x9F\xCC\xAD\x1D\xE2wM\xC9\x82w\xB6\xE7i:T\x9FGQ\xC3\x19\xCF\x877\x9B\xC3\x0C` c\n\x85\xBD\x01`\xE1\x1B\x92\xA4`@Qc\n\x85\xBD\x01`\xE1\x1B\x81R` \x90\xF3[\x91\x81`\x1F\x84\x01\x12\x15a\x02\xF5W\x825\x91`\x01`\x01`@\x1B\x03\x83\x11a\x02\xF5W` \x83\x81\x86\x01\x95\x01\x01\x11a\x02\xF5WV[4a\x02\xF5W`@6`\x03\x19\x01\x12a\x02\xF5W`$5`\x045`\x01`\x01`@\x1B\x03\x82\x11a\x02\xF5Wa\x05\xE8a\x05\xEE\x926\x90`\x04\x01a\x05\x85V[\x91a4\x02V[\x15a\x06\x12Wc\x0B\x13]?`\xE1\x1B[`@Q`\x01`\x01`\xE0\x1B\x03\x19\x90\x91\x16\x81R` \x90\xF3[`\x01`\x01`\xE0\x1B\x03\x19a\x05\xFCV[4a\x02\xF5W``6`\x03\x19\x01\x12a\x02\xF5W`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\x01 `\x03\x19\x826\x03\x01\x12a\x02\xF5W`$5\x90`D5\x90oqr}\xE2.^\x9D\x8B\xAF\x0E\xDA\xC6\xF3}\xA023\x03a\x06\xCAW`\xFF\x92a\x05\xE8\x82a\x01\x04a\x06\x88\x94\x01\x90`\x04\x01a\"\xC1V[\x15a\x06\xC2W_\x90[\x80a\x06\xA3W[P`@Q\x91\x16\x81R` \x90\xF3[_\x80\x80\x80\x93oqr}\xE2.^\x9D\x8B\xAF\x0E\xDA\xC6\xF3}\xA02Z\xF1P_a\x06\x96V[`\x01\x90a\x06\x90V[ck1\xBA\x15`\xE1\x1B_R`\x04_\xFD[_\x91\x03\x12a\x02\xF5WV[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W` `@Qs\xBA\x12\"\"\"\"\x8D\x8B\xA4E\x95\x8Au\xA0pMVk\xF2\xC8\x81R\xF3[\x80\x15\x15\x03a\x02\xF5WV[`@\x90`\x03\x19\x01\x12a\x02\xF5W`\x045a\x073\x81a\x03\xCDV[\x90`$5a\x04\xF0\x81a\x07\x11V[4a\x02\xF5Wa\0\x18a\x07Q6a\x07\x1BV[\x90a\x07Za5\x13V[a\"\xF3V[\x91\x81`\x1F\x84\x01\x12\x15a\x02\xF5W\x825\x91`\x01`\x01`@\x1B\x03\x83\x11a\x02\xF5W` \x80\x85\x01\x94\x84`\x05\x1B\x01\x01\x11a\x02\xF5WV[` 6`\x03\x19\x01\x12a\x02\xF5W`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\x07\xBA\x906\x90`\x04\x01a\x07_V[a\x07\xC2a5&V[a\x07\xCAa5\x13V[a\x07\xD2a5^V[_[\x81\x81\x10a\x07\xE9W_h\x80\0\0\0\0\xAB\x14<\x06]\0[a\x07\xF4\x81\x83\x85a#\xCFV[`\x01`\x01`\xA0\x1B\x03a\x08\x05\x82a%\xF4V[\x16\x15a\t%W`@\x81\x01\x90a\x08#a\x08\x1D\x83\x83a\"\xC1V[\x90a9\xBBV[\x90`\x01`\x01`\xE0\x1B\x03\x19\x82\x16cU\x8Ar\x97`\xE0\x1B\x81\x14\x90\x81\x15a\t\x14W[Pa\t\x05W_\x80a\x08d\x94a\x08U\x84a%\xF4V[\x90` \x85\x015\x96\x87\x91\x86a\"\xC1V[\x91\x90a\x08u`@Q\x80\x94\x81\x93a+\xDAV[\x03\x92Z\xF1a\x08\x81a&3V[\x90\x15a\x08\xE3WP`\x01\x93\x92\x91\x7F\xE0\x8F\x89%\xF4\\3}QK\x07\xAF%&\xE1DI\xBC\xF9\n\xFD\x92\xEF\xD8\xB6\x11\xF1~\xBFA\x9D\xB0\x91`\x01`\x01`\xA0\x1B\x03\x90a\x08\xC1\x90a%\xF4V[`@\x80Q\x95\x86R`\x01`\x01`\xE0\x1B\x03\x19\x93\x90\x93\x16` \x86\x01R\x16\x92\xA2\x01a\x07\xD4V[`@Qc\\\r\xEE]`\xE0\x1B\x81R\x90\x81\x90a\t\x01\x90\x87`\x04\x84\x01a+\xE7V[\x03\x90\xFD[c\x1F\xB7\xCC\xA5`\xE0\x1B_R`\x04_\xFD[cBj\x84\x93`\xE0\x1B\x14\x90P_a\x08AV[cT5\xB2\x89`\xE1\x1B_R`\x04_\xFD[4a\x02\xF5W`\x806`\x03\x19\x01\x12a\x02\xF5W`\x045a\tQ\x81a\x03\xCDV[`$5`\x02\x7F5\xE2Y\xC1\xA7\x81\xDC&I\xE7b\x98\xB5\xA4\xD5H\xC7\x90R\x87\xCA=|C7H\x0C\xE3\xFD\x05\xEBi`d5`D5a\t\x86\x82a\x03\xCDV[a\t\x8Ea5&V[a\t\x96a5\x13V[a\t\x9Ea5^V[a\t\xAA\x82\x82\x87\x89a5\x91V[`@\x80Q\x95\x86R` \x86\x01\x91\x90\x91R`\x01`\x01`\xA0\x1B\x03\x91\x82\x16\x95\x90\x91\x16\x93\xA4_h\x80\0\0\0\0\xAB\x14<\x06]\0[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W` `@QsyJa5\x8DhEYO\x94\xDC\x1D\xB0*%+[H\x14\xAD\x81R\xF3[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5Wa\n\x1Ea5\x13V[`\x01T`\xFF\x81`\xA0\x1C\x16\x15a\neW`\xFF`\xA0\x1B\x19\x16`\x01U`@Q3\x81R\x7F]\xB9\xEE\nI[\xF2\xE6\xFF\x9C\x91\xA7\x83L\x1B\xA4\xFD\xD2D\xA5\xE8\xAANS{\xD3\x8A\xEA\xE4\xB0s\xAA\x90` \x90\xA1\0[c\x8D\xFC +`\xE0\x1B_R`\x04_\xFD[4a\x02\xF5W` 6`\x03\x19\x01\x12a\x02\xF5W`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5W6`#\x82\x01\x12\x15a\x02\xF5W\x80`\x04\x015`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5W6`$`\xC0\x83\x02\x84\x01\x01\x11a\x02\xF5W`$a\0\x18\x92\x01a#\xF6V[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W` `\x05T`@Q\x90\x81R\xF3[``\x90`\x03\x19\x01\x12a\x02\xF5W`\x045a\x0B\x04\x81a\x03\xCDV[\x90`$5a\x0B\x11\x81a\x03\xCDV[\x90`D5\x90V[4a\x02\xF5Wa\x0B&6a\n\xECV[\x90a\x0B/a5\x13V[`\x01`\x01`\xA0\x1B\x03\x81\x16\x15a\t%W`\x01`\x01`\xA0\x1B\x03\x83\x16a\x0BuW_\x80\x93P\x80\x92\x81\x92Z\xF1a\x0B^a&3V[P\x15a\x0BfW\0[c\xD1\xD8v\x03`\xE0\x1B_R`\x04_\xFD[a\0\x18\x92a7OV[4a\x02\xF5W` 6`\x03\x19\x01\x12a\x02\xF5W`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5W\x80`\x04\x01a\x01\x80`\x03\x19\x836\x03\x01\x12a\x02\xF5Wa\x0B\xBBa5&V[a\x0B\xC3a5\x13V[a\x0B\xCBa5^V[`\x01`\x01`\xA0\x1B\x03a\x0B\xDC\x82a%\xF4V[\x16\x15\x80\x15a\r\x91W[\x80\x15a\r\x85W[\x80\x15a\roW[a\t%Ws\xBA\x12\"\"\"\"\x8D\x8B\xA4E\x95\x8Au\xA0pMVk\xF2\xC8`\x01`\x01`\xA0\x1B\x03a\x0C\x1D\x83a%\xF4V[\x16\x03a\rEWa\x0C4a\x0C/\x82a%\xF4V[a7\x81V[`@Q` \x81\x01a\x0CW\x82a\x0CI\x85\x84a'xV[\x03`\x1F\x19\x81\x01\x84R\x83a\x04\x1FV[\x81Q\x90 _Q` a9\xE4_9_Q\x90_R]a\x0C\xCBa\x0C\xBFa\x0C\xBFa\x0C{a(\x94V[\x94`Da\x0C\x86a(\x94V[\x97a\x0C\xAEa\x0C\x96`$\x83\x01a%\xF4V[a\x0C\x9F\x8Aa(\xB6V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x90RV[\x015a\x0C\xB9\x88a(\xB6V[Ra%\xF4V[`\x01`\x01`\xA0\x1B\x03\x16\x90V[\x80;\x15a\x02\xF5Wa\x0C\xF7\x93_\x80\x94`@Q\x96\x87\x95\x86\x94\x85\x93c.\x1C\"O`\xE1\x1B\x85R0`\x04\x86\x01a(\xFBV[\x03\x92Z\xF1\x80\x15a\r@Wa\r&W[a\r\x0Ea7\x9BV[__Q` a9\xE4_9_Q\x90_R]a\0\x18a5PV[\x80a\r4_a\r:\x93a\x04\x1FV[\x80a\x06\xD9V[_a\r\x06V[a)\x99V[a\rQa\rl\x91a%\xF4V[c\xA0\xAA\xD8\xBB`\xE0\x1B_R`\x01`\x01`\xA0\x1B\x03\x16`\x04R`$\x90V[_\xFD[Pa\r\x7Fa\x0C\xBF`\x84\x84\x01a%\xF4V[\x15a\x0B\xF3V[P`D\x82\x015\x15a\x0B\xECV[Pa\r\xA1a\x0C\xBF`$\x84\x01a%\xF4V[\x15a\x0B\xE5V[4a\x02\xF5W``6`\x03\x19\x01\x12a\x02\xF5W`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\r\xD7\x906\x90`\x04\x01a\x07_V[`$5a\r\xE3\x81a\x03\xCDV[`D5`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\x0E\x02\x906\x90`\x04\x01a\x07_V[\x91a\x0E\x0Ba5\x13V[`\x01`\x01`\xA0\x1B\x03\x81\x16\x15\x80\x15a\x0E`W[a\t%W_[\x84\x81\x10a\x0E,W\0[\x80a\x0EZa\x0E=`\x01\x93\x88\x8Aa)\xADV[5a\x0EG\x81a\x03\xCDV[\x84a\x0ES\x84\x89\x89a)\xADV[5\x91a7OV[\x01a\x0E#V[P\x82\x84\x14\x15a\x0E\x1DV[` 6`\x03\x19\x01\x12a\x02\xF5W`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5W\x80`\x04\x01\x90```\x03\x19\x826\x03\x01\x12a\x02\xF5Wa\x0E\xA2a5&V[a\x0E\xAAa5\x13V[a\x0E\xB2a5^V[`\x01`\x01`\xA0\x1B\x03a\x0E\xC3\x83a%\xF4V[\x16\x15a\t%W`D\x81\x01\x90a\x0E\xDBa\x08\x1D\x83\x85a\"\xC1V[\x90`\x01`\x01`\xE0\x1B\x03\x19\x82\x16cU\x8Ar\x97`\xE0\x1B\x81\x14\x90\x81\x15a\x0F\xBBW[Pa\t\x05W_\x80\x91a\x0F\x1C\x94`$a\x0F\x10\x88a%\xF4V[\x92\x015\x95\x86\x91\x88a\"\xC1V[\x91\x90a\x0F-`@Q\x80\x94\x81\x93a+\xDAV[\x03\x92Z\xF1\x92a\x0F:a&3V[\x93\x15a\x0F\x9FW\x7F\xE0\x8F\x89%\xF4\\3}QK\x07\xAF%&\xE1DI\xBC\xF9\n\xFD\x92\xEF\xD8\xB6\x11\xF1~\xBFA\x9D\xB0\x91\x90`\x01`\x01`\xA0\x1B\x03\x90a\x0Fu\x90a%\xF4V[`@\x80Q\x95\x86R`\x01`\x01`\xE0\x1B\x03\x19\x93\x90\x93\x16` \x86\x01R\x16\x92\xA2_h\x80\0\0\0\0\xAB\x14<\x06]\0[`@Qc\\\r\xEE]`\xE0\x1B\x81R\x80a\t\x01\x86_`\x04\x84\x01a+\xE7V[cBj\x84\x93`\xE0\x1B\x14\x90P_a\x0E\xF9V[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W` `\xFF`\x01T`\xA0\x1C\x16`@Q\x90\x15\x15\x81R\xF3[4a\x02\xF5W``6`\x03\x19\x01\x12a\x02\xF5W`\x045a\x10\x0E\x81a\x03\xCDV[`$5a\x10\x1A\x81a\x03\xCDV[\x7F\x82\xE41@\xFCA\xDB\xCA\xB6\x16;\xC2\xBD}\xDF@\xD7Gr\x86\xFB|\x95\xC3|\x1D\xBC\x95wV\xA9\xBA` `D5\x92a\x10J\x84a\x07\x11V[a\x10Ra5\x13V[a\x10]\x84\x82\x87a6JV[`@Q\x93\x15\x15\x84R`\x01`\x01`\xA0\x1B\x03\x90\x81\x16\x94\x16\x92\xA3\0[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5Wa\x10\x8Ea5\x13V[`\x01\x80T`\x01`\x01`\xA0\x1B\x03\x19\x90\x81\x16\x90\x91U_\x80T\x91\x82\x16\x81U\x90`\x01`\x01`\xA0\x1B\x03\x16\x7F\x8B\xE0\x07\x9CS\x16Y\x14\x13D\xCD\x1F\xD0\xA4\xF2\x84\x19I\x7F\x97\"\xA3\xDA\xAF\xE3\xB4\x18okdW\xE0\x82\x80\xA3\0[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W`\x01T3`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x03a\x11KW`\x01\x80T`\x01`\x01`\xA0\x1B\x03\x19\x90\x81\x16\x90\x91U_\x80T3\x92\x81\x16\x83\x17\x82U`\x01`\x01`\xA0\x1B\x03\x16\x90\x7F\x8B\xE0\x07\x9CS\x16Y\x14\x13D\xCD\x1F\xD0\xA4\xF2\x84\x19I\x7F\x97\"\xA3\xDA\xAF\xE3\xB4\x18okdW\xE0\x90\x80\xA3\0[c\x11\x8C\xDA\xA7`\xE0\x1B_R3`\x04R`$_\xFD[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W` `\x04T`@Q\x90\x81R\xF3[4a\x02\xF5W` 6`\x03\x19\x01\x12a\x02\xF5W`\x045a\x11\x98\x81a\x03\xCDV[`\x01\x80`\xA0\x1B\x03\x16_R`\x02` R` `\xFF`@_ T\x16`@Q\x90\x15\x15\x81R\xF3[4a\x02\xF5W` 6`\x03\x19\x01\x12a\x02\xF5W`\x045a\x11\xD8\x81a\x03\xCDV[`\x01\x80`\xA0\x1B\x03\x16_R`\x03` R` `\xFF`@_ T\x16`@Q\x90\x15\x15\x81R\xF3[`\x80\x90`\x03\x19\x01\x12a\x02\xF5W`\x045a\x12\x13\x81a\x03\xCDV[\x90`$5a\x12 \x81a\x03\xCDV[\x90`D5\x90`d5\x90V[4a\x02\xF5Wa\x1296a\x11\xFBV[\x90\x92a\x12Ca5\x13V[a\x12Ka5^V[`\x01`\x01`\xA0\x1B\x03\x16\x91\x82;\x15a\x02\xF5Wa\x12\x83\x92_\x92\x83`@Q\x80\x96\x81\x95\x82\x94c\xA4\x15\xBC\xAD`\xE0\x1B\x84R\x84\x9A0\x92`\x04\x86\x01a)\xBDV[\x03\x92Z\xF1\x80\x15a\r@Wa\x12\x95WP\x80\xF3[a\0\x18\x91P_\x90a\x04\x1FV[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5Wa\x12\xB9a5\x13V[a\x12\xC1a5^V[`\x01\x80T`\xFF`\xA0\x1B\x19\x16`\x01`\xA0\x1B\x17\x90U`@Q3\x81R\x7Fb\xE7\x8C\xEA\x01\xBE\xE3 \xCDNB\x02p\xB5\xEAt\0\r\x11\xB0\xC9\xF7GT\xEB\xDB\xFCTK\x05\xA2X\x90` \x90\xA1\0[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W_T`@Q`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x81R` \x90\xF3[` `\x03\x19\x82\x01\x12a\x02\xF5W`\x045\x90`\x01`\x01`@\x1B\x03\x82\x11a\x02\xF5Wa\x01`\x90\x82\x90\x03`\x03\x19\x01\x12a\x02\xF5W`\x04\x01\x90V[4a\x02\xF5Wa\x13k6a\x13)V[a\x13sa5&V[a\x13{a5\x13V[a\x13\x83a5^V[`\x01`\x01`\xA0\x1B\x03a\x13\x94\x82a%\xF4V[\x16\x15\x80\x15a\x14vW[\x80\x15a\x14`W[a\t%W_a\x14\x05\x91`@Q\x90a\x13\xD0\x82a\x0CI` \x82\x01\x93c\xD4\x1E]?`\xE0\x1B\x85R`$\x83\x01a)\xEDV[a\x13\xD8a7\xADV[\x81Q\x90 _Q` a9\xE4_9_Q\x90_R]`@Q\x80\x93\x81\x92cH\xC8\x94\x91`\xE0\x1B\x83R`\x04\x83\x01a+\x13V[\x03\x81\x83s\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA9Z\xF1\x90\x81\x15a\r@W_\x91a\x14>W[PQ_\x19\x14a\t%Wa\r\x0Ea7\x9BV[a\x14Z\x91P=\x80_\x83>a\x14R\x81\x83a\x04\x1FV[\x81\x01\x90a*\xB1V[_a\x14-V[Pa\x14pa\x0C\xBF``\x83\x01a%\xF4V[\x15a\x13\xA4V[P` \x81\x015\x15a\x13\x9DV[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W` `@Qoqr}\xE2.^\x9D\x8B\xAF\x0E\xDA\xC6\xF3}\xA02\x81R\xF3[4a\x02\xF5W` 6`\x03\x19\x01\x12a\x02\xF5W`\x045a\x14\xC8a5\x13V[\x80\x15\x80\x15a\x15\x06W[a\t%W` \x81\x7Fn\x8A\x18}yD\x99\x80\x85\xDB\xD1\xF1k\x84\xC5\x1C\x90;\xB7'Sl\xDB\xA8ibC\x9A\xDE\xD2\xCF\xD7\x92`\x05U`@Q\x90\x81R\xA1\0[P`\x04T\x81\x11a\x14\xD1V[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W` `@Qsl${\x1Fa\x821\x88w1\x177\xBA\xC0\x84K\xAAQ\x8F^\x81R\xF3[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W` `@Qs\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA9\x81R\xF3[4a\x02\xF5W``6`\x03\x19\x01\x12a\x02\xF5W`\x045a\x15\x8A\x81a\x03\xCDV[`$5\x90`D5\x90a\x15\x9B\x82a\x03\xCDV[a\x15\xA3a5&V[a\x15\xABa5\x13V[a\x15\xB3a5^V[`\x01`\x01`\xA0\x1B\x03\x16\x90\x81\x15\x80\x15a\x16xW[a\t%W\x81;\x15a\x02\xF5W`@Qc!B\x17\x07`\xE1\x1B\x81R0`\x04\x82\x01R`\x01`\x01`\xA0\x1B\x03\x82\x16`$\x82\x01R`D\x81\x01\x84\x90R_\x81`d\x81\x83\x87Z\xF1\x80\x15a\r@W`\x01\x92\x7F5\xE2Y\xC1\xA7\x81\xDC&I\xE7b\x98\xB5\xA4\xD5H\xC7\x90R\x87\xCA=|C7H\x0C\xE3\xFD\x05\xEBi\x92a\x16Y\x92a\x16dW[P`@Q\x91\x82\x91\x85\x80`\xA0\x1B\x03\x16\x96\x82` _\x91\x93\x92\x93`@\x81\x01\x94\x81R\x01RV[\x03\x90\xA4a\0\x18a5PV[\x80a\r4_a\x16r\x93a\x04\x1FV[_a\x167V[P`\x01`\x01`\xA0\x1B\x03\x81\x16\x15a\x15\xC6V[`\x01`\x01`@\x1B\x03\x81\x11a\x04\x1AW`\x05\x1B` \x01\x90V[\x90\x80`\x1F\x83\x01\x12\x15a\x02\xF5W\x815a\x16\xB7\x81a\x16\x89V[\x92a\x16\xC5`@Q\x94\x85a\x04\x1FV[\x81\x84R` \x80\x85\x01\x92`\x05\x1B\x82\x01\x01\x92\x83\x11a\x02\xF5W` \x01\x90[\x82\x82\x10a\x16\xEDWPPP\x90V[\x815\x81R` \x91\x82\x01\x91\x01a\x16\xE0V[4a\x02\xF5W`\xA06`\x03\x19\x01\x12a\x02\xF5W`\x045a\x17\x1A\x81a\x03\xCDV[`$5\x90a\x17'\x82a\x03\xCDV[`D5`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\x17F\x906\x90`\x04\x01a\x16\xA0V[\x90`d5`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\x17f\x906\x90`\x04\x01a\x16\xA0V[`\x845\x92`\x01`\x01`@\x1B\x03\x84\x11a\x02\xF5Wa\x03c\x94a\x17\x8Da\x17\x93\x956\x90`\x04\x01a\x04\xD5V[\x93a+$V[`@Q`\x01`\x01`\xE0\x1B\x03\x19\x90\x91\x16\x81R\x90\x81\x90` \x82\x01\x90V[4a\x02\xF5Wa\x17\xBC6a\x13)V[s\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA93\x03a\x1E9W_Q` a9\xE4_9_Q\x90_R\\\x80\x15a\x1E*Wa\x17\xF666a\x04kV[` \x81Q\x91\x01 \x03a\x1E\x1BWa\x18\x11a\x0C\xBFa\x0C\xBF\x83a%\xF4V[`@Qcp\xA0\x821`\xE0\x1B\x81R0`\x04\x82\x01R\x90` \x90\x82\x90`$\x90\x82\x90Z\xFA\x90\x81\x15a\r@W_\x91a\x1D\xFCW[Pa\x18I\x82a%\xF4V[\x91` \x81\x015\x92s\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA9;\x15a\x02\xF5W`@Qc\xAEc\x93)`\xE0\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x91\x90\x91\x16`\x04\x82\x01R0`$\x82\x01R`D\x81\x01\x84\x90R_\x81`d\x81\x83s\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA9Z\xF1\x80\x15a\r@Wa\x1D\xE8W[P\x90\x91`@\x82\x01\x91\x90_[a\x18\xD3\x84\x84a+\xA5V[\x90P\x81\x10\x15a\x19pW_\x80a\x18\xFAa\x18\xF5\x84a\x18\xEF\x89\x89a+\xA5V[\x90a#\xCFV[a%\xF4V[` a\x19\n\x85a\x18\xEF\x8A\x8Aa+\xA5V[\x015a\x19'a\x19\x1D\x86a\x18\xEF\x8B\x8Ba+\xA5V[`@\x81\x01\x90a\"\xC1V[\x91\x90a\x198`@Q\x80\x94\x81\x93a+\xDAV[\x03\x92Z\xF1a\x19Da&3V[\x90\x15a\x19SWP`\x01\x01a\x18\xC9V[`@Qc\\\r\xEE]`\xE0\x1B\x81R\x91\x82\x91a\t\x01\x91`\x04\x84\x01a+\xE7V[P\x92\x90\x91`\xA0\x83\x015\x90\x81a\x1DEW[`\xE0\x84\x015\x90\x81a\x1C\xCAW[a\x01 \x85\x01\x93a\x19\x9C\x85\x87a+\xA5V[\x90P_[\x81\x81\x10a\x1C4WPPa\x19\xB8a\x0C\xBFa\x0C\xBF\x88a%\xF4V[`@Qcp\xA0\x821`\xE0\x1B\x81R0`\x04\x82\x01R\x91\x90` \x90\x83\x90`$\x90\x82\x90Z\xFA\x80\x15a\r@Wa\x1A\x01\x89a\x19\xFBa\x1A\x06\x93a\x1A\x0C\x96_\x91a\x1C\x15W[Pa8kV[\x93a,%V[a8kV[\x90a,2V[\x93a\x01@\x86\x015\x80\x86\x12a\x1B\xFEWPa\x1A-\x87a\x1A(\x88a%\xF4V[a6\xEAV[a\x1Am` \x88a\x1A<\x89a%\xF4V[`@Qc\x15\xAF\xD4\t`\xE0\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x90\x91\x16`\x04\x82\x01R`$\x81\x01\x91\x90\x91R\x91\x82\x90\x81\x90`D\x82\x01\x90V[\x03\x81_s\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA9Z\xF1\x90\x81\x15a\r@W_\x91a\x1B\xCFW[P\x87\x81\x10a\x1B\xB8WP\x7FS\xF2\x133U\x06;\x07\x87\xBE\x9Bs\xF9\xF2\xC3\xD6\xE1Fp\xE2\xA3}\xD4b6\x8B]\x1A\x94\xE8k\x06\x93\x92\x91a\x1B\xB3\x91a\x1BLa\x1A\xE8a\x1A\xDFa\x1A\xD8\x8Ba%\xF4V[\x94\x8Ba+\xA5V[\x93\x90P\x8Aa+\xA5V[`@\x80Q0` \x82\x01\x90\x81Rs\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA9\x92\x82\x01\x92\x90\x92R`\x01`\x01`\xA0\x1B\x03\x90\x96\x16``\x87\x01R`\x80\x86\x01\x8D\x90R`\xA0\x86\x01\x94\x90\x94R`\xC0\x85\x01RP\x90\x91\x90\x81`\xE0\x81\x01[\x03`\x1F\x19\x81\x01\x83R\x82a\x04\x1FV[Q\x90 \x94a\x1BY\x87a%\xF4V[\x97a\x1Br`\xC0a\x1Bk`\x80\x8B\x01a%\xF4V[\x99\x01a%\xF4V[`@\x80Q\x83\x81R`\x01`\x01`\xA0\x1B\x03\x9A\x8B\x16` \x82\x01R\x90\x81\x01\x96\x90\x96R``\x86\x01\x94\x90\x94R`\x80\x85\x01R`\xA0\x84\x01R\x90\x85\x16\x95\x90\x94\x16\x93\x81\x90`\xC0\x82\x01\x90V[\x03\x90\xA4\0[c>c9\xE3`\xE1\x1B_R`\x04\x88\x90R`$R`D_\xFD[a\x1B\xF1\x91P` =` \x11a\x1B\xF7W[a\x1B\xE9\x81\x83a\x04\x1FV[\x81\x01\x90a+\x96V[_a\x1A\x95V[P=a\x1B\xDFV[c\n;\t\xA1`\xE0\x1B_R`\x04\x86\x90R`$R`D_\xFD[a\x1C.\x91P` =` \x11a\x1B\xF7Wa\x1B\xE9\x81\x83a\x04\x1FV[_a\x19\xF5V[_\x80\x88\x8Aa\x1Cla\x19\x1D\x86a\x18\xEFa\x1CSa\x18\xF5\x83a\x18\xEF\x89\x89a+\xA5V[\x95` a\x1Cd\x84a\x18\xEF\x84\x8Aa+\xA5V[\x015\x95a+\xA5V[\x91\x90a\x1C}`@Q\x80\x94\x81\x93a+\xDAV[\x03\x92Z\xF1a\x1C\x89a&3V[\x90\x15a\x1C\x98WP`\x01\x01a\x19\xA0V[a\x1C\xAE\x82a\x1C\xA6\x87\x8Ca+\xA5V[\x91\x90Pa,%V[a\t\x01`@Q\x92\x83\x92c\\\r\xEE]`\xE0\x1B\x84R`\x04\x84\x01a+\xE7V[a\x1C\xDCa\x0C\xBFa\x0C\xBF``\x88\x01a%\xF4V[a\x1C\xE8`\xC0\x87\x01a%\xF4V[a\x01\0\x87\x015\x90\x82;\x15a\x02\xF5Wa\x1D\x1C\x92_\x92\x83`@Q\x80\x96\x81\x95\x82\x94c\xA4\x15\xBC\xAD`\xE0\x1B\x84R\x8B0\x92`\x04\x86\x01a)\xBDV[\x03\x92Z\xF1\x80\x15a\r@Wa\x1D1W[Pa\x19\x8CV[\x80a\r4_a\x1D?\x93a\x04\x1FV[_a\x1D+V[`\x80\x84\x01a\x1D|a\x1Dva\x0C\xBFa\x0C\xBFa\x1D^\x85a%\xF4V[a\x18\xF5\x88``\x8C\x01\x92a\x1Dp\x84a%\xF4V[\x90a7\xD3V[\x91a%\xF4V[\x90\x80;\x15a\x02\xF5W`@Qca{\xA07`\xE0\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x92\x90\x92\x16`\x04\x83\x01R`$\x82\x01\x84\x90R0`D\x83\x01R_`d\x83\x01\x81\x90R\x90\x82\x90`\x84\x90\x82\x90\x84\x90Z\xF1\x80\x15a\r@Wa\x1D\xD4W[Pa\x19\x80V[\x80a\r4_a\x1D\xE2\x93a\x04\x1FV[_a\x1D\xCEV[\x80a\r4_a\x1D\xF6\x93a\x04\x1FV[_a\x18\xBEV[a\x1E\x15\x91P` =` \x11a\x1B\xF7Wa\x1B\xE9\x81\x83a\x04\x1FV[_a\x18?V[cb\x0B\xE6#`\xE0\x1B_R`\x04_\xFD[cx\xFC\xF8\x0B`\xE1\x1B_R`\x04_\xFD[c\x8E^P=`\xE0\x1B_R`\x04_\xFD[4a\x02\xF5W`d` a\x1EZ6a\n\xECV[\x91\x93\x90a\x1Eea5\x13V[a\x1Ema5^V[`@Qc\x1AL\xA3{`\xE2\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x91\x82\x16`\x04\x82\x01R`$\x81\x01\x93\x90\x93R0`D\x84\x01R\x91\x93\x84\x92\x83\x91_\x91\x16Z\xF1\x80\x15a\r@Wa\x03c\x91_\x91a\x1E\xC5W[P`@Q\x90\x81R\x90\x81\x90` \x82\x01\x90V[a\x1E\xDE\x91P` =` \x11a\x1B\xF7Wa\x1B\xE9\x81\x83a\x04\x1FV[_a\x1E\xB4V[4a\x02\xF5W` \x7F\x045Aj_H\xD4\x1D^^\xDE-\x05\xC0\xE1\xFFk\x1Fq\xCBW\x17k\x10\x11\xEA\x7F\xBD\x8CrZ\x83a\x1F\x156a\x07\x1BV[\x92\x90a\x1F\x1Fa5\x13V[`\x01`\x01`\xA0\x1B\x03\x16_\x81\x81R`\x02\x83R`@\x90 \x80T`\xFF\x19\x16`\xFF\x86\x15\x15\x16\x17\x90U\x92`@Q\x90\x15\x15\x81R\xA2\0[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W`\x01T`@Q`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x81R` \x90\xF3[4a\x02\xF5W``6`\x03\x19\x01\x12a\x02\xF5W`\x045a\x1F\x94\x81a\x03\xCDV[`$5\x90a\x1F\xA1\x82a\x03\xCDV[`D5`\x01`\x01`\xE0\x1B\x03\x19\x81\x16\x81\x03a\x02\xF5W` \x92a\x1F\xC1\x92a8\x80V[`@Q\x90\x15\x15\x81R\xF3[4a\x02\xF5W`\x806`\x03\x19\x01\x12a\x02\xF5W`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\x1F\xFB\x906\x90`\x04\x01a\x07_V[`$5`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa \x1A\x906\x90`\x04\x01a\x07_V[`D\x93\x91\x935`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa <\x906\x90`\x04\x01a\x07_V[\x91`d5\x95`\x01`\x01`@\x1B\x03\x87\x11a\x02\xF5Wa `a\0\x18\x976\x90`\x04\x01a\x05\x85V[\x96\x90\x95a.+V[4a\x02\xF5W`\xA06`\x03\x19\x01\x12a\x02\xF5Wa \x84`\x045a\x03\xCDV[a \x8F`$5a\x03\xCDV[`D5`d5`\x845`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa \xB4\x906\x90`\x04\x01a\x04\xD5V[P`@Q\x90\x81R3\x90\x7F\xD1\xEC\x03\r\xA0\xF9\x9F\xCC\xAD\x1D\xE2wM\xC9\x82w\xB6\xE7i:T\x9FGQ\xC3\x19\xCF\x877\x9B\xC3\x0C` c\xF2:na`\xE0\x1B\x92\xA4`@Qc\xF2:na`\xE0\x1B\x81R` \x90\xF3[4a\x02\xF5W` 6`\x03\x19\x01\x12a\x02\xF5W`\x045a!\x19\x81a\x03\xCDV[a!!a5\x13V[`\x01\x80`\xA0\x1B\x03\x16\x80k\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF`\xA0\x1B`\x01T\x16\x17`\x01U`\x01\x80`\xA0\x1B\x03_T\x16\x7F8\xD1k\x8C\xAC\"\xD9\x9F\xC7\xC1$\xB9\xCD\r\xE2\xD3\xFA\x1F\xAE\xF4 \xBF\xE7\x91\xD8\xC3b\xD7e\xE2'\0_\x80\xA3\0[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W`@Q\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16\x81R` \x90\xF3[4a\x02\xF5Wa!\xC46a\n\xECV[\x91a!\xCDa5\x13V[a!\xD5a5^V[a!\xE0\x83\x82\x84a7\xD3V[`\x01`\x01`\xA0\x1B\x03\x16\x80;\x15a\x02\xF5W`@Qca{\xA07`\xE0\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x90\x92\x16`\x04\x83\x01R`$\x82\x01\x92\x90\x92R0`D\x82\x01R_`d\x82\x01\x81\x90R\x91\x82\x90\x82\x90\x81\x83\x81`\x84\x81\x01a\x12\x83V[4a\x02\xF5W`\x84` _a\"G6a\x11\xFBV[\x95\x92a\"T\x94\x91\x94a5\x13V[a\"\\a5^V[a\"g\x85\x82\x85a7\xD3V[`@Q\x96\x87\x95\x86\x94cW:\xDE\x81`\xE0\x1B\x86R`\x01\x80`\xA0\x1B\x03\x16`\x04\x86\x01R`$\x85\x01R`D\x84\x01R0`d\x84\x01R`\x01\x80`\xA0\x1B\x03\x16Z\xF1\x80\x15a\r@Wa\x03c\x91_\x91a\x1E\xC5WP`@Q\x90\x81R\x90\x81\x90` \x82\x01\x90V[\x905\x90`\x1E\x19\x816\x03\x01\x82\x12\x15a\x02\xF5W\x01\x805\x90`\x01`\x01`@\x1B\x03\x82\x11a\x02\xF5W` \x01\x91\x816\x03\x83\x13a\x02\xF5WV[`\x01`\x01`\xA0\x1B\x03\x81\x16\x91\x90\x82\x15a\t%W_\x83\x81R`\x03` R`@\x90 T`\xFF\x16\x15\x15\x82\x15\x15\x14a#\xB6W`\x01`\x01`\xA0\x1B\x03\x16_\x90\x81R`\x03` R`@\x90 \x7F\xFCJ\xCBI\x94\x91\xCD\x85\n\x8A!\xAB\x98\xC7\xF1(\x85\x0C\x0F\x0E_\x1A\x87Zb\xB7\xFA\x05\\.\xCF\x19\x91a#\x9D\x91a#q\x90\x82`\xFF\x80\x19\x83T\x16\x91\x15\x15\x16\x17\x90UV[\x80\x15a#\xA2Wa#\x8Ba#\x86`\x04T`\x01\x01\x90V[`\x04UV[`@Q\x90\x15\x15\x81R\x90\x81\x90` \x82\x01\x90V[\x03\x90\xA2V[`\x04Ta#\xB1\x90_\x19\x01`\x04UV[a#\x8BV[PPPV[cNH{q`\xE0\x1B_R`2`\x04R`$_\xFD[\x91\x90\x81\x10\x15a#\xF1W`\x05\x1B\x81\x015\x90`^\x19\x816\x03\x01\x82\x12\x15a\x02\xF5W\x01\x90V[a#\xBBV[\x91\x90a$\0a5&V[a$\x08a5\x13V[a$\x10a5^V[_[\x81\x81\x10a$%WPP\x90Pa\x03\xE9a5PV[a$0\x81\x83\x86a%\xE4V[\x90` \x82\x01a$Aa\x0C\xBF\x82a%\xF4V[\x15\x80\x15a%\xCEW[a\t%Wa$V\x83a&\x1CV[a$_\x81a%\xFEV[a%\x0BWa$o`\xA0\x84\x01a&)V[a$\xFCW`\x01\x92a$\x7F\x82a%\xF4V[\x90\x7FJ\x94\xF8\x9E\x13\x16\x99\xED4\x16g\x0C\x01\x1C\xE6Mb\xE5\xA5\x81\xA4\xEB\xB4`;\xF6\xC4\xA5\xD0j\x06\xCEa$\xF2a$\xD4a$\xCE``\x85\x015\x96a\x18\xF5`@`\x80\x88\x015\x97\x01\x97\x87\x8Aa$\xC8\x8Ba%\xF4V[\x92a5\x91V[\x94a%\xF4V[`@Q\x93\x84R`\xA0\x88\x90\x1B\x88\x90\x03\x90\x81\x16\x94\x16\x92\x90\x81\x90` \x82\x01\x90V[\x03\x90\xA4[\x01a$\x12V[c\xB0&\xD5\xA3`\xE0\x1B_R`\x04_\xFD[``\x83\x015\x15\x80\x15\x90a%\xC1W[a%\xB2W`\x01\x92\x7F\x9C\x8E\x17\xFA\x11M$\xCF\xC8\xF6|=l\xE6\xBC.$\x06}\xBEA%f@\xBCH\xFDm\x10fV/a%\xAAa%\x88a%\x82a%|a%V\x87a%\xF4V[\x96a\x18\xF5`@\x88\x01\x98`\xA0a%j\x8Ba%\xF4V[\x99\x01\x98a%v\x8Aa&)V[\x91a6JV[\x95a%\xF4V[\x93a&)V[`@Q\x90\x15\x15\x81R`\xA0\x87\x90\x1B\x87\x90\x03\x93\x84\x16\x94\x90\x93\x16\x92\x90\x81\x90` \x82\x01\x90V[\x03\x90\xA3a$\xF6V[cA\xF5!\xF9`\xE1\x1B_R`\x04_\xFD[P`\x80\x83\x015\x15\x15a%\x19V[Pa%\xDEa\x0C\xBF`@\x85\x01a%\xF4V[\x15a$IV[\x91\x90\x81\x10\x15a#\xF1W`\xC0\x02\x01\x90V[5a\x04\xF0\x81a\x03\xCDV[`\x02\x11\x15a&\x08WV[cNH{q`\xE0\x1B_R`!`\x04R`$_\xFD[5`\x02\x81\x10\x15a\x02\xF5W\x90V[5a\x04\xF0\x81a\x07\x11V[=\x15a&]W=\x90a&D\x82a\x04PV[\x91a&R`@Q\x93\x84a\x04\x1FV[\x82R=_` \x84\x01>V[``\x90V[\x905`\x1E\x19\x826\x03\x01\x81\x12\x15a\x02\xF5W\x01` \x815\x91\x01\x91`\x01`\x01`@\x1B\x03\x82\x11a\x02\xF5W\x81`\x05\x1B6\x03\x83\x13a\x02\xF5WV[\x90\x80` \x93\x92\x81\x84R\x84\x84\x017_\x82\x82\x01\x84\x01R`\x1F\x01`\x1F\x19\x16\x01\x01\x90V[\x90` \x83\x82\x81R\x01` \x82`\x05\x1B\x85\x01\x01\x93\x83_\x91[\x84\x83\x10a&\xDCWPPPPPP\x90V[\x90\x91\x92\x93\x94\x95`\x1F\x19\x82\x82\x03\x01\x85R\x865`^\x19\x846\x03\x01\x81\x12\x15a\x02\xF5W\x83\x01\x805a'\x08\x81a\x03\xCDV[`\x01`\x01`\xA0\x1B\x03\x16\x82R` \x81\x81\x015\x90\x83\x01R`@\x81\x0156\x82\x90\x03`\x1E\x19\x01\x81\x12\x15a\x02\xF5W\x01` \x815\x91\x01\x90`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5W\x806\x03\x82\x13a\x02\xF5Wa'j` \x92\x83\x92``\x86\x81`@`\x01\x99\x01R\x01\x91a&\x96V[\x98\x01\x96\x95\x01\x93\x01\x91\x90a&\xCCV[` \x81Ra'\x99` \x82\x01a'\x8C\x84a\x03\xDEV[`\x01`\x01`\xA0\x1B\x03\x16\x90RV[a'\xB8a'\xA8` \x84\x01a\x03\xDEV[`\x01`\x01`\xA0\x1B\x03\x16`@\x83\x01RV[`@\x82\x015``\x82\x01Ra\x01\x80a\x01`a(\x8Ba'\xECa'\xDB``\x87\x01\x87a&bV[\x85`\x80\x88\x01Ra\x01\xA0\x87\x01\x91a&\xB6V[a(\x0Ba'\xFB`\x80\x88\x01a\x03\xDEV[`\x01`\x01`\xA0\x1B\x03\x16`\xA0\x87\x01RV[a(*a(\x1A`\xA0\x88\x01a\x03\xDEV[`\x01`\x01`\xA0\x1B\x03\x16`\xC0\x87\x01RV[`\xC0\x86\x015`\xE0\x86\x01Ra(Ta(C`\xE0\x88\x01a\x03\xDEV[`\x01`\x01`\xA0\x1B\x03\x16a\x01\0\x87\x01RV[a\x01\0\x86\x015a\x01 \x86\x01Ra\x01 \x86\x015a\x01@\x86\x01Ra(za\x01@\x87\x01\x87a&bV[\x86\x83\x03`\x1F\x19\x01\x85\x88\x01R\x90a&\xB6V[\x93\x015\x91\x01R\x90V[`@\x80Q\x90\x91\x90a(\xA5\x83\x82a\x04\x1FV[`\x01\x81R\x91`\x1F\x19\x016` \x84\x017V[\x80Q\x15a#\xF1W` \x01\x90V[\x80Q\x82\x10\x15a#\xF1W` \x91`\x05\x1B\x01\x01\x90V[\x80Q\x80\x83R` \x92\x91\x81\x90\x84\x01\x84\x84\x01^_\x82\x82\x01\x84\x01R`\x1F\x01`\x1F\x19\x16\x01\x01\x90V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x81R`\x80` \x80\x83\x01\x82\x90R\x83Q\x91\x83\x01\x82\x90R`\xA0\x83\x01\x96\x95\x93\x01\x90_[\x81\x81\x10a)zWPPP\x80\x85\x03`@\x82\x01R` \x80\x84Q\x96\x87\x81R\x01\x93\x01_\x95[\x80\x87\x10a)bWPPa\x04\xF0\x93\x94P``\x81\x84\x03\x91\x01Ra(\xD7V[\x90\x93` \x80`\x01\x92\x87Q\x81R\x01\x95\x01\x96\x01\x95\x90a)FV[\x82Q`\x01`\x01`\xA0\x1B\x03\x16\x88R` \x97\x88\x01\x97\x90\x92\x01\x91`\x01\x01a)%V[`@Q=_\x82>=\x90\xFD[\x90\x15a#\xF1W\x90V[\x91\x90\x81\x10\x15a#\xF1W`\x05\x1B\x01\x90V[`\x01`\x01`\xA0\x1B\x03\x91\x82\x16\x81R` \x81\x01\x92\x90\x92R`@\x82\x01\x92\x90\x92R_``\x82\x01R\x91\x16`\x80\x82\x01R`\xA0\x01\x90V[` \x81Ra*\x01` \x82\x01a'\x8C\x84a\x03\xDEV[` \x82\x015`@\x82\x01Ra\x01`a\x01@a(\x8Ba*5a*$`@\x87\x01\x87a&bV[\x85``\x88\x01Ra\x01\x80\x87\x01\x91a&\xB6V[a*Ta*D``\x88\x01a\x03\xDEV[`\x01`\x01`\xA0\x1B\x03\x16`\x80\x87\x01RV[a*ca'\xFB`\x80\x88\x01a\x03\xDEV[`\xA0\x86\x015`\xC0\x86\x01Ra*\x8Ca*|`\xC0\x88\x01a\x03\xDEV[`\x01`\x01`\xA0\x1B\x03\x16`\xE0\x87\x01RV[`\xE0\x86\x015a\x01\0\x86\x01Ra\x01\0\x86\x015a\x01 \x86\x01Ra(za\x01 \x87\x01\x87a&bV[` \x81\x83\x03\x12a\x02\xF5W\x80Q\x90`\x01`\x01`@\x1B\x03\x82\x11a\x02\xF5W\x01\x81`\x1F\x82\x01\x12\x15a\x02\xF5W\x80Q\x90a*\xE4\x82a\x04PV[\x92a*\xF2`@Q\x94\x85a\x04\x1FV[\x82\x84R` \x83\x83\x01\x01\x11a\x02\xF5W\x81_\x92` \x80\x93\x01\x83\x86\x01^\x83\x01\x01R\x90V[\x90` a\x04\xF0\x92\x81\x81R\x01\x90a(\xD7V[PP\x92\x91P_[\x83Q\x81\x10\x15a+\x88W\x80a+A`\x01\x92\x86a(\xC3V[Qa+L\x82\x85a(\xC3V[Q`@Q\x90\x81R3\x90\x7F\xD1\xEC\x03\r\xA0\xF9\x9F\xCC\xAD\x1D\xE2wM\xC9\x82w\xB6\xE7i:T\x9FGQ\xC3\x19\xCF\x877\x9B\xC3\x0C` c\xBC\x19|\x81`\xE0\x1B\x92\xA4\x01a++V[Pc\xBC\x19|\x81`\xE0\x1B\x92PPV[\x90\x81` \x91\x03\x12a\x02\xF5WQ\x90V[\x905\x90`\x1E\x19\x816\x03\x01\x82\x12\x15a\x02\xF5W\x01\x805\x90`\x01`\x01`@\x1B\x03\x82\x11a\x02\xF5W` \x01\x91\x81`\x05\x1B6\x03\x83\x13a\x02\xF5WV[\x90\x80\x92\x91\x827\x01_\x81R\x90V[`@\x90a\x04\xF0\x93\x92\x81R\x81` \x82\x01R\x01\x90a(\xD7V[cNH{q`\xE0\x1B_R`\x11`\x04R`$_\xFD[\x90`A\x82\x01\x80\x92\x11a, WV[a+\xFEV[\x91\x90\x82\x01\x80\x92\x11a, WV[\x81\x81\x03\x92\x91_\x13\x80\x15\x82\x85\x13\x16\x91\x84\x12\x16\x17a, WV[\x90\x80`\x1F\x83\x01\x12\x15a\x02\xF5W\x815\x91a,b\x83a\x16\x89V[\x92a,p`@Q\x94\x85a\x04\x1FV[\x80\x84R` \x80\x85\x01\x91`\x05\x1B\x83\x01\x01\x91\x83\x83\x11a\x02\xF5W` \x81\x01\x91[\x83\x83\x10a,\x9CWPPPPP\x90V[\x825`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5W\x82\x01\x90``\x82\x87\x03`\x1F\x19\x01\x12a\x02\xF5W`@Q\x90a,\xCA\x82a\x03\xFFV[` \x83\x015a,\xD8\x81a\x03\xCDV[\x82R`@\x83\x015` \x83\x01R``\x83\x015\x91`\x01`\x01`@\x1B\x03\x83\x11a\x02\xF5Wa-\n\x88` \x80\x96\x95\x81\x96\x01\x01a\x04\xD5V[`@\x82\x01R\x81R\x01\x92\x01\x91a,\x8DV[` \x81\x83\x03\x12a\x02\xF5W\x805\x90`\x01`\x01`@\x1B\x03\x82\x11a\x02\xF5W\x01\x90a\x01\x80\x82\x82\x03\x12a\x02\xF5Wa-Ja\x04@V[\x91a-T\x81a\x03\xDEV[\x83Ra-b` \x82\x01a\x03\xDEV[` \x84\x01R`@\x81\x015`@\x84\x01R``\x81\x015`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5W\x82a-\x90\x91\x83\x01a,JV[``\x84\x01Ra-\xA1`\x80\x82\x01a\x03\xDEV[`\x80\x84\x01Ra-\xB2`\xA0\x82\x01a\x03\xDEV[`\xA0\x84\x01R`\xC0\x81\x015`\xC0\x84\x01Ra-\xCD`\xE0\x82\x01a\x03\xDEV[`\xE0\x84\x01Ra\x01\0\x81\x015a\x01\0\x84\x01Ra\x01 \x81\x015a\x01 \x84\x01Ra\x01@\x81\x015`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\x01`\x92a.\r\x91\x83\x01a,JV[a\x01@\x84\x01R\x015a\x01`\x82\x01R\x90V[\x91\x90\x82\x03\x91\x82\x11a, WV[\x92\x97_Q` a:\x04_9_Q\x90_R\\\x97\x94\x96\x90\x95\x92\x94\x91`\x01`\x01`\xA0\x1B\x03\x89\x16\x80\x15a\x1E*W3\x03a3\xB4W`\x01\x81\x14\x80\x15\x90a3\xA9W[\x80\x15a3\x9EW[a3wW_Q` a9\xE4_9_Q\x90_R\\a.\x8B6\x85\x85a\x04\x9FV[` \x81Q\x91\x01 \x03a\x1E\x1BWa.\xAA\x82a.\xB0\x94a\x18\xF5\x94\x01\x90a-\x1AV[\x94a)\xA4V[` \x83\x01\x80Q\x90\x92\x91\x90a.\xCC\x90`\x01`\x01`\xA0\x1B\x03\x16a\x0C\xBFV[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x14\x80\x15\x90a3\x86W[a3wW\x81Qa.\xFA\x90a\x0C\xBF\x90`\x01`\x01`\xA0\x1B\x03\x16\x81V[`@Qcp\xA0\x821`\xE0\x1B\x81R0`\x04\x82\x01R\x90` \x90\x82\x90`$\x90\x82\x90Z\xFA\x80\x15a\r@Wa/@\x91_\x91a3XW[Pa/9\x8A\x87\x9B\x9A\x9Ba)\xA4V[5\x90a.\x1EV[``\x84\x01\x97\x90_[\x89Q\x80Q\x82\x10\x15a/\xB2W_\x8Ba/qa/c\x85\x84\x95a(\xC3V[QQ`\x01`\x01`\xA0\x1B\x03\x16\x90V[`@a/\x8E\x86` a/\x84\x82\x87Qa(\xC3V[Q\x01Q\x94Qa(\xC3V[Q\x01Q\x91` \x83Q\x93\x01\x91Z\xF1a/\xA3a&3V[\x90\x15a\x19SWP`\x01\x01a/HV[PP\x91\x94\x97\x92\x95\x98\x90\x93\x96`\xC0\x88\x01\x95\x86Q\x80a2\xA4W[Pa\x01\0\x89\x01\x95\x86Q\x8A\x81a2\x1EW[a\x01@\x91P\x01\x9A\x8BQQ_[\x8D\x82\x82\x10a1\xC9WPPP\x91a0\x03a0\n\x92a0\x11\x95\x94a)\xA4V[5\x92a)\xA4V[5\x90a,%V[\x86Q\x90\x92\x90a0*\x90a\x0C\xBF\x90`\x01`\x01`\xA0\x1B\x03\x16\x81V[`@Qcp\xA0\x821`\xE0\x1B\x81R0`\x04\x82\x01R\x91\x90` \x90\x83\x90`$\x90\x82\x90Z\xFA\x80\x15a\r@Wa\x1A\x01\x85a\x19\xFBa\x1A\x06\x93a0l\x96_\x91a\x1C\x15WPa8kV[\x92a\x01`\x88\x01Q\x80\x85\x12a1\xB2WP\x91a1B\x82a1\xAD\x94a11\x8Ba\x1B>`@a0\xDD\x8Ea0\xD0\x7FS\xF2\x133U\x06;\x07\x87\xBE\x9Bs\xF9\xF2\xC3\xD6\xE1Fp\xE2\xA3}\xD4b6\x8B]\x1A\x94\xE8k\x06\x9F\x9E\x9D\x9B\x8B\x90a0\xCB\x84Q`\x01\x80`\xA0\x1B\x03\x16\x90V[a7OV[Q`\x01`\x01`\xA0\x1B\x03\x16\x90V[\x92\x01\x80Q\x94QQ\x9FQQ`@\x80Q0` \x82\x01\x90\x81R`\x01`\x01`\xA0\x1B\x03\x99\x8A\x16\x92\x82\x01\x92\x90\x92R\x97\x90\x94\x16``\x88\x01R`\x80\x87\x01\x95\x90\x95R`\xA0\x86\x01\x9F\x90\x9FR`\xC0\x85\x01\x93\x90\x93R\x92\x91\x82\x90`\xE0\x82\x01\x90V[Q\x90 \x97Q`\x01`\x01`\xA0\x1B\x03\x16\x90V[\x98Q`\xA0\x89\x01Q\x90\x98\x90a1m\x90`\xE0\x90`\x01`\x01`\xA0\x1B\x03\x16\x97Q\x92\x01Q`\x01`\x01`\xA0\x1B\x03\x16\x90V[\x94Q`@\x80Q\x9A\x8BR`\x01`\x01`\xA0\x1B\x03\x97\x88\x16` \x8C\x01R\x8A\x01\x91\x90\x91R``\x89\x01R`\x80\x88\x01R`\xA0\x87\x01R\x90\x82\x16\x95\x90\x91\x16\x93\x90\x81\x90`\xC0\x82\x01\x90V[\x03\x90\xA4V[c\n;\t\xA1`\xE0\x1B_R`\x04\x85\x90R`$R`D_\xFD[_\x81a1\xDAa/c\x85\x84\x95Qa(\xC3V[`@a1\xED\x86` a/\x84\x82\x87Qa(\xC3V[Q\x01Q\x91` \x83Q\x93\x01\x91Z\xF1a2\x02a&3V[\x90\x15a2\x11WP`\x01\x01a/\xE6V[a\x1C\xAE\x82\x8BQQ\x90a,%V[`\x80\x01Qa26\x90a\x0C\xBF\x90`\x01`\x01`\xA0\x1B\x03\x16\x81V[`\xE0\x8C\x01Q\x90\x91\x90`\x01`\x01`\xA0\x1B\x03\x16a\x01 \x8D\x01Q\x92\x80;\x15a\x02\xF5Wa2{\x93_\x80\x94`@Q\x96\x87\x95\x86\x94\x85\x93c\xA4\x15\xBC\xAD`\xE0\x1B\x85R0\x92`\x04\x86\x01a)\xBDV[\x03\x92Z\xF1\x80\x15a\r@Wa2\x90W[\x8Aa/\xDAV[\x80a\r4_a2\x9E\x93a\x04\x1FV[_a2\x8AV[a2\xEEa2\xE0a\x0C\xBFa\x0C\xBF\x8Da0\xD0`\xA0\x82\x01\x96`\x80a2\xCB\x89Q`\x01\x80`\xA0\x1B\x03\x16\x90V[\x93\x01\x80Q\x90\x93\x90`\x01`\x01`\xA0\x1B\x03\x16a\x1DpV[\x91Q`\x01`\x01`\xA0\x1B\x03\x16\x90V[\x88Q\x90\x82;\x15a\x02\xF5W`@Qca{\xA07`\xE0\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x91\x90\x91\x16`\x04\x82\x01R`$\x81\x01\x91\x90\x91R0`D\x82\x01R_`d\x82\x01\x81\x90R\x90\x91\x82\x90`\x84\x90\x82\x90\x84\x90Z\xF1\x80\x15a\r@W\x15a/\xCAW\x80a\r4_a3R\x93a\x04\x1FV[_a/\xCAV[a3q\x91P` =` \x11a\x1B\xF7Wa\x1B\xE9\x81\x83a\x04\x1FV[_a/+V[c\x04\x15\xB9\xDB`\xE1\x1B_R`\x04_\xFD[Pa3\x91\x88\x85a)\xA4V[5`@\x84\x01Q\x14\x15a.\xE0V[P`\x01\x84\x14\x15a.mV[P`\x01\x8A\x14\x15a.fV[c%\0\xC5%`\xE1\x1B_R`\x04_\xFD[\x90\x81`A\x02\x91`A\x83\x04\x03a, WV[\x90`A\x82\x02\x91\x80\x83\x04`A\x14\x90\x15\x17\x15a, WV[\x90\x93\x92\x93\x84\x83\x11a\x02\xF5W\x84\x11a\x02\xF5W\x81\x01\x92\x03\x90V[\x90\x91`\x05T\x91a4\x11\x83a3\xC3V[\x82\x10a5\x0BW`A\x82\x06a5\x0BW_\x93\x84\x92`A\x81\x04\x92\x91\x90\x84[\x84\x86\x10a4NWPPPPPP\x80\x82\x11\x91\x82\x15a4HWPP\x90V[\x14\x91\x90PV[a4\x80a4ya4`\x88\x9A\x97\x98a3\xD4V[a4qa4l\x8Ca3\xD4V[a,\x12V[\x90\x85\x87a3\xEAV[\x90\x86a9:V[\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x80\x15a4\xFFW`\x01`\x01`\xA0\x1B\x03\x82\x16\x80\x82\x10\x91\x82\x15a4\xF5W[PPa4\xE8W`\x01`\x01`\xA0\x1B\x03\x82\x16_\x90\x81R`\x03` R`@\x90 T`\xFF\x16\x15a4\xDDWP`\x01\x80\x91\x95\x01\x97[\x01\x94\x93a4,V[\x94\x97`\x01\x91Pa4\xD5V[PPPPPPPPP_\x90V[\x14\x90P_\x80a4\xA6V[P\x94\x97`\x01\x91Pa4\xD5V[PPPP_\x90V[_T`\x01`\x01`\xA0\x1B\x03\x163\x03a\x11KWV[h\x80\0\0\0\0\xAB\x14<\x06\\a5CW0h\x80\0\0\0\0\xAB\x14<\x06]V[c\xAB\x14<\x06_R`\x04`\x1C\xFD[_h\x80\0\0\0\0\xAB\x14<\x06]V[`\xFF`\x01T`\xA0\x1C\x16a5mWV[c\xD9<\x06e`\xE0\x1B_R`\x04_\xFD[\x90\x81` \x91\x03\x12a\x02\xF5WQa\x04\xF0\x81a\x07\x11V[\x90\x92\x91\x90`\x01`\x01`\xA0\x1B\x03\x16\x80\x15\x80\x15a69W[a\t%W`@Qc\x04\xAD\xE6\xDB`\xE1\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x93\x90\x93\x16`\x04\x84\x01R`$\x83\x01\x93\x90\x93R`D\x82\x01R\x90` \x90\x82\x90`d\x90\x82\x90_\x90Z\xF1\x90\x81\x15a\r@W_\x91a6\nW[P\x15a5\xFBWV[c1\x14o\x15`\xE0\x1B_R`\x04_\xFD[a6,\x91P` =` \x11a62W[a6$\x81\x83a\x04\x1FV[\x81\x01\x90a5|V[_a5\xF3V[P=a6\x1AV[P`\x01`\x01`\xA0\x1B\x03\x83\x16\x15a5\xA7V[`\x01`\x01`\xA0\x1B\x03\x16\x91\x82\x15\x80\x15a6\xD9W[a\t%W`@QcU\x8Ar\x97`\xE0\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x92\x90\x92\x16`\x04\x83\x01R\x15\x15`$\x82\x01R\x90` \x90\x82\x90`D\x90\x82\x90_\x90Z\xF1\x90\x81\x15a\r@W_\x91a6\xBAW[P\x15a6\xABWV[c-\xB3\x0EG`\xE1\x1B_R`\x04_\xFD[a6\xD3\x91P` =` \x11a62Wa6$\x81\x83a\x04\x1FV[_a6\xA3V[P`\x01`\x01`\xA0\x1B\x03\x82\x16\x15a6]V[\x90s\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA9`\x14R`4Rc\xA9\x05\x9C\xBB``\x1B_R` _`D`\x10\x82\x85Z\xF1\x90\x81`\x01_Q\x14\x16\x15a71W[PP_`4RV[;\x15=\x17\x10\x15a7BW_\x80a7)V[c\x90\xB8\xEC\x18_R`\x04`\x1C\xFD[\x91\x90`\x14R`4Rc\xA9\x05\x9C\xBB``\x1B_R` _`D`\x10\x82\x85Z\xF1\x90\x81`\x01_Q\x14\x16\x15a71WPP_`4RV[`\x01`\x01`\xA0\x1B\x03\x16_Q` a:\x04_9_Q\x90_R]V[__Q` a:\x04_9_Q\x90_R]V[s\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA9_Q` a:\x04_9_Q\x90_R]V[\x91\x90`\x14R\x80`4Rc\t^\xA7\xB3``\x1B_R` _`D`\x10\x82\x86Z\xF1\x80`\x01_Q\x14\x16\x15a8\x07W[PPP_`4RV[=\x83;\x15\x17\x10\x15a8\x19W[\x80a7\xFEV[_`4\x81\x90Rc\t^\xA7\xB3``\x1B\x81R8`D`\x10\x83\x86Z\xF1P`4R` _`D`\x10\x82\x85Z\xF1\x90\x81`\x01_Q\x14\x16a8\x13W;\x15=\x17\x10\x15a8^W_\x80a8\x13V[c>?\x8Fs_R`\x04`\x1C\xFD[_\x81\x12\x15a\x04\xF0Wc5'\x8D\x12_R`\x04`\x1C\xFD[\x90`\x01\x80`\xA0\x1B\x03\x82\x16_R`\x02` R`\xFF`@_ T\x16a92W`@Qc\x15w\x18E`\xE0\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x92\x83\x16`\x04\x82\x01R\x91\x16`$\x82\x01R`\x01`\x01`\xE0\x1B\x03\x19\x90\x91\x16`D\x82\x01R` \x81\x80`d\x81\x01\x03\x81\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16Z\xFA\x90\x81\x15a\r@W_\x91a9\x19WP\x90V[a\x04\xF0\x91P` =` \x11a62Wa6$\x81\x83a\x04\x1FV[PPP`\x01\x90V[\x90\x92\x91\x92`@Q\x93\x80`@\x14a9\x93W`A\x14a9cWPPPP[c\x8B\xAAW\x9F_R`\x04`\x1C\xFD[\x80`@\x80\x92\x015_\x1A` R\x817[_R` `\x01`\x80_\x82Z\xFAQ\x91_``R`@R=a\x03\xE9WPPa9VV[P` \x81\x81\x015`\xFF\x81\x90\x1C`\x1B\x01\x90\x91R\x905`@R`\x01`\x01`\xFF\x1B\x03\x16``Ra9rV[_\x92\x91`\x03\x81\x11a9\xCAWPPV[\x90\x91\x92P`\x04\x11a\x02\xF5W5`\x01`\x01`\xE0\x1B\x03\x19\x16\x90V\xFE\xAD\xC7\xF6[\xDD\xB3o\xDF\xCF4\xDBxE\xA6\xB3R\xD0\xB8.\x98\x8C\xD1\x9F\x14az\xA0\x97\x0B\xAD\xEDs\xB7+\xB2\xDB\xDB\xBE\x81\x80\x12\xAB\xB2\x1A\x93~\xC7\x0B.mh\xF4\\\x16C\xFC\xF3\xF1;`!Qy\xDF\xA1dsolcC\0\x08\"\0\n",
    );
    /// The runtime bytecode of the contract, as deployed on the network.
    ///
    /// ```text
    ///0x6080604052600436101561001a575b3615610018575f80fd5b005b5f3560e01c806301ffc9a7146102d9578063150b7a02146102d45780631626ba7e146102cf57806319822f7c146102ca578063213c5033146102c557806331cb6105146102c057806334fcd5be146102bb578063376794d8146102b65780633b303705146102b15780633f4ba83a146102ac5780634203a934146102a757806342cde4e8146102a2578063503690d11461029d5780635303ad28146102985780635589e272146102935780635c1c6dcd1461028e5780635c975abb1461028957806366579be814610284578063715018a61461027f57806379ba50971461027a5780637ca548c6146102755780637d281caa146102705780637df73e271461026b578063804a0566146102665780638456cb59146102615780638da5cb5b1461025c5780638f205a1d1461025757806394430fa514610252578063960bfe041461024d57806399fec7a014610248578063a4c01bbb14610243578063b06c944a1461023e578063bc197c8114610239578063d41e5d3f14610234578063d475c0981461022f578063e24d8c4c1461022a578063e30c397814610225578063e99f5b1614610220578063f04f27071461021b578063f23a6e6114610216578063f2fde38b14610211578063f434c9141461020c578063f6eb79c7146102075763fdb020980361000e57612234565b6121b6565b612172565b6120fc565b612068565b611fcb565b611f77565b611f4f565b611ee4565b611e48565b6117ae565b6116fd565b61156d565b61153f565b611511565b6114ac565b611482565b61135d565b611302565b6112a1565b61122b565b6111bb565b61117b565b61115e565b6110d9565b611076565b610ff1565b610fcc565b610e6a565b610da7565b610b7e565b610b18565b610acf565b610a74565b610a06565b6109d8565b610934565b61078f565b610740565b6106e3565b610620565b6105b2565b6104f3565b6102f9565b600435906001600160e01b0319821682036102f557565b5f80fd5b346102f55760203660031901126102f5576103636001600160e01b031961031e6102de565b166301ffc9a760e01b811490819082156103bc575b82156103ab575b821561039a575b8215610389575b8215610367575b505060405190151581529081906020820190565b0390f35b630271189760e51b1491508115610381575b505f8061034f565b90505f610379565b6306608bdf60e21b81149250610348565b630271189760e51b81149250610341565b630a85bd0160e11b8114925061033a565b630b135d3f60e11b81149250610333565b6001600160a01b038116036102f557565b35906103e9826103cd565b565b634e487b7160e01b5f52604160045260245ffd5b606081019081106001600160401b0382111761041a57604052565b6103eb565b90601f801991011681019081106001600160401b0382111761041a57604052565b604051906103e96101808361041f565b6001600160401b03811161041a57601f01601f191660200190565b91909161047781610450565b610484604051918261041f565b809382825282116102f55781815f9384602080950137010152565b9291926104ab82610450565b916104b9604051938461041f565b8294818452818301116102f5578281602093845f960137010152565b9080601f830112156102f5578160206104f09335910161049f565b90565b346102f55760803660031901126102f55761050f6004356103cd565b61051a6024356103cd565b6044356064356001600160401b0381116102f55761053c9036906004016104d5565b506040516001815233907fd1ec030da0f99fccad1de2774dc98277b6e7693a549f4751c319cf87379bc30c6020630a85bd0160e11b92a4604051630a85bd0160e11b8152602090f35b9181601f840112156102f5578235916001600160401b0383116102f557602083818601950101116102f557565b346102f55760403660031901126102f5576024356004356001600160401b0382116102f5576105e86105ee923690600401610585565b91613402565b1561061257630b135d3f60e11b5b6040516001600160e01b03199091168152602090f35b6001600160e01b03196105fc565b346102f55760603660031901126102f5576004356001600160401b0381116102f55761012060031982360301126102f55760243590604435906f71727de22e5e9d8baf0edac6f37da03233036106ca5760ff926105e8826101046106889401906004016122c1565b156106c2575f905b806106a3575b5060405191168152602090f35b5f808080936f71727de22e5e9d8baf0edac6f37da0325af1505f610696565b600190610690565b636b31ba1560e11b5f5260045ffd5b5f9103126102f557565b346102f5575f3660031901126102f557602060405173ba12222222228d8ba445958a75a0704d566bf2c88152f35b801515036102f557565b60409060031901126102f557600435610733816103cd565b906024356104f081610711565b346102f5576100186107513661071b565b9061075a613513565b6122f3565b9181601f840112156102f5578235916001600160401b0383116102f5576020808501948460051b0101116102f557565b60203660031901126102f5576004356001600160401b0381116102f5576107ba90369060040161075f565b6107c2613526565b6107ca613513565b6107d261355e565b5f5b8181106107e9575f688000000000ab143c065d005b6107f48183856123cf565b6001600160a01b03610805826125f4565b161561092557604081019061082361081d83836122c1565b906139bb565b906001600160e01b0319821663558a729760e01b8114908115610914575b50610905575f8061086494610855846125f4565b906020850135968791866122c1565b919061087560405180948193612bda565b03925af1610881612633565b90156108e3575060019392917fe08f8925f45c337d514b07af2526e14449bcf90afd92efd8b611f17ebf419db0916001600160a01b03906108c1906125f4565b604080519586526001600160e01b03199390931660208601521692a2016107d4565b604051635c0dee5d60e01b8152908190610901908760048401612be7565b0390fd5b631fb7cca560e01b5f5260045ffd5b63426a849360e01b1490505f610841565b635435b28960e11b5f5260045ffd5b346102f55760803660031901126102f557600435610951816103cd565b60243560027f35e259c1a781dc2649e76298b5a4d548c7905287ca3d7c4337480ce3fd05eb69606435604435610986826103cd565b61098e613526565b610996613513565b61099e61355e565b6109aa82828789613591565b6040805195865260208601919091526001600160a01b039182169590911693a45f688000000000ab143c065d005b346102f5575f3660031901126102f557602060405173794a61358d6845594f94dc1db02a252b5b4814ad8152f35b346102f5575f3660031901126102f557610a1e613513565b60015460ff8160a01c1615610a655760ff60a01b19166001556040513381527f5db9ee0a495bf2e6ff9c91a7834c1ba4fdd244a5e8aa4e537bd38aeae4b073aa90602090a1005b638dfc202b60e01b5f5260045ffd5b346102f55760203660031901126102f5576004356001600160401b0381116102f557366023820112156102f55780600401356001600160401b0381116102f55736602460c08302840101116102f557602461001892016123f6565b346102f5575f3660031901126102f5576020600554604051908152f35b60609060031901126102f557600435610b04816103cd565b90602435610b11816103cd565b9060443590565b346102f557610b2636610aec565b90610b2f613513565b6001600160a01b03811615610925576001600160a01b038316610b75575f809350809281925af1610b5e612633565b5015610b6657005b63d1d8760360e01b5f5260045ffd5b6100189261374f565b346102f55760203660031901126102f5576004356001600160401b0381116102f5578060040161018060031983360301126102f557610bbb613526565b610bc3613513565b610bcb61355e565b6001600160a01b03610bdc826125f4565b16158015610d91575b8015610d85575b8015610d6f575b6109255773ba12222222228d8ba445958a75a0704d566bf2c86001600160a01b03610c1d836125f4565b1603610d4557610c34610c2f826125f4565b613781565b60405160208101610c5782610c498584612778565b03601f19810184528361041f565b815190205f5160206139e45f395f51905f525d610ccb610cbf610cbf610c7b612894565b946044610c86612894565b97610cae610c96602483016125f4565b610c9f8a6128b6565b6001600160a01b039091169052565b0135610cb9886128b6565b526125f4565b6001600160a01b031690565b803b156102f557610cf7935f809460405196879586948593632e1c224f60e11b855230600486016128fb565b03925af18015610d4057610d26575b610d0e61379b565b5f5f5160206139e45f395f51905f525d610018613550565b80610d345f610d3a9361041f565b806106d9565b5f610d06565b612999565b610d51610d6c916125f4565b63a0aad8bb60e01b5f526001600160a01b0316600452602490565b5ffd5b50610d7f610cbf608484016125f4565b15610bf3565b50604482013515610bec565b50610da1610cbf602484016125f4565b15610be5565b346102f55760603660031901126102f5576004356001600160401b0381116102f557610dd790369060040161075f565b602435610de3816103cd565b6044356001600160401b0381116102f557610e0290369060040161075f565b91610e0b613513565b6001600160a01b038116158015610e60575b610925575f5b848110610e2c57005b80610e5a610e3d600193888a6129ad565b35610e47816103cd565b84610e538489896129ad565b359161374f565b01610e23565b5082841415610e1d565b60203660031901126102f5576004356001600160401b0381116102f5578060040190606060031982360301126102f557610ea2613526565b610eaa613513565b610eb261355e565b6001600160a01b03610ec3836125f4565b1615610925576044810190610edb61081d83856122c1565b906001600160e01b0319821663558a729760e01b8114908115610fbb575b50610905575f8091610f1c946024610f10886125f4565b920135958691886122c1565b9190610f2d60405180948193612bda565b03925af192610f3a612633565b9315610f9f577fe08f8925f45c337d514b07af2526e14449bcf90afd92efd8b611f17ebf419db091906001600160a01b0390610f75906125f4565b604080519586526001600160e01b03199390931660208601521692a25f688000000000ab143c065d005b604051635c0dee5d60e01b815280610901865f60048401612be7565b63426a849360e01b1490505f610ef9565b346102f5575f3660031901126102f557602060ff60015460a01c166040519015158152f35b346102f55760603660031901126102f55760043561100e816103cd565b60243561101a816103cd565b7f82e43140fc41dbcab6163bc2bd7ddf40d7477286fb7c95c37c1dbc957756a9ba60206044359261104a84610711565b611052613513565b61105d84828761364a565b60405193151584526001600160a01b03908116941692a3005b346102f5575f3660031901126102f55761108e613513565b600180546001600160a01b03199081169091555f80549182168155906001600160a01b03167f8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e08280a3005b346102f5575f3660031901126102f557600154336001600160a01b039091160361114b57600180546001600160a01b03199081169091555f805433928116831782556001600160a01b0316907f8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e09080a3005b63118cdaa760e01b5f523360045260245ffd5b346102f5575f3660031901126102f5576020600454604051908152f35b346102f55760203660031901126102f557600435611198816103cd565b60018060a01b03165f526002602052602060ff60405f2054166040519015158152f35b346102f55760203660031901126102f5576004356111d8816103cd565b60018060a01b03165f526003602052602060ff60405f2054166040519015158152f35b60809060031901126102f557600435611213816103cd565b90602435611220816103cd565b906044359060643590565b346102f557611239366111fb565b9092611243613513565b61124b61355e565b6001600160a01b031691823b156102f557611283925f928360405180968195829463a415bcad60e01b8452849a3092600486016129bd565b03925af18015610d4057611295575080f35b61001891505f9061041f565b346102f5575f3660031901126102f5576112b9613513565b6112c161355e565b6001805460ff60a01b1916600160a01b1790556040513381527f62e78cea01bee320cd4e420270b5ea74000d11b0c9f74754ebdbfc544b05a25890602090a1005b346102f5575f3660031901126102f5575f546040516001600160a01b039091168152602090f35b60206003198201126102f557600435906001600160401b0382116102f5576101609082900360031901126102f55760040190565b346102f55761136b36611329565b611373613526565b61137b613513565b61138361355e565b6001600160a01b03611394826125f4565b16158015611476575b8015611460575b610925575f61140591604051906113d082610c49602082019363d41e5d3f60e01b8552602483016129ed565b6113d86137ad565b815190205f5160206139e45f395f51905f525d604051809381926348c8949160e01b835260048301612b13565b03818373ba1333333333a1ba1108e8412f11850a5c319ba95af1908115610d40575f9161143e575b50515f191461092557610d0e61379b565b61145a91503d805f833e611452818361041f565b810190612ab1565b5f61142d565b50611470610cbf606083016125f4565b156113a4565b5060208101351561139d565b346102f5575f3660031901126102f55760206040516f71727de22e5e9d8baf0edac6f37da0328152f35b346102f55760203660031901126102f5576004356114c8613513565b80158015611506575b610925576020817f6e8a187d7944998085dbd1f16b84c51c903bb727536cdba86962439aded2cfd792600555604051908152a1005b5060045481116114d1565b346102f5575f3660031901126102f5576020604051736c247b1f6182318877311737bac0844baa518f5e8152f35b346102f5575f3660031901126102f557602060405173ba1333333333a1ba1108e8412f11850a5c319ba98152f35b346102f55760603660031901126102f55760043561158a816103cd565b602435906044359061159b826103cd565b6115a3613526565b6115ab613513565b6115b361355e565b6001600160a01b03169081158015611678575b61092557813b156102f557604051632142170760e11b81523060048201526001600160a01b0382166024820152604481018490525f8160648183875af18015610d40576001927f35e259c1a781dc2649e76298b5a4d548c7905287ca3d7c4337480ce3fd05eb699261165992611664575b50604051918291858060a01b0316968260205f91939293604081019481520152565b0390a4610018613550565b80610d345f6116729361041f565b5f611637565b506001600160a01b038116156115c6565b6001600160401b03811161041a5760051b60200190565b9080601f830112156102f55781356116b781611689565b926116c5604051948561041f565b81845260208085019260051b8201019283116102f557602001905b8282106116ed5750505090565b81358152602091820191016116e0565b346102f55760a03660031901126102f55760043561171a816103cd565b60243590611727826103cd565b6044356001600160401b0381116102f5576117469036906004016116a0565b906064356001600160401b0381116102f5576117669036906004016116a0565b608435926001600160401b0384116102f5576103639461178d6117939536906004016104d5565b93612b24565b6040516001600160e01b031990911681529081906020820190565b346102f5576117bc36611329565b73ba1333333333a1ba1108e8412f11850a5c319ba93303611e39575f5160206139e45f395f51905f525c8015611e2a576117f6363661046b565b6020815191012003611e1b57611811610cbf610cbf836125f4565b6040516370a0823160e01b815230600482015290602090829060249082905afa908115610d40575f91611dfc575b50611849826125f4565b9160208101359273ba1333333333a1ba1108e8412f11850a5c319ba93b156102f55760405163ae63932960e01b81526001600160a01b03919091166004820152306024820152604481018490525f816064818373ba1333333333a1ba1108e8412f11850a5c319ba95af18015610d4057611de8575b5090916040820191905f5b6118d38484612ba5565b9050811015611970575f806118fa6118f5846118ef8989612ba5565b906123cf565b6125f4565b602061190a856118ef8a8a612ba5565b013561192761191d866118ef8b8b612ba5565b60408101906122c1565b919061193860405180948193612bda565b03925af1611944612633565b901561195357506001016118c9565b604051635c0dee5d60e01b81529182916109019160048401612be7565b5092909160a08301359081611d45575b60e08401359081611cca575b61012085019361199c8587612ba5565b90505f5b818110611c345750506119b8610cbf610cbf886125f4565b6040516370a0823160e01b81523060048201529190602090839060249082905afa8015610d4057611a01896119fb611a0693611a0c965f91611c15575b5061386b565b93612c25565b61386b565b90612c32565b93610140860135808612611bfe5750611a2d87611a28886125f4565b6136ea565b611a6d602088611a3c896125f4565b6040516315afd40960e01b81526001600160a01b039091166004820152602481019190915291829081906044820190565b03815f73ba1333333333a1ba1108e8412f11850a5c319ba95af1908115610d40575f91611bcf575b50878110611bb857507f53f2133355063b0787be9b73f9f2c3d6e14670e2a37dd462368b5d1a94e86b06939291611bb391611b4c611ae8611adf611ad88b6125f4565b948b612ba5565b9390508a612ba5565b60408051306020820190815273ba1333333333a1ba1108e8412f11850a5c319ba9928201929092526001600160a01b039096166060870152608086018d905260a086019490945260c0850152509091908160e081015b03601f19810183528261041f565b51902094611b59876125f4565b97611b7260c0611b6b60808b016125f4565b99016125f4565b604080518381526001600160a01b039a8b166020820152908101969096526060860194909452608085015260a08401529085169590941693819060c0820190565b0390a4005b633e6339e360e11b5f52600488905260245260445ffd5b611bf1915060203d602011611bf7575b611be9818361041f565b810190612b96565b5f611a95565b503d611bdf565b630a3b09a160e01b5f52600486905260245260445ffd5b611c2e915060203d602011611bf757611be9818361041f565b5f6119f5565b5f80888a611c6c61191d866118ef611c536118f5836118ef8989612ba5565b956020611c64846118ef848a612ba5565b013595612ba5565b9190611c7d60405180948193612bda565b03925af1611c89612633565b9015611c9857506001016119a0565b611cae82611ca6878c612ba5565b919050612c25565b610901604051928392635c0dee5d60e01b845260048401612be7565b611cdc610cbf610cbf606088016125f4565b611ce860c087016125f4565b61010087013590823b156102f557611d1c925f928360405180968195829463a415bcad60e01b84528b3092600486016129bd565b03925af18015610d4057611d31575b5061198c565b80610d345f611d3f9361041f565b5f611d2b565b60808401611d7c611d76610cbf610cbf611d5e856125f4565b6118f58860608c0192611d70846125f4565b906137d3565b916125f4565b90803b156102f55760405163617ba03760e01b81526001600160a01b03929092166004830152602482018490523060448301525f60648301819052908290608490829084905af18015610d4057611dd4575b50611980565b80610d345f611de29361041f565b5f611dce565b80610d345f611df69361041f565b5f6118be565b611e15915060203d602011611bf757611be9818361041f565b5f61183f565b63620be62360e01b5f5260045ffd5b6378fcf80b60e11b5f5260045ffd5b638e5e503d60e01b5f5260045ffd5b346102f55760646020611e5a36610aec565b919390611e65613513565b611e6d61355e565b604051631a4ca37b60e21b81526001600160a01b03918216600482015260248101939093523060448401529193849283915f91165af18015610d4057610363915f91611ec5575b506040519081529081906020820190565b611ede915060203d602011611bf757611be9818361041f565b5f611eb4565b346102f55760207f0435416a5f48d41d5e5ede2d05c0e1ff6b1f71cb57176b1011ea7fbd8c725a83611f153661071b565b9290611f1f613513565b6001600160a01b03165f8181526002835260409020805460ff191660ff86151516179055926040519015158152a2005b346102f5575f3660031901126102f5576001546040516001600160a01b039091168152602090f35b346102f55760603660031901126102f557600435611f94816103cd565b60243590611fa1826103cd565b6044356001600160e01b0319811681036102f557602092611fc192613880565b6040519015158152f35b346102f55760803660031901126102f5576004356001600160401b0381116102f557611ffb90369060040161075f565b6024356001600160401b0381116102f55761201a90369060040161075f565b6044939193356001600160401b0381116102f55761203c90369060040161075f565b91606435956001600160401b0387116102f557612060610018973690600401610585565b969095612e2b565b346102f55760a03660031901126102f5576120846004356103cd565b61208f6024356103cd565b6044356064356084356001600160401b0381116102f5576120b49036906004016104d5565b5060405190815233907fd1ec030da0f99fccad1de2774dc98277b6e7693a549f4751c319cf87379bc30c602063f23a6e6160e01b92a460405163f23a6e6160e01b8152602090f35b346102f55760203660031901126102f557600435612119816103cd565b612121613513565b60018060a01b0316806bffffffffffffffffffffffff60a01b600154161760015560018060a01b035f54167f38d16b8cac22d99fc7c124b9cd0de2d3fa1faef420bfe791d8c362d765e227005f80a3005b346102f5575f3660031901126102f5576040517f00000000000000000000000000000000000000000000000000000000000000006001600160a01b03168152602090f35b346102f5576121c436610aec565b916121cd613513565b6121d561355e565b6121e08382846137d3565b6001600160a01b0316803b156102f55760405163617ba03760e01b81526001600160a01b03909216600483015260248201929092523060448201525f60648201819052918290829081838160848101611283565b346102f557608460205f612247366111fb565b9592612254949194613513565b61225c61355e565b6122678582856137d3565b604051968795869463573ade8160e01b865260018060a01b031660048601526024850152604484015230606484015260018060a01b03165af18015610d4057610363915f91611ec557506040519081529081906020820190565b903590601e19813603018212156102f557018035906001600160401b0382116102f5576020019181360383136102f557565b6001600160a01b03811691908215610925575f8381526003602052604090205460ff161515821515146123b6576001600160a01b03165f9081526003602052604090207ffc4acb499491cd850a8a21ab98c7f128850c0f0e5f1a875a62b7fa055c2ecf199161239d91612371908260ff801983541691151516179055565b80156123a25761238b61238660045460010190565b600455565b60405190151581529081906020820190565b0390a2565b6004546123b1905f1901600455565b61238b565b505050565b634e487b7160e01b5f52603260045260245ffd5b91908110156123f15760051b81013590605e19813603018212156102f5570190565b6123bb565b9190612400613526565b612408613513565b61241061355e565b5f5b81811061242557505090506103e9613550565b6124308183866125e4565b9060208201612441610cbf826125f4565b1580156125ce575b610925576124568361261c565b61245f816125fe565b61250b5761246f60a08401612629565b6124fc5760019261247f826125f4565b907f4a94f89e131699ed3416670c011ce64d62e5a581a4ebb4603bf6c4a5d06a06ce6124f26124d46124ce6060850135966118f560406080880135970197878a6124c88b6125f4565b92613591565b946125f4565b60405193845260a088901b8890039081169416929081906020820190565b0390a45b01612412565b63b026d5a360e01b5f5260045ffd5b6060830135158015906125c1575b6125b2576001927f9c8e17fa114d24cfc8f67c3d6ce6bc2e24067dbe41256640bc48fd6d1066562f6125aa61258861258261257c612556876125f4565b966118f5604088019860a061256a8b6125f4565b9901986125768a612629565b9161364a565b956125f4565b93612629565b604051901515815260a087901b87900393841694909316929081906020820190565b0390a36124f6565b6341f521f960e11b5f5260045ffd5b5060808301351515612519565b506125de610cbf604085016125f4565b15612449565b91908110156123f15760c0020190565b356104f0816103cd565b6002111561260857565b634e487b7160e01b5f52602160045260245ffd5b3560028110156102f55790565b356104f081610711565b3d1561265d573d9061264482610450565b91612652604051938461041f565b82523d5f602084013e565b606090565b9035601e19823603018112156102f55701602081359101916001600160401b0382116102f5578160051b360383136102f557565b908060209392818452848401375f828201840152601f01601f1916010190565b906020838281520160208260051b85010193835f915b8483106126dc5750505050505090565b909192939495601f198282030185528635605e19843603018112156102f55783018035612708816103cd565b6001600160a01b0316825260208181013590830152604081013536829003601e19018112156102f55701602081359101906001600160401b0381116102f55780360382136102f55761276a602092839260608681604060019901520191612696565b9801969501930191906126cc565b602081526127996020820161278c846103de565b6001600160a01b03169052565b6127b86127a8602084016103de565b6001600160a01b03166040830152565b6040820135606082015261018061016061288b6127ec6127db6060870187612662565b8560808801526101a08701916126b6565b61280b6127fb608088016103de565b6001600160a01b031660a0870152565b61282a61281a60a088016103de565b6001600160a01b031660c0870152565b60c086013560e086015261285461284360e088016103de565b6001600160a01b0316610100870152565b61010086013561012086015261012086013561014086015261287a610140870187612662565b868303601f190185880152906126b6565b93013591015290565b604080519091906128a5838261041f565b6001815291601f1901366020840137565b8051156123f15760200190565b80518210156123f15760209160051b010190565b805180835260209291819084018484015e5f828201840152601f01601f1916010190565b6001600160a01b03909116815260806020808301829052835191830182905260a0830196959301905f5b81811061297a5750505080850360408201526020808451968781520193015f955b8087106129625750506104f093945060608184039101526128d7565b90936020806001928751815201950196019590612946565b82516001600160a01b0316885260209788019790920191600101612925565b6040513d5f823e3d90fd5b90156123f15790565b91908110156123f15760051b0190565b6001600160a01b039182168152602081019290925260408201929092525f60608201529116608082015260a00190565b60208152612a016020820161278c846103de565b6020820135604082015261016061014061288b612a35612a246040870187612662565b8560608801526101808701916126b6565b612a54612a44606088016103de565b6001600160a01b03166080870152565b612a636127fb608088016103de565b60a086013560c0860152612a8c612a7c60c088016103de565b6001600160a01b031660e0870152565b60e086013561010086015261010086013561012086015261287a610120870187612662565b6020818303126102f5578051906001600160401b0382116102f5570181601f820112156102f557805190612ae482610450565b92612af2604051948561041f565b828452602083830101116102f557815f9260208093018386015e8301015290565b9060206104f09281815201906128d7565b50509291505f5b8351811015612b885780612b41600192866128c3565b51612b4c82856128c3565b5160405190815233907fd1ec030da0f99fccad1de2774dc98277b6e7693a549f4751c319cf87379bc30c602063bc197c8160e01b92a401612b2b565b5063bc197c8160e01b925050565b908160209103126102f5575190565b903590601e19813603018212156102f557018035906001600160401b0382116102f557602001918160051b360383136102f557565b908092918237015f815290565b6040906104f09392815281602082015201906128d7565b634e487b7160e01b5f52601160045260245ffd5b9060418201809211612c2057565b612bfe565b91908201809211612c2057565b81810392915f138015828513169184121617612c2057565b9080601f830112156102f557813591612c6283611689565b92612c70604051948561041f565b80845260208085019160051b830101918383116102f55760208101915b838310612c9c57505050505090565b82356001600160401b0381116102f5578201906060828703601f1901126102f55760405190612cca826103ff565b6020830135612cd8816103cd565b8252604083013560208301526060830135916001600160401b0383116102f557612d0a886020809695819601016104d5565b6040820152815201920191612c8d565b6020818303126102f5578035906001600160401b0382116102f5570190610180828203126102f557612d4a610440565b91612d54816103de565b8352612d62602082016103de565b60208401526040810135604084015260608101356001600160401b0381116102f55782612d90918301612c4a565b6060840152612da1608082016103de565b6080840152612db260a082016103de565b60a084015260c081013560c0840152612dcd60e082016103de565b60e08401526101008101356101008401526101208101356101208401526101408101356001600160401b0381116102f55761016092612e0d918301612c4a565b610140840152013561016082015290565b91908203918211612c2057565b92975f516020613a045f395f51905f525c97949690959294916001600160a01b0389168015611e2a5733036133b457600181148015906133a9575b801561339e575b613377575f5160206139e45f395f51905f525c612e8b36858561049f565b6020815191012003611e1b57612eaa82612eb0946118f5940190612d1a565b946129a4565b60208301805190929190612ecc906001600160a01b0316610cbf565b6001600160a01b0390911614801590613386575b613377578151612efa90610cbf906001600160a01b031681565b6040516370a0823160e01b815230600482015290602090829060249082905afa8015610d4057612f40915f91613358575b50612f398a879b9a9b6129a4565b3590612e1e565b6060840197905f5b89518051821015612fb2575f8b612f71612f638584956128c3565b51516001600160a01b031690565b6040612f8e866020612f848287516128c3565b51015194516128c3565b51015191602083519301915af1612fa3612633565b90156119535750600101612f48565b505091949792959890939660c08801958651806132a4575b5061010089019586518a8161321e575b6101409150019a8b51515f5b8d8282106131c9575050509161300361300a9261301195946129a4565b35926129a4565b3590612c25565b865190929061302a90610cbf906001600160a01b031681565b6040516370a0823160e01b81523060048201529190602090839060249082905afa8015610d4057611a01856119fb611a069361306c965f91611c15575061386b565b926101608801518085126131b2575091613142826131ad946131318b611b3e60406130dd8e6130d07f53f2133355063b0787be9b73f9f2c3d6e14670e2a37dd462368b5d1a94e86b069f9e9d9b8b906130cb845160018060a01b031690565b61374f565b516001600160a01b031690565b920180519451519f51516040805130602082019081526001600160a01b03998a1692820192909252979094166060880152608087019590955260a086019f909f5260c08501939093529291829060e0820190565b51902097516001600160a01b031690565b985160a089015190989061316d9060e0906001600160a01b031697519201516001600160a01b031690565b9451604080519a8b526001600160a01b0397881660208c01528a01919091526060890152608088015260a0870152908216959091169390819060c0820190565b0390a4565b630a3b09a160e01b5f52600485905260245260445ffd5b5f816131da612f63858495516128c3565b60406131ed866020612f848287516128c3565b51015191602083519301915af1613202612633565b90156132115750600101612fe6565b611cae828b515190612c25565b6080015161323690610cbf906001600160a01b031681565b60e08c01519091906001600160a01b03166101208d015192803b156102f55761327b935f80946040519687958694859363a415bcad60e01b85523092600486016129bd565b03925af18015610d4057613290575b8a612fda565b80610d345f61329e9361041f565b5f61328a565b6132ee6132e0610cbf610cbf8d6130d060a082019660806132cb895160018060a01b031690565b930180519093906001600160a01b0316611d70565b91516001600160a01b031690565b885190823b156102f55760405163617ba03760e01b81526001600160a01b0391909116600482015260248101919091523060448201525f6064820181905290918290608490829084905af18015610d405715612fca5780610d345f6133529361041f565b5f612fca565b613371915060203d602011611bf757611be9818361041f565b5f612f2b565b630415b9db60e11b5f5260045ffd5b5061339188856129a4565b3560408401511415612ee0565b506001841415612e6d565b5060018a1415612e66565b632500c52560e11b5f5260045ffd5b9081604102916041830403612c2057565b90604182029180830460411490151715612c2057565b909392938483116102f55784116102f5578101920390565b909160055491613411836133c3565b821061350b576041820661350b575f93849260418104929190845b84861061344e5750505050505080821191821561344857505090565b14919050565b613480613479613460889a97986133d4565b61347161346c8c6133d4565b612c12565b9085876133ea565b908661393a565b906001600160a01b03821680156134ff576001600160a01b0382168082109182156134f5575b50506134e8576001600160a01b0382165f9081526003602052604090205460ff16156134dd5750600180919501975b01949361342c565b9497600191506134d5565b5050505050505050505f90565b1490505f806134a6565b509497600191506134d5565b505050505f90565b5f546001600160a01b0316330361114b57565b688000000000ab143c065c6135435730688000000000ab143c065d565b63ab143c065f526004601cfd5b5f688000000000ab143c065d565b60ff60015460a01c1661356d57565b63d93c066560e01b5f5260045ffd5b908160209103126102f557516104f081610711565b909291906001600160a01b031680158015613639575b610925576040516304ade6db60e11b81526001600160a01b039390931660048401526024830193909352604482015290602090829060649082905f905af1908115610d40575f9161360a575b50156135fb57565b6331146f1560e01b5f5260045ffd5b61362c915060203d602011613632575b613624818361041f565b81019061357c565b5f6135f3565b503d61361a565b506001600160a01b038316156135a7565b6001600160a01b031691821580156136d9575b6109255760405163558a729760e01b81526001600160a01b039290921660048301521515602482015290602090829060449082905f905af1908115610d40575f916136ba575b50156136ab57565b632db30e4760e11b5f5260045ffd5b6136d3915060203d60201161363257613624818361041f565b5f6136a3565b506001600160a01b0382161561365d565b9073ba1333333333a1ba1108e8412f11850a5c319ba960145260345263a9059cbb60601b5f5260205f6044601082855af1908160015f51141615613731575b50505f603452565b3b153d171015613742575f80613729565b6390b8ec185f526004601cfd5b919060145260345263a9059cbb60601b5f5260205f6044601082855af1908160015f511416156137315750505f603452565b6001600160a01b03165f516020613a045f395f51905f525d565b5f5f516020613a045f395f51905f525d565b73ba1333333333a1ba1108e8412f11850a5c319ba95f516020613a045f395f51905f525d565b91906014528060345263095ea7b360601b5f5260205f6044601082865af18060015f51141615613807575b5050505f603452565b3d833b15171015613819575b806137fe565b5f603481905263095ea7b360601b8152386044601083865af15060345260205f6044601082855af1908160015f511416613813573b153d17101561385e575f80613813565b633e3f8f735f526004601cfd5b5f8112156104f0576335278d125f526004601cfd5b9060018060a01b0382165f52600260205260ff60405f20541661393257604051631577184560e01b81526001600160a01b039283166004820152911660248201526001600160e01b03199091166044820152602081806064810103817f00000000000000000000000000000000000000000000000000000000000000006001600160a01b03165afa908115610d40575f91613919575090565b6104f0915060203d60201161363257613624818361041f565b505050600190565b9092919260405193806040146139935760411461396357505050505b638baa579f5f526004601cfd5b806040809201355f1a60205281375b5f526020600160805f825afa51915f6060526040523d6103e9575050613956565b5060208181013560ff81901c601b0190915290356040526001600160ff1b0316606052613972565b5f9291600381116139ca575050565b909192506004116102f557356001600160e01b0319169056feadc7f65bddb36fdfcf34db7845a6b352d0b82e988cd19f14617aa0970baded73b72bb2dbdbbe818012abb21a937ec70b2e6d68f45c1643fcf3f13b60215179dfa164736f6c6343000822000a
    /// ```
    #[rustfmt::skip]
    #[allow(clippy::all)]
    pub static DEPLOYED_BYTECODE: alloy_sol_types::private::Bytes = alloy_sol_types::private::Bytes::from_static(
        b"`\x80`@R`\x046\x10\x15a\0\x1AW[6\x15a\0\x18W_\x80\xFD[\0[_5`\xE0\x1C\x80c\x01\xFF\xC9\xA7\x14a\x02\xD9W\x80c\x15\x0Bz\x02\x14a\x02\xD4W\x80c\x16&\xBA~\x14a\x02\xCFW\x80c\x19\x82/|\x14a\x02\xCAW\x80c!<P3\x14a\x02\xC5W\x80c1\xCBa\x05\x14a\x02\xC0W\x80c4\xFC\xD5\xBE\x14a\x02\xBBW\x80c7g\x94\xD8\x14a\x02\xB6W\x80c;07\x05\x14a\x02\xB1W\x80c?K\xA8:\x14a\x02\xACW\x80cB\x03\xA94\x14a\x02\xA7W\x80cB\xCD\xE4\xE8\x14a\x02\xA2W\x80cP6\x90\xD1\x14a\x02\x9DW\x80cS\x03\xAD(\x14a\x02\x98W\x80cU\x89\xE2r\x14a\x02\x93W\x80c\\\x1Cm\xCD\x14a\x02\x8EW\x80c\\\x97Z\xBB\x14a\x02\x89W\x80cfW\x9B\xE8\x14a\x02\x84W\x80cqP\x18\xA6\x14a\x02\x7FW\x80cy\xBAP\x97\x14a\x02zW\x80c|\xA5H\xC6\x14a\x02uW\x80c}(\x1C\xAA\x14a\x02pW\x80c}\xF7>'\x14a\x02kW\x80c\x80J\x05f\x14a\x02fW\x80c\x84V\xCBY\x14a\x02aW\x80c\x8D\xA5\xCB[\x14a\x02\\W\x80c\x8F Z\x1D\x14a\x02WW\x80c\x94C\x0F\xA5\x14a\x02RW\x80c\x96\x0B\xFE\x04\x14a\x02MW\x80c\x99\xFE\xC7\xA0\x14a\x02HW\x80c\xA4\xC0\x1B\xBB\x14a\x02CW\x80c\xB0l\x94J\x14a\x02>W\x80c\xBC\x19|\x81\x14a\x029W\x80c\xD4\x1E]?\x14a\x024W\x80c\xD4u\xC0\x98\x14a\x02/W\x80c\xE2M\x8CL\x14a\x02*W\x80c\xE3\x0C9x\x14a\x02%W\x80c\xE9\x9F[\x16\x14a\x02 W\x80c\xF0O'\x07\x14a\x02\x1BW\x80c\xF2:na\x14a\x02\x16W\x80c\xF2\xFD\xE3\x8B\x14a\x02\x11W\x80c\xF44\xC9\x14\x14a\x02\x0CW\x80c\xF6\xEBy\xC7\x14a\x02\x07Wc\xFD\xB0 \x98\x03a\0\x0EWa\"4V[a!\xB6V[a!rV[a \xFCV[a hV[a\x1F\xCBV[a\x1FwV[a\x1FOV[a\x1E\xE4V[a\x1EHV[a\x17\xAEV[a\x16\xFDV[a\x15mV[a\x15?V[a\x15\x11V[a\x14\xACV[a\x14\x82V[a\x13]V[a\x13\x02V[a\x12\xA1V[a\x12+V[a\x11\xBBV[a\x11{V[a\x11^V[a\x10\xD9V[a\x10vV[a\x0F\xF1V[a\x0F\xCCV[a\x0EjV[a\r\xA7V[a\x0B~V[a\x0B\x18V[a\n\xCFV[a\ntV[a\n\x06V[a\t\xD8V[a\t4V[a\x07\x8FV[a\x07@V[a\x06\xE3V[a\x06 V[a\x05\xB2V[a\x04\xF3V[a\x02\xF9V[`\x045\x90`\x01`\x01`\xE0\x1B\x03\x19\x82\x16\x82\x03a\x02\xF5WV[_\x80\xFD[4a\x02\xF5W` 6`\x03\x19\x01\x12a\x02\xF5Wa\x03c`\x01`\x01`\xE0\x1B\x03\x19a\x03\x1Ea\x02\xDEV[\x16c\x01\xFF\xC9\xA7`\xE0\x1B\x81\x14\x90\x81\x90\x82\x15a\x03\xBCW[\x82\x15a\x03\xABW[\x82\x15a\x03\x9AW[\x82\x15a\x03\x89W[\x82\x15a\x03gW[PP`@Q\x90\x15\x15\x81R\x90\x81\x90` \x82\x01\x90V[\x03\x90\xF3[c\x02q\x18\x97`\xE5\x1B\x14\x91P\x81\x15a\x03\x81W[P_\x80a\x03OV[\x90P_a\x03yV[c\x06`\x8B\xDF`\xE2\x1B\x81\x14\x92Pa\x03HV[c\x02q\x18\x97`\xE5\x1B\x81\x14\x92Pa\x03AV[c\n\x85\xBD\x01`\xE1\x1B\x81\x14\x92Pa\x03:V[c\x0B\x13]?`\xE1\x1B\x81\x14\x92Pa\x033V[`\x01`\x01`\xA0\x1B\x03\x81\x16\x03a\x02\xF5WV[5\x90a\x03\xE9\x82a\x03\xCDV[V[cNH{q`\xE0\x1B_R`A`\x04R`$_\xFD[``\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x04\x1AW`@RV[a\x03\xEBV[\x90`\x1F\x80\x19\x91\x01\x16\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x04\x1AW`@RV[`@Q\x90a\x03\xE9a\x01\x80\x83a\x04\x1FV[`\x01`\x01`@\x1B\x03\x81\x11a\x04\x1AW`\x1F\x01`\x1F\x19\x16` \x01\x90V[\x91\x90\x91a\x04w\x81a\x04PV[a\x04\x84`@Q\x91\x82a\x04\x1FV[\x80\x93\x82\x82R\x82\x11a\x02\xF5W\x81\x81_\x93\x84` \x80\x95\x017\x01\x01RV[\x92\x91\x92a\x04\xAB\x82a\x04PV[\x91a\x04\xB9`@Q\x93\x84a\x04\x1FV[\x82\x94\x81\x84R\x81\x83\x01\x11a\x02\xF5W\x82\x81` \x93\x84_\x96\x017\x01\x01RV[\x90\x80`\x1F\x83\x01\x12\x15a\x02\xF5W\x81` a\x04\xF0\x935\x91\x01a\x04\x9FV[\x90V[4a\x02\xF5W`\x806`\x03\x19\x01\x12a\x02\xF5Wa\x05\x0F`\x045a\x03\xCDV[a\x05\x1A`$5a\x03\xCDV[`D5`d5`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\x05<\x906\x90`\x04\x01a\x04\xD5V[P`@Q`\x01\x81R3\x90\x7F\xD1\xEC\x03\r\xA0\xF9\x9F\xCC\xAD\x1D\xE2wM\xC9\x82w\xB6\xE7i:T\x9FGQ\xC3\x19\xCF\x877\x9B\xC3\x0C` c\n\x85\xBD\x01`\xE1\x1B\x92\xA4`@Qc\n\x85\xBD\x01`\xE1\x1B\x81R` \x90\xF3[\x91\x81`\x1F\x84\x01\x12\x15a\x02\xF5W\x825\x91`\x01`\x01`@\x1B\x03\x83\x11a\x02\xF5W` \x83\x81\x86\x01\x95\x01\x01\x11a\x02\xF5WV[4a\x02\xF5W`@6`\x03\x19\x01\x12a\x02\xF5W`$5`\x045`\x01`\x01`@\x1B\x03\x82\x11a\x02\xF5Wa\x05\xE8a\x05\xEE\x926\x90`\x04\x01a\x05\x85V[\x91a4\x02V[\x15a\x06\x12Wc\x0B\x13]?`\xE1\x1B[`@Q`\x01`\x01`\xE0\x1B\x03\x19\x90\x91\x16\x81R` \x90\xF3[`\x01`\x01`\xE0\x1B\x03\x19a\x05\xFCV[4a\x02\xF5W``6`\x03\x19\x01\x12a\x02\xF5W`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\x01 `\x03\x19\x826\x03\x01\x12a\x02\xF5W`$5\x90`D5\x90oqr}\xE2.^\x9D\x8B\xAF\x0E\xDA\xC6\xF3}\xA023\x03a\x06\xCAW`\xFF\x92a\x05\xE8\x82a\x01\x04a\x06\x88\x94\x01\x90`\x04\x01a\"\xC1V[\x15a\x06\xC2W_\x90[\x80a\x06\xA3W[P`@Q\x91\x16\x81R` \x90\xF3[_\x80\x80\x80\x93oqr}\xE2.^\x9D\x8B\xAF\x0E\xDA\xC6\xF3}\xA02Z\xF1P_a\x06\x96V[`\x01\x90a\x06\x90V[ck1\xBA\x15`\xE1\x1B_R`\x04_\xFD[_\x91\x03\x12a\x02\xF5WV[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W` `@Qs\xBA\x12\"\"\"\"\x8D\x8B\xA4E\x95\x8Au\xA0pMVk\xF2\xC8\x81R\xF3[\x80\x15\x15\x03a\x02\xF5WV[`@\x90`\x03\x19\x01\x12a\x02\xF5W`\x045a\x073\x81a\x03\xCDV[\x90`$5a\x04\xF0\x81a\x07\x11V[4a\x02\xF5Wa\0\x18a\x07Q6a\x07\x1BV[\x90a\x07Za5\x13V[a\"\xF3V[\x91\x81`\x1F\x84\x01\x12\x15a\x02\xF5W\x825\x91`\x01`\x01`@\x1B\x03\x83\x11a\x02\xF5W` \x80\x85\x01\x94\x84`\x05\x1B\x01\x01\x11a\x02\xF5WV[` 6`\x03\x19\x01\x12a\x02\xF5W`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\x07\xBA\x906\x90`\x04\x01a\x07_V[a\x07\xC2a5&V[a\x07\xCAa5\x13V[a\x07\xD2a5^V[_[\x81\x81\x10a\x07\xE9W_h\x80\0\0\0\0\xAB\x14<\x06]\0[a\x07\xF4\x81\x83\x85a#\xCFV[`\x01`\x01`\xA0\x1B\x03a\x08\x05\x82a%\xF4V[\x16\x15a\t%W`@\x81\x01\x90a\x08#a\x08\x1D\x83\x83a\"\xC1V[\x90a9\xBBV[\x90`\x01`\x01`\xE0\x1B\x03\x19\x82\x16cU\x8Ar\x97`\xE0\x1B\x81\x14\x90\x81\x15a\t\x14W[Pa\t\x05W_\x80a\x08d\x94a\x08U\x84a%\xF4V[\x90` \x85\x015\x96\x87\x91\x86a\"\xC1V[\x91\x90a\x08u`@Q\x80\x94\x81\x93a+\xDAV[\x03\x92Z\xF1a\x08\x81a&3V[\x90\x15a\x08\xE3WP`\x01\x93\x92\x91\x7F\xE0\x8F\x89%\xF4\\3}QK\x07\xAF%&\xE1DI\xBC\xF9\n\xFD\x92\xEF\xD8\xB6\x11\xF1~\xBFA\x9D\xB0\x91`\x01`\x01`\xA0\x1B\x03\x90a\x08\xC1\x90a%\xF4V[`@\x80Q\x95\x86R`\x01`\x01`\xE0\x1B\x03\x19\x93\x90\x93\x16` \x86\x01R\x16\x92\xA2\x01a\x07\xD4V[`@Qc\\\r\xEE]`\xE0\x1B\x81R\x90\x81\x90a\t\x01\x90\x87`\x04\x84\x01a+\xE7V[\x03\x90\xFD[c\x1F\xB7\xCC\xA5`\xE0\x1B_R`\x04_\xFD[cBj\x84\x93`\xE0\x1B\x14\x90P_a\x08AV[cT5\xB2\x89`\xE1\x1B_R`\x04_\xFD[4a\x02\xF5W`\x806`\x03\x19\x01\x12a\x02\xF5W`\x045a\tQ\x81a\x03\xCDV[`$5`\x02\x7F5\xE2Y\xC1\xA7\x81\xDC&I\xE7b\x98\xB5\xA4\xD5H\xC7\x90R\x87\xCA=|C7H\x0C\xE3\xFD\x05\xEBi`d5`D5a\t\x86\x82a\x03\xCDV[a\t\x8Ea5&V[a\t\x96a5\x13V[a\t\x9Ea5^V[a\t\xAA\x82\x82\x87\x89a5\x91V[`@\x80Q\x95\x86R` \x86\x01\x91\x90\x91R`\x01`\x01`\xA0\x1B\x03\x91\x82\x16\x95\x90\x91\x16\x93\xA4_h\x80\0\0\0\0\xAB\x14<\x06]\0[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W` `@QsyJa5\x8DhEYO\x94\xDC\x1D\xB0*%+[H\x14\xAD\x81R\xF3[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5Wa\n\x1Ea5\x13V[`\x01T`\xFF\x81`\xA0\x1C\x16\x15a\neW`\xFF`\xA0\x1B\x19\x16`\x01U`@Q3\x81R\x7F]\xB9\xEE\nI[\xF2\xE6\xFF\x9C\x91\xA7\x83L\x1B\xA4\xFD\xD2D\xA5\xE8\xAANS{\xD3\x8A\xEA\xE4\xB0s\xAA\x90` \x90\xA1\0[c\x8D\xFC +`\xE0\x1B_R`\x04_\xFD[4a\x02\xF5W` 6`\x03\x19\x01\x12a\x02\xF5W`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5W6`#\x82\x01\x12\x15a\x02\xF5W\x80`\x04\x015`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5W6`$`\xC0\x83\x02\x84\x01\x01\x11a\x02\xF5W`$a\0\x18\x92\x01a#\xF6V[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W` `\x05T`@Q\x90\x81R\xF3[``\x90`\x03\x19\x01\x12a\x02\xF5W`\x045a\x0B\x04\x81a\x03\xCDV[\x90`$5a\x0B\x11\x81a\x03\xCDV[\x90`D5\x90V[4a\x02\xF5Wa\x0B&6a\n\xECV[\x90a\x0B/a5\x13V[`\x01`\x01`\xA0\x1B\x03\x81\x16\x15a\t%W`\x01`\x01`\xA0\x1B\x03\x83\x16a\x0BuW_\x80\x93P\x80\x92\x81\x92Z\xF1a\x0B^a&3V[P\x15a\x0BfW\0[c\xD1\xD8v\x03`\xE0\x1B_R`\x04_\xFD[a\0\x18\x92a7OV[4a\x02\xF5W` 6`\x03\x19\x01\x12a\x02\xF5W`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5W\x80`\x04\x01a\x01\x80`\x03\x19\x836\x03\x01\x12a\x02\xF5Wa\x0B\xBBa5&V[a\x0B\xC3a5\x13V[a\x0B\xCBa5^V[`\x01`\x01`\xA0\x1B\x03a\x0B\xDC\x82a%\xF4V[\x16\x15\x80\x15a\r\x91W[\x80\x15a\r\x85W[\x80\x15a\roW[a\t%Ws\xBA\x12\"\"\"\"\x8D\x8B\xA4E\x95\x8Au\xA0pMVk\xF2\xC8`\x01`\x01`\xA0\x1B\x03a\x0C\x1D\x83a%\xF4V[\x16\x03a\rEWa\x0C4a\x0C/\x82a%\xF4V[a7\x81V[`@Q` \x81\x01a\x0CW\x82a\x0CI\x85\x84a'xV[\x03`\x1F\x19\x81\x01\x84R\x83a\x04\x1FV[\x81Q\x90 _Q` a9\xE4_9_Q\x90_R]a\x0C\xCBa\x0C\xBFa\x0C\xBFa\x0C{a(\x94V[\x94`Da\x0C\x86a(\x94V[\x97a\x0C\xAEa\x0C\x96`$\x83\x01a%\xF4V[a\x0C\x9F\x8Aa(\xB6V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x90RV[\x015a\x0C\xB9\x88a(\xB6V[Ra%\xF4V[`\x01`\x01`\xA0\x1B\x03\x16\x90V[\x80;\x15a\x02\xF5Wa\x0C\xF7\x93_\x80\x94`@Q\x96\x87\x95\x86\x94\x85\x93c.\x1C\"O`\xE1\x1B\x85R0`\x04\x86\x01a(\xFBV[\x03\x92Z\xF1\x80\x15a\r@Wa\r&W[a\r\x0Ea7\x9BV[__Q` a9\xE4_9_Q\x90_R]a\0\x18a5PV[\x80a\r4_a\r:\x93a\x04\x1FV[\x80a\x06\xD9V[_a\r\x06V[a)\x99V[a\rQa\rl\x91a%\xF4V[c\xA0\xAA\xD8\xBB`\xE0\x1B_R`\x01`\x01`\xA0\x1B\x03\x16`\x04R`$\x90V[_\xFD[Pa\r\x7Fa\x0C\xBF`\x84\x84\x01a%\xF4V[\x15a\x0B\xF3V[P`D\x82\x015\x15a\x0B\xECV[Pa\r\xA1a\x0C\xBF`$\x84\x01a%\xF4V[\x15a\x0B\xE5V[4a\x02\xF5W``6`\x03\x19\x01\x12a\x02\xF5W`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\r\xD7\x906\x90`\x04\x01a\x07_V[`$5a\r\xE3\x81a\x03\xCDV[`D5`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\x0E\x02\x906\x90`\x04\x01a\x07_V[\x91a\x0E\x0Ba5\x13V[`\x01`\x01`\xA0\x1B\x03\x81\x16\x15\x80\x15a\x0E`W[a\t%W_[\x84\x81\x10a\x0E,W\0[\x80a\x0EZa\x0E=`\x01\x93\x88\x8Aa)\xADV[5a\x0EG\x81a\x03\xCDV[\x84a\x0ES\x84\x89\x89a)\xADV[5\x91a7OV[\x01a\x0E#V[P\x82\x84\x14\x15a\x0E\x1DV[` 6`\x03\x19\x01\x12a\x02\xF5W`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5W\x80`\x04\x01\x90```\x03\x19\x826\x03\x01\x12a\x02\xF5Wa\x0E\xA2a5&V[a\x0E\xAAa5\x13V[a\x0E\xB2a5^V[`\x01`\x01`\xA0\x1B\x03a\x0E\xC3\x83a%\xF4V[\x16\x15a\t%W`D\x81\x01\x90a\x0E\xDBa\x08\x1D\x83\x85a\"\xC1V[\x90`\x01`\x01`\xE0\x1B\x03\x19\x82\x16cU\x8Ar\x97`\xE0\x1B\x81\x14\x90\x81\x15a\x0F\xBBW[Pa\t\x05W_\x80\x91a\x0F\x1C\x94`$a\x0F\x10\x88a%\xF4V[\x92\x015\x95\x86\x91\x88a\"\xC1V[\x91\x90a\x0F-`@Q\x80\x94\x81\x93a+\xDAV[\x03\x92Z\xF1\x92a\x0F:a&3V[\x93\x15a\x0F\x9FW\x7F\xE0\x8F\x89%\xF4\\3}QK\x07\xAF%&\xE1DI\xBC\xF9\n\xFD\x92\xEF\xD8\xB6\x11\xF1~\xBFA\x9D\xB0\x91\x90`\x01`\x01`\xA0\x1B\x03\x90a\x0Fu\x90a%\xF4V[`@\x80Q\x95\x86R`\x01`\x01`\xE0\x1B\x03\x19\x93\x90\x93\x16` \x86\x01R\x16\x92\xA2_h\x80\0\0\0\0\xAB\x14<\x06]\0[`@Qc\\\r\xEE]`\xE0\x1B\x81R\x80a\t\x01\x86_`\x04\x84\x01a+\xE7V[cBj\x84\x93`\xE0\x1B\x14\x90P_a\x0E\xF9V[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W` `\xFF`\x01T`\xA0\x1C\x16`@Q\x90\x15\x15\x81R\xF3[4a\x02\xF5W``6`\x03\x19\x01\x12a\x02\xF5W`\x045a\x10\x0E\x81a\x03\xCDV[`$5a\x10\x1A\x81a\x03\xCDV[\x7F\x82\xE41@\xFCA\xDB\xCA\xB6\x16;\xC2\xBD}\xDF@\xD7Gr\x86\xFB|\x95\xC3|\x1D\xBC\x95wV\xA9\xBA` `D5\x92a\x10J\x84a\x07\x11V[a\x10Ra5\x13V[a\x10]\x84\x82\x87a6JV[`@Q\x93\x15\x15\x84R`\x01`\x01`\xA0\x1B\x03\x90\x81\x16\x94\x16\x92\xA3\0[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5Wa\x10\x8Ea5\x13V[`\x01\x80T`\x01`\x01`\xA0\x1B\x03\x19\x90\x81\x16\x90\x91U_\x80T\x91\x82\x16\x81U\x90`\x01`\x01`\xA0\x1B\x03\x16\x7F\x8B\xE0\x07\x9CS\x16Y\x14\x13D\xCD\x1F\xD0\xA4\xF2\x84\x19I\x7F\x97\"\xA3\xDA\xAF\xE3\xB4\x18okdW\xE0\x82\x80\xA3\0[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W`\x01T3`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x03a\x11KW`\x01\x80T`\x01`\x01`\xA0\x1B\x03\x19\x90\x81\x16\x90\x91U_\x80T3\x92\x81\x16\x83\x17\x82U`\x01`\x01`\xA0\x1B\x03\x16\x90\x7F\x8B\xE0\x07\x9CS\x16Y\x14\x13D\xCD\x1F\xD0\xA4\xF2\x84\x19I\x7F\x97\"\xA3\xDA\xAF\xE3\xB4\x18okdW\xE0\x90\x80\xA3\0[c\x11\x8C\xDA\xA7`\xE0\x1B_R3`\x04R`$_\xFD[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W` `\x04T`@Q\x90\x81R\xF3[4a\x02\xF5W` 6`\x03\x19\x01\x12a\x02\xF5W`\x045a\x11\x98\x81a\x03\xCDV[`\x01\x80`\xA0\x1B\x03\x16_R`\x02` R` `\xFF`@_ T\x16`@Q\x90\x15\x15\x81R\xF3[4a\x02\xF5W` 6`\x03\x19\x01\x12a\x02\xF5W`\x045a\x11\xD8\x81a\x03\xCDV[`\x01\x80`\xA0\x1B\x03\x16_R`\x03` R` `\xFF`@_ T\x16`@Q\x90\x15\x15\x81R\xF3[`\x80\x90`\x03\x19\x01\x12a\x02\xF5W`\x045a\x12\x13\x81a\x03\xCDV[\x90`$5a\x12 \x81a\x03\xCDV[\x90`D5\x90`d5\x90V[4a\x02\xF5Wa\x1296a\x11\xFBV[\x90\x92a\x12Ca5\x13V[a\x12Ka5^V[`\x01`\x01`\xA0\x1B\x03\x16\x91\x82;\x15a\x02\xF5Wa\x12\x83\x92_\x92\x83`@Q\x80\x96\x81\x95\x82\x94c\xA4\x15\xBC\xAD`\xE0\x1B\x84R\x84\x9A0\x92`\x04\x86\x01a)\xBDV[\x03\x92Z\xF1\x80\x15a\r@Wa\x12\x95WP\x80\xF3[a\0\x18\x91P_\x90a\x04\x1FV[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5Wa\x12\xB9a5\x13V[a\x12\xC1a5^V[`\x01\x80T`\xFF`\xA0\x1B\x19\x16`\x01`\xA0\x1B\x17\x90U`@Q3\x81R\x7Fb\xE7\x8C\xEA\x01\xBE\xE3 \xCDNB\x02p\xB5\xEAt\0\r\x11\xB0\xC9\xF7GT\xEB\xDB\xFCTK\x05\xA2X\x90` \x90\xA1\0[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W_T`@Q`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x81R` \x90\xF3[` `\x03\x19\x82\x01\x12a\x02\xF5W`\x045\x90`\x01`\x01`@\x1B\x03\x82\x11a\x02\xF5Wa\x01`\x90\x82\x90\x03`\x03\x19\x01\x12a\x02\xF5W`\x04\x01\x90V[4a\x02\xF5Wa\x13k6a\x13)V[a\x13sa5&V[a\x13{a5\x13V[a\x13\x83a5^V[`\x01`\x01`\xA0\x1B\x03a\x13\x94\x82a%\xF4V[\x16\x15\x80\x15a\x14vW[\x80\x15a\x14`W[a\t%W_a\x14\x05\x91`@Q\x90a\x13\xD0\x82a\x0CI` \x82\x01\x93c\xD4\x1E]?`\xE0\x1B\x85R`$\x83\x01a)\xEDV[a\x13\xD8a7\xADV[\x81Q\x90 _Q` a9\xE4_9_Q\x90_R]`@Q\x80\x93\x81\x92cH\xC8\x94\x91`\xE0\x1B\x83R`\x04\x83\x01a+\x13V[\x03\x81\x83s\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA9Z\xF1\x90\x81\x15a\r@W_\x91a\x14>W[PQ_\x19\x14a\t%Wa\r\x0Ea7\x9BV[a\x14Z\x91P=\x80_\x83>a\x14R\x81\x83a\x04\x1FV[\x81\x01\x90a*\xB1V[_a\x14-V[Pa\x14pa\x0C\xBF``\x83\x01a%\xF4V[\x15a\x13\xA4V[P` \x81\x015\x15a\x13\x9DV[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W` `@Qoqr}\xE2.^\x9D\x8B\xAF\x0E\xDA\xC6\xF3}\xA02\x81R\xF3[4a\x02\xF5W` 6`\x03\x19\x01\x12a\x02\xF5W`\x045a\x14\xC8a5\x13V[\x80\x15\x80\x15a\x15\x06W[a\t%W` \x81\x7Fn\x8A\x18}yD\x99\x80\x85\xDB\xD1\xF1k\x84\xC5\x1C\x90;\xB7'Sl\xDB\xA8ibC\x9A\xDE\xD2\xCF\xD7\x92`\x05U`@Q\x90\x81R\xA1\0[P`\x04T\x81\x11a\x14\xD1V[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W` `@Qsl${\x1Fa\x821\x88w1\x177\xBA\xC0\x84K\xAAQ\x8F^\x81R\xF3[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W` `@Qs\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA9\x81R\xF3[4a\x02\xF5W``6`\x03\x19\x01\x12a\x02\xF5W`\x045a\x15\x8A\x81a\x03\xCDV[`$5\x90`D5\x90a\x15\x9B\x82a\x03\xCDV[a\x15\xA3a5&V[a\x15\xABa5\x13V[a\x15\xB3a5^V[`\x01`\x01`\xA0\x1B\x03\x16\x90\x81\x15\x80\x15a\x16xW[a\t%W\x81;\x15a\x02\xF5W`@Qc!B\x17\x07`\xE1\x1B\x81R0`\x04\x82\x01R`\x01`\x01`\xA0\x1B\x03\x82\x16`$\x82\x01R`D\x81\x01\x84\x90R_\x81`d\x81\x83\x87Z\xF1\x80\x15a\r@W`\x01\x92\x7F5\xE2Y\xC1\xA7\x81\xDC&I\xE7b\x98\xB5\xA4\xD5H\xC7\x90R\x87\xCA=|C7H\x0C\xE3\xFD\x05\xEBi\x92a\x16Y\x92a\x16dW[P`@Q\x91\x82\x91\x85\x80`\xA0\x1B\x03\x16\x96\x82` _\x91\x93\x92\x93`@\x81\x01\x94\x81R\x01RV[\x03\x90\xA4a\0\x18a5PV[\x80a\r4_a\x16r\x93a\x04\x1FV[_a\x167V[P`\x01`\x01`\xA0\x1B\x03\x81\x16\x15a\x15\xC6V[`\x01`\x01`@\x1B\x03\x81\x11a\x04\x1AW`\x05\x1B` \x01\x90V[\x90\x80`\x1F\x83\x01\x12\x15a\x02\xF5W\x815a\x16\xB7\x81a\x16\x89V[\x92a\x16\xC5`@Q\x94\x85a\x04\x1FV[\x81\x84R` \x80\x85\x01\x92`\x05\x1B\x82\x01\x01\x92\x83\x11a\x02\xF5W` \x01\x90[\x82\x82\x10a\x16\xEDWPPP\x90V[\x815\x81R` \x91\x82\x01\x91\x01a\x16\xE0V[4a\x02\xF5W`\xA06`\x03\x19\x01\x12a\x02\xF5W`\x045a\x17\x1A\x81a\x03\xCDV[`$5\x90a\x17'\x82a\x03\xCDV[`D5`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\x17F\x906\x90`\x04\x01a\x16\xA0V[\x90`d5`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\x17f\x906\x90`\x04\x01a\x16\xA0V[`\x845\x92`\x01`\x01`@\x1B\x03\x84\x11a\x02\xF5Wa\x03c\x94a\x17\x8Da\x17\x93\x956\x90`\x04\x01a\x04\xD5V[\x93a+$V[`@Q`\x01`\x01`\xE0\x1B\x03\x19\x90\x91\x16\x81R\x90\x81\x90` \x82\x01\x90V[4a\x02\xF5Wa\x17\xBC6a\x13)V[s\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA93\x03a\x1E9W_Q` a9\xE4_9_Q\x90_R\\\x80\x15a\x1E*Wa\x17\xF666a\x04kV[` \x81Q\x91\x01 \x03a\x1E\x1BWa\x18\x11a\x0C\xBFa\x0C\xBF\x83a%\xF4V[`@Qcp\xA0\x821`\xE0\x1B\x81R0`\x04\x82\x01R\x90` \x90\x82\x90`$\x90\x82\x90Z\xFA\x90\x81\x15a\r@W_\x91a\x1D\xFCW[Pa\x18I\x82a%\xF4V[\x91` \x81\x015\x92s\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA9;\x15a\x02\xF5W`@Qc\xAEc\x93)`\xE0\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x91\x90\x91\x16`\x04\x82\x01R0`$\x82\x01R`D\x81\x01\x84\x90R_\x81`d\x81\x83s\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA9Z\xF1\x80\x15a\r@Wa\x1D\xE8W[P\x90\x91`@\x82\x01\x91\x90_[a\x18\xD3\x84\x84a+\xA5V[\x90P\x81\x10\x15a\x19pW_\x80a\x18\xFAa\x18\xF5\x84a\x18\xEF\x89\x89a+\xA5V[\x90a#\xCFV[a%\xF4V[` a\x19\n\x85a\x18\xEF\x8A\x8Aa+\xA5V[\x015a\x19'a\x19\x1D\x86a\x18\xEF\x8B\x8Ba+\xA5V[`@\x81\x01\x90a\"\xC1V[\x91\x90a\x198`@Q\x80\x94\x81\x93a+\xDAV[\x03\x92Z\xF1a\x19Da&3V[\x90\x15a\x19SWP`\x01\x01a\x18\xC9V[`@Qc\\\r\xEE]`\xE0\x1B\x81R\x91\x82\x91a\t\x01\x91`\x04\x84\x01a+\xE7V[P\x92\x90\x91`\xA0\x83\x015\x90\x81a\x1DEW[`\xE0\x84\x015\x90\x81a\x1C\xCAW[a\x01 \x85\x01\x93a\x19\x9C\x85\x87a+\xA5V[\x90P_[\x81\x81\x10a\x1C4WPPa\x19\xB8a\x0C\xBFa\x0C\xBF\x88a%\xF4V[`@Qcp\xA0\x821`\xE0\x1B\x81R0`\x04\x82\x01R\x91\x90` \x90\x83\x90`$\x90\x82\x90Z\xFA\x80\x15a\r@Wa\x1A\x01\x89a\x19\xFBa\x1A\x06\x93a\x1A\x0C\x96_\x91a\x1C\x15W[Pa8kV[\x93a,%V[a8kV[\x90a,2V[\x93a\x01@\x86\x015\x80\x86\x12a\x1B\xFEWPa\x1A-\x87a\x1A(\x88a%\xF4V[a6\xEAV[a\x1Am` \x88a\x1A<\x89a%\xF4V[`@Qc\x15\xAF\xD4\t`\xE0\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x90\x91\x16`\x04\x82\x01R`$\x81\x01\x91\x90\x91R\x91\x82\x90\x81\x90`D\x82\x01\x90V[\x03\x81_s\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA9Z\xF1\x90\x81\x15a\r@W_\x91a\x1B\xCFW[P\x87\x81\x10a\x1B\xB8WP\x7FS\xF2\x133U\x06;\x07\x87\xBE\x9Bs\xF9\xF2\xC3\xD6\xE1Fp\xE2\xA3}\xD4b6\x8B]\x1A\x94\xE8k\x06\x93\x92\x91a\x1B\xB3\x91a\x1BLa\x1A\xE8a\x1A\xDFa\x1A\xD8\x8Ba%\xF4V[\x94\x8Ba+\xA5V[\x93\x90P\x8Aa+\xA5V[`@\x80Q0` \x82\x01\x90\x81Rs\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA9\x92\x82\x01\x92\x90\x92R`\x01`\x01`\xA0\x1B\x03\x90\x96\x16``\x87\x01R`\x80\x86\x01\x8D\x90R`\xA0\x86\x01\x94\x90\x94R`\xC0\x85\x01RP\x90\x91\x90\x81`\xE0\x81\x01[\x03`\x1F\x19\x81\x01\x83R\x82a\x04\x1FV[Q\x90 \x94a\x1BY\x87a%\xF4V[\x97a\x1Br`\xC0a\x1Bk`\x80\x8B\x01a%\xF4V[\x99\x01a%\xF4V[`@\x80Q\x83\x81R`\x01`\x01`\xA0\x1B\x03\x9A\x8B\x16` \x82\x01R\x90\x81\x01\x96\x90\x96R``\x86\x01\x94\x90\x94R`\x80\x85\x01R`\xA0\x84\x01R\x90\x85\x16\x95\x90\x94\x16\x93\x81\x90`\xC0\x82\x01\x90V[\x03\x90\xA4\0[c>c9\xE3`\xE1\x1B_R`\x04\x88\x90R`$R`D_\xFD[a\x1B\xF1\x91P` =` \x11a\x1B\xF7W[a\x1B\xE9\x81\x83a\x04\x1FV[\x81\x01\x90a+\x96V[_a\x1A\x95V[P=a\x1B\xDFV[c\n;\t\xA1`\xE0\x1B_R`\x04\x86\x90R`$R`D_\xFD[a\x1C.\x91P` =` \x11a\x1B\xF7Wa\x1B\xE9\x81\x83a\x04\x1FV[_a\x19\xF5V[_\x80\x88\x8Aa\x1Cla\x19\x1D\x86a\x18\xEFa\x1CSa\x18\xF5\x83a\x18\xEF\x89\x89a+\xA5V[\x95` a\x1Cd\x84a\x18\xEF\x84\x8Aa+\xA5V[\x015\x95a+\xA5V[\x91\x90a\x1C}`@Q\x80\x94\x81\x93a+\xDAV[\x03\x92Z\xF1a\x1C\x89a&3V[\x90\x15a\x1C\x98WP`\x01\x01a\x19\xA0V[a\x1C\xAE\x82a\x1C\xA6\x87\x8Ca+\xA5V[\x91\x90Pa,%V[a\t\x01`@Q\x92\x83\x92c\\\r\xEE]`\xE0\x1B\x84R`\x04\x84\x01a+\xE7V[a\x1C\xDCa\x0C\xBFa\x0C\xBF``\x88\x01a%\xF4V[a\x1C\xE8`\xC0\x87\x01a%\xF4V[a\x01\0\x87\x015\x90\x82;\x15a\x02\xF5Wa\x1D\x1C\x92_\x92\x83`@Q\x80\x96\x81\x95\x82\x94c\xA4\x15\xBC\xAD`\xE0\x1B\x84R\x8B0\x92`\x04\x86\x01a)\xBDV[\x03\x92Z\xF1\x80\x15a\r@Wa\x1D1W[Pa\x19\x8CV[\x80a\r4_a\x1D?\x93a\x04\x1FV[_a\x1D+V[`\x80\x84\x01a\x1D|a\x1Dva\x0C\xBFa\x0C\xBFa\x1D^\x85a%\xF4V[a\x18\xF5\x88``\x8C\x01\x92a\x1Dp\x84a%\xF4V[\x90a7\xD3V[\x91a%\xF4V[\x90\x80;\x15a\x02\xF5W`@Qca{\xA07`\xE0\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x92\x90\x92\x16`\x04\x83\x01R`$\x82\x01\x84\x90R0`D\x83\x01R_`d\x83\x01\x81\x90R\x90\x82\x90`\x84\x90\x82\x90\x84\x90Z\xF1\x80\x15a\r@Wa\x1D\xD4W[Pa\x19\x80V[\x80a\r4_a\x1D\xE2\x93a\x04\x1FV[_a\x1D\xCEV[\x80a\r4_a\x1D\xF6\x93a\x04\x1FV[_a\x18\xBEV[a\x1E\x15\x91P` =` \x11a\x1B\xF7Wa\x1B\xE9\x81\x83a\x04\x1FV[_a\x18?V[cb\x0B\xE6#`\xE0\x1B_R`\x04_\xFD[cx\xFC\xF8\x0B`\xE1\x1B_R`\x04_\xFD[c\x8E^P=`\xE0\x1B_R`\x04_\xFD[4a\x02\xF5W`d` a\x1EZ6a\n\xECV[\x91\x93\x90a\x1Eea5\x13V[a\x1Ema5^V[`@Qc\x1AL\xA3{`\xE2\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x91\x82\x16`\x04\x82\x01R`$\x81\x01\x93\x90\x93R0`D\x84\x01R\x91\x93\x84\x92\x83\x91_\x91\x16Z\xF1\x80\x15a\r@Wa\x03c\x91_\x91a\x1E\xC5W[P`@Q\x90\x81R\x90\x81\x90` \x82\x01\x90V[a\x1E\xDE\x91P` =` \x11a\x1B\xF7Wa\x1B\xE9\x81\x83a\x04\x1FV[_a\x1E\xB4V[4a\x02\xF5W` \x7F\x045Aj_H\xD4\x1D^^\xDE-\x05\xC0\xE1\xFFk\x1Fq\xCBW\x17k\x10\x11\xEA\x7F\xBD\x8CrZ\x83a\x1F\x156a\x07\x1BV[\x92\x90a\x1F\x1Fa5\x13V[`\x01`\x01`\xA0\x1B\x03\x16_\x81\x81R`\x02\x83R`@\x90 \x80T`\xFF\x19\x16`\xFF\x86\x15\x15\x16\x17\x90U\x92`@Q\x90\x15\x15\x81R\xA2\0[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W`\x01T`@Q`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x81R` \x90\xF3[4a\x02\xF5W``6`\x03\x19\x01\x12a\x02\xF5W`\x045a\x1F\x94\x81a\x03\xCDV[`$5\x90a\x1F\xA1\x82a\x03\xCDV[`D5`\x01`\x01`\xE0\x1B\x03\x19\x81\x16\x81\x03a\x02\xF5W` \x92a\x1F\xC1\x92a8\x80V[`@Q\x90\x15\x15\x81R\xF3[4a\x02\xF5W`\x806`\x03\x19\x01\x12a\x02\xF5W`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\x1F\xFB\x906\x90`\x04\x01a\x07_V[`$5`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa \x1A\x906\x90`\x04\x01a\x07_V[`D\x93\x91\x935`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa <\x906\x90`\x04\x01a\x07_V[\x91`d5\x95`\x01`\x01`@\x1B\x03\x87\x11a\x02\xF5Wa `a\0\x18\x976\x90`\x04\x01a\x05\x85V[\x96\x90\x95a.+V[4a\x02\xF5W`\xA06`\x03\x19\x01\x12a\x02\xF5Wa \x84`\x045a\x03\xCDV[a \x8F`$5a\x03\xCDV[`D5`d5`\x845`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa \xB4\x906\x90`\x04\x01a\x04\xD5V[P`@Q\x90\x81R3\x90\x7F\xD1\xEC\x03\r\xA0\xF9\x9F\xCC\xAD\x1D\xE2wM\xC9\x82w\xB6\xE7i:T\x9FGQ\xC3\x19\xCF\x877\x9B\xC3\x0C` c\xF2:na`\xE0\x1B\x92\xA4`@Qc\xF2:na`\xE0\x1B\x81R` \x90\xF3[4a\x02\xF5W` 6`\x03\x19\x01\x12a\x02\xF5W`\x045a!\x19\x81a\x03\xCDV[a!!a5\x13V[`\x01\x80`\xA0\x1B\x03\x16\x80k\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF`\xA0\x1B`\x01T\x16\x17`\x01U`\x01\x80`\xA0\x1B\x03_T\x16\x7F8\xD1k\x8C\xAC\"\xD9\x9F\xC7\xC1$\xB9\xCD\r\xE2\xD3\xFA\x1F\xAE\xF4 \xBF\xE7\x91\xD8\xC3b\xD7e\xE2'\0_\x80\xA3\0[4a\x02\xF5W_6`\x03\x19\x01\x12a\x02\xF5W`@Q\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16\x81R` \x90\xF3[4a\x02\xF5Wa!\xC46a\n\xECV[\x91a!\xCDa5\x13V[a!\xD5a5^V[a!\xE0\x83\x82\x84a7\xD3V[`\x01`\x01`\xA0\x1B\x03\x16\x80;\x15a\x02\xF5W`@Qca{\xA07`\xE0\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x90\x92\x16`\x04\x83\x01R`$\x82\x01\x92\x90\x92R0`D\x82\x01R_`d\x82\x01\x81\x90R\x91\x82\x90\x82\x90\x81\x83\x81`\x84\x81\x01a\x12\x83V[4a\x02\xF5W`\x84` _a\"G6a\x11\xFBV[\x95\x92a\"T\x94\x91\x94a5\x13V[a\"\\a5^V[a\"g\x85\x82\x85a7\xD3V[`@Q\x96\x87\x95\x86\x94cW:\xDE\x81`\xE0\x1B\x86R`\x01\x80`\xA0\x1B\x03\x16`\x04\x86\x01R`$\x85\x01R`D\x84\x01R0`d\x84\x01R`\x01\x80`\xA0\x1B\x03\x16Z\xF1\x80\x15a\r@Wa\x03c\x91_\x91a\x1E\xC5WP`@Q\x90\x81R\x90\x81\x90` \x82\x01\x90V[\x905\x90`\x1E\x19\x816\x03\x01\x82\x12\x15a\x02\xF5W\x01\x805\x90`\x01`\x01`@\x1B\x03\x82\x11a\x02\xF5W` \x01\x91\x816\x03\x83\x13a\x02\xF5WV[`\x01`\x01`\xA0\x1B\x03\x81\x16\x91\x90\x82\x15a\t%W_\x83\x81R`\x03` R`@\x90 T`\xFF\x16\x15\x15\x82\x15\x15\x14a#\xB6W`\x01`\x01`\xA0\x1B\x03\x16_\x90\x81R`\x03` R`@\x90 \x7F\xFCJ\xCBI\x94\x91\xCD\x85\n\x8A!\xAB\x98\xC7\xF1(\x85\x0C\x0F\x0E_\x1A\x87Zb\xB7\xFA\x05\\.\xCF\x19\x91a#\x9D\x91a#q\x90\x82`\xFF\x80\x19\x83T\x16\x91\x15\x15\x16\x17\x90UV[\x80\x15a#\xA2Wa#\x8Ba#\x86`\x04T`\x01\x01\x90V[`\x04UV[`@Q\x90\x15\x15\x81R\x90\x81\x90` \x82\x01\x90V[\x03\x90\xA2V[`\x04Ta#\xB1\x90_\x19\x01`\x04UV[a#\x8BV[PPPV[cNH{q`\xE0\x1B_R`2`\x04R`$_\xFD[\x91\x90\x81\x10\x15a#\xF1W`\x05\x1B\x81\x015\x90`^\x19\x816\x03\x01\x82\x12\x15a\x02\xF5W\x01\x90V[a#\xBBV[\x91\x90a$\0a5&V[a$\x08a5\x13V[a$\x10a5^V[_[\x81\x81\x10a$%WPP\x90Pa\x03\xE9a5PV[a$0\x81\x83\x86a%\xE4V[\x90` \x82\x01a$Aa\x0C\xBF\x82a%\xF4V[\x15\x80\x15a%\xCEW[a\t%Wa$V\x83a&\x1CV[a$_\x81a%\xFEV[a%\x0BWa$o`\xA0\x84\x01a&)V[a$\xFCW`\x01\x92a$\x7F\x82a%\xF4V[\x90\x7FJ\x94\xF8\x9E\x13\x16\x99\xED4\x16g\x0C\x01\x1C\xE6Mb\xE5\xA5\x81\xA4\xEB\xB4`;\xF6\xC4\xA5\xD0j\x06\xCEa$\xF2a$\xD4a$\xCE``\x85\x015\x96a\x18\xF5`@`\x80\x88\x015\x97\x01\x97\x87\x8Aa$\xC8\x8Ba%\xF4V[\x92a5\x91V[\x94a%\xF4V[`@Q\x93\x84R`\xA0\x88\x90\x1B\x88\x90\x03\x90\x81\x16\x94\x16\x92\x90\x81\x90` \x82\x01\x90V[\x03\x90\xA4[\x01a$\x12V[c\xB0&\xD5\xA3`\xE0\x1B_R`\x04_\xFD[``\x83\x015\x15\x80\x15\x90a%\xC1W[a%\xB2W`\x01\x92\x7F\x9C\x8E\x17\xFA\x11M$\xCF\xC8\xF6|=l\xE6\xBC.$\x06}\xBEA%f@\xBCH\xFDm\x10fV/a%\xAAa%\x88a%\x82a%|a%V\x87a%\xF4V[\x96a\x18\xF5`@\x88\x01\x98`\xA0a%j\x8Ba%\xF4V[\x99\x01\x98a%v\x8Aa&)V[\x91a6JV[\x95a%\xF4V[\x93a&)V[`@Q\x90\x15\x15\x81R`\xA0\x87\x90\x1B\x87\x90\x03\x93\x84\x16\x94\x90\x93\x16\x92\x90\x81\x90` \x82\x01\x90V[\x03\x90\xA3a$\xF6V[cA\xF5!\xF9`\xE1\x1B_R`\x04_\xFD[P`\x80\x83\x015\x15\x15a%\x19V[Pa%\xDEa\x0C\xBF`@\x85\x01a%\xF4V[\x15a$IV[\x91\x90\x81\x10\x15a#\xF1W`\xC0\x02\x01\x90V[5a\x04\xF0\x81a\x03\xCDV[`\x02\x11\x15a&\x08WV[cNH{q`\xE0\x1B_R`!`\x04R`$_\xFD[5`\x02\x81\x10\x15a\x02\xF5W\x90V[5a\x04\xF0\x81a\x07\x11V[=\x15a&]W=\x90a&D\x82a\x04PV[\x91a&R`@Q\x93\x84a\x04\x1FV[\x82R=_` \x84\x01>V[``\x90V[\x905`\x1E\x19\x826\x03\x01\x81\x12\x15a\x02\xF5W\x01` \x815\x91\x01\x91`\x01`\x01`@\x1B\x03\x82\x11a\x02\xF5W\x81`\x05\x1B6\x03\x83\x13a\x02\xF5WV[\x90\x80` \x93\x92\x81\x84R\x84\x84\x017_\x82\x82\x01\x84\x01R`\x1F\x01`\x1F\x19\x16\x01\x01\x90V[\x90` \x83\x82\x81R\x01` \x82`\x05\x1B\x85\x01\x01\x93\x83_\x91[\x84\x83\x10a&\xDCWPPPPPP\x90V[\x90\x91\x92\x93\x94\x95`\x1F\x19\x82\x82\x03\x01\x85R\x865`^\x19\x846\x03\x01\x81\x12\x15a\x02\xF5W\x83\x01\x805a'\x08\x81a\x03\xCDV[`\x01`\x01`\xA0\x1B\x03\x16\x82R` \x81\x81\x015\x90\x83\x01R`@\x81\x0156\x82\x90\x03`\x1E\x19\x01\x81\x12\x15a\x02\xF5W\x01` \x815\x91\x01\x90`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5W\x806\x03\x82\x13a\x02\xF5Wa'j` \x92\x83\x92``\x86\x81`@`\x01\x99\x01R\x01\x91a&\x96V[\x98\x01\x96\x95\x01\x93\x01\x91\x90a&\xCCV[` \x81Ra'\x99` \x82\x01a'\x8C\x84a\x03\xDEV[`\x01`\x01`\xA0\x1B\x03\x16\x90RV[a'\xB8a'\xA8` \x84\x01a\x03\xDEV[`\x01`\x01`\xA0\x1B\x03\x16`@\x83\x01RV[`@\x82\x015``\x82\x01Ra\x01\x80a\x01`a(\x8Ba'\xECa'\xDB``\x87\x01\x87a&bV[\x85`\x80\x88\x01Ra\x01\xA0\x87\x01\x91a&\xB6V[a(\x0Ba'\xFB`\x80\x88\x01a\x03\xDEV[`\x01`\x01`\xA0\x1B\x03\x16`\xA0\x87\x01RV[a(*a(\x1A`\xA0\x88\x01a\x03\xDEV[`\x01`\x01`\xA0\x1B\x03\x16`\xC0\x87\x01RV[`\xC0\x86\x015`\xE0\x86\x01Ra(Ta(C`\xE0\x88\x01a\x03\xDEV[`\x01`\x01`\xA0\x1B\x03\x16a\x01\0\x87\x01RV[a\x01\0\x86\x015a\x01 \x86\x01Ra\x01 \x86\x015a\x01@\x86\x01Ra(za\x01@\x87\x01\x87a&bV[\x86\x83\x03`\x1F\x19\x01\x85\x88\x01R\x90a&\xB6V[\x93\x015\x91\x01R\x90V[`@\x80Q\x90\x91\x90a(\xA5\x83\x82a\x04\x1FV[`\x01\x81R\x91`\x1F\x19\x016` \x84\x017V[\x80Q\x15a#\xF1W` \x01\x90V[\x80Q\x82\x10\x15a#\xF1W` \x91`\x05\x1B\x01\x01\x90V[\x80Q\x80\x83R` \x92\x91\x81\x90\x84\x01\x84\x84\x01^_\x82\x82\x01\x84\x01R`\x1F\x01`\x1F\x19\x16\x01\x01\x90V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x81R`\x80` \x80\x83\x01\x82\x90R\x83Q\x91\x83\x01\x82\x90R`\xA0\x83\x01\x96\x95\x93\x01\x90_[\x81\x81\x10a)zWPPP\x80\x85\x03`@\x82\x01R` \x80\x84Q\x96\x87\x81R\x01\x93\x01_\x95[\x80\x87\x10a)bWPPa\x04\xF0\x93\x94P``\x81\x84\x03\x91\x01Ra(\xD7V[\x90\x93` \x80`\x01\x92\x87Q\x81R\x01\x95\x01\x96\x01\x95\x90a)FV[\x82Q`\x01`\x01`\xA0\x1B\x03\x16\x88R` \x97\x88\x01\x97\x90\x92\x01\x91`\x01\x01a)%V[`@Q=_\x82>=\x90\xFD[\x90\x15a#\xF1W\x90V[\x91\x90\x81\x10\x15a#\xF1W`\x05\x1B\x01\x90V[`\x01`\x01`\xA0\x1B\x03\x91\x82\x16\x81R` \x81\x01\x92\x90\x92R`@\x82\x01\x92\x90\x92R_``\x82\x01R\x91\x16`\x80\x82\x01R`\xA0\x01\x90V[` \x81Ra*\x01` \x82\x01a'\x8C\x84a\x03\xDEV[` \x82\x015`@\x82\x01Ra\x01`a\x01@a(\x8Ba*5a*$`@\x87\x01\x87a&bV[\x85``\x88\x01Ra\x01\x80\x87\x01\x91a&\xB6V[a*Ta*D``\x88\x01a\x03\xDEV[`\x01`\x01`\xA0\x1B\x03\x16`\x80\x87\x01RV[a*ca'\xFB`\x80\x88\x01a\x03\xDEV[`\xA0\x86\x015`\xC0\x86\x01Ra*\x8Ca*|`\xC0\x88\x01a\x03\xDEV[`\x01`\x01`\xA0\x1B\x03\x16`\xE0\x87\x01RV[`\xE0\x86\x015a\x01\0\x86\x01Ra\x01\0\x86\x015a\x01 \x86\x01Ra(za\x01 \x87\x01\x87a&bV[` \x81\x83\x03\x12a\x02\xF5W\x80Q\x90`\x01`\x01`@\x1B\x03\x82\x11a\x02\xF5W\x01\x81`\x1F\x82\x01\x12\x15a\x02\xF5W\x80Q\x90a*\xE4\x82a\x04PV[\x92a*\xF2`@Q\x94\x85a\x04\x1FV[\x82\x84R` \x83\x83\x01\x01\x11a\x02\xF5W\x81_\x92` \x80\x93\x01\x83\x86\x01^\x83\x01\x01R\x90V[\x90` a\x04\xF0\x92\x81\x81R\x01\x90a(\xD7V[PP\x92\x91P_[\x83Q\x81\x10\x15a+\x88W\x80a+A`\x01\x92\x86a(\xC3V[Qa+L\x82\x85a(\xC3V[Q`@Q\x90\x81R3\x90\x7F\xD1\xEC\x03\r\xA0\xF9\x9F\xCC\xAD\x1D\xE2wM\xC9\x82w\xB6\xE7i:T\x9FGQ\xC3\x19\xCF\x877\x9B\xC3\x0C` c\xBC\x19|\x81`\xE0\x1B\x92\xA4\x01a++V[Pc\xBC\x19|\x81`\xE0\x1B\x92PPV[\x90\x81` \x91\x03\x12a\x02\xF5WQ\x90V[\x905\x90`\x1E\x19\x816\x03\x01\x82\x12\x15a\x02\xF5W\x01\x805\x90`\x01`\x01`@\x1B\x03\x82\x11a\x02\xF5W` \x01\x91\x81`\x05\x1B6\x03\x83\x13a\x02\xF5WV[\x90\x80\x92\x91\x827\x01_\x81R\x90V[`@\x90a\x04\xF0\x93\x92\x81R\x81` \x82\x01R\x01\x90a(\xD7V[cNH{q`\xE0\x1B_R`\x11`\x04R`$_\xFD[\x90`A\x82\x01\x80\x92\x11a, WV[a+\xFEV[\x91\x90\x82\x01\x80\x92\x11a, WV[\x81\x81\x03\x92\x91_\x13\x80\x15\x82\x85\x13\x16\x91\x84\x12\x16\x17a, WV[\x90\x80`\x1F\x83\x01\x12\x15a\x02\xF5W\x815\x91a,b\x83a\x16\x89V[\x92a,p`@Q\x94\x85a\x04\x1FV[\x80\x84R` \x80\x85\x01\x91`\x05\x1B\x83\x01\x01\x91\x83\x83\x11a\x02\xF5W` \x81\x01\x91[\x83\x83\x10a,\x9CWPPPPP\x90V[\x825`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5W\x82\x01\x90``\x82\x87\x03`\x1F\x19\x01\x12a\x02\xF5W`@Q\x90a,\xCA\x82a\x03\xFFV[` \x83\x015a,\xD8\x81a\x03\xCDV[\x82R`@\x83\x015` \x83\x01R``\x83\x015\x91`\x01`\x01`@\x1B\x03\x83\x11a\x02\xF5Wa-\n\x88` \x80\x96\x95\x81\x96\x01\x01a\x04\xD5V[`@\x82\x01R\x81R\x01\x92\x01\x91a,\x8DV[` \x81\x83\x03\x12a\x02\xF5W\x805\x90`\x01`\x01`@\x1B\x03\x82\x11a\x02\xF5W\x01\x90a\x01\x80\x82\x82\x03\x12a\x02\xF5Wa-Ja\x04@V[\x91a-T\x81a\x03\xDEV[\x83Ra-b` \x82\x01a\x03\xDEV[` \x84\x01R`@\x81\x015`@\x84\x01R``\x81\x015`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5W\x82a-\x90\x91\x83\x01a,JV[``\x84\x01Ra-\xA1`\x80\x82\x01a\x03\xDEV[`\x80\x84\x01Ra-\xB2`\xA0\x82\x01a\x03\xDEV[`\xA0\x84\x01R`\xC0\x81\x015`\xC0\x84\x01Ra-\xCD`\xE0\x82\x01a\x03\xDEV[`\xE0\x84\x01Ra\x01\0\x81\x015a\x01\0\x84\x01Ra\x01 \x81\x015a\x01 \x84\x01Ra\x01@\x81\x015`\x01`\x01`@\x1B\x03\x81\x11a\x02\xF5Wa\x01`\x92a.\r\x91\x83\x01a,JV[a\x01@\x84\x01R\x015a\x01`\x82\x01R\x90V[\x91\x90\x82\x03\x91\x82\x11a, WV[\x92\x97_Q` a:\x04_9_Q\x90_R\\\x97\x94\x96\x90\x95\x92\x94\x91`\x01`\x01`\xA0\x1B\x03\x89\x16\x80\x15a\x1E*W3\x03a3\xB4W`\x01\x81\x14\x80\x15\x90a3\xA9W[\x80\x15a3\x9EW[a3wW_Q` a9\xE4_9_Q\x90_R\\a.\x8B6\x85\x85a\x04\x9FV[` \x81Q\x91\x01 \x03a\x1E\x1BWa.\xAA\x82a.\xB0\x94a\x18\xF5\x94\x01\x90a-\x1AV[\x94a)\xA4V[` \x83\x01\x80Q\x90\x92\x91\x90a.\xCC\x90`\x01`\x01`\xA0\x1B\x03\x16a\x0C\xBFV[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x14\x80\x15\x90a3\x86W[a3wW\x81Qa.\xFA\x90a\x0C\xBF\x90`\x01`\x01`\xA0\x1B\x03\x16\x81V[`@Qcp\xA0\x821`\xE0\x1B\x81R0`\x04\x82\x01R\x90` \x90\x82\x90`$\x90\x82\x90Z\xFA\x80\x15a\r@Wa/@\x91_\x91a3XW[Pa/9\x8A\x87\x9B\x9A\x9Ba)\xA4V[5\x90a.\x1EV[``\x84\x01\x97\x90_[\x89Q\x80Q\x82\x10\x15a/\xB2W_\x8Ba/qa/c\x85\x84\x95a(\xC3V[QQ`\x01`\x01`\xA0\x1B\x03\x16\x90V[`@a/\x8E\x86` a/\x84\x82\x87Qa(\xC3V[Q\x01Q\x94Qa(\xC3V[Q\x01Q\x91` \x83Q\x93\x01\x91Z\xF1a/\xA3a&3V[\x90\x15a\x19SWP`\x01\x01a/HV[PP\x91\x94\x97\x92\x95\x98\x90\x93\x96`\xC0\x88\x01\x95\x86Q\x80a2\xA4W[Pa\x01\0\x89\x01\x95\x86Q\x8A\x81a2\x1EW[a\x01@\x91P\x01\x9A\x8BQQ_[\x8D\x82\x82\x10a1\xC9WPPP\x91a0\x03a0\n\x92a0\x11\x95\x94a)\xA4V[5\x92a)\xA4V[5\x90a,%V[\x86Q\x90\x92\x90a0*\x90a\x0C\xBF\x90`\x01`\x01`\xA0\x1B\x03\x16\x81V[`@Qcp\xA0\x821`\xE0\x1B\x81R0`\x04\x82\x01R\x91\x90` \x90\x83\x90`$\x90\x82\x90Z\xFA\x80\x15a\r@Wa\x1A\x01\x85a\x19\xFBa\x1A\x06\x93a0l\x96_\x91a\x1C\x15WPa8kV[\x92a\x01`\x88\x01Q\x80\x85\x12a1\xB2WP\x91a1B\x82a1\xAD\x94a11\x8Ba\x1B>`@a0\xDD\x8Ea0\xD0\x7FS\xF2\x133U\x06;\x07\x87\xBE\x9Bs\xF9\xF2\xC3\xD6\xE1Fp\xE2\xA3}\xD4b6\x8B]\x1A\x94\xE8k\x06\x9F\x9E\x9D\x9B\x8B\x90a0\xCB\x84Q`\x01\x80`\xA0\x1B\x03\x16\x90V[a7OV[Q`\x01`\x01`\xA0\x1B\x03\x16\x90V[\x92\x01\x80Q\x94QQ\x9FQQ`@\x80Q0` \x82\x01\x90\x81R`\x01`\x01`\xA0\x1B\x03\x99\x8A\x16\x92\x82\x01\x92\x90\x92R\x97\x90\x94\x16``\x88\x01R`\x80\x87\x01\x95\x90\x95R`\xA0\x86\x01\x9F\x90\x9FR`\xC0\x85\x01\x93\x90\x93R\x92\x91\x82\x90`\xE0\x82\x01\x90V[Q\x90 \x97Q`\x01`\x01`\xA0\x1B\x03\x16\x90V[\x98Q`\xA0\x89\x01Q\x90\x98\x90a1m\x90`\xE0\x90`\x01`\x01`\xA0\x1B\x03\x16\x97Q\x92\x01Q`\x01`\x01`\xA0\x1B\x03\x16\x90V[\x94Q`@\x80Q\x9A\x8BR`\x01`\x01`\xA0\x1B\x03\x97\x88\x16` \x8C\x01R\x8A\x01\x91\x90\x91R``\x89\x01R`\x80\x88\x01R`\xA0\x87\x01R\x90\x82\x16\x95\x90\x91\x16\x93\x90\x81\x90`\xC0\x82\x01\x90V[\x03\x90\xA4V[c\n;\t\xA1`\xE0\x1B_R`\x04\x85\x90R`$R`D_\xFD[_\x81a1\xDAa/c\x85\x84\x95Qa(\xC3V[`@a1\xED\x86` a/\x84\x82\x87Qa(\xC3V[Q\x01Q\x91` \x83Q\x93\x01\x91Z\xF1a2\x02a&3V[\x90\x15a2\x11WP`\x01\x01a/\xE6V[a\x1C\xAE\x82\x8BQQ\x90a,%V[`\x80\x01Qa26\x90a\x0C\xBF\x90`\x01`\x01`\xA0\x1B\x03\x16\x81V[`\xE0\x8C\x01Q\x90\x91\x90`\x01`\x01`\xA0\x1B\x03\x16a\x01 \x8D\x01Q\x92\x80;\x15a\x02\xF5Wa2{\x93_\x80\x94`@Q\x96\x87\x95\x86\x94\x85\x93c\xA4\x15\xBC\xAD`\xE0\x1B\x85R0\x92`\x04\x86\x01a)\xBDV[\x03\x92Z\xF1\x80\x15a\r@Wa2\x90W[\x8Aa/\xDAV[\x80a\r4_a2\x9E\x93a\x04\x1FV[_a2\x8AV[a2\xEEa2\xE0a\x0C\xBFa\x0C\xBF\x8Da0\xD0`\xA0\x82\x01\x96`\x80a2\xCB\x89Q`\x01\x80`\xA0\x1B\x03\x16\x90V[\x93\x01\x80Q\x90\x93\x90`\x01`\x01`\xA0\x1B\x03\x16a\x1DpV[\x91Q`\x01`\x01`\xA0\x1B\x03\x16\x90V[\x88Q\x90\x82;\x15a\x02\xF5W`@Qca{\xA07`\xE0\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x91\x90\x91\x16`\x04\x82\x01R`$\x81\x01\x91\x90\x91R0`D\x82\x01R_`d\x82\x01\x81\x90R\x90\x91\x82\x90`\x84\x90\x82\x90\x84\x90Z\xF1\x80\x15a\r@W\x15a/\xCAW\x80a\r4_a3R\x93a\x04\x1FV[_a/\xCAV[a3q\x91P` =` \x11a\x1B\xF7Wa\x1B\xE9\x81\x83a\x04\x1FV[_a/+V[c\x04\x15\xB9\xDB`\xE1\x1B_R`\x04_\xFD[Pa3\x91\x88\x85a)\xA4V[5`@\x84\x01Q\x14\x15a.\xE0V[P`\x01\x84\x14\x15a.mV[P`\x01\x8A\x14\x15a.fV[c%\0\xC5%`\xE1\x1B_R`\x04_\xFD[\x90\x81`A\x02\x91`A\x83\x04\x03a, WV[\x90`A\x82\x02\x91\x80\x83\x04`A\x14\x90\x15\x17\x15a, WV[\x90\x93\x92\x93\x84\x83\x11a\x02\xF5W\x84\x11a\x02\xF5W\x81\x01\x92\x03\x90V[\x90\x91`\x05T\x91a4\x11\x83a3\xC3V[\x82\x10a5\x0BW`A\x82\x06a5\x0BW_\x93\x84\x92`A\x81\x04\x92\x91\x90\x84[\x84\x86\x10a4NWPPPPPP\x80\x82\x11\x91\x82\x15a4HWPP\x90V[\x14\x91\x90PV[a4\x80a4ya4`\x88\x9A\x97\x98a3\xD4V[a4qa4l\x8Ca3\xD4V[a,\x12V[\x90\x85\x87a3\xEAV[\x90\x86a9:V[\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x80\x15a4\xFFW`\x01`\x01`\xA0\x1B\x03\x82\x16\x80\x82\x10\x91\x82\x15a4\xF5W[PPa4\xE8W`\x01`\x01`\xA0\x1B\x03\x82\x16_\x90\x81R`\x03` R`@\x90 T`\xFF\x16\x15a4\xDDWP`\x01\x80\x91\x95\x01\x97[\x01\x94\x93a4,V[\x94\x97`\x01\x91Pa4\xD5V[PPPPPPPPP_\x90V[\x14\x90P_\x80a4\xA6V[P\x94\x97`\x01\x91Pa4\xD5V[PPPP_\x90V[_T`\x01`\x01`\xA0\x1B\x03\x163\x03a\x11KWV[h\x80\0\0\0\0\xAB\x14<\x06\\a5CW0h\x80\0\0\0\0\xAB\x14<\x06]V[c\xAB\x14<\x06_R`\x04`\x1C\xFD[_h\x80\0\0\0\0\xAB\x14<\x06]V[`\xFF`\x01T`\xA0\x1C\x16a5mWV[c\xD9<\x06e`\xE0\x1B_R`\x04_\xFD[\x90\x81` \x91\x03\x12a\x02\xF5WQa\x04\xF0\x81a\x07\x11V[\x90\x92\x91\x90`\x01`\x01`\xA0\x1B\x03\x16\x80\x15\x80\x15a69W[a\t%W`@Qc\x04\xAD\xE6\xDB`\xE1\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x93\x90\x93\x16`\x04\x84\x01R`$\x83\x01\x93\x90\x93R`D\x82\x01R\x90` \x90\x82\x90`d\x90\x82\x90_\x90Z\xF1\x90\x81\x15a\r@W_\x91a6\nW[P\x15a5\xFBWV[c1\x14o\x15`\xE0\x1B_R`\x04_\xFD[a6,\x91P` =` \x11a62W[a6$\x81\x83a\x04\x1FV[\x81\x01\x90a5|V[_a5\xF3V[P=a6\x1AV[P`\x01`\x01`\xA0\x1B\x03\x83\x16\x15a5\xA7V[`\x01`\x01`\xA0\x1B\x03\x16\x91\x82\x15\x80\x15a6\xD9W[a\t%W`@QcU\x8Ar\x97`\xE0\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x92\x90\x92\x16`\x04\x83\x01R\x15\x15`$\x82\x01R\x90` \x90\x82\x90`D\x90\x82\x90_\x90Z\xF1\x90\x81\x15a\r@W_\x91a6\xBAW[P\x15a6\xABWV[c-\xB3\x0EG`\xE1\x1B_R`\x04_\xFD[a6\xD3\x91P` =` \x11a62Wa6$\x81\x83a\x04\x1FV[_a6\xA3V[P`\x01`\x01`\xA0\x1B\x03\x82\x16\x15a6]V[\x90s\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA9`\x14R`4Rc\xA9\x05\x9C\xBB``\x1B_R` _`D`\x10\x82\x85Z\xF1\x90\x81`\x01_Q\x14\x16\x15a71W[PP_`4RV[;\x15=\x17\x10\x15a7BW_\x80a7)V[c\x90\xB8\xEC\x18_R`\x04`\x1C\xFD[\x91\x90`\x14R`4Rc\xA9\x05\x9C\xBB``\x1B_R` _`D`\x10\x82\x85Z\xF1\x90\x81`\x01_Q\x14\x16\x15a71WPP_`4RV[`\x01`\x01`\xA0\x1B\x03\x16_Q` a:\x04_9_Q\x90_R]V[__Q` a:\x04_9_Q\x90_R]V[s\xBA\x133333\xA1\xBA\x11\x08\xE8A/\x11\x85\n\\1\x9B\xA9_Q` a:\x04_9_Q\x90_R]V[\x91\x90`\x14R\x80`4Rc\t^\xA7\xB3``\x1B_R` _`D`\x10\x82\x86Z\xF1\x80`\x01_Q\x14\x16\x15a8\x07W[PPP_`4RV[=\x83;\x15\x17\x10\x15a8\x19W[\x80a7\xFEV[_`4\x81\x90Rc\t^\xA7\xB3``\x1B\x81R8`D`\x10\x83\x86Z\xF1P`4R` _`D`\x10\x82\x85Z\xF1\x90\x81`\x01_Q\x14\x16a8\x13W;\x15=\x17\x10\x15a8^W_\x80a8\x13V[c>?\x8Fs_R`\x04`\x1C\xFD[_\x81\x12\x15a\x04\xF0Wc5'\x8D\x12_R`\x04`\x1C\xFD[\x90`\x01\x80`\xA0\x1B\x03\x82\x16_R`\x02` R`\xFF`@_ T\x16a92W`@Qc\x15w\x18E`\xE0\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x92\x83\x16`\x04\x82\x01R\x91\x16`$\x82\x01R`\x01`\x01`\xE0\x1B\x03\x19\x90\x91\x16`D\x82\x01R` \x81\x80`d\x81\x01\x03\x81\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16Z\xFA\x90\x81\x15a\r@W_\x91a9\x19WP\x90V[a\x04\xF0\x91P` =` \x11a62Wa6$\x81\x83a\x04\x1FV[PPP`\x01\x90V[\x90\x92\x91\x92`@Q\x93\x80`@\x14a9\x93W`A\x14a9cWPPPP[c\x8B\xAAW\x9F_R`\x04`\x1C\xFD[\x80`@\x80\x92\x015_\x1A` R\x817[_R` `\x01`\x80_\x82Z\xFAQ\x91_``R`@R=a\x03\xE9WPPa9VV[P` \x81\x81\x015`\xFF\x81\x90\x1C`\x1B\x01\x90\x91R\x905`@R`\x01`\x01`\xFF\x1B\x03\x16``Ra9rV[_\x92\x91`\x03\x81\x11a9\xCAWPPV[\x90\x91\x92P`\x04\x11a\x02\xF5W5`\x01`\x01`\xE0\x1B\x03\x19\x16\x90V\xFE\xAD\xC7\xF6[\xDD\xB3o\xDF\xCF4\xDBxE\xA6\xB3R\xD0\xB8.\x98\x8C\xD1\x9F\x14az\xA0\x97\x0B\xAD\xEDs\xB7+\xB2\xDB\xDB\xBE\x81\x80\x12\xAB\xB2\x1A\x93~\xC7\x0B.mh\xF4\\\x16C\xFC\xF3\xF1;`!Qy\xDF\xA1dsolcC\0\x08\"\0\n",
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
struct FinancePlan { address flashLender; address flashAsset; uint256 flashAmount; Call[] preActions; address aavePool; address collateralAsset; uint256 supplyAmount; address debtAsset; uint256 borrowAmount; uint256 interestRateMode; Call[] postActions; int256 minDeltaFlashAsset; }
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct FinancePlan {
        #[allow(missing_docs)]
        pub flashLender: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub flashAsset: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub flashAmount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub preActions: alloy::sol_types::private::Vec<
            <Call as alloy::sol_types::SolType>::RustType,
        >,
        #[allow(missing_docs)]
        pub aavePool: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub collateralAsset: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub supplyAmount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub debtAsset: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub borrowAmount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub interestRateMode: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub postActions: alloy::sol_types::private::Vec<
            <Call as alloy::sol_types::SolType>::RustType,
        >,
        #[allow(missing_docs)]
        pub minDeltaFlashAsset: alloy::sol_types::private::primitives::aliases::I256,
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
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Array<Call>,
            alloy::sol_types::sol_data::Address,
            alloy::sol_types::sol_data::Address,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Address,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Array<Call>,
            alloy::sol_types::sol_data::Int<256>,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
            alloy::sol_types::private::Address,
            alloy::sol_types::private::Address,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::Vec<
                <Call as alloy::sol_types::SolType>::RustType,
            >,
            alloy::sol_types::private::Address,
            alloy::sol_types::private::Address,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::Address,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::Vec<
                <Call as alloy::sol_types::SolType>::RustType,
            >,
            alloy::sol_types::private::primitives::aliases::I256,
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
        impl ::core::convert::From<FinancePlan> for UnderlyingRustTuple<'_> {
            fn from(value: FinancePlan) -> Self {
                (
                    value.flashLender,
                    value.flashAsset,
                    value.flashAmount,
                    value.preActions,
                    value.aavePool,
                    value.collateralAsset,
                    value.supplyAmount,
                    value.debtAsset,
                    value.borrowAmount,
                    value.interestRateMode,
                    value.postActions,
                    value.minDeltaFlashAsset,
                )
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for FinancePlan {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    flashLender: tuple.0,
                    flashAsset: tuple.1,
                    flashAmount: tuple.2,
                    preActions: tuple.3,
                    aavePool: tuple.4,
                    collateralAsset: tuple.5,
                    supplyAmount: tuple.6,
                    debtAsset: tuple.7,
                    borrowAmount: tuple.8,
                    interestRateMode: tuple.9,
                    postActions: tuple.10,
                    minDeltaFlashAsset: tuple.11,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolValue for FinancePlan {
            type SolType = Self;
        }
        #[automatically_derived]
        impl alloy_sol_types::private::SolTypeValue<Self> for FinancePlan {
            #[inline]
            fn stv_to_tokens(&self) -> <Self as alloy_sol_types::SolType>::Token<'_> {
                (
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.flashLender,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.flashAsset,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.flashAmount),
                    <alloy::sol_types::sol_data::Array<
                        Call,
                    > as alloy_sol_types::SolType>::tokenize(&self.preActions),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.aavePool,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.collateralAsset,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.supplyAmount),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.debtAsset,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.borrowAmount),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.interestRateMode),
                    <alloy::sol_types::sol_data::Array<
                        Call,
                    > as alloy_sol_types::SolType>::tokenize(&self.postActions),
                    <alloy::sol_types::sol_data::Int<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.minDeltaFlashAsset),
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
        impl alloy_sol_types::SolType for FinancePlan {
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
        impl alloy_sol_types::SolStruct for FinancePlan {
            const NAME: &'static str = "FinancePlan";
            #[inline]
            fn eip712_root_type() -> alloy_sol_types::private::Cow<'static, str> {
                alloy_sol_types::private::Cow::Borrowed(
                    "FinancePlan(address flashLender,address flashAsset,uint256 flashAmount,Call[] preActions,address aavePool,address collateralAsset,uint256 supplyAmount,address debtAsset,uint256 borrowAmount,uint256 interestRateMode,Call[] postActions,int256 minDeltaFlashAsset)",
                )
            }
            #[inline]
            fn eip712_components() -> alloy_sol_types::private::Vec<
                alloy_sol_types::private::Cow<'static, str>,
            > {
                let mut components = alloy_sol_types::private::Vec::with_capacity(2);
                components
                    .push(<Call as alloy_sol_types::SolStruct>::eip712_root_type());
                components
                    .extend(<Call as alloy_sol_types::SolStruct>::eip712_components());
                components
                    .push(<Call as alloy_sol_types::SolStruct>::eip712_root_type());
                components
                    .extend(<Call as alloy_sol_types::SolStruct>::eip712_components());
                components
            }
            #[inline]
            fn eip712_encode_data(&self) -> alloy_sol_types::private::Vec<u8> {
                [
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::eip712_data_word(
                            &self.flashLender,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::eip712_data_word(
                            &self.flashAsset,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.flashAmount)
                        .0,
                    <alloy::sol_types::sol_data::Array<
                        Call,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.preActions)
                        .0,
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::eip712_data_word(
                            &self.aavePool,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::eip712_data_word(
                            &self.collateralAsset,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.supplyAmount)
                        .0,
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::eip712_data_word(
                            &self.debtAsset,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.borrowAmount)
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(
                            &self.interestRateMode,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Array<
                        Call,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.postActions)
                        .0,
                    <alloy::sol_types::sol_data::Int<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(
                            &self.minDeltaFlashAsset,
                        )
                        .0,
                ]
                    .concat()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::EventTopic for FinancePlan {
            #[inline]
            fn topic_preimage_length(rust: &Self::RustType) -> usize {
                0usize
                    + <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.flashLender,
                    )
                    + <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.flashAsset,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.flashAmount,
                    )
                    + <alloy::sol_types::sol_data::Array<
                        Call,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.preActions,
                    )
                    + <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.aavePool,
                    )
                    + <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.collateralAsset,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.supplyAmount,
                    )
                    + <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.debtAsset,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.borrowAmount,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.interestRateMode,
                    )
                    + <alloy::sol_types::sol_data::Array<
                        Call,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.postActions,
                    )
                    + <alloy::sol_types::sol_data::Int<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.minDeltaFlashAsset,
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
                    &rust.flashLender,
                    out,
                );
                <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.flashAsset,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.flashAmount,
                    out,
                );
                <alloy::sol_types::sol_data::Array<
                    Call,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.preActions,
                    out,
                );
                <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.aavePool,
                    out,
                );
                <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.collateralAsset,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.supplyAmount,
                    out,
                );
                <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.debtAsset,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.borrowAmount,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.interestRateMode,
                    out,
                );
                <alloy::sol_types::sol_data::Array<
                    Call,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.postActions,
                    out,
                );
                <alloy::sol_types::sol_data::Int<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.minDeltaFlashAsset,
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
struct FinancePlanV3 { address flashAsset; uint256 flashAmount; Call[] preActions; address aavePool; address collateralAsset; uint256 supplyAmount; address debtAsset; uint256 borrowAmount; uint256 interestRateMode; Call[] postActions; int256 minDeltaFlashAsset; }
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct FinancePlanV3 {
        #[allow(missing_docs)]
        pub flashAsset: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub flashAmount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub preActions: alloy::sol_types::private::Vec<
            <Call as alloy::sol_types::SolType>::RustType,
        >,
        #[allow(missing_docs)]
        pub aavePool: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub collateralAsset: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub supplyAmount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub debtAsset: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub borrowAmount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub interestRateMode: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub postActions: alloy::sol_types::private::Vec<
            <Call as alloy::sol_types::SolType>::RustType,
        >,
        #[allow(missing_docs)]
        pub minDeltaFlashAsset: alloy::sol_types::private::primitives::aliases::I256,
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
            alloy::sol_types::sol_data::Array<Call>,
            alloy::sol_types::sol_data::Address,
            alloy::sol_types::sol_data::Address,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Address,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Array<Call>,
            alloy::sol_types::sol_data::Int<256>,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
            alloy::sol_types::private::Address,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::Vec<
                <Call as alloy::sol_types::SolType>::RustType,
            >,
            alloy::sol_types::private::Address,
            alloy::sol_types::private::Address,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::Address,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::primitives::aliases::U256,
            alloy::sol_types::private::Vec<
                <Call as alloy::sol_types::SolType>::RustType,
            >,
            alloy::sol_types::private::primitives::aliases::I256,
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
        impl ::core::convert::From<FinancePlanV3> for UnderlyingRustTuple<'_> {
            fn from(value: FinancePlanV3) -> Self {
                (
                    value.flashAsset,
                    value.flashAmount,
                    value.preActions,
                    value.aavePool,
                    value.collateralAsset,
                    value.supplyAmount,
                    value.debtAsset,
                    value.borrowAmount,
                    value.interestRateMode,
                    value.postActions,
                    value.minDeltaFlashAsset,
                )
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for FinancePlanV3 {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    flashAsset: tuple.0,
                    flashAmount: tuple.1,
                    preActions: tuple.2,
                    aavePool: tuple.3,
                    collateralAsset: tuple.4,
                    supplyAmount: tuple.5,
                    debtAsset: tuple.6,
                    borrowAmount: tuple.7,
                    interestRateMode: tuple.8,
                    postActions: tuple.9,
                    minDeltaFlashAsset: tuple.10,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolValue for FinancePlanV3 {
            type SolType = Self;
        }
        #[automatically_derived]
        impl alloy_sol_types::private::SolTypeValue<Self> for FinancePlanV3 {
            #[inline]
            fn stv_to_tokens(&self) -> <Self as alloy_sol_types::SolType>::Token<'_> {
                (
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.flashAsset,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.flashAmount),
                    <alloy::sol_types::sol_data::Array<
                        Call,
                    > as alloy_sol_types::SolType>::tokenize(&self.preActions),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.aavePool,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.collateralAsset,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.supplyAmount),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.debtAsset,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.borrowAmount),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.interestRateMode),
                    <alloy::sol_types::sol_data::Array<
                        Call,
                    > as alloy_sol_types::SolType>::tokenize(&self.postActions),
                    <alloy::sol_types::sol_data::Int<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.minDeltaFlashAsset),
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
        impl alloy_sol_types::SolType for FinancePlanV3 {
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
        impl alloy_sol_types::SolStruct for FinancePlanV3 {
            const NAME: &'static str = "FinancePlanV3";
            #[inline]
            fn eip712_root_type() -> alloy_sol_types::private::Cow<'static, str> {
                alloy_sol_types::private::Cow::Borrowed(
                    "FinancePlanV3(address flashAsset,uint256 flashAmount,Call[] preActions,address aavePool,address collateralAsset,uint256 supplyAmount,address debtAsset,uint256 borrowAmount,uint256 interestRateMode,Call[] postActions,int256 minDeltaFlashAsset)",
                )
            }
            #[inline]
            fn eip712_components() -> alloy_sol_types::private::Vec<
                alloy_sol_types::private::Cow<'static, str>,
            > {
                let mut components = alloy_sol_types::private::Vec::with_capacity(2);
                components
                    .push(<Call as alloy_sol_types::SolStruct>::eip712_root_type());
                components
                    .extend(<Call as alloy_sol_types::SolStruct>::eip712_components());
                components
                    .push(<Call as alloy_sol_types::SolStruct>::eip712_root_type());
                components
                    .extend(<Call as alloy_sol_types::SolStruct>::eip712_components());
                components
            }
            #[inline]
            fn eip712_encode_data(&self) -> alloy_sol_types::private::Vec<u8> {
                [
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::eip712_data_word(
                            &self.flashAsset,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.flashAmount)
                        .0,
                    <alloy::sol_types::sol_data::Array<
                        Call,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.preActions)
                        .0,
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::eip712_data_word(
                            &self.aavePool,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::eip712_data_word(
                            &self.collateralAsset,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.supplyAmount)
                        .0,
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::eip712_data_word(
                            &self.debtAsset,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.borrowAmount)
                        .0,
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(
                            &self.interestRateMode,
                        )
                        .0,
                    <alloy::sol_types::sol_data::Array<
                        Call,
                    > as alloy_sol_types::SolType>::eip712_data_word(&self.postActions)
                        .0,
                    <alloy::sol_types::sol_data::Int<
                        256,
                    > as alloy_sol_types::SolType>::eip712_data_word(
                            &self.minDeltaFlashAsset,
                        )
                        .0,
                ]
                    .concat()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::EventTopic for FinancePlanV3 {
            #[inline]
            fn topic_preimage_length(rust: &Self::RustType) -> usize {
                0usize
                    + <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.flashAsset,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.flashAmount,
                    )
                    + <alloy::sol_types::sol_data::Array<
                        Call,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.preActions,
                    )
                    + <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.aavePool,
                    )
                    + <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.collateralAsset,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.supplyAmount,
                    )
                    + <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.debtAsset,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.borrowAmount,
                    )
                    + <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.interestRateMode,
                    )
                    + <alloy::sol_types::sol_data::Array<
                        Call,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.postActions,
                    )
                    + <alloy::sol_types::sol_data::Int<
                        256,
                    > as alloy_sol_types::EventTopic>::topic_preimage_length(
                        &rust.minDeltaFlashAsset,
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
                    &rust.flashAsset,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.flashAmount,
                    out,
                );
                <alloy::sol_types::sol_data::Array<
                    Call,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.preActions,
                    out,
                );
                <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.aavePool,
                    out,
                );
                <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.collateralAsset,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.supplyAmount,
                    out,
                );
                <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.debtAsset,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.borrowAmount,
                    out,
                );
                <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.interestRateMode,
                    out,
                );
                <alloy::sol_types::sol_data::Array<
                    Call,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.postActions,
                    out,
                );
                <alloy::sol_types::sol_data::Int<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic_preimage(
                    &rust.minDeltaFlashAsset,
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
    /**Custom error with signature `CallFailed(uint256,bytes)` and selector `0x5c0dee5d`.
```solidity
error CallFailed(uint256 index, bytes returnData);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct CallFailed {
        #[allow(missing_docs)]
        pub index: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub returnData: alloy::sol_types::private::Bytes,
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
        impl ::core::convert::From<CallFailed> for UnderlyingRustTuple<'_> {
            fn from(value: CallFailed) -> Self {
                (value.index, value.returnData)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for CallFailed {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    index: tuple.0,
                    returnData: tuple.1,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for CallFailed {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "CallFailed(uint256,bytes)";
            const SELECTOR: [u8; 4] = [92u8, 13u8, 238u8, 93u8];
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
                        &self.returnData,
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
    /**Custom error with signature `EnforcedPause()` and selector `0xd93c0665`.
```solidity
error EnforcedPause();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct EnforcedPause;
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
        impl ::core::convert::From<EnforcedPause> for UnderlyingRustTuple<'_> {
            fn from(value: EnforcedPause) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for EnforcedPause {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for EnforcedPause {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "EnforcedPause()";
            const SELECTOR: [u8; 4] = [217u8, 60u8, 6u8, 101u8];
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
    /**Custom error with signature `ExpectedPause()` and selector `0x8dfc202b`.
```solidity
error ExpectedPause();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ExpectedPause;
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
        impl ::core::convert::From<ExpectedPause> for UnderlyingRustTuple<'_> {
            fn from(value: ExpectedPause) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for ExpectedPause {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for ExpectedPause {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "ExpectedPause()";
            const SELECTOR: [u8; 4] = [141u8, 252u8, 32u8, 43u8];
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
    /**Custom error with signature `FinanceUnprofitable(int256,int256)` and selector `0x0a3b09a1`.
```solidity
error FinanceUnprofitable(int256 delta, int256 minDelta);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct FinanceUnprofitable {
        #[allow(missing_docs)]
        pub delta: alloy::sol_types::private::primitives::aliases::I256,
        #[allow(missing_docs)]
        pub minDelta: alloy::sol_types::private::primitives::aliases::I256,
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
            alloy::sol_types::sol_data::Int<256>,
            alloy::sol_types::sol_data::Int<256>,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
            alloy::sol_types::private::primitives::aliases::I256,
            alloy::sol_types::private::primitives::aliases::I256,
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
        impl ::core::convert::From<FinanceUnprofitable> for UnderlyingRustTuple<'_> {
            fn from(value: FinanceUnprofitable) -> Self {
                (value.delta, value.minDelta)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for FinanceUnprofitable {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    delta: tuple.0,
                    minDelta: tuple.1,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for FinanceUnprofitable {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "FinanceUnprofitable(int256,int256)";
            const SELECTOR: [u8; 4] = [10u8, 59u8, 9u8, 161u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                (
                    <alloy::sol_types::sol_data::Int<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.delta),
                    <alloy::sol_types::sol_data::Int<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.minDelta),
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
    /**Custom error with signature `FlashAmountMismatch()` and selector `0x082b73b6`.
```solidity
error FlashAmountMismatch();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct FlashAmountMismatch;
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
        impl ::core::convert::From<FlashAmountMismatch> for UnderlyingRustTuple<'_> {
            fn from(value: FlashAmountMismatch) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for FlashAmountMismatch {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for FlashAmountMismatch {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "FlashAmountMismatch()";
            const SELECTOR: [u8; 4] = [8u8, 43u8, 115u8, 182u8];
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
    /**Custom error with signature `LpTransferLib__V4SetOperatorFailed()` and selector `0x5b661c8e`.
```solidity
error LpTransferLib__V4SetOperatorFailed();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct LpTransferLib__V4SetOperatorFailed;
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
        impl ::core::convert::From<LpTransferLib__V4SetOperatorFailed>
        for UnderlyingRustTuple<'_> {
            fn from(value: LpTransferLib__V4SetOperatorFailed) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for LpTransferLib__V4SetOperatorFailed {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for LpTransferLib__V4SetOperatorFailed {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "LpTransferLib__V4SetOperatorFailed()";
            const SELECTOR: [u8; 4] = [91u8, 102u8, 28u8, 142u8];
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
    /**Custom error with signature `LpTransferLib__V4TransferFailed()` and selector `0x31146f15`.
```solidity
error LpTransferLib__V4TransferFailed();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct LpTransferLib__V4TransferFailed;
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
        impl ::core::convert::From<LpTransferLib__V4TransferFailed>
        for UnderlyingRustTuple<'_> {
            fn from(value: LpTransferLib__V4TransferFailed) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for LpTransferLib__V4TransferFailed {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for LpTransferLib__V4TransferFailed {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "LpTransferLib__V4TransferFailed()";
            const SELECTOR: [u8; 4] = [49u8, 20u8, 111u8, 21u8];
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
    /**Custom error with signature `MevSafe__NativeSweepFailed()` and selector `0xd1d87603`.
```solidity
error MevSafe__NativeSweepFailed();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MevSafe__NativeSweepFailed;
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
        impl ::core::convert::From<MevSafe__NativeSweepFailed>
        for UnderlyingRustTuple<'_> {
            fn from(value: MevSafe__NativeSweepFailed) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for MevSafe__NativeSweepFailed {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for MevSafe__NativeSweepFailed {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "MevSafe__NativeSweepFailed()";
            const SELECTOR: [u8; 4] = [209u8, 216u8, 118u8, 3u8];
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
    /**Custom error with signature `MevSafe__UnsupportedFlashLender(address)` and selector `0xa0aad8bb`.
```solidity
error MevSafe__UnsupportedFlashLender(address lender);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MevSafe__UnsupportedFlashLender {
        #[allow(missing_docs)]
        pub lender: alloy::sol_types::private::Address,
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
        impl ::core::convert::From<MevSafe__UnsupportedFlashLender>
        for UnderlyingRustTuple<'_> {
            fn from(value: MevSafe__UnsupportedFlashLender) -> Self {
                (value.lender,)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for MevSafe__UnsupportedFlashLender {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self { lender: tuple.0 }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for MevSafe__UnsupportedFlashLender {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "MevSafe__UnsupportedFlashLender(address)";
            const SELECTOR: [u8; 4] = [160u8, 170u8, 216u8, 187u8];
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
                        &self.lender,
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
    /**Custom error with signature `NoActiveFlash()` and selector `0xf1f9f016`.
```solidity
error NoActiveFlash();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct NoActiveFlash;
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
        impl ::core::convert::From<NoActiveFlash> for UnderlyingRustTuple<'_> {
            fn from(value: NoActiveFlash) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for NoActiveFlash {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for NoActiveFlash {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "NoActiveFlash()";
            const SELECTOR: [u8; 4] = [241u8, 249u8, 240u8, 22u8];
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
    /**Custom error with signature `NotAuthorized()` and selector `0xea8e4eb5`.
```solidity
error NotAuthorized();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct NotAuthorized;
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
        impl ::core::convert::From<NotAuthorized> for UnderlyingRustTuple<'_> {
            fn from(value: NotAuthorized) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for NotAuthorized {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for NotAuthorized {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "NotAuthorized()";
            const SELECTOR: [u8; 4] = [234u8, 142u8, 78u8, 181u8];
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
    /**Custom error with signature `NotEntryPoint()` and selector `0xd663742a`.
```solidity
error NotEntryPoint();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct NotEntryPoint;
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
        impl ::core::convert::From<NotEntryPoint> for UnderlyingRustTuple<'_> {
            fn from(value: NotEntryPoint) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for NotEntryPoint {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for NotEntryPoint {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "NotEntryPoint()";
            const SELECTOR: [u8; 4] = [214u8, 99u8, 116u8, 42u8];
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
    /**Custom error with signature `NotFlashLender()` and selector `0x4a018a4a`.
```solidity
error NotFlashLender();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct NotFlashLender;
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
        impl ::core::convert::From<NotFlashLender> for UnderlyingRustTuple<'_> {
            fn from(value: NotFlashLender) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for NotFlashLender {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for NotFlashLender {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "NotFlashLender()";
            const SELECTOR: [u8; 4] = [74u8, 1u8, 138u8, 74u8];
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
    /**Custom error with signature `OnlyV3Vault()` and selector `0x8e5e503d`.
```solidity
error OnlyV3Vault();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct OnlyV3Vault;
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
        impl ::core::convert::From<OnlyV3Vault> for UnderlyingRustTuple<'_> {
            fn from(value: OnlyV3Vault) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for OnlyV3Vault {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for OnlyV3Vault {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "OnlyV3Vault()";
            const SELECTOR: [u8; 4] = [142u8, 94u8, 80u8, 61u8];
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
    /**Custom error with signature `OwnableInvalidOwner(address)` and selector `0x1e4fbdf7`.
```solidity
error OwnableInvalidOwner(address owner);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct OwnableInvalidOwner {
        #[allow(missing_docs)]
        pub owner: alloy::sol_types::private::Address,
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
        impl ::core::convert::From<OwnableInvalidOwner> for UnderlyingRustTuple<'_> {
            fn from(value: OwnableInvalidOwner) -> Self {
                (value.owner,)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for OwnableInvalidOwner {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self { owner: tuple.0 }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for OwnableInvalidOwner {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "OwnableInvalidOwner(address)";
            const SELECTOR: [u8; 4] = [30u8, 79u8, 189u8, 247u8];
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
                        &self.owner,
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
    /**Custom error with signature `OwnableUnauthorizedAccount(address)` and selector `0x118cdaa7`.
```solidity
error OwnableUnauthorizedAccount(address account);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct OwnableUnauthorizedAccount {
        #[allow(missing_docs)]
        pub account: alloy::sol_types::private::Address,
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
        impl ::core::convert::From<OwnableUnauthorizedAccount>
        for UnderlyingRustTuple<'_> {
            fn from(value: OwnableUnauthorizedAccount) -> Self {
                (value.account,)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for OwnableUnauthorizedAccount {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self { account: tuple.0 }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for OwnableUnauthorizedAccount {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "OwnableUnauthorizedAccount(address)";
            const SELECTOR: [u8; 4] = [17u8, 140u8, 218u8, 167u8];
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
                        &self.account,
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
    /**Custom error with signature `PlanHashMismatch()` and selector `0x620be623`.
```solidity
error PlanHashMismatch();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct PlanHashMismatch;
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
        impl ::core::convert::From<PlanHashMismatch> for UnderlyingRustTuple<'_> {
            fn from(value: PlanHashMismatch) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for PlanHashMismatch {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for PlanHashMismatch {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "PlanHashMismatch()";
            const SELECTOR: [u8; 4] = [98u8, 11u8, 230u8, 35u8];
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
    /**Custom error with signature `Reentrancy()` and selector `0xab143c06`.
```solidity
error Reentrancy();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Reentrancy;
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
        impl ::core::convert::From<Reentrancy> for UnderlyingRustTuple<'_> {
            fn from(value: Reentrancy) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for Reentrancy {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for Reentrancy {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "Reentrancy()";
            const SELECTOR: [u8; 4] = [171u8, 20u8, 60u8, 6u8];
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
    /**Custom error with signature `V3SettleShortfall(uint256,uint256)` and selector `0x7cc673c6`.
```solidity
error V3SettleShortfall(uint256 expected, uint256 credit);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct V3SettleShortfall {
        #[allow(missing_docs)]
        pub expected: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub credit: alloy::sol_types::private::primitives::aliases::U256,
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
            alloy::sol_types::sol_data::Uint<256>,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
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
        impl ::core::convert::From<V3SettleShortfall> for UnderlyingRustTuple<'_> {
            fn from(value: V3SettleShortfall) -> Self {
                (value.expected, value.credit)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for V3SettleShortfall {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    expected: tuple.0,
                    credit: tuple.1,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for V3SettleShortfall {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "V3SettleShortfall(uint256,uint256)";
            const SELECTOR: [u8; 4] = [124u8, 198u8, 115u8, 198u8];
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
                    > as alloy_sol_types::SolType>::tokenize(&self.expected),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.credit),
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
    /**Event with signature `CollateralizationFinanced(bytes32,address,uint256,address,uint256,address,uint256,uint256,int256)` and selector `0x53f2133355063b0787be9b73f9f2c3d6e14670e2a37dd462368b5d1a94e86b06`.
```solidity
event CollateralizationFinanced(bytes32 indexed opId, address indexed flashAsset, uint256 flashAmount, address collateralAsset, uint256 supplyAmount, address indexed debtAsset, uint256 borrowAmount, uint256 flashRepaid, int256 netDeltaFlash);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct CollateralizationFinanced {
        #[allow(missing_docs)]
        pub opId: alloy::sol_types::private::FixedBytes<32>,
        #[allow(missing_docs)]
        pub flashAsset: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub flashAmount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub collateralAsset: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub supplyAmount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub debtAsset: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub borrowAmount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub flashRepaid: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub netDeltaFlash: alloy::sol_types::private::primitives::aliases::I256,
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
        impl alloy_sol_types::SolEvent for CollateralizationFinanced {
            type DataTuple<'a> = (
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Int<256>,
            );
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "CollateralizationFinanced(bytes32,address,uint256,address,uint256,address,uint256,uint256,int256)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                83u8, 242u8, 19u8, 51u8, 85u8, 6u8, 59u8, 7u8, 135u8, 190u8, 155u8,
                115u8, 249u8, 242u8, 195u8, 214u8, 225u8, 70u8, 112u8, 226u8, 163u8,
                125u8, 212u8, 98u8, 54u8, 139u8, 93u8, 26u8, 148u8, 232u8, 107u8, 6u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    opId: topics.1,
                    flashAsset: topics.2,
                    flashAmount: data.0,
                    collateralAsset: data.1,
                    supplyAmount: data.2,
                    debtAsset: topics.3,
                    borrowAmount: data.3,
                    flashRepaid: data.4,
                    netDeltaFlash: data.5,
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
                    > as alloy_sol_types::SolType>::tokenize(&self.flashAmount),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.collateralAsset,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.supplyAmount),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.borrowAmount),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.flashRepaid),
                    <alloy::sol_types::sol_data::Int<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.netDeltaFlash),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (
                    Self::SIGNATURE_HASH.into(),
                    self.opId.clone(),
                    self.flashAsset.clone(),
                    self.debtAsset.clone(),
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
                out[1usize] = <alloy::sol_types::sol_data::FixedBytes<
                    32,
                > as alloy_sol_types::EventTopic>::encode_topic(&self.opId);
                out[2usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.flashAsset,
                );
                out[3usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.debtAsset,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for CollateralizationFinanced {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&CollateralizationFinanced> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(
                this: &CollateralizationFinanced,
            ) -> alloy_sol_types::private::LogData {
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
    /**Event with signature `Executed(address,uint256,bytes4)` and selector `0xe08f8925f45c337d514b07af2526e14449bcf90afd92efd8b611f17ebf419db0`.
```solidity
event Executed(address indexed target, uint256 value, bytes4 selector);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct Executed {
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
        impl alloy_sol_types::SolEvent for Executed {
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
            const SIGNATURE: &'static str = "Executed(address,uint256,bytes4)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                224u8, 143u8, 137u8, 37u8, 244u8, 92u8, 51u8, 125u8, 81u8, 75u8, 7u8,
                175u8, 37u8, 38u8, 225u8, 68u8, 73u8, 188u8, 249u8, 10u8, 253u8, 146u8,
                239u8, 216u8, 182u8, 17u8, 241u8, 126u8, 191u8, 65u8, 157u8, 176u8,
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
        impl alloy_sol_types::private::IntoLogData for Executed {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&Executed> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &Executed) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `LegacyDelegateeSet(address,bool)` and selector `0x0435416a5f48d41d5e5ede2d05c0e1ff6b1f71cb57176b1011ea7fbd8c725a83`.
```solidity
event LegacyDelegateeSet(address indexed account, bool allowed);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct LegacyDelegateeSet {
        #[allow(missing_docs)]
        pub account: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub allowed: bool,
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
        impl alloy_sol_types::SolEvent for LegacyDelegateeSet {
            type DataTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "LegacyDelegateeSet(address,bool)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                4u8, 53u8, 65u8, 106u8, 95u8, 72u8, 212u8, 29u8, 94u8, 94u8, 222u8, 45u8,
                5u8, 192u8, 225u8, 255u8, 107u8, 31u8, 113u8, 203u8, 87u8, 23u8, 107u8,
                16u8, 17u8, 234u8, 127u8, 189u8, 140u8, 114u8, 90u8, 131u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    account: topics.1,
                    allowed: data.0,
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
                        &self.allowed,
                    ),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(), self.account.clone())
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
                    &self.account,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for LegacyDelegateeSet {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&LegacyDelegateeSet> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &LegacyDelegateeSet) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `LpMoved(uint8,address,uint256,uint256,address)` and selector `0x35e259c1a781dc2649e76298b5a4d548c7905287ca3d7c4337480ce3fd05eb69`.
```solidity
event LpMoved(LpTransferLib.LpKind indexed kind, address indexed pool, uint256 idOrTokenId, uint256 amount, address indexed to);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct LpMoved {
        #[allow(missing_docs)]
        pub kind: <LpTransferLib::LpKind as alloy::sol_types::SolType>::RustType,
        #[allow(missing_docs)]
        pub pool: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub idOrTokenId: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub amount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub to: alloy::sol_types::private::Address,
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
        impl alloy_sol_types::SolEvent for LpMoved {
            type DataTuple<'a> = (
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                LpTransferLib::LpKind,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "LpMoved(uint8,address,uint256,uint256,address)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                53u8, 226u8, 89u8, 193u8, 167u8, 129u8, 220u8, 38u8, 73u8, 231u8, 98u8,
                152u8, 181u8, 164u8, 213u8, 72u8, 199u8, 144u8, 82u8, 135u8, 202u8, 61u8,
                124u8, 67u8, 55u8, 72u8, 12u8, 227u8, 253u8, 5u8, 235u8, 105u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    kind: topics.1,
                    pool: topics.2,
                    idOrTokenId: data.0,
                    amount: data.1,
                    to: topics.3,
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
                    > as alloy_sol_types::SolType>::tokenize(&self.idOrTokenId),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amount),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (
                    Self::SIGNATURE_HASH.into(),
                    self.kind.clone(),
                    self.pool.clone(),
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
                out[1usize] = <LpTransferLib::LpKind as alloy_sol_types::EventTopic>::encode_topic(
                    &self.kind,
                );
                out[2usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.pool,
                );
                out[3usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.to,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for LpMoved {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&LpMoved> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &LpMoved) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `OwnershipTransferStarted(address,address)` and selector `0x38d16b8cac22d99fc7c124b9cd0de2d3fa1faef420bfe791d8c362d765e22700`.
```solidity
event OwnershipTransferStarted(address indexed previousOwner, address indexed newOwner);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct OwnershipTransferStarted {
        #[allow(missing_docs)]
        pub previousOwner: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub newOwner: alloy::sol_types::private::Address,
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
        impl alloy_sol_types::SolEvent for OwnershipTransferStarted {
            type DataTuple<'a> = ();
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "OwnershipTransferStarted(address,address)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                56u8, 209u8, 107u8, 140u8, 172u8, 34u8, 217u8, 159u8, 199u8, 193u8, 36u8,
                185u8, 205u8, 13u8, 226u8, 211u8, 250u8, 31u8, 174u8, 244u8, 32u8, 191u8,
                231u8, 145u8, 216u8, 195u8, 98u8, 215u8, 101u8, 226u8, 39u8, 0u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    previousOwner: topics.1,
                    newOwner: topics.2,
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
                ()
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (
                    Self::SIGNATURE_HASH.into(),
                    self.previousOwner.clone(),
                    self.newOwner.clone(),
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
                    &self.previousOwner,
                );
                out[2usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.newOwner,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for OwnershipTransferStarted {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&OwnershipTransferStarted> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(
                this: &OwnershipTransferStarted,
            ) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `OwnershipTransferred(address,address)` and selector `0x8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e0`.
```solidity
event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct OwnershipTransferred {
        #[allow(missing_docs)]
        pub previousOwner: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub newOwner: alloy::sol_types::private::Address,
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
        impl alloy_sol_types::SolEvent for OwnershipTransferred {
            type DataTuple<'a> = ();
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "OwnershipTransferred(address,address)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                139u8, 224u8, 7u8, 156u8, 83u8, 22u8, 89u8, 20u8, 19u8, 68u8, 205u8,
                31u8, 208u8, 164u8, 242u8, 132u8, 25u8, 73u8, 127u8, 151u8, 34u8, 163u8,
                218u8, 175u8, 227u8, 180u8, 24u8, 111u8, 107u8, 100u8, 87u8, 224u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    previousOwner: topics.1,
                    newOwner: topics.2,
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
                ()
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (
                    Self::SIGNATURE_HASH.into(),
                    self.previousOwner.clone(),
                    self.newOwner.clone(),
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
                    &self.previousOwner,
                );
                out[2usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.newOwner,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for OwnershipTransferred {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&OwnershipTransferred> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &OwnershipTransferred) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `Paused(address)` and selector `0x62e78cea01bee320cd4e420270b5ea74000d11b0c9f74754ebdbfc544b05a258`.
```solidity
event Paused(address account);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct Paused {
        #[allow(missing_docs)]
        pub account: alloy::sol_types::private::Address,
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
        impl alloy_sol_types::SolEvent for Paused {
            type DataTuple<'a> = (alloy::sol_types::sol_data::Address,);
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (alloy_sol_types::sol_data::FixedBytes<32>,);
            const SIGNATURE: &'static str = "Paused(address)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                98u8, 231u8, 140u8, 234u8, 1u8, 190u8, 227u8, 32u8, 205u8, 78u8, 66u8,
                2u8, 112u8, 181u8, 234u8, 116u8, 0u8, 13u8, 17u8, 176u8, 201u8, 247u8,
                71u8, 84u8, 235u8, 219u8, 252u8, 84u8, 75u8, 5u8, 162u8, 88u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self { account: data.0 }
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
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.account,
                    ),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(),)
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
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for Paused {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&Paused> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &Paused) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `Received(bytes4,address,uint256,uint256)` and selector `0xd1ec030da0f99fccad1de2774dc98277b6e7693a549f4751c319cf87379bc30c`.
```solidity
event Received(bytes4 indexed standard, address indexed sender, uint256 indexed tokenId, uint256 value);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct Received {
        #[allow(missing_docs)]
        pub standard: alloy::sol_types::private::FixedBytes<4>,
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub tokenId: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub value: alloy::sol_types::private::primitives::aliases::U256,
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
        impl alloy_sol_types::SolEvent for Received {
            type DataTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::FixedBytes<4>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
            );
            const SIGNATURE: &'static str = "Received(bytes4,address,uint256,uint256)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                209u8, 236u8, 3u8, 13u8, 160u8, 249u8, 159u8, 204u8, 173u8, 29u8, 226u8,
                119u8, 77u8, 201u8, 130u8, 119u8, 182u8, 231u8, 105u8, 58u8, 84u8, 159u8,
                71u8, 81u8, 195u8, 25u8, 207u8, 135u8, 55u8, 155u8, 195u8, 12u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    standard: topics.1,
                    sender: topics.2,
                    tokenId: topics.3,
                    value: data.0,
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
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (
                    Self::SIGNATURE_HASH.into(),
                    self.standard.clone(),
                    self.sender.clone(),
                    self.tokenId.clone(),
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
                out[1usize] = <alloy::sol_types::sol_data::FixedBytes<
                    4,
                > as alloy_sol_types::EventTopic>::encode_topic(&self.standard);
                out[2usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.sender,
                );
                out[3usize] = <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic(&self.tokenId);
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for Received {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&Received> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &Received) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `SignerSet(address,bool)` and selector `0xfc4acb499491cd850a8a21ab98c7f128850c0f0e5f1a875a62b7fa055c2ecf19`.
```solidity
event SignerSet(address indexed signer, bool allowed);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct SignerSet {
        #[allow(missing_docs)]
        pub signer: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub allowed: bool,
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
        impl alloy_sol_types::SolEvent for SignerSet {
            type DataTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "SignerSet(address,bool)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                252u8, 74u8, 203u8, 73u8, 148u8, 145u8, 205u8, 133u8, 10u8, 138u8, 33u8,
                171u8, 152u8, 199u8, 241u8, 40u8, 133u8, 12u8, 15u8, 14u8, 95u8, 26u8,
                135u8, 90u8, 98u8, 183u8, 250u8, 5u8, 92u8, 46u8, 207u8, 25u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    signer: topics.1,
                    allowed: data.0,
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
                        &self.allowed,
                    ),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(), self.signer.clone())
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
                    &self.signer,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for SignerSet {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&SignerSet> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &SignerSet) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `ThresholdSet(uint256)` and selector `0x6e8a187d7944998085dbd1f16b84c51c903bb727536cdba86962439aded2cfd7`.
```solidity
event ThresholdSet(uint256 newThreshold);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct ThresholdSet {
        #[allow(missing_docs)]
        pub newThreshold: alloy::sol_types::private::primitives::aliases::U256,
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
        impl alloy_sol_types::SolEvent for ThresholdSet {
            type DataTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (alloy_sol_types::sol_data::FixedBytes<32>,);
            const SIGNATURE: &'static str = "ThresholdSet(uint256)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                110u8, 138u8, 24u8, 125u8, 121u8, 68u8, 153u8, 128u8, 133u8, 219u8,
                209u8, 241u8, 107u8, 132u8, 197u8, 28u8, 144u8, 59u8, 183u8, 39u8, 83u8,
                108u8, 219u8, 168u8, 105u8, 98u8, 67u8, 154u8, 222u8, 210u8, 207u8, 215u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self { newThreshold: data.0 }
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
                    > as alloy_sol_types::SolType>::tokenize(&self.newThreshold),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(),)
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
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for ThresholdSet {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&ThresholdSet> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &ThresholdSet) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `Unpaused(address)` and selector `0x5db9ee0a495bf2e6ff9c91a7834c1ba4fdd244a5e8aa4e537bd38aeae4b073aa`.
```solidity
event Unpaused(address account);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct Unpaused {
        #[allow(missing_docs)]
        pub account: alloy::sol_types::private::Address,
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
        impl alloy_sol_types::SolEvent for Unpaused {
            type DataTuple<'a> = (alloy::sol_types::sol_data::Address,);
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (alloy_sol_types::sol_data::FixedBytes<32>,);
            const SIGNATURE: &'static str = "Unpaused(address)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                93u8, 185u8, 238u8, 10u8, 73u8, 91u8, 242u8, 230u8, 255u8, 156u8, 145u8,
                167u8, 131u8, 76u8, 27u8, 164u8, 253u8, 210u8, 68u8, 165u8, 232u8, 170u8,
                78u8, 83u8, 123u8, 211u8, 138u8, 234u8, 228u8, 176u8, 115u8, 170u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self { account: data.0 }
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
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.account,
                    ),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(),)
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
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for Unpaused {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&Unpaused> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &Unpaused) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `V4OperatorSet(address,address,bool)` and selector `0x82e43140fc41dbcab6163bc2bd7ddf40d7477286fb7c95c37c1dbc957756a9ba`.
```solidity
event V4OperatorSet(address indexed poolManager, address indexed operator, bool approved);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct V4OperatorSet {
        #[allow(missing_docs)]
        pub poolManager: alloy::sol_types::private::Address,
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
        impl alloy_sol_types::SolEvent for V4OperatorSet {
            type DataTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "V4OperatorSet(address,address,bool)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                130u8, 228u8, 49u8, 64u8, 252u8, 65u8, 219u8, 202u8, 182u8, 22u8, 59u8,
                194u8, 189u8, 125u8, 223u8, 64u8, 215u8, 71u8, 114u8, 134u8, 251u8,
                124u8, 149u8, 195u8, 124u8, 29u8, 188u8, 149u8, 119u8, 86u8, 169u8, 186u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    poolManager: topics.1,
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
                (
                    Self::SIGNATURE_HASH.into(),
                    self.poolManager.clone(),
                    self.operator.clone(),
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
                    &self.poolManager,
                );
                out[2usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.operator,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for V4OperatorSet {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&V4OperatorSet> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &V4OperatorSet) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    /**Constructor`.
```solidity
constructor(address initialOwner, address permissions);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct constructorCall {
        #[allow(missing_docs)]
        pub initialOwner: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub permissions: alloy::sol_types::private::Address,
    }
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
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
            impl ::core::convert::From<constructorCall> for UnderlyingRustTuple<'_> {
                fn from(value: constructorCall) -> Self {
                    (value.initialOwner, value.permissions)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for constructorCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        initialOwner: tuple.0,
                        permissions: tuple.1,
                    }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolConstructor for constructorCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
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
                        &self.initialOwner,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.permissions,
                    ),
                )
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `AAVE_V3_POOL()` and selector `0x3b303705`.
```solidity
function AAVE_V3_POOL() external view returns (address);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct AAVE_V3_POOLCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`AAVE_V3_POOL()`](AAVE_V3_POOLCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct AAVE_V3_POOLReturn {
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
            impl ::core::convert::From<AAVE_V3_POOLCall> for UnderlyingRustTuple<'_> {
                fn from(value: AAVE_V3_POOLCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for AAVE_V3_POOLCall {
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
            impl ::core::convert::From<AAVE_V3_POOLReturn> for UnderlyingRustTuple<'_> {
                fn from(value: AAVE_V3_POOLReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for AAVE_V3_POOLReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for AAVE_V3_POOLCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::Address;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Address,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "AAVE_V3_POOL()";
            const SELECTOR: [u8; 4] = [59u8, 48u8, 55u8, 5u8];
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
                        let r: AAVE_V3_POOLReturn = r.into();
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
                        let r: AAVE_V3_POOLReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `BALANCER_V2_VAULT()` and selector `0x213c5033`.
```solidity
function BALANCER_V2_VAULT() external view returns (address);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct BALANCER_V2_VAULTCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`BALANCER_V2_VAULT()`](BALANCER_V2_VAULTCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct BALANCER_V2_VAULTReturn {
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
            impl ::core::convert::From<BALANCER_V2_VAULTCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: BALANCER_V2_VAULTCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for BALANCER_V2_VAULTCall {
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
            impl ::core::convert::From<BALANCER_V2_VAULTReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: BALANCER_V2_VAULTReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for BALANCER_V2_VAULTReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for BALANCER_V2_VAULTCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::Address;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Address,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "BALANCER_V2_VAULT()";
            const SELECTOR: [u8; 4] = [33u8, 60u8, 80u8, 51u8];
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
                        let r: BALANCER_V2_VAULTReturn = r.into();
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
                        let r: BALANCER_V2_VAULTReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `BALANCER_V3_VAULT()` and selector `0xa4c01bbb`.
```solidity
function BALANCER_V3_VAULT() external view returns (address);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct BALANCER_V3_VAULTCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`BALANCER_V3_VAULT()`](BALANCER_V3_VAULTCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct BALANCER_V3_VAULTReturn {
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
            impl ::core::convert::From<BALANCER_V3_VAULTCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: BALANCER_V3_VAULTCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for BALANCER_V3_VAULTCall {
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
            impl ::core::convert::From<BALANCER_V3_VAULTReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: BALANCER_V3_VAULTReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for BALANCER_V3_VAULTReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for BALANCER_V3_VAULTCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::Address;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Address,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "BALANCER_V3_VAULT()";
            const SELECTOR: [u8; 4] = [164u8, 192u8, 27u8, 187u8];
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
                        let r: BALANCER_V3_VAULTReturn = r.into();
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
                        let r: BALANCER_V3_VAULTReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `ENTRY_POINT()` and selector `0x94430fa5`.
```solidity
function ENTRY_POINT() external view returns (address);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ENTRY_POINTCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`ENTRY_POINT()`](ENTRY_POINTCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ENTRY_POINTReturn {
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
            impl ::core::convert::From<ENTRY_POINTCall> for UnderlyingRustTuple<'_> {
                fn from(value: ENTRY_POINTCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for ENTRY_POINTCall {
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
            impl ::core::convert::From<ENTRY_POINTReturn> for UnderlyingRustTuple<'_> {
                fn from(value: ENTRY_POINTReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for ENTRY_POINTReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for ENTRY_POINTCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::Address;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Address,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "ENTRY_POINT()";
            const SELECTOR: [u8; 4] = [148u8, 67u8, 15u8, 165u8];
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
                        let r: ENTRY_POINTReturn = r.into();
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
                        let r: ENTRY_POINTReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `MORPHO_BLUE()` and selector `0x99fec7a0`.
```solidity
function MORPHO_BLUE() external view returns (address);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MORPHO_BLUECall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`MORPHO_BLUE()`](MORPHO_BLUECall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MORPHO_BLUEReturn {
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
            impl ::core::convert::From<MORPHO_BLUECall> for UnderlyingRustTuple<'_> {
                fn from(value: MORPHO_BLUECall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for MORPHO_BLUECall {
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
            impl ::core::convert::From<MORPHO_BLUEReturn> for UnderlyingRustTuple<'_> {
                fn from(value: MORPHO_BLUEReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for MORPHO_BLUEReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for MORPHO_BLUECall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::Address;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Address,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "MORPHO_BLUE()";
            const SELECTOR: [u8; 4] = [153u8, 254u8, 199u8, 160u8];
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
                        let r: MORPHO_BLUEReturn = r.into();
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
                        let r: MORPHO_BLUEReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `PERMISSIONS()` and selector `0xf434c914`.
```solidity
function PERMISSIONS() external view returns (address);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct PERMISSIONSCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`PERMISSIONS()`](PERMISSIONSCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct PERMISSIONSReturn {
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
            impl ::core::convert::From<PERMISSIONSCall> for UnderlyingRustTuple<'_> {
                fn from(value: PERMISSIONSCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for PERMISSIONSCall {
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
            impl ::core::convert::From<PERMISSIONSReturn> for UnderlyingRustTuple<'_> {
                fn from(value: PERMISSIONSReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for PERMISSIONSReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for PERMISSIONSCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::Address;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Address,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "PERMISSIONS()";
            const SELECTOR: [u8; 4] = [244u8, 52u8, 201u8, 20u8];
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
                        let r: PERMISSIONSReturn = r.into();
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
                        let r: PERMISSIONSReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `aaveBorrow(address,address,uint256,uint256)` and selector `0x804a0566`.
```solidity
function aaveBorrow(address pool, address asset, uint256 amount, uint256 interestRateMode) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct aaveBorrowCall {
        #[allow(missing_docs)]
        pub pool: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub asset: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub amount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub interestRateMode: alloy::sol_types::private::primitives::aliases::U256,
    }
    ///Container type for the return parameters of the [`aaveBorrow(address,address,uint256,uint256)`](aaveBorrowCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct aaveBorrowReturn {}
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
            impl ::core::convert::From<aaveBorrowCall> for UnderlyingRustTuple<'_> {
                fn from(value: aaveBorrowCall) -> Self {
                    (value.pool, value.asset, value.amount, value.interestRateMode)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for aaveBorrowCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        pool: tuple.0,
                        asset: tuple.1,
                        amount: tuple.2,
                        interestRateMode: tuple.3,
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
            impl ::core::convert::From<aaveBorrowReturn> for UnderlyingRustTuple<'_> {
                fn from(value: aaveBorrowReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for aaveBorrowReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl aaveBorrowReturn {
            fn _tokenize(
                &self,
            ) -> <aaveBorrowCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for aaveBorrowCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = aaveBorrowReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "aaveBorrow(address,address,uint256,uint256)";
            const SELECTOR: [u8; 4] = [128u8, 74u8, 5u8, 102u8];
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
                        &self.pool,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.asset,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amount),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.interestRateMode),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                aaveBorrowReturn::_tokenize(ret)
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
    /**Function with signature `aaveRepay(address,address,uint256,uint256)` and selector `0xfdb02098`.
```solidity
function aaveRepay(address pool, address asset, uint256 amount, uint256 interestRateMode) external returns (uint256 repaid);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct aaveRepayCall {
        #[allow(missing_docs)]
        pub pool: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub asset: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub amount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub interestRateMode: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`aaveRepay(address,address,uint256,uint256)`](aaveRepayCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct aaveRepayReturn {
        #[allow(missing_docs)]
        pub repaid: alloy::sol_types::private::primitives::aliases::U256,
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
            impl ::core::convert::From<aaveRepayCall> for UnderlyingRustTuple<'_> {
                fn from(value: aaveRepayCall) -> Self {
                    (value.pool, value.asset, value.amount, value.interestRateMode)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for aaveRepayCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        pool: tuple.0,
                        asset: tuple.1,
                        amount: tuple.2,
                        interestRateMode: tuple.3,
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
            impl ::core::convert::From<aaveRepayReturn> for UnderlyingRustTuple<'_> {
                fn from(value: aaveRepayReturn) -> Self {
                    (value.repaid,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for aaveRepayReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { repaid: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for aaveRepayCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
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
            const SIGNATURE: &'static str = "aaveRepay(address,address,uint256,uint256)";
            const SELECTOR: [u8; 4] = [253u8, 176u8, 32u8, 152u8];
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
                        &self.pool,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.asset,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amount),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.interestRateMode),
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
                        let r: aaveRepayReturn = r.into();
                        r.repaid
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
                        let r: aaveRepayReturn = r.into();
                        r.repaid
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `aaveSupply(address,address,uint256)` and selector `0xf6eb79c7`.
```solidity
function aaveSupply(address pool, address asset, uint256 amount) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct aaveSupplyCall {
        #[allow(missing_docs)]
        pub pool: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub asset: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub amount: alloy::sol_types::private::primitives::aliases::U256,
    }
    ///Container type for the return parameters of the [`aaveSupply(address,address,uint256)`](aaveSupplyCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct aaveSupplyReturn {}
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
            impl ::core::convert::From<aaveSupplyCall> for UnderlyingRustTuple<'_> {
                fn from(value: aaveSupplyCall) -> Self {
                    (value.pool, value.asset, value.amount)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for aaveSupplyCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        pool: tuple.0,
                        asset: tuple.1,
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
            impl ::core::convert::From<aaveSupplyReturn> for UnderlyingRustTuple<'_> {
                fn from(value: aaveSupplyReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for aaveSupplyReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl aaveSupplyReturn {
            fn _tokenize(
                &self,
            ) -> <aaveSupplyCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for aaveSupplyCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = aaveSupplyReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "aaveSupply(address,address,uint256)";
            const SELECTOR: [u8; 4] = [246u8, 235u8, 121u8, 199u8];
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
                        &self.pool,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.asset,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amount),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                aaveSupplyReturn::_tokenize(ret)
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
    /**Function with signature `aaveWithdraw(address,address,uint256)` and selector `0xd475c098`.
```solidity
function aaveWithdraw(address pool, address asset, uint256 amount) external returns (uint256 withdrawn);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct aaveWithdrawCall {
        #[allow(missing_docs)]
        pub pool: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub asset: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub amount: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`aaveWithdraw(address,address,uint256)`](aaveWithdrawCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct aaveWithdrawReturn {
        #[allow(missing_docs)]
        pub withdrawn: alloy::sol_types::private::primitives::aliases::U256,
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
            impl ::core::convert::From<aaveWithdrawCall> for UnderlyingRustTuple<'_> {
                fn from(value: aaveWithdrawCall) -> Self {
                    (value.pool, value.asset, value.amount)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for aaveWithdrawCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        pool: tuple.0,
                        asset: tuple.1,
                        amount: tuple.2,
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
            impl ::core::convert::From<aaveWithdrawReturn> for UnderlyingRustTuple<'_> {
                fn from(value: aaveWithdrawReturn) -> Self {
                    (value.withdrawn,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for aaveWithdrawReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { withdrawn: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for aaveWithdrawCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
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
            const SIGNATURE: &'static str = "aaveWithdraw(address,address,uint256)";
            const SELECTOR: [u8; 4] = [212u8, 117u8, 192u8, 152u8];
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
                        &self.pool,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.asset,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amount),
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
                        let r: aaveWithdrawReturn = r.into();
                        r.withdrawn
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
                        let r: aaveWithdrawReturn = r.into();
                        r.withdrawn
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `acceptOwnership()` and selector `0x79ba5097`.
```solidity
function acceptOwnership() external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct acceptOwnershipCall;
    ///Container type for the return parameters of the [`acceptOwnership()`](acceptOwnershipCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct acceptOwnershipReturn {}
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
            impl ::core::convert::From<acceptOwnershipCall> for UnderlyingRustTuple<'_> {
                fn from(value: acceptOwnershipCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for acceptOwnershipCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self
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
            impl ::core::convert::From<acceptOwnershipReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: acceptOwnershipReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for acceptOwnershipReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl acceptOwnershipReturn {
            fn _tokenize(
                &self,
            ) -> <acceptOwnershipCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for acceptOwnershipCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = acceptOwnershipReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "acceptOwnership()";
            const SELECTOR: [u8; 4] = [121u8, 186u8, 80u8, 151u8];
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
                acceptOwnershipReturn::_tokenize(ret)
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
    /**Function with signature `execute((address,uint256,bytes))` and selector `0x5c1c6dcd`.
```solidity
function execute(Call memory c) external payable;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct executeCall {
        #[allow(missing_docs)]
        pub c: <Call as alloy::sol_types::SolType>::RustType,
    }
    ///Container type for the return parameters of the [`execute((address,uint256,bytes))`](executeCall) function.
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
            type UnderlyingSolTuple<'a> = (Call,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                <Call as alloy::sol_types::SolType>::RustType,
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
                    (value.c,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for executeCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { c: tuple.0 }
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
            type Parameters<'a> = (Call,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = executeReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "execute((address,uint256,bytes))";
            const SELECTOR: [u8; 4] = [92u8, 28u8, 109u8, 205u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                (<Call as alloy_sol_types::SolType>::tokenize(&self.c),)
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
function executeBatch(Call[] memory calls) external payable;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct executeBatchCall {
        #[allow(missing_docs)]
        pub calls: alloy::sol_types::private::Vec<
            <Call as alloy::sol_types::SolType>::RustType,
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
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Array<Call>,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Vec<
                    <Call as alloy::sol_types::SolType>::RustType,
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
            type Parameters<'a> = (alloy::sol_types::sol_data::Array<Call>,);
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
                        Call,
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
    /**Function with signature `executeFinanceUnlock((address,uint256,(address,uint256,bytes)[],address,address,uint256,address,uint256,uint256,(address,uint256,bytes)[],int256))` and selector `0xd41e5d3f`.
```solidity
function executeFinanceUnlock(FinancePlanV3 memory plan) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct executeFinanceUnlockCall {
        #[allow(missing_docs)]
        pub plan: <FinancePlanV3 as alloy::sol_types::SolType>::RustType,
    }
    ///Container type for the return parameters of the [`executeFinanceUnlock((address,uint256,(address,uint256,bytes)[],address,address,uint256,address,uint256,uint256,(address,uint256,bytes)[],int256))`](executeFinanceUnlockCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct executeFinanceUnlockReturn {}
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
            type UnderlyingSolTuple<'a> = (FinancePlanV3,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                <FinancePlanV3 as alloy::sol_types::SolType>::RustType,
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
            impl ::core::convert::From<executeFinanceUnlockCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: executeFinanceUnlockCall) -> Self {
                    (value.plan,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for executeFinanceUnlockCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { plan: tuple.0 }
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
            impl ::core::convert::From<executeFinanceUnlockReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: executeFinanceUnlockReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for executeFinanceUnlockReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl executeFinanceUnlockReturn {
            fn _tokenize(
                &self,
            ) -> <executeFinanceUnlockCall as alloy_sol_types::SolCall>::ReturnToken<
                '_,
            > {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for executeFinanceUnlockCall {
            type Parameters<'a> = (FinancePlanV3,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = executeFinanceUnlockReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "executeFinanceUnlock((address,uint256,(address,uint256,bytes)[],address,address,uint256,address,uint256,uint256,(address,uint256,bytes)[],int256))";
            const SELECTOR: [u8; 4] = [212u8, 30u8, 93u8, 63u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                (<FinancePlanV3 as alloy_sol_types::SolType>::tokenize(&self.plan),)
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                executeFinanceUnlockReturn::_tokenize(ret)
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
    /**Function with signature `flashCollateralize((address,address,uint256,(address,uint256,bytes)[],address,address,uint256,address,uint256,uint256,(address,uint256,bytes)[],int256))` and selector `0x5303ad28`.
```solidity
function flashCollateralize(FinancePlan memory plan) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct flashCollateralizeCall {
        #[allow(missing_docs)]
        pub plan: <FinancePlan as alloy::sol_types::SolType>::RustType,
    }
    ///Container type for the return parameters of the [`flashCollateralize((address,address,uint256,(address,uint256,bytes)[],address,address,uint256,address,uint256,uint256,(address,uint256,bytes)[],int256))`](flashCollateralizeCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct flashCollateralizeReturn {}
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
            type UnderlyingSolTuple<'a> = (FinancePlan,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                <FinancePlan as alloy::sol_types::SolType>::RustType,
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
            impl ::core::convert::From<flashCollateralizeCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: flashCollateralizeCall) -> Self {
                    (value.plan,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for flashCollateralizeCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { plan: tuple.0 }
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
            impl ::core::convert::From<flashCollateralizeReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: flashCollateralizeReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for flashCollateralizeReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl flashCollateralizeReturn {
            fn _tokenize(
                &self,
            ) -> <flashCollateralizeCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for flashCollateralizeCall {
            type Parameters<'a> = (FinancePlan,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = flashCollateralizeReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "flashCollateralize((address,address,uint256,(address,uint256,bytes)[],address,address,uint256,address,uint256,uint256,(address,uint256,bytes)[],int256))";
            const SELECTOR: [u8; 4] = [83u8, 3u8, 173u8, 40u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                (<FinancePlan as alloy_sol_types::SolType>::tokenize(&self.plan),)
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                flashCollateralizeReturn::_tokenize(ret)
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
    /**Function with signature `flashCollateralizeV3((address,uint256,(address,uint256,bytes)[],address,address,uint256,address,uint256,uint256,(address,uint256,bytes)[],int256))` and selector `0x8f205a1d`.
```solidity
function flashCollateralizeV3(FinancePlanV3 memory plan) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct flashCollateralizeV3Call {
        #[allow(missing_docs)]
        pub plan: <FinancePlanV3 as alloy::sol_types::SolType>::RustType,
    }
    ///Container type for the return parameters of the [`flashCollateralizeV3((address,uint256,(address,uint256,bytes)[],address,address,uint256,address,uint256,uint256,(address,uint256,bytes)[],int256))`](flashCollateralizeV3Call) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct flashCollateralizeV3Return {}
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
            type UnderlyingSolTuple<'a> = (FinancePlanV3,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                <FinancePlanV3 as alloy::sol_types::SolType>::RustType,
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
            impl ::core::convert::From<flashCollateralizeV3Call>
            for UnderlyingRustTuple<'_> {
                fn from(value: flashCollateralizeV3Call) -> Self {
                    (value.plan,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for flashCollateralizeV3Call {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { plan: tuple.0 }
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
            impl ::core::convert::From<flashCollateralizeV3Return>
            for UnderlyingRustTuple<'_> {
                fn from(value: flashCollateralizeV3Return) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for flashCollateralizeV3Return {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl flashCollateralizeV3Return {
            fn _tokenize(
                &self,
            ) -> <flashCollateralizeV3Call as alloy_sol_types::SolCall>::ReturnToken<
                '_,
            > {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for flashCollateralizeV3Call {
            type Parameters<'a> = (FinancePlanV3,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = flashCollateralizeV3Return;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "flashCollateralizeV3((address,uint256,(address,uint256,bytes)[],address,address,uint256,address,uint256,uint256,(address,uint256,bytes)[],int256))";
            const SELECTOR: [u8; 4] = [143u8, 32u8, 90u8, 29u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                (<FinancePlanV3 as alloy_sol_types::SolType>::tokenize(&self.plan),)
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                flashCollateralizeV3Return::_tokenize(ret)
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
    /**Function with signature `isAuthorized(address,address,bytes4)` and selector `0xe99f5b16`.
```solidity
function isAuthorized(address account, address target, bytes4 selector) external view returns (bool);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct isAuthorizedCall {
        #[allow(missing_docs)]
        pub account: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub target: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub selector: alloy::sol_types::private::FixedBytes<4>,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`isAuthorized(address,address,bytes4)`](isAuthorizedCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct isAuthorizedReturn {
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
            type UnderlyingSolTuple<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::FixedBytes<4>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Address,
                alloy::sol_types::private::Address,
                alloy::sol_types::private::FixedBytes<4>,
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
            impl ::core::convert::From<isAuthorizedCall> for UnderlyingRustTuple<'_> {
                fn from(value: isAuthorizedCall) -> Self {
                    (value.account, value.target, value.selector)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for isAuthorizedCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        account: tuple.0,
                        target: tuple.1,
                        selector: tuple.2,
                    }
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
            impl ::core::convert::From<isAuthorizedReturn> for UnderlyingRustTuple<'_> {
                fn from(value: isAuthorizedReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for isAuthorizedReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for isAuthorizedCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::FixedBytes<4>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = bool;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "isAuthorized(address,address,bytes4)";
            const SELECTOR: [u8; 4] = [233u8, 159u8, 91u8, 22u8];
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
                        &self.account,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.target,
                    ),
                    <alloy::sol_types::sol_data::FixedBytes<
                        4,
                    > as alloy_sol_types::SolType>::tokenize(&self.selector),
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
                        let r: isAuthorizedReturn = r.into();
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
                        let r: isAuthorizedReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `isSigner(address)` and selector `0x7df73e27`.
```solidity
function isSigner(address) external view returns (bool);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct isSignerCall(pub alloy::sol_types::private::Address);
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`isSigner(address)`](isSignerCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct isSignerReturn {
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
            impl ::core::convert::From<isSignerCall> for UnderlyingRustTuple<'_> {
                fn from(value: isSignerCall) -> Self {
                    (value.0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for isSignerCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self(tuple.0)
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
            impl ::core::convert::From<isSignerReturn> for UnderlyingRustTuple<'_> {
                fn from(value: isSignerReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for isSignerReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for isSignerCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Address,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = bool;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "isSigner(address)";
            const SELECTOR: [u8; 4] = [125u8, 247u8, 62u8, 39u8];
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
                        &self.0,
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
                        let r: isSignerReturn = r.into();
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
                        let r: isSignerReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `isValidSignature(bytes32,bytes)` and selector `0x1626ba7e`.
```solidity
function isValidSignature(bytes32 hash, bytes memory signature) external view returns (bytes4);
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
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for isValidSignatureReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
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
                        let r: isValidSignatureReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `legacyDelegatees(address)` and selector `0x7d281caa`.
```solidity
function legacyDelegatees(address) external view returns (bool);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct legacyDelegateesCall(pub alloy::sol_types::private::Address);
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`legacyDelegatees(address)`](legacyDelegateesCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct legacyDelegateesReturn {
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
            impl ::core::convert::From<legacyDelegateesCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: legacyDelegateesCall) -> Self {
                    (value.0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for legacyDelegateesCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self(tuple.0)
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
            impl ::core::convert::From<legacyDelegateesReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: legacyDelegateesReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for legacyDelegateesReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for legacyDelegateesCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Address,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = bool;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "legacyDelegatees(address)";
            const SELECTOR: [u8; 4] = [125u8, 40u8, 28u8, 170u8];
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
                        &self.0,
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
                        let r: legacyDelegateesReturn = r.into();
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
                        let r: legacyDelegateesReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `moveLpV3Nft(address,uint256,address)` and selector `0xb06c944a`.
```solidity
function moveLpV3Nft(address positionManager, uint256 tokenId, address to) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct moveLpV3NftCall {
        #[allow(missing_docs)]
        pub positionManager: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub tokenId: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub to: alloy::sol_types::private::Address,
    }
    ///Container type for the return parameters of the [`moveLpV3Nft(address,uint256,address)`](moveLpV3NftCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct moveLpV3NftReturn {}
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
                alloy::sol_types::sol_data::Address,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Address,
                alloy::sol_types::private::primitives::aliases::U256,
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
            impl ::core::convert::From<moveLpV3NftCall> for UnderlyingRustTuple<'_> {
                fn from(value: moveLpV3NftCall) -> Self {
                    (value.positionManager, value.tokenId, value.to)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for moveLpV3NftCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        positionManager: tuple.0,
                        tokenId: tuple.1,
                        to: tuple.2,
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
            impl ::core::convert::From<moveLpV3NftReturn> for UnderlyingRustTuple<'_> {
                fn from(value: moveLpV3NftReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for moveLpV3NftReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl moveLpV3NftReturn {
            fn _tokenize(
                &self,
            ) -> <moveLpV3NftCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for moveLpV3NftCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Address,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = moveLpV3NftReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "moveLpV3Nft(address,uint256,address)";
            const SELECTOR: [u8; 4] = [176u8, 108u8, 148u8, 74u8];
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
                        &self.positionManager,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.tokenId),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.to,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                moveLpV3NftReturn::_tokenize(ret)
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
    /**Function with signature `moveLpV4(address,uint256,uint256,address)` and selector `0x376794d8`.
```solidity
function moveLpV4(address poolManager, uint256 id, uint256 amount, address to) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct moveLpV4Call {
        #[allow(missing_docs)]
        pub poolManager: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub id: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub amount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub to: alloy::sol_types::private::Address,
    }
    ///Container type for the return parameters of the [`moveLpV4(address,uint256,uint256,address)`](moveLpV4Call) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct moveLpV4Return {}
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
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Address,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Address,
                alloy::sol_types::private::primitives::aliases::U256,
                alloy::sol_types::private::primitives::aliases::U256,
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
            impl ::core::convert::From<moveLpV4Call> for UnderlyingRustTuple<'_> {
                fn from(value: moveLpV4Call) -> Self {
                    (value.poolManager, value.id, value.amount, value.to)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for moveLpV4Call {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        poolManager: tuple.0,
                        id: tuple.1,
                        amount: tuple.2,
                        to: tuple.3,
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
            impl ::core::convert::From<moveLpV4Return> for UnderlyingRustTuple<'_> {
                fn from(value: moveLpV4Return) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for moveLpV4Return {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl moveLpV4Return {
            fn _tokenize(
                &self,
            ) -> <moveLpV4Call as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for moveLpV4Call {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Address,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = moveLpV4Return;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "moveLpV4(address,uint256,uint256,address)";
            const SELECTOR: [u8; 4] = [55u8, 103u8, 148u8, 216u8];
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
                        &self.poolManager,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.id),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amount),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.to,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                moveLpV4Return::_tokenize(ret)
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
    /**Function with signature `onERC1155BatchReceived(address,address,uint256[],uint256[],bytes)` and selector `0xbc197c81`.
```solidity
function onERC1155BatchReceived(address operator, address from, uint256[] memory ids, uint256[] memory values, bytes memory data) external returns (bytes4);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct onERC1155BatchReceivedCall {
        #[allow(missing_docs)]
        pub operator: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub from: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub ids: alloy::sol_types::private::Vec<
            alloy::sol_types::private::primitives::aliases::U256,
        >,
        #[allow(missing_docs)]
        pub values: alloy::sol_types::private::Vec<
            alloy::sol_types::private::primitives::aliases::U256,
        >,
        #[allow(missing_docs)]
        pub data: alloy::sol_types::private::Bytes,
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
                    (value.operator, value.from, value.ids, value.values, value.data)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for onERC1155BatchReceivedCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        operator: tuple.0,
                        from: tuple.1,
                        ids: tuple.2,
                        values: tuple.3,
                        data: tuple.4,
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
                        &self.operator,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.from,
                    ),
                    <alloy::sol_types::sol_data::Array<
                        alloy::sol_types::sol_data::Uint<256>,
                    > as alloy_sol_types::SolType>::tokenize(&self.ids),
                    <alloy::sol_types::sol_data::Array<
                        alloy::sol_types::sol_data::Uint<256>,
                    > as alloy_sol_types::SolType>::tokenize(&self.values),
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self.data,
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
function onERC1155Received(address operator, address from, uint256 id, uint256 value, bytes memory data) external returns (bytes4);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct onERC1155ReceivedCall {
        #[allow(missing_docs)]
        pub operator: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub from: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub id: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub value: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub data: alloy::sol_types::private::Bytes,
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
                    (value.operator, value.from, value.id, value.value, value.data)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for onERC1155ReceivedCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        operator: tuple.0,
                        from: tuple.1,
                        id: tuple.2,
                        value: tuple.3,
                        data: tuple.4,
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
                        &self.operator,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.from,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.id),
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
function onERC721Received(address operator, address from, uint256 tokenId, bytes memory data) external returns (bytes4);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct onERC721ReceivedCall {
        #[allow(missing_docs)]
        pub operator: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub from: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub tokenId: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub data: alloy::sol_types::private::Bytes,
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
                    (value.operator, value.from, value.tokenId, value.data)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for onERC721ReceivedCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        operator: tuple.0,
                        from: tuple.1,
                        tokenId: tuple.2,
                        data: tuple.3,
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
                        &self.operator,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.from,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.tokenId),
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self.data,
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
    /**Function with signature `owner()` and selector `0x8da5cb5b`.
```solidity
function owner() external view returns (address);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ownerCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`owner()`](ownerCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ownerReturn {
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
            impl ::core::convert::From<ownerCall> for UnderlyingRustTuple<'_> {
                fn from(value: ownerCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for ownerCall {
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
            impl ::core::convert::From<ownerReturn> for UnderlyingRustTuple<'_> {
                fn from(value: ownerReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for ownerReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for ownerCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::Address;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Address,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "owner()";
            const SELECTOR: [u8; 4] = [141u8, 165u8, 203u8, 91u8];
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
                        let r: ownerReturn = r.into();
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
                        let r: ownerReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `pause()` and selector `0x8456cb59`.
```solidity
function pause() external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct pauseCall;
    ///Container type for the return parameters of the [`pause()`](pauseCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct pauseReturn {}
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
            impl ::core::convert::From<pauseCall> for UnderlyingRustTuple<'_> {
                fn from(value: pauseCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for pauseCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self
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
            impl ::core::convert::From<pauseReturn> for UnderlyingRustTuple<'_> {
                fn from(value: pauseReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for pauseReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl pauseReturn {
            fn _tokenize(
                &self,
            ) -> <pauseCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for pauseCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = pauseReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "pause()";
            const SELECTOR: [u8; 4] = [132u8, 86u8, 203u8, 89u8];
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
                pauseReturn::_tokenize(ret)
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
    /**Function with signature `paused()` and selector `0x5c975abb`.
```solidity
function paused() external view returns (bool);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct pausedCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`paused()`](pausedCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct pausedReturn {
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
            impl ::core::convert::From<pausedCall> for UnderlyingRustTuple<'_> {
                fn from(value: pausedCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for pausedCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self
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
            impl ::core::convert::From<pausedReturn> for UnderlyingRustTuple<'_> {
                fn from(value: pausedReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for pausedReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for pausedCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = bool;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "paused()";
            const SELECTOR: [u8; 4] = [92u8, 151u8, 90u8, 187u8];
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
                        let r: pausedReturn = r.into();
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
                        let r: pausedReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `pendingOwner()` and selector `0xe30c3978`.
```solidity
function pendingOwner() external view returns (address);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct pendingOwnerCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`pendingOwner()`](pendingOwnerCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct pendingOwnerReturn {
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
            impl ::core::convert::From<pendingOwnerCall> for UnderlyingRustTuple<'_> {
                fn from(value: pendingOwnerCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for pendingOwnerCall {
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
            impl ::core::convert::From<pendingOwnerReturn> for UnderlyingRustTuple<'_> {
                fn from(value: pendingOwnerReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for pendingOwnerReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for pendingOwnerCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::Address;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Address,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "pendingOwner()";
            const SELECTOR: [u8; 4] = [227u8, 12u8, 57u8, 120u8];
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
                        let r: pendingOwnerReturn = r.into();
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
                        let r: pendingOwnerReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `receiveFlashLoan(address[],uint256[],uint256[],bytes)` and selector `0xf04f2707`.
```solidity
function receiveFlashLoan(address[] memory tokens, uint256[] memory amounts, uint256[] memory feeAmounts, bytes memory userData) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct receiveFlashLoanCall {
        #[allow(missing_docs)]
        pub tokens: alloy::sol_types::private::Vec<alloy::sol_types::private::Address>,
        #[allow(missing_docs)]
        pub amounts: alloy::sol_types::private::Vec<
            alloy::sol_types::private::primitives::aliases::U256,
        >,
        #[allow(missing_docs)]
        pub feeAmounts: alloy::sol_types::private::Vec<
            alloy::sol_types::private::primitives::aliases::U256,
        >,
        #[allow(missing_docs)]
        pub userData: alloy::sol_types::private::Bytes,
    }
    ///Container type for the return parameters of the [`receiveFlashLoan(address[],uint256[],uint256[],bytes)`](receiveFlashLoanCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct receiveFlashLoanReturn {}
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
                alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Uint<256>>,
                alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Uint<256>>,
                alloy::sol_types::sol_data::Bytes,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Vec<alloy::sol_types::private::Address>,
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
            impl ::core::convert::From<receiveFlashLoanCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: receiveFlashLoanCall) -> Self {
                    (value.tokens, value.amounts, value.feeAmounts, value.userData)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for receiveFlashLoanCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        tokens: tuple.0,
                        amounts: tuple.1,
                        feeAmounts: tuple.2,
                        userData: tuple.3,
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
            impl ::core::convert::From<receiveFlashLoanReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: receiveFlashLoanReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for receiveFlashLoanReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl receiveFlashLoanReturn {
            fn _tokenize(
                &self,
            ) -> <receiveFlashLoanCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for receiveFlashLoanCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Address>,
                alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Uint<256>>,
                alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Uint<256>>,
                alloy::sol_types::sol_data::Bytes,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = receiveFlashLoanReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "receiveFlashLoan(address[],uint256[],uint256[],bytes)";
            const SELECTOR: [u8; 4] = [240u8, 79u8, 39u8, 7u8];
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
                    <alloy::sol_types::sol_data::Array<
                        alloy::sol_types::sol_data::Uint<256>,
                    > as alloy_sol_types::SolType>::tokenize(&self.amounts),
                    <alloy::sol_types::sol_data::Array<
                        alloy::sol_types::sol_data::Uint<256>,
                    > as alloy_sol_types::SolType>::tokenize(&self.feeAmounts),
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self.userData,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                receiveFlashLoanReturn::_tokenize(ret)
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
    /**Function with signature `renounceOwnership()` and selector `0x715018a6`.
```solidity
function renounceOwnership() external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct renounceOwnershipCall;
    ///Container type for the return parameters of the [`renounceOwnership()`](renounceOwnershipCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct renounceOwnershipReturn {}
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
            impl ::core::convert::From<renounceOwnershipCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: renounceOwnershipCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for renounceOwnershipCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self
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
            impl ::core::convert::From<renounceOwnershipReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: renounceOwnershipReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for renounceOwnershipReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl renounceOwnershipReturn {
            fn _tokenize(
                &self,
            ) -> <renounceOwnershipCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for renounceOwnershipCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = renounceOwnershipReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "renounceOwnership()";
            const SELECTOR: [u8; 4] = [113u8, 80u8, 24u8, 166u8];
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
                renounceOwnershipReturn::_tokenize(ret)
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
    /**Function with signature `setLegacyDelegatee(address,bool)` and selector `0xe24d8c4c`.
```solidity
function setLegacyDelegatee(address account, bool allowed) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setLegacyDelegateeCall {
        #[allow(missing_docs)]
        pub account: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub allowed: bool,
    }
    ///Container type for the return parameters of the [`setLegacyDelegatee(address,bool)`](setLegacyDelegateeCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setLegacyDelegateeReturn {}
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
            impl ::core::convert::From<setLegacyDelegateeCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: setLegacyDelegateeCall) -> Self {
                    (value.account, value.allowed)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for setLegacyDelegateeCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        account: tuple.0,
                        allowed: tuple.1,
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
            impl ::core::convert::From<setLegacyDelegateeReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: setLegacyDelegateeReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for setLegacyDelegateeReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl setLegacyDelegateeReturn {
            fn _tokenize(
                &self,
            ) -> <setLegacyDelegateeCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for setLegacyDelegateeCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Bool,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = setLegacyDelegateeReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "setLegacyDelegatee(address,bool)";
            const SELECTOR: [u8; 4] = [226u8, 77u8, 140u8, 76u8];
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
                        &self.account,
                    ),
                    <alloy::sol_types::sol_data::Bool as alloy_sol_types::SolType>::tokenize(
                        &self.allowed,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                setLegacyDelegateeReturn::_tokenize(ret)
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
    /**Function with signature `setSigner(address,bool)` and selector `0x31cb6105`.
```solidity
function setSigner(address signer, bool allowed) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setSignerCall {
        #[allow(missing_docs)]
        pub signer: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub allowed: bool,
    }
    ///Container type for the return parameters of the [`setSigner(address,bool)`](setSignerCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setSignerReturn {}
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
            impl ::core::convert::From<setSignerCall> for UnderlyingRustTuple<'_> {
                fn from(value: setSignerCall) -> Self {
                    (value.signer, value.allowed)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setSignerCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        signer: tuple.0,
                        allowed: tuple.1,
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
            impl ::core::convert::From<setSignerReturn> for UnderlyingRustTuple<'_> {
                fn from(value: setSignerReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setSignerReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl setSignerReturn {
            fn _tokenize(
                &self,
            ) -> <setSignerCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for setSignerCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Bool,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = setSignerReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "setSigner(address,bool)";
            const SELECTOR: [u8; 4] = [49u8, 203u8, 97u8, 5u8];
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
                        &self.signer,
                    ),
                    <alloy::sol_types::sol_data::Bool as alloy_sol_types::SolType>::tokenize(
                        &self.allowed,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                setSignerReturn::_tokenize(ret)
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
    /**Function with signature `setThreshold(uint256)` and selector `0x960bfe04`.
```solidity
function setThreshold(uint256 t) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setThresholdCall {
        #[allow(missing_docs)]
        pub t: alloy::sol_types::private::primitives::aliases::U256,
    }
    ///Container type for the return parameters of the [`setThreshold(uint256)`](setThresholdCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setThresholdReturn {}
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
            impl ::core::convert::From<setThresholdCall> for UnderlyingRustTuple<'_> {
                fn from(value: setThresholdCall) -> Self {
                    (value.t,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setThresholdCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { t: tuple.0 }
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
            impl ::core::convert::From<setThresholdReturn> for UnderlyingRustTuple<'_> {
                fn from(value: setThresholdReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setThresholdReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl setThresholdReturn {
            fn _tokenize(
                &self,
            ) -> <setThresholdCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for setThresholdCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = setThresholdReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "setThreshold(uint256)";
            const SELECTOR: [u8; 4] = [150u8, 11u8, 254u8, 4u8];
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
                    > as alloy_sol_types::SolType>::tokenize(&self.t),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                setThresholdReturn::_tokenize(ret)
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
    /**Function with signature `setV4Operator(address,address,bool)` and selector `0x66579be8`.
```solidity
function setV4Operator(address poolManager, address operator, bool approved) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setV4OperatorCall {
        #[allow(missing_docs)]
        pub poolManager: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub operator: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub approved: bool,
    }
    ///Container type for the return parameters of the [`setV4Operator(address,address,bool)`](setV4OperatorCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setV4OperatorReturn {}
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
                alloy::sol_types::sol_data::Bool,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Address,
                alloy::sol_types::private::Address,
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
            impl ::core::convert::From<setV4OperatorCall> for UnderlyingRustTuple<'_> {
                fn from(value: setV4OperatorCall) -> Self {
                    (value.poolManager, value.operator, value.approved)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setV4OperatorCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        poolManager: tuple.0,
                        operator: tuple.1,
                        approved: tuple.2,
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
            impl ::core::convert::From<setV4OperatorReturn> for UnderlyingRustTuple<'_> {
                fn from(value: setV4OperatorReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setV4OperatorReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl setV4OperatorReturn {
            fn _tokenize(
                &self,
            ) -> <setV4OperatorCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for setV4OperatorCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Bool,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = setV4OperatorReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "setV4Operator(address,address,bool)";
            const SELECTOR: [u8; 4] = [102u8, 87u8, 155u8, 232u8];
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
                        &self.poolManager,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.operator,
                    ),
                    <alloy::sol_types::sol_data::Bool as alloy_sol_types::SolType>::tokenize(
                        &self.approved,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                setV4OperatorReturn::_tokenize(ret)
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
    /**Function with signature `signerCount()` and selector `0x7ca548c6`.
```solidity
function signerCount() external view returns (uint256);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct signerCountCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`signerCount()`](signerCountCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct signerCountReturn {
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
            impl ::core::convert::From<signerCountCall> for UnderlyingRustTuple<'_> {
                fn from(value: signerCountCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for signerCountCall {
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
            impl ::core::convert::From<signerCountReturn> for UnderlyingRustTuple<'_> {
                fn from(value: signerCountReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for signerCountReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for signerCountCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::primitives::aliases::U256;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "signerCount()";
            const SELECTOR: [u8; 4] = [124u8, 165u8, 72u8, 198u8];
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
                        let r: signerCountReturn = r.into();
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
                        let r: signerCountReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `supportsInterface(bytes4)` and selector `0x01ffc9a7`.
```solidity
function supportsInterface(bytes4 interfaceId) external view returns (bool);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct supportsInterfaceCall {
        #[allow(missing_docs)]
        pub interfaceId: alloy::sol_types::private::FixedBytes<4>,
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
                    (value.interfaceId,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for supportsInterfaceCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { interfaceId: tuple.0 }
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
                    > as alloy_sol_types::SolType>::tokenize(&self.interfaceId),
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
    /**Function with signature `sweepBatch(address[],address,uint256[])` and selector `0x5589e272`.
```solidity
function sweepBatch(address[] memory tokens, address to, uint256[] memory amounts) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct sweepBatchCall {
        #[allow(missing_docs)]
        pub tokens: alloy::sol_types::private::Vec<alloy::sol_types::private::Address>,
        #[allow(missing_docs)]
        pub to: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub amounts: alloy::sol_types::private::Vec<
            alloy::sol_types::private::primitives::aliases::U256,
        >,
    }
    ///Container type for the return parameters of the [`sweepBatch(address[],address,uint256[])`](sweepBatchCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct sweepBatchReturn {}
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
            impl ::core::convert::From<sweepBatchCall> for UnderlyingRustTuple<'_> {
                fn from(value: sweepBatchCall) -> Self {
                    (value.tokens, value.to, value.amounts)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for sweepBatchCall {
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
            impl ::core::convert::From<sweepBatchReturn> for UnderlyingRustTuple<'_> {
                fn from(value: sweepBatchReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for sweepBatchReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl sweepBatchReturn {
            fn _tokenize(
                &self,
            ) -> <sweepBatchCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for sweepBatchCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Address>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Array<alloy::sol_types::sol_data::Uint<256>>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = sweepBatchReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "sweepBatch(address[],address,uint256[])";
            const SELECTOR: [u8; 4] = [85u8, 137u8, 226u8, 114u8];
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
                sweepBatchReturn::_tokenize(ret)
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
    /**Function with signature `threshold()` and selector `0x42cde4e8`.
```solidity
function threshold() external view returns (uint256);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct thresholdCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`threshold()`](thresholdCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct thresholdReturn {
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
            impl ::core::convert::From<thresholdCall> for UnderlyingRustTuple<'_> {
                fn from(value: thresholdCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for thresholdCall {
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
            impl ::core::convert::From<thresholdReturn> for UnderlyingRustTuple<'_> {
                fn from(value: thresholdReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for thresholdReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for thresholdCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::primitives::aliases::U256;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "threshold()";
            const SELECTOR: [u8; 4] = [66u8, 205u8, 228u8, 232u8];
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
                        let r: thresholdReturn = r.into();
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
                        let r: thresholdReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `transferOwnership(address)` and selector `0xf2fde38b`.
```solidity
function transferOwnership(address newOwner) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct transferOwnershipCall {
        #[allow(missing_docs)]
        pub newOwner: alloy::sol_types::private::Address,
    }
    ///Container type for the return parameters of the [`transferOwnership(address)`](transferOwnershipCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct transferOwnershipReturn {}
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
            impl ::core::convert::From<transferOwnershipCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: transferOwnershipCall) -> Self {
                    (value.newOwner,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for transferOwnershipCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { newOwner: tuple.0 }
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
            impl ::core::convert::From<transferOwnershipReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: transferOwnershipReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for transferOwnershipReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl transferOwnershipReturn {
            fn _tokenize(
                &self,
            ) -> <transferOwnershipCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for transferOwnershipCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Address,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = transferOwnershipReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "transferOwnership(address)";
            const SELECTOR: [u8; 4] = [242u8, 253u8, 227u8, 139u8];
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
                        &self.newOwner,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                transferOwnershipReturn::_tokenize(ret)
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
    /**Function with signature `unpause()` and selector `0x3f4ba83a`.
```solidity
function unpause() external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct unpauseCall;
    ///Container type for the return parameters of the [`unpause()`](unpauseCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct unpauseReturn {}
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
            impl ::core::convert::From<unpauseCall> for UnderlyingRustTuple<'_> {
                fn from(value: unpauseCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for unpauseCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self
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
            impl ::core::convert::From<unpauseReturn> for UnderlyingRustTuple<'_> {
                fn from(value: unpauseReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for unpauseReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl unpauseReturn {
            fn _tokenize(
                &self,
            ) -> <unpauseCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for unpauseCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = unpauseReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "unpause()";
            const SELECTOR: [u8; 4] = [63u8, 75u8, 168u8, 58u8];
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
                unpauseReturn::_tokenize(ret)
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
    ///Container for all the [`MevSafe`](self) function calls.
    #[derive(Clone)]
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive()]
    pub enum MevSafeCalls {
        #[allow(missing_docs)]
        AAVE_V3_POOL(AAVE_V3_POOLCall),
        #[allow(missing_docs)]
        BALANCER_V2_VAULT(BALANCER_V2_VAULTCall),
        #[allow(missing_docs)]
        BALANCER_V3_VAULT(BALANCER_V3_VAULTCall),
        #[allow(missing_docs)]
        ENTRY_POINT(ENTRY_POINTCall),
        #[allow(missing_docs)]
        MORPHO_BLUE(MORPHO_BLUECall),
        #[allow(missing_docs)]
        PERMISSIONS(PERMISSIONSCall),
        #[allow(missing_docs)]
        aaveBorrow(aaveBorrowCall),
        #[allow(missing_docs)]
        aaveRepay(aaveRepayCall),
        #[allow(missing_docs)]
        aaveSupply(aaveSupplyCall),
        #[allow(missing_docs)]
        aaveWithdraw(aaveWithdrawCall),
        #[allow(missing_docs)]
        acceptOwnership(acceptOwnershipCall),
        #[allow(missing_docs)]
        execute(executeCall),
        #[allow(missing_docs)]
        executeBatch(executeBatchCall),
        #[allow(missing_docs)]
        executeErc6909Batch(executeErc6909BatchCall),
        #[allow(missing_docs)]
        executeFinanceUnlock(executeFinanceUnlockCall),
        #[allow(missing_docs)]
        flashCollateralize(flashCollateralizeCall),
        #[allow(missing_docs)]
        flashCollateralizeV3(flashCollateralizeV3Call),
        #[allow(missing_docs)]
        isAuthorized(isAuthorizedCall),
        #[allow(missing_docs)]
        isSigner(isSignerCall),
        #[allow(missing_docs)]
        isValidSignature(isValidSignatureCall),
        #[allow(missing_docs)]
        legacyDelegatees(legacyDelegateesCall),
        #[allow(missing_docs)]
        moveLpV3Nft(moveLpV3NftCall),
        #[allow(missing_docs)]
        moveLpV4(moveLpV4Call),
        #[allow(missing_docs)]
        onERC1155BatchReceived(onERC1155BatchReceivedCall),
        #[allow(missing_docs)]
        onERC1155Received(onERC1155ReceivedCall),
        #[allow(missing_docs)]
        onERC721Received(onERC721ReceivedCall),
        #[allow(missing_docs)]
        owner(ownerCall),
        #[allow(missing_docs)]
        pause(pauseCall),
        #[allow(missing_docs)]
        paused(pausedCall),
        #[allow(missing_docs)]
        pendingOwner(pendingOwnerCall),
        #[allow(missing_docs)]
        receiveFlashLoan(receiveFlashLoanCall),
        #[allow(missing_docs)]
        renounceOwnership(renounceOwnershipCall),
        #[allow(missing_docs)]
        setLegacyDelegatee(setLegacyDelegateeCall),
        #[allow(missing_docs)]
        setSigner(setSignerCall),
        #[allow(missing_docs)]
        setThreshold(setThresholdCall),
        #[allow(missing_docs)]
        setV4Operator(setV4OperatorCall),
        #[allow(missing_docs)]
        signerCount(signerCountCall),
        #[allow(missing_docs)]
        supportsInterface(supportsInterfaceCall),
        #[allow(missing_docs)]
        sweepBatch(sweepBatchCall),
        #[allow(missing_docs)]
        sweepERC20(sweepERC20Call),
        #[allow(missing_docs)]
        threshold(thresholdCall),
        #[allow(missing_docs)]
        transferOwnership(transferOwnershipCall),
        #[allow(missing_docs)]
        unpause(unpauseCall),
        #[allow(missing_docs)]
        validateUserOp(validateUserOpCall),
    }
    impl MevSafeCalls {
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
            [33u8, 60u8, 80u8, 51u8],
            [49u8, 203u8, 97u8, 5u8],
            [52u8, 252u8, 213u8, 190u8],
            [55u8, 103u8, 148u8, 216u8],
            [59u8, 48u8, 55u8, 5u8],
            [63u8, 75u8, 168u8, 58u8],
            [66u8, 3u8, 169u8, 52u8],
            [66u8, 205u8, 228u8, 232u8],
            [80u8, 54u8, 144u8, 209u8],
            [83u8, 3u8, 173u8, 40u8],
            [85u8, 137u8, 226u8, 114u8],
            [92u8, 28u8, 109u8, 205u8],
            [92u8, 151u8, 90u8, 187u8],
            [102u8, 87u8, 155u8, 232u8],
            [113u8, 80u8, 24u8, 166u8],
            [121u8, 186u8, 80u8, 151u8],
            [124u8, 165u8, 72u8, 198u8],
            [125u8, 40u8, 28u8, 170u8],
            [125u8, 247u8, 62u8, 39u8],
            [128u8, 74u8, 5u8, 102u8],
            [132u8, 86u8, 203u8, 89u8],
            [141u8, 165u8, 203u8, 91u8],
            [143u8, 32u8, 90u8, 29u8],
            [148u8, 67u8, 15u8, 165u8],
            [150u8, 11u8, 254u8, 4u8],
            [153u8, 254u8, 199u8, 160u8],
            [164u8, 192u8, 27u8, 187u8],
            [176u8, 108u8, 148u8, 74u8],
            [188u8, 25u8, 124u8, 129u8],
            [212u8, 30u8, 93u8, 63u8],
            [212u8, 117u8, 192u8, 152u8],
            [226u8, 77u8, 140u8, 76u8],
            [227u8, 12u8, 57u8, 120u8],
            [233u8, 159u8, 91u8, 22u8],
            [240u8, 79u8, 39u8, 7u8],
            [242u8, 58u8, 110u8, 97u8],
            [242u8, 253u8, 227u8, 139u8],
            [244u8, 52u8, 201u8, 20u8],
            [246u8, 235u8, 121u8, 199u8],
            [253u8, 176u8, 32u8, 152u8],
        ];
        /// The names of the variants in the same order as `SELECTORS`.
        pub const VARIANT_NAMES: &'static [&'static str] = &[
            ::core::stringify!(supportsInterface),
            ::core::stringify!(onERC721Received),
            ::core::stringify!(isValidSignature),
            ::core::stringify!(validateUserOp),
            ::core::stringify!(BALANCER_V2_VAULT),
            ::core::stringify!(setSigner),
            ::core::stringify!(executeBatch),
            ::core::stringify!(moveLpV4),
            ::core::stringify!(AAVE_V3_POOL),
            ::core::stringify!(unpause),
            ::core::stringify!(executeErc6909Batch),
            ::core::stringify!(threshold),
            ::core::stringify!(sweepERC20),
            ::core::stringify!(flashCollateralize),
            ::core::stringify!(sweepBatch),
            ::core::stringify!(execute),
            ::core::stringify!(paused),
            ::core::stringify!(setV4Operator),
            ::core::stringify!(renounceOwnership),
            ::core::stringify!(acceptOwnership),
            ::core::stringify!(signerCount),
            ::core::stringify!(legacyDelegatees),
            ::core::stringify!(isSigner),
            ::core::stringify!(aaveBorrow),
            ::core::stringify!(pause),
            ::core::stringify!(owner),
            ::core::stringify!(flashCollateralizeV3),
            ::core::stringify!(ENTRY_POINT),
            ::core::stringify!(setThreshold),
            ::core::stringify!(MORPHO_BLUE),
            ::core::stringify!(BALANCER_V3_VAULT),
            ::core::stringify!(moveLpV3Nft),
            ::core::stringify!(onERC1155BatchReceived),
            ::core::stringify!(executeFinanceUnlock),
            ::core::stringify!(aaveWithdraw),
            ::core::stringify!(setLegacyDelegatee),
            ::core::stringify!(pendingOwner),
            ::core::stringify!(isAuthorized),
            ::core::stringify!(receiveFlashLoan),
            ::core::stringify!(onERC1155Received),
            ::core::stringify!(transferOwnership),
            ::core::stringify!(PERMISSIONS),
            ::core::stringify!(aaveSupply),
            ::core::stringify!(aaveRepay),
        ];
        /// The signatures in the same order as `SELECTORS`.
        pub const SIGNATURES: &'static [&'static str] = &[
            <supportsInterfaceCall as alloy_sol_types::SolCall>::SIGNATURE,
            <onERC721ReceivedCall as alloy_sol_types::SolCall>::SIGNATURE,
            <isValidSignatureCall as alloy_sol_types::SolCall>::SIGNATURE,
            <validateUserOpCall as alloy_sol_types::SolCall>::SIGNATURE,
            <BALANCER_V2_VAULTCall as alloy_sol_types::SolCall>::SIGNATURE,
            <setSignerCall as alloy_sol_types::SolCall>::SIGNATURE,
            <executeBatchCall as alloy_sol_types::SolCall>::SIGNATURE,
            <moveLpV4Call as alloy_sol_types::SolCall>::SIGNATURE,
            <AAVE_V3_POOLCall as alloy_sol_types::SolCall>::SIGNATURE,
            <unpauseCall as alloy_sol_types::SolCall>::SIGNATURE,
            <executeErc6909BatchCall as alloy_sol_types::SolCall>::SIGNATURE,
            <thresholdCall as alloy_sol_types::SolCall>::SIGNATURE,
            <sweepERC20Call as alloy_sol_types::SolCall>::SIGNATURE,
            <flashCollateralizeCall as alloy_sol_types::SolCall>::SIGNATURE,
            <sweepBatchCall as alloy_sol_types::SolCall>::SIGNATURE,
            <executeCall as alloy_sol_types::SolCall>::SIGNATURE,
            <pausedCall as alloy_sol_types::SolCall>::SIGNATURE,
            <setV4OperatorCall as alloy_sol_types::SolCall>::SIGNATURE,
            <renounceOwnershipCall as alloy_sol_types::SolCall>::SIGNATURE,
            <acceptOwnershipCall as alloy_sol_types::SolCall>::SIGNATURE,
            <signerCountCall as alloy_sol_types::SolCall>::SIGNATURE,
            <legacyDelegateesCall as alloy_sol_types::SolCall>::SIGNATURE,
            <isSignerCall as alloy_sol_types::SolCall>::SIGNATURE,
            <aaveBorrowCall as alloy_sol_types::SolCall>::SIGNATURE,
            <pauseCall as alloy_sol_types::SolCall>::SIGNATURE,
            <ownerCall as alloy_sol_types::SolCall>::SIGNATURE,
            <flashCollateralizeV3Call as alloy_sol_types::SolCall>::SIGNATURE,
            <ENTRY_POINTCall as alloy_sol_types::SolCall>::SIGNATURE,
            <setThresholdCall as alloy_sol_types::SolCall>::SIGNATURE,
            <MORPHO_BLUECall as alloy_sol_types::SolCall>::SIGNATURE,
            <BALANCER_V3_VAULTCall as alloy_sol_types::SolCall>::SIGNATURE,
            <moveLpV3NftCall as alloy_sol_types::SolCall>::SIGNATURE,
            <onERC1155BatchReceivedCall as alloy_sol_types::SolCall>::SIGNATURE,
            <executeFinanceUnlockCall as alloy_sol_types::SolCall>::SIGNATURE,
            <aaveWithdrawCall as alloy_sol_types::SolCall>::SIGNATURE,
            <setLegacyDelegateeCall as alloy_sol_types::SolCall>::SIGNATURE,
            <pendingOwnerCall as alloy_sol_types::SolCall>::SIGNATURE,
            <isAuthorizedCall as alloy_sol_types::SolCall>::SIGNATURE,
            <receiveFlashLoanCall as alloy_sol_types::SolCall>::SIGNATURE,
            <onERC1155ReceivedCall as alloy_sol_types::SolCall>::SIGNATURE,
            <transferOwnershipCall as alloy_sol_types::SolCall>::SIGNATURE,
            <PERMISSIONSCall as alloy_sol_types::SolCall>::SIGNATURE,
            <aaveSupplyCall as alloy_sol_types::SolCall>::SIGNATURE,
            <aaveRepayCall as alloy_sol_types::SolCall>::SIGNATURE,
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
    impl alloy_sol_types::SolInterface for MevSafeCalls {
        const NAME: &'static str = "MevSafeCalls";
        const MIN_DATA_LENGTH: usize = 0usize;
        const COUNT: usize = 44usize;
        #[inline]
        fn selector(&self) -> [u8; 4] {
            match self {
                Self::AAVE_V3_POOL(_) => {
                    <AAVE_V3_POOLCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::BALANCER_V2_VAULT(_) => {
                    <BALANCER_V2_VAULTCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::BALANCER_V3_VAULT(_) => {
                    <BALANCER_V3_VAULTCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::ENTRY_POINT(_) => {
                    <ENTRY_POINTCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::MORPHO_BLUE(_) => {
                    <MORPHO_BLUECall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::PERMISSIONS(_) => {
                    <PERMISSIONSCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::aaveBorrow(_) => {
                    <aaveBorrowCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::aaveRepay(_) => {
                    <aaveRepayCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::aaveSupply(_) => {
                    <aaveSupplyCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::aaveWithdraw(_) => {
                    <aaveWithdrawCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::acceptOwnership(_) => {
                    <acceptOwnershipCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::execute(_) => <executeCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::executeBatch(_) => {
                    <executeBatchCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::executeErc6909Batch(_) => {
                    <executeErc6909BatchCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::executeFinanceUnlock(_) => {
                    <executeFinanceUnlockCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::flashCollateralize(_) => {
                    <flashCollateralizeCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::flashCollateralizeV3(_) => {
                    <flashCollateralizeV3Call as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::isAuthorized(_) => {
                    <isAuthorizedCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::isSigner(_) => <isSignerCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::isValidSignature(_) => {
                    <isValidSignatureCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::legacyDelegatees(_) => {
                    <legacyDelegateesCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::moveLpV3Nft(_) => {
                    <moveLpV3NftCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::moveLpV4(_) => <moveLpV4Call as alloy_sol_types::SolCall>::SELECTOR,
                Self::onERC1155BatchReceived(_) => {
                    <onERC1155BatchReceivedCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::onERC1155Received(_) => {
                    <onERC1155ReceivedCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::onERC721Received(_) => {
                    <onERC721ReceivedCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::owner(_) => <ownerCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::pause(_) => <pauseCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::paused(_) => <pausedCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::pendingOwner(_) => {
                    <pendingOwnerCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::receiveFlashLoan(_) => {
                    <receiveFlashLoanCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::renounceOwnership(_) => {
                    <renounceOwnershipCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::setLegacyDelegatee(_) => {
                    <setLegacyDelegateeCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::setSigner(_) => {
                    <setSignerCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::setThreshold(_) => {
                    <setThresholdCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::setV4Operator(_) => {
                    <setV4OperatorCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::signerCount(_) => {
                    <signerCountCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::supportsInterface(_) => {
                    <supportsInterfaceCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::sweepBatch(_) => {
                    <sweepBatchCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::sweepERC20(_) => {
                    <sweepERC20Call as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::threshold(_) => {
                    <thresholdCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::transferOwnership(_) => {
                    <transferOwnershipCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::unpause(_) => <unpauseCall as alloy_sol_types::SolCall>::SELECTOR,
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
            static DECODE_SHIMS: &[fn(&[u8]) -> alloy_sol_types::Result<MevSafeCalls>] = &[
                {
                    fn supportsInterface(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <supportsInterfaceCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::supportsInterface)
                    }
                    supportsInterface
                },
                {
                    fn onERC721Received(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <onERC721ReceivedCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::onERC721Received)
                    }
                    onERC721Received
                },
                {
                    fn isValidSignature(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <isValidSignatureCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::isValidSignature)
                    }
                    isValidSignature
                },
                {
                    fn validateUserOp(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <validateUserOpCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::validateUserOp)
                    }
                    validateUserOp
                },
                {
                    fn BALANCER_V2_VAULT(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <BALANCER_V2_VAULTCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::BALANCER_V2_VAULT)
                    }
                    BALANCER_V2_VAULT
                },
                {
                    fn setSigner(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <setSignerCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevSafeCalls::setSigner)
                    }
                    setSigner
                },
                {
                    fn executeBatch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <executeBatchCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::executeBatch)
                    }
                    executeBatch
                },
                {
                    fn moveLpV4(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <moveLpV4Call as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevSafeCalls::moveLpV4)
                    }
                    moveLpV4
                },
                {
                    fn AAVE_V3_POOL(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <AAVE_V3_POOLCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::AAVE_V3_POOL)
                    }
                    AAVE_V3_POOL
                },
                {
                    fn unpause(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <unpauseCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevSafeCalls::unpause)
                    }
                    unpause
                },
                {
                    fn executeErc6909Batch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <executeErc6909BatchCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::executeErc6909Batch)
                    }
                    executeErc6909Batch
                },
                {
                    fn threshold(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <thresholdCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevSafeCalls::threshold)
                    }
                    threshold
                },
                {
                    fn sweepERC20(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <sweepERC20Call as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::sweepERC20)
                    }
                    sweepERC20
                },
                {
                    fn flashCollateralize(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <flashCollateralizeCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::flashCollateralize)
                    }
                    flashCollateralize
                },
                {
                    fn sweepBatch(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <sweepBatchCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::sweepBatch)
                    }
                    sweepBatch
                },
                {
                    fn execute(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <executeCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevSafeCalls::execute)
                    }
                    execute
                },
                {
                    fn paused(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <pausedCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevSafeCalls::paused)
                    }
                    paused
                },
                {
                    fn setV4Operator(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <setV4OperatorCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::setV4Operator)
                    }
                    setV4Operator
                },
                {
                    fn renounceOwnership(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <renounceOwnershipCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::renounceOwnership)
                    }
                    renounceOwnership
                },
                {
                    fn acceptOwnership(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <acceptOwnershipCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::acceptOwnership)
                    }
                    acceptOwnership
                },
                {
                    fn signerCount(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <signerCountCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::signerCount)
                    }
                    signerCount
                },
                {
                    fn legacyDelegatees(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <legacyDelegateesCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::legacyDelegatees)
                    }
                    legacyDelegatees
                },
                {
                    fn isSigner(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <isSignerCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevSafeCalls::isSigner)
                    }
                    isSigner
                },
                {
                    fn aaveBorrow(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <aaveBorrowCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::aaveBorrow)
                    }
                    aaveBorrow
                },
                {
                    fn pause(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <pauseCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevSafeCalls::pause)
                    }
                    pause
                },
                {
                    fn owner(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <ownerCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevSafeCalls::owner)
                    }
                    owner
                },
                {
                    fn flashCollateralizeV3(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <flashCollateralizeV3Call as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::flashCollateralizeV3)
                    }
                    flashCollateralizeV3
                },
                {
                    fn ENTRY_POINT(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <ENTRY_POINTCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::ENTRY_POINT)
                    }
                    ENTRY_POINT
                },
                {
                    fn setThreshold(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <setThresholdCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::setThreshold)
                    }
                    setThreshold
                },
                {
                    fn MORPHO_BLUE(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <MORPHO_BLUECall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::MORPHO_BLUE)
                    }
                    MORPHO_BLUE
                },
                {
                    fn BALANCER_V3_VAULT(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <BALANCER_V3_VAULTCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::BALANCER_V3_VAULT)
                    }
                    BALANCER_V3_VAULT
                },
                {
                    fn moveLpV3Nft(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <moveLpV3NftCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::moveLpV3Nft)
                    }
                    moveLpV3Nft
                },
                {
                    fn onERC1155BatchReceived(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <onERC1155BatchReceivedCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::onERC1155BatchReceived)
                    }
                    onERC1155BatchReceived
                },
                {
                    fn executeFinanceUnlock(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <executeFinanceUnlockCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::executeFinanceUnlock)
                    }
                    executeFinanceUnlock
                },
                {
                    fn aaveWithdraw(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <aaveWithdrawCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::aaveWithdraw)
                    }
                    aaveWithdraw
                },
                {
                    fn setLegacyDelegatee(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <setLegacyDelegateeCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::setLegacyDelegatee)
                    }
                    setLegacyDelegatee
                },
                {
                    fn pendingOwner(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <pendingOwnerCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::pendingOwner)
                    }
                    pendingOwner
                },
                {
                    fn isAuthorized(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <isAuthorizedCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::isAuthorized)
                    }
                    isAuthorized
                },
                {
                    fn receiveFlashLoan(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <receiveFlashLoanCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::receiveFlashLoan)
                    }
                    receiveFlashLoan
                },
                {
                    fn onERC1155Received(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <onERC1155ReceivedCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::onERC1155Received)
                    }
                    onERC1155Received
                },
                {
                    fn transferOwnership(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <transferOwnershipCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::transferOwnership)
                    }
                    transferOwnership
                },
                {
                    fn PERMISSIONS(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <PERMISSIONSCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::PERMISSIONS)
                    }
                    PERMISSIONS
                },
                {
                    fn aaveSupply(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <aaveSupplyCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeCalls::aaveSupply)
                    }
                    aaveSupply
                },
                {
                    fn aaveRepay(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <aaveRepayCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevSafeCalls::aaveRepay)
                    }
                    aaveRepay
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
            ) -> alloy_sol_types::Result<MevSafeCalls>] = &[
                {
                    fn supportsInterface(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <supportsInterfaceCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::supportsInterface)
                    }
                    supportsInterface
                },
                {
                    fn onERC721Received(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <onERC721ReceivedCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::onERC721Received)
                    }
                    onERC721Received
                },
                {
                    fn isValidSignature(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <isValidSignatureCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::isValidSignature)
                    }
                    isValidSignature
                },
                {
                    fn validateUserOp(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <validateUserOpCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::validateUserOp)
                    }
                    validateUserOp
                },
                {
                    fn BALANCER_V2_VAULT(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <BALANCER_V2_VAULTCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::BALANCER_V2_VAULT)
                    }
                    BALANCER_V2_VAULT
                },
                {
                    fn setSigner(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <setSignerCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::setSigner)
                    }
                    setSigner
                },
                {
                    fn executeBatch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <executeBatchCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::executeBatch)
                    }
                    executeBatch
                },
                {
                    fn moveLpV4(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <moveLpV4Call as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::moveLpV4)
                    }
                    moveLpV4
                },
                {
                    fn AAVE_V3_POOL(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <AAVE_V3_POOLCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::AAVE_V3_POOL)
                    }
                    AAVE_V3_POOL
                },
                {
                    fn unpause(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <unpauseCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::unpause)
                    }
                    unpause
                },
                {
                    fn executeErc6909Batch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <executeErc6909BatchCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::executeErc6909Batch)
                    }
                    executeErc6909Batch
                },
                {
                    fn threshold(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <thresholdCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::threshold)
                    }
                    threshold
                },
                {
                    fn sweepERC20(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <sweepERC20Call as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::sweepERC20)
                    }
                    sweepERC20
                },
                {
                    fn flashCollateralize(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <flashCollateralizeCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::flashCollateralize)
                    }
                    flashCollateralize
                },
                {
                    fn sweepBatch(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <sweepBatchCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::sweepBatch)
                    }
                    sweepBatch
                },
                {
                    fn execute(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <executeCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::execute)
                    }
                    execute
                },
                {
                    fn paused(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <pausedCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::paused)
                    }
                    paused
                },
                {
                    fn setV4Operator(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <setV4OperatorCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::setV4Operator)
                    }
                    setV4Operator
                },
                {
                    fn renounceOwnership(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <renounceOwnershipCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::renounceOwnership)
                    }
                    renounceOwnership
                },
                {
                    fn acceptOwnership(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <acceptOwnershipCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::acceptOwnership)
                    }
                    acceptOwnership
                },
                {
                    fn signerCount(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <signerCountCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::signerCount)
                    }
                    signerCount
                },
                {
                    fn legacyDelegatees(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <legacyDelegateesCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::legacyDelegatees)
                    }
                    legacyDelegatees
                },
                {
                    fn isSigner(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <isSignerCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::isSigner)
                    }
                    isSigner
                },
                {
                    fn aaveBorrow(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <aaveBorrowCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::aaveBorrow)
                    }
                    aaveBorrow
                },
                {
                    fn pause(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <pauseCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::pause)
                    }
                    pause
                },
                {
                    fn owner(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <ownerCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::owner)
                    }
                    owner
                },
                {
                    fn flashCollateralizeV3(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <flashCollateralizeV3Call as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::flashCollateralizeV3)
                    }
                    flashCollateralizeV3
                },
                {
                    fn ENTRY_POINT(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <ENTRY_POINTCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::ENTRY_POINT)
                    }
                    ENTRY_POINT
                },
                {
                    fn setThreshold(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <setThresholdCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::setThreshold)
                    }
                    setThreshold
                },
                {
                    fn MORPHO_BLUE(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <MORPHO_BLUECall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::MORPHO_BLUE)
                    }
                    MORPHO_BLUE
                },
                {
                    fn BALANCER_V3_VAULT(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <BALANCER_V3_VAULTCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::BALANCER_V3_VAULT)
                    }
                    BALANCER_V3_VAULT
                },
                {
                    fn moveLpV3Nft(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <moveLpV3NftCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::moveLpV3Nft)
                    }
                    moveLpV3Nft
                },
                {
                    fn onERC1155BatchReceived(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <onERC1155BatchReceivedCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::onERC1155BatchReceived)
                    }
                    onERC1155BatchReceived
                },
                {
                    fn executeFinanceUnlock(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <executeFinanceUnlockCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::executeFinanceUnlock)
                    }
                    executeFinanceUnlock
                },
                {
                    fn aaveWithdraw(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <aaveWithdrawCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::aaveWithdraw)
                    }
                    aaveWithdraw
                },
                {
                    fn setLegacyDelegatee(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <setLegacyDelegateeCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::setLegacyDelegatee)
                    }
                    setLegacyDelegatee
                },
                {
                    fn pendingOwner(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <pendingOwnerCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::pendingOwner)
                    }
                    pendingOwner
                },
                {
                    fn isAuthorized(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <isAuthorizedCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::isAuthorized)
                    }
                    isAuthorized
                },
                {
                    fn receiveFlashLoan(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <receiveFlashLoanCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::receiveFlashLoan)
                    }
                    receiveFlashLoan
                },
                {
                    fn onERC1155Received(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <onERC1155ReceivedCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::onERC1155Received)
                    }
                    onERC1155Received
                },
                {
                    fn transferOwnership(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <transferOwnershipCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::transferOwnership)
                    }
                    transferOwnership
                },
                {
                    fn PERMISSIONS(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeCalls> {
                        <PERMISSIONSCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::PERMISSIONS)
                    }
                    PERMISSIONS
                },
                {
                    fn aaveSupply(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <aaveSupplyCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::aaveSupply)
                    }
                    aaveSupply
                },
                {
                    fn aaveRepay(data: &[u8]) -> alloy_sol_types::Result<MevSafeCalls> {
                        <aaveRepayCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeCalls::aaveRepay)
                    }
                    aaveRepay
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
                Self::AAVE_V3_POOL(inner) => {
                    <AAVE_V3_POOLCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::BALANCER_V2_VAULT(inner) => {
                    <BALANCER_V2_VAULTCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::BALANCER_V3_VAULT(inner) => {
                    <BALANCER_V3_VAULTCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::ENTRY_POINT(inner) => {
                    <ENTRY_POINTCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::MORPHO_BLUE(inner) => {
                    <MORPHO_BLUECall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::PERMISSIONS(inner) => {
                    <PERMISSIONSCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::aaveBorrow(inner) => {
                    <aaveBorrowCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::aaveRepay(inner) => {
                    <aaveRepayCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::aaveSupply(inner) => {
                    <aaveSupplyCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::aaveWithdraw(inner) => {
                    <aaveWithdrawCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::acceptOwnership(inner) => {
                    <acceptOwnershipCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
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
                Self::executeFinanceUnlock(inner) => {
                    <executeFinanceUnlockCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::flashCollateralize(inner) => {
                    <flashCollateralizeCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::flashCollateralizeV3(inner) => {
                    <flashCollateralizeV3Call as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::isAuthorized(inner) => {
                    <isAuthorizedCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::isSigner(inner) => {
                    <isSignerCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::isValidSignature(inner) => {
                    <isValidSignatureCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::legacyDelegatees(inner) => {
                    <legacyDelegateesCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::moveLpV3Nft(inner) => {
                    <moveLpV3NftCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::moveLpV4(inner) => {
                    <moveLpV4Call as alloy_sol_types::SolCall>::abi_encoded_size(inner)
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
                Self::owner(inner) => {
                    <ownerCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::pause(inner) => {
                    <pauseCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::paused(inner) => {
                    <pausedCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::pendingOwner(inner) => {
                    <pendingOwnerCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::receiveFlashLoan(inner) => {
                    <receiveFlashLoanCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::renounceOwnership(inner) => {
                    <renounceOwnershipCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::setLegacyDelegatee(inner) => {
                    <setLegacyDelegateeCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::setSigner(inner) => {
                    <setSignerCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::setThreshold(inner) => {
                    <setThresholdCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::setV4Operator(inner) => {
                    <setV4OperatorCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::signerCount(inner) => {
                    <signerCountCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::supportsInterface(inner) => {
                    <supportsInterfaceCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::sweepBatch(inner) => {
                    <sweepBatchCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::sweepERC20(inner) => {
                    <sweepERC20Call as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::threshold(inner) => {
                    <thresholdCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::transferOwnership(inner) => {
                    <transferOwnershipCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::unpause(inner) => {
                    <unpauseCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
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
                Self::AAVE_V3_POOL(inner) => {
                    <AAVE_V3_POOLCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::BALANCER_V2_VAULT(inner) => {
                    <BALANCER_V2_VAULTCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::BALANCER_V3_VAULT(inner) => {
                    <BALANCER_V3_VAULTCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::ENTRY_POINT(inner) => {
                    <ENTRY_POINTCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::MORPHO_BLUE(inner) => {
                    <MORPHO_BLUECall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::PERMISSIONS(inner) => {
                    <PERMISSIONSCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::aaveBorrow(inner) => {
                    <aaveBorrowCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::aaveRepay(inner) => {
                    <aaveRepayCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::aaveSupply(inner) => {
                    <aaveSupplyCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::aaveWithdraw(inner) => {
                    <aaveWithdrawCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::acceptOwnership(inner) => {
                    <acceptOwnershipCall as alloy_sol_types::SolCall>::abi_encode_raw(
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
                Self::executeFinanceUnlock(inner) => {
                    <executeFinanceUnlockCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::flashCollateralize(inner) => {
                    <flashCollateralizeCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::flashCollateralizeV3(inner) => {
                    <flashCollateralizeV3Call as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::isAuthorized(inner) => {
                    <isAuthorizedCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::isSigner(inner) => {
                    <isSignerCall as alloy_sol_types::SolCall>::abi_encode_raw(
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
                Self::legacyDelegatees(inner) => {
                    <legacyDelegateesCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::moveLpV3Nft(inner) => {
                    <moveLpV3NftCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::moveLpV4(inner) => {
                    <moveLpV4Call as alloy_sol_types::SolCall>::abi_encode_raw(
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
                Self::owner(inner) => {
                    <ownerCall as alloy_sol_types::SolCall>::abi_encode_raw(inner, out)
                }
                Self::pause(inner) => {
                    <pauseCall as alloy_sol_types::SolCall>::abi_encode_raw(inner, out)
                }
                Self::paused(inner) => {
                    <pausedCall as alloy_sol_types::SolCall>::abi_encode_raw(inner, out)
                }
                Self::pendingOwner(inner) => {
                    <pendingOwnerCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::receiveFlashLoan(inner) => {
                    <receiveFlashLoanCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::renounceOwnership(inner) => {
                    <renounceOwnershipCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::setLegacyDelegatee(inner) => {
                    <setLegacyDelegateeCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::setSigner(inner) => {
                    <setSignerCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::setThreshold(inner) => {
                    <setThresholdCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::setV4Operator(inner) => {
                    <setV4OperatorCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::signerCount(inner) => {
                    <signerCountCall as alloy_sol_types::SolCall>::abi_encode_raw(
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
                Self::sweepBatch(inner) => {
                    <sweepBatchCall as alloy_sol_types::SolCall>::abi_encode_raw(
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
                Self::threshold(inner) => {
                    <thresholdCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::transferOwnership(inner) => {
                    <transferOwnershipCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::unpause(inner) => {
                    <unpauseCall as alloy_sol_types::SolCall>::abi_encode_raw(inner, out)
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
    ///Container for all the [`MevSafe`](self) custom errors.
    #[derive(Clone)]
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub enum MevSafeErrors {
        #[allow(missing_docs)]
        CallFailed(CallFailed),
        #[allow(missing_docs)]
        EnforcedPause(EnforcedPause),
        #[allow(missing_docs)]
        Erc6909Op_SetOperatorRequiresZeroIdAndAmount(
            Erc6909Op_SetOperatorRequiresZeroIdAndAmount,
        ),
        #[allow(missing_docs)]
        Erc6909Op_TransferRequiresZeroApproved(Erc6909Op_TransferRequiresZeroApproved),
        #[allow(missing_docs)]
        Erc6909SetOperatorBlocked(Erc6909SetOperatorBlocked),
        #[allow(missing_docs)]
        ExpectedPause(ExpectedPause),
        #[allow(missing_docs)]
        FinanceUnprofitable(FinanceUnprofitable),
        #[allow(missing_docs)]
        FlashAmountMismatch(FlashAmountMismatch),
        #[allow(missing_docs)]
        InvalidParams(InvalidParams),
        #[allow(missing_docs)]
        LpTransferLib__V4SetOperatorFailed(LpTransferLib__V4SetOperatorFailed),
        #[allow(missing_docs)]
        LpTransferLib__V4TransferFailed(LpTransferLib__V4TransferFailed),
        #[allow(missing_docs)]
        MevSafe__NativeSweepFailed(MevSafe__NativeSweepFailed),
        #[allow(missing_docs)]
        MevSafe__UnsupportedFlashLender(MevSafe__UnsupportedFlashLender),
        #[allow(missing_docs)]
        NoActiveFlash(NoActiveFlash),
        #[allow(missing_docs)]
        NotAuthorized(NotAuthorized),
        #[allow(missing_docs)]
        NotEntryPoint(NotEntryPoint),
        #[allow(missing_docs)]
        NotFlashLender(NotFlashLender),
        #[allow(missing_docs)]
        OnlyV3Vault(OnlyV3Vault),
        #[allow(missing_docs)]
        OwnableInvalidOwner(OwnableInvalidOwner),
        #[allow(missing_docs)]
        OwnableUnauthorizedAccount(OwnableUnauthorizedAccount),
        #[allow(missing_docs)]
        PlanHashMismatch(PlanHashMismatch),
        #[allow(missing_docs)]
        Reentrancy(Reentrancy),
        #[allow(missing_docs)]
        V3SettleShortfall(V3SettleShortfall),
    }
    impl MevSafeErrors {
        /// All the selectors of this enum.
        ///
        /// Note that the selectors might not be in the same order as the variants.
        /// No guarantees are made about the order of the selectors.
        ///
        /// Prefer using `SolInterface` methods instead.
        pub const SELECTORS: &'static [[u8; 4usize]] = &[
            [8u8, 43u8, 115u8, 182u8],
            [10u8, 59u8, 9u8, 161u8],
            [17u8, 140u8, 218u8, 167u8],
            [30u8, 79u8, 189u8, 247u8],
            [31u8, 183u8, 204u8, 165u8],
            [49u8, 20u8, 111u8, 21u8],
            [74u8, 1u8, 138u8, 74u8],
            [91u8, 102u8, 28u8, 142u8],
            [92u8, 13u8, 238u8, 93u8],
            [98u8, 11u8, 230u8, 35u8],
            [124u8, 198u8, 115u8, 198u8],
            [131u8, 234u8, 67u8, 242u8],
            [141u8, 252u8, 32u8, 43u8],
            [142u8, 94u8, 80u8, 61u8],
            [160u8, 170u8, 216u8, 187u8],
            [168u8, 107u8, 101u8, 18u8],
            [171u8, 20u8, 60u8, 6u8],
            [176u8, 38u8, 213u8, 163u8],
            [209u8, 216u8, 118u8, 3u8],
            [214u8, 99u8, 116u8, 42u8],
            [217u8, 60u8, 6u8, 101u8],
            [234u8, 142u8, 78u8, 181u8],
            [241u8, 249u8, 240u8, 22u8],
        ];
        /// The names of the variants in the same order as `SELECTORS`.
        pub const VARIANT_NAMES: &'static [&'static str] = &[
            ::core::stringify!(FlashAmountMismatch),
            ::core::stringify!(FinanceUnprofitable),
            ::core::stringify!(OwnableUnauthorizedAccount),
            ::core::stringify!(OwnableInvalidOwner),
            ::core::stringify!(Erc6909SetOperatorBlocked),
            ::core::stringify!(LpTransferLib__V4TransferFailed),
            ::core::stringify!(NotFlashLender),
            ::core::stringify!(LpTransferLib__V4SetOperatorFailed),
            ::core::stringify!(CallFailed),
            ::core::stringify!(PlanHashMismatch),
            ::core::stringify!(V3SettleShortfall),
            ::core::stringify!(Erc6909Op_SetOperatorRequiresZeroIdAndAmount),
            ::core::stringify!(ExpectedPause),
            ::core::stringify!(OnlyV3Vault),
            ::core::stringify!(MevSafe__UnsupportedFlashLender),
            ::core::stringify!(InvalidParams),
            ::core::stringify!(Reentrancy),
            ::core::stringify!(Erc6909Op_TransferRequiresZeroApproved),
            ::core::stringify!(MevSafe__NativeSweepFailed),
            ::core::stringify!(NotEntryPoint),
            ::core::stringify!(EnforcedPause),
            ::core::stringify!(NotAuthorized),
            ::core::stringify!(NoActiveFlash),
        ];
        /// The signatures in the same order as `SELECTORS`.
        pub const SIGNATURES: &'static [&'static str] = &[
            <FlashAmountMismatch as alloy_sol_types::SolError>::SIGNATURE,
            <FinanceUnprofitable as alloy_sol_types::SolError>::SIGNATURE,
            <OwnableUnauthorizedAccount as alloy_sol_types::SolError>::SIGNATURE,
            <OwnableInvalidOwner as alloy_sol_types::SolError>::SIGNATURE,
            <Erc6909SetOperatorBlocked as alloy_sol_types::SolError>::SIGNATURE,
            <LpTransferLib__V4TransferFailed as alloy_sol_types::SolError>::SIGNATURE,
            <NotFlashLender as alloy_sol_types::SolError>::SIGNATURE,
            <LpTransferLib__V4SetOperatorFailed as alloy_sol_types::SolError>::SIGNATURE,
            <CallFailed as alloy_sol_types::SolError>::SIGNATURE,
            <PlanHashMismatch as alloy_sol_types::SolError>::SIGNATURE,
            <V3SettleShortfall as alloy_sol_types::SolError>::SIGNATURE,
            <Erc6909Op_SetOperatorRequiresZeroIdAndAmount as alloy_sol_types::SolError>::SIGNATURE,
            <ExpectedPause as alloy_sol_types::SolError>::SIGNATURE,
            <OnlyV3Vault as alloy_sol_types::SolError>::SIGNATURE,
            <MevSafe__UnsupportedFlashLender as alloy_sol_types::SolError>::SIGNATURE,
            <InvalidParams as alloy_sol_types::SolError>::SIGNATURE,
            <Reentrancy as alloy_sol_types::SolError>::SIGNATURE,
            <Erc6909Op_TransferRequiresZeroApproved as alloy_sol_types::SolError>::SIGNATURE,
            <MevSafe__NativeSweepFailed as alloy_sol_types::SolError>::SIGNATURE,
            <NotEntryPoint as alloy_sol_types::SolError>::SIGNATURE,
            <EnforcedPause as alloy_sol_types::SolError>::SIGNATURE,
            <NotAuthorized as alloy_sol_types::SolError>::SIGNATURE,
            <NoActiveFlash as alloy_sol_types::SolError>::SIGNATURE,
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
    impl alloy_sol_types::SolInterface for MevSafeErrors {
        const NAME: &'static str = "MevSafeErrors";
        const MIN_DATA_LENGTH: usize = 0usize;
        const COUNT: usize = 23usize;
        #[inline]
        fn selector(&self) -> [u8; 4] {
            match self {
                Self::CallFailed(_) => {
                    <CallFailed as alloy_sol_types::SolError>::SELECTOR
                }
                Self::EnforcedPause(_) => {
                    <EnforcedPause as alloy_sol_types::SolError>::SELECTOR
                }
                Self::Erc6909Op_SetOperatorRequiresZeroIdAndAmount(_) => {
                    <Erc6909Op_SetOperatorRequiresZeroIdAndAmount as alloy_sol_types::SolError>::SELECTOR
                }
                Self::Erc6909Op_TransferRequiresZeroApproved(_) => {
                    <Erc6909Op_TransferRequiresZeroApproved as alloy_sol_types::SolError>::SELECTOR
                }
                Self::Erc6909SetOperatorBlocked(_) => {
                    <Erc6909SetOperatorBlocked as alloy_sol_types::SolError>::SELECTOR
                }
                Self::ExpectedPause(_) => {
                    <ExpectedPause as alloy_sol_types::SolError>::SELECTOR
                }
                Self::FinanceUnprofitable(_) => {
                    <FinanceUnprofitable as alloy_sol_types::SolError>::SELECTOR
                }
                Self::FlashAmountMismatch(_) => {
                    <FlashAmountMismatch as alloy_sol_types::SolError>::SELECTOR
                }
                Self::InvalidParams(_) => {
                    <InvalidParams as alloy_sol_types::SolError>::SELECTOR
                }
                Self::LpTransferLib__V4SetOperatorFailed(_) => {
                    <LpTransferLib__V4SetOperatorFailed as alloy_sol_types::SolError>::SELECTOR
                }
                Self::LpTransferLib__V4TransferFailed(_) => {
                    <LpTransferLib__V4TransferFailed as alloy_sol_types::SolError>::SELECTOR
                }
                Self::MevSafe__NativeSweepFailed(_) => {
                    <MevSafe__NativeSweepFailed as alloy_sol_types::SolError>::SELECTOR
                }
                Self::MevSafe__UnsupportedFlashLender(_) => {
                    <MevSafe__UnsupportedFlashLender as alloy_sol_types::SolError>::SELECTOR
                }
                Self::NoActiveFlash(_) => {
                    <NoActiveFlash as alloy_sol_types::SolError>::SELECTOR
                }
                Self::NotAuthorized(_) => {
                    <NotAuthorized as alloy_sol_types::SolError>::SELECTOR
                }
                Self::NotEntryPoint(_) => {
                    <NotEntryPoint as alloy_sol_types::SolError>::SELECTOR
                }
                Self::NotFlashLender(_) => {
                    <NotFlashLender as alloy_sol_types::SolError>::SELECTOR
                }
                Self::OnlyV3Vault(_) => {
                    <OnlyV3Vault as alloy_sol_types::SolError>::SELECTOR
                }
                Self::OwnableInvalidOwner(_) => {
                    <OwnableInvalidOwner as alloy_sol_types::SolError>::SELECTOR
                }
                Self::OwnableUnauthorizedAccount(_) => {
                    <OwnableUnauthorizedAccount as alloy_sol_types::SolError>::SELECTOR
                }
                Self::PlanHashMismatch(_) => {
                    <PlanHashMismatch as alloy_sol_types::SolError>::SELECTOR
                }
                Self::Reentrancy(_) => {
                    <Reentrancy as alloy_sol_types::SolError>::SELECTOR
                }
                Self::V3SettleShortfall(_) => {
                    <V3SettleShortfall as alloy_sol_types::SolError>::SELECTOR
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
            ) -> alloy_sol_types::Result<MevSafeErrors>] = &[
                {
                    fn FlashAmountMismatch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <FlashAmountMismatch as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::FlashAmountMismatch)
                    }
                    FlashAmountMismatch
                },
                {
                    fn FinanceUnprofitable(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <FinanceUnprofitable as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::FinanceUnprofitable)
                    }
                    FinanceUnprofitable
                },
                {
                    fn OwnableUnauthorizedAccount(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <OwnableUnauthorizedAccount as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::OwnableUnauthorizedAccount)
                    }
                    OwnableUnauthorizedAccount
                },
                {
                    fn OwnableInvalidOwner(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <OwnableInvalidOwner as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::OwnableInvalidOwner)
                    }
                    OwnableInvalidOwner
                },
                {
                    fn Erc6909SetOperatorBlocked(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <Erc6909SetOperatorBlocked as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::Erc6909SetOperatorBlocked)
                    }
                    Erc6909SetOperatorBlocked
                },
                {
                    fn LpTransferLib__V4TransferFailed(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <LpTransferLib__V4TransferFailed as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::LpTransferLib__V4TransferFailed)
                    }
                    LpTransferLib__V4TransferFailed
                },
                {
                    fn NotFlashLender(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <NotFlashLender as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::NotFlashLender)
                    }
                    NotFlashLender
                },
                {
                    fn LpTransferLib__V4SetOperatorFailed(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <LpTransferLib__V4SetOperatorFailed as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::LpTransferLib__V4SetOperatorFailed)
                    }
                    LpTransferLib__V4SetOperatorFailed
                },
                {
                    fn CallFailed(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <CallFailed as alloy_sol_types::SolError>::abi_decode_raw(data)
                            .map(MevSafeErrors::CallFailed)
                    }
                    CallFailed
                },
                {
                    fn PlanHashMismatch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <PlanHashMismatch as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::PlanHashMismatch)
                    }
                    PlanHashMismatch
                },
                {
                    fn V3SettleShortfall(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <V3SettleShortfall as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::V3SettleShortfall)
                    }
                    V3SettleShortfall
                },
                {
                    fn Erc6909Op_SetOperatorRequiresZeroIdAndAmount(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <Erc6909Op_SetOperatorRequiresZeroIdAndAmount as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(
                                MevSafeErrors::Erc6909Op_SetOperatorRequiresZeroIdAndAmount,
                            )
                    }
                    Erc6909Op_SetOperatorRequiresZeroIdAndAmount
                },
                {
                    fn ExpectedPause(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <ExpectedPause as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::ExpectedPause)
                    }
                    ExpectedPause
                },
                {
                    fn OnlyV3Vault(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <OnlyV3Vault as alloy_sol_types::SolError>::abi_decode_raw(data)
                            .map(MevSafeErrors::OnlyV3Vault)
                    }
                    OnlyV3Vault
                },
                {
                    fn MevSafe__UnsupportedFlashLender(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <MevSafe__UnsupportedFlashLender as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::MevSafe__UnsupportedFlashLender)
                    }
                    MevSafe__UnsupportedFlashLender
                },
                {
                    fn InvalidParams(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <InvalidParams as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::InvalidParams)
                    }
                    InvalidParams
                },
                {
                    fn Reentrancy(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <Reentrancy as alloy_sol_types::SolError>::abi_decode_raw(data)
                            .map(MevSafeErrors::Reentrancy)
                    }
                    Reentrancy
                },
                {
                    fn Erc6909Op_TransferRequiresZeroApproved(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <Erc6909Op_TransferRequiresZeroApproved as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::Erc6909Op_TransferRequiresZeroApproved)
                    }
                    Erc6909Op_TransferRequiresZeroApproved
                },
                {
                    fn MevSafe__NativeSweepFailed(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <MevSafe__NativeSweepFailed as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::MevSafe__NativeSweepFailed)
                    }
                    MevSafe__NativeSweepFailed
                },
                {
                    fn NotEntryPoint(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <NotEntryPoint as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::NotEntryPoint)
                    }
                    NotEntryPoint
                },
                {
                    fn EnforcedPause(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <EnforcedPause as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::EnforcedPause)
                    }
                    EnforcedPause
                },
                {
                    fn NotAuthorized(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <NotAuthorized as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::NotAuthorized)
                    }
                    NotAuthorized
                },
                {
                    fn NoActiveFlash(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <NoActiveFlash as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevSafeErrors::NoActiveFlash)
                    }
                    NoActiveFlash
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
            ) -> alloy_sol_types::Result<MevSafeErrors>] = &[
                {
                    fn FlashAmountMismatch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <FlashAmountMismatch as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::FlashAmountMismatch)
                    }
                    FlashAmountMismatch
                },
                {
                    fn FinanceUnprofitable(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <FinanceUnprofitable as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::FinanceUnprofitable)
                    }
                    FinanceUnprofitable
                },
                {
                    fn OwnableUnauthorizedAccount(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <OwnableUnauthorizedAccount as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::OwnableUnauthorizedAccount)
                    }
                    OwnableUnauthorizedAccount
                },
                {
                    fn OwnableInvalidOwner(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <OwnableInvalidOwner as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::OwnableInvalidOwner)
                    }
                    OwnableInvalidOwner
                },
                {
                    fn Erc6909SetOperatorBlocked(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <Erc6909SetOperatorBlocked as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::Erc6909SetOperatorBlocked)
                    }
                    Erc6909SetOperatorBlocked
                },
                {
                    fn LpTransferLib__V4TransferFailed(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <LpTransferLib__V4TransferFailed as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::LpTransferLib__V4TransferFailed)
                    }
                    LpTransferLib__V4TransferFailed
                },
                {
                    fn NotFlashLender(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <NotFlashLender as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::NotFlashLender)
                    }
                    NotFlashLender
                },
                {
                    fn LpTransferLib__V4SetOperatorFailed(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <LpTransferLib__V4SetOperatorFailed as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::LpTransferLib__V4SetOperatorFailed)
                    }
                    LpTransferLib__V4SetOperatorFailed
                },
                {
                    fn CallFailed(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <CallFailed as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::CallFailed)
                    }
                    CallFailed
                },
                {
                    fn PlanHashMismatch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <PlanHashMismatch as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::PlanHashMismatch)
                    }
                    PlanHashMismatch
                },
                {
                    fn V3SettleShortfall(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <V3SettleShortfall as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::V3SettleShortfall)
                    }
                    V3SettleShortfall
                },
                {
                    fn Erc6909Op_SetOperatorRequiresZeroIdAndAmount(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <Erc6909Op_SetOperatorRequiresZeroIdAndAmount as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(
                                MevSafeErrors::Erc6909Op_SetOperatorRequiresZeroIdAndAmount,
                            )
                    }
                    Erc6909Op_SetOperatorRequiresZeroIdAndAmount
                },
                {
                    fn ExpectedPause(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <ExpectedPause as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::ExpectedPause)
                    }
                    ExpectedPause
                },
                {
                    fn OnlyV3Vault(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <OnlyV3Vault as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::OnlyV3Vault)
                    }
                    OnlyV3Vault
                },
                {
                    fn MevSafe__UnsupportedFlashLender(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <MevSafe__UnsupportedFlashLender as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::MevSafe__UnsupportedFlashLender)
                    }
                    MevSafe__UnsupportedFlashLender
                },
                {
                    fn InvalidParams(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <InvalidParams as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::InvalidParams)
                    }
                    InvalidParams
                },
                {
                    fn Reentrancy(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <Reentrancy as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::Reentrancy)
                    }
                    Reentrancy
                },
                {
                    fn Erc6909Op_TransferRequiresZeroApproved(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <Erc6909Op_TransferRequiresZeroApproved as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::Erc6909Op_TransferRequiresZeroApproved)
                    }
                    Erc6909Op_TransferRequiresZeroApproved
                },
                {
                    fn MevSafe__NativeSweepFailed(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <MevSafe__NativeSweepFailed as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::MevSafe__NativeSweepFailed)
                    }
                    MevSafe__NativeSweepFailed
                },
                {
                    fn NotEntryPoint(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <NotEntryPoint as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::NotEntryPoint)
                    }
                    NotEntryPoint
                },
                {
                    fn EnforcedPause(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <EnforcedPause as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::EnforcedPause)
                    }
                    EnforcedPause
                },
                {
                    fn NotAuthorized(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <NotAuthorized as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::NotAuthorized)
                    }
                    NotAuthorized
                },
                {
                    fn NoActiveFlash(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevSafeErrors> {
                        <NoActiveFlash as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevSafeErrors::NoActiveFlash)
                    }
                    NoActiveFlash
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
                Self::CallFailed(inner) => {
                    <CallFailed as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
                Self::EnforcedPause(inner) => {
                    <EnforcedPause as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
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
                Self::ExpectedPause(inner) => {
                    <ExpectedPause as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
                Self::FinanceUnprofitable(inner) => {
                    <FinanceUnprofitable as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::FlashAmountMismatch(inner) => {
                    <FlashAmountMismatch as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::InvalidParams(inner) => {
                    <InvalidParams as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
                Self::LpTransferLib__V4SetOperatorFailed(inner) => {
                    <LpTransferLib__V4SetOperatorFailed as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::LpTransferLib__V4TransferFailed(inner) => {
                    <LpTransferLib__V4TransferFailed as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::MevSafe__NativeSweepFailed(inner) => {
                    <MevSafe__NativeSweepFailed as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::MevSafe__UnsupportedFlashLender(inner) => {
                    <MevSafe__UnsupportedFlashLender as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::NoActiveFlash(inner) => {
                    <NoActiveFlash as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
                Self::NotAuthorized(inner) => {
                    <NotAuthorized as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
                Self::NotEntryPoint(inner) => {
                    <NotEntryPoint as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
                Self::NotFlashLender(inner) => {
                    <NotFlashLender as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::OnlyV3Vault(inner) => {
                    <OnlyV3Vault as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
                Self::OwnableInvalidOwner(inner) => {
                    <OwnableInvalidOwner as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::OwnableUnauthorizedAccount(inner) => {
                    <OwnableUnauthorizedAccount as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::PlanHashMismatch(inner) => {
                    <PlanHashMismatch as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::Reentrancy(inner) => {
                    <Reentrancy as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
                Self::V3SettleShortfall(inner) => {
                    <V3SettleShortfall as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
            }
        }
        #[inline]
        fn abi_encode_raw(&self, out: &mut alloy_sol_types::private::Vec<u8>) {
            match self {
                Self::CallFailed(inner) => {
                    <CallFailed as alloy_sol_types::SolError>::abi_encode_raw(inner, out)
                }
                Self::EnforcedPause(inner) => {
                    <EnforcedPause as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
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
                Self::ExpectedPause(inner) => {
                    <ExpectedPause as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::FinanceUnprofitable(inner) => {
                    <FinanceUnprofitable as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::FlashAmountMismatch(inner) => {
                    <FlashAmountMismatch as alloy_sol_types::SolError>::abi_encode_raw(
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
                Self::LpTransferLib__V4SetOperatorFailed(inner) => {
                    <LpTransferLib__V4SetOperatorFailed as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::LpTransferLib__V4TransferFailed(inner) => {
                    <LpTransferLib__V4TransferFailed as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::MevSafe__NativeSweepFailed(inner) => {
                    <MevSafe__NativeSweepFailed as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::MevSafe__UnsupportedFlashLender(inner) => {
                    <MevSafe__UnsupportedFlashLender as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::NoActiveFlash(inner) => {
                    <NoActiveFlash as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::NotAuthorized(inner) => {
                    <NotAuthorized as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::NotEntryPoint(inner) => {
                    <NotEntryPoint as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::NotFlashLender(inner) => {
                    <NotFlashLender as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::OnlyV3Vault(inner) => {
                    <OnlyV3Vault as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::OwnableInvalidOwner(inner) => {
                    <OwnableInvalidOwner as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::OwnableUnauthorizedAccount(inner) => {
                    <OwnableUnauthorizedAccount as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::PlanHashMismatch(inner) => {
                    <PlanHashMismatch as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::Reentrancy(inner) => {
                    <Reentrancy as alloy_sol_types::SolError>::abi_encode_raw(inner, out)
                }
                Self::V3SettleShortfall(inner) => {
                    <V3SettleShortfall as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
            }
        }
    }
    #[automatically_derived]
    impl MevSafeErrors {
        /**Creates a [`CallFailed`] error.

```solidity
error CallFailed(uint256,bytes)
```*/
        #[inline]
        pub fn call_failed(
            index: alloy::sol_types::private::primitives::aliases::U256,
            return_data: alloy::sol_types::private::Bytes,
        ) -> Self {
            Self::CallFailed(CallFailed {
                index: index,
                returnData: return_data,
            })
        }
        /**Creates a [`EnforcedPause`] error.

```solidity
error EnforcedPause()
```*/
        #[inline]
        pub fn enforced_pause() -> Self {
            Self::EnforcedPause(EnforcedPause)
        }
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
        /**Creates a [`ExpectedPause`] error.

```solidity
error ExpectedPause()
```*/
        #[inline]
        pub fn expected_pause() -> Self {
            Self::ExpectedPause(ExpectedPause)
        }
        /**Creates a [`FinanceUnprofitable`] error.

```solidity
error FinanceUnprofitable(int256,int256)
```*/
        #[inline]
        pub fn finance_unprofitable(
            delta: alloy::sol_types::private::primitives::aliases::I256,
            min_delta: alloy::sol_types::private::primitives::aliases::I256,
        ) -> Self {
            Self::FinanceUnprofitable(FinanceUnprofitable {
                delta: delta,
                minDelta: min_delta,
            })
        }
        /**Creates a [`FlashAmountMismatch`] error.

```solidity
error FlashAmountMismatch()
```*/
        #[inline]
        pub fn flash_amount_mismatch() -> Self {
            Self::FlashAmountMismatch(FlashAmountMismatch)
        }
        /**Creates a [`InvalidParams`] error.

```solidity
error InvalidParams()
```*/
        #[inline]
        pub fn invalid_params() -> Self {
            Self::InvalidParams(InvalidParams)
        }
        /**Creates a [`LpTransferLib__V4SetOperatorFailed`] error.

```solidity
error LpTransferLib__V4SetOperatorFailed()
```*/
        #[inline]
        pub fn lp_transfer_lib_v_4_set_operator_failed() -> Self {
            Self::LpTransferLib__V4SetOperatorFailed(LpTransferLib__V4SetOperatorFailed)
        }
        /**Creates a [`LpTransferLib__V4TransferFailed`] error.

```solidity
error LpTransferLib__V4TransferFailed()
```*/
        #[inline]
        pub fn lp_transfer_lib_v_4_transfer_failed() -> Self {
            Self::LpTransferLib__V4TransferFailed(LpTransferLib__V4TransferFailed)
        }
        /**Creates a [`MevSafe__NativeSweepFailed`] error.

```solidity
error MevSafe__NativeSweepFailed()
```*/
        #[inline]
        pub fn mev_safe_native_sweep_failed() -> Self {
            Self::MevSafe__NativeSweepFailed(MevSafe__NativeSweepFailed)
        }
        /**Creates a [`MevSafe__UnsupportedFlashLender`] error.

```solidity
error MevSafe__UnsupportedFlashLender(address)
```*/
        #[inline]
        pub fn mev_safe_unsupported_flash_lender(
            lender: alloy::sol_types::private::Address,
        ) -> Self {
            Self::MevSafe__UnsupportedFlashLender(MevSafe__UnsupportedFlashLender {
                lender: lender,
            })
        }
        /**Creates a [`NoActiveFlash`] error.

```solidity
error NoActiveFlash()
```*/
        #[inline]
        pub fn no_active_flash() -> Self {
            Self::NoActiveFlash(NoActiveFlash)
        }
        /**Creates a [`NotAuthorized`] error.

```solidity
error NotAuthorized()
```*/
        #[inline]
        pub fn not_authorized() -> Self {
            Self::NotAuthorized(NotAuthorized)
        }
        /**Creates a [`NotEntryPoint`] error.

```solidity
error NotEntryPoint()
```*/
        #[inline]
        pub fn not_entry_point() -> Self {
            Self::NotEntryPoint(NotEntryPoint)
        }
        /**Creates a [`NotFlashLender`] error.

```solidity
error NotFlashLender()
```*/
        #[inline]
        pub fn not_flash_lender() -> Self {
            Self::NotFlashLender(NotFlashLender)
        }
        /**Creates a [`OnlyV3Vault`] error.

```solidity
error OnlyV3Vault()
```*/
        #[inline]
        pub fn only_v_3_vault() -> Self {
            Self::OnlyV3Vault(OnlyV3Vault)
        }
        /**Creates a [`OwnableInvalidOwner`] error.

```solidity
error OwnableInvalidOwner(address)
```*/
        #[inline]
        pub fn ownable_invalid_owner(owner: alloy::sol_types::private::Address) -> Self {
            Self::OwnableInvalidOwner(OwnableInvalidOwner {
                owner: owner,
            })
        }
        /**Creates a [`OwnableUnauthorizedAccount`] error.

```solidity
error OwnableUnauthorizedAccount(address)
```*/
        #[inline]
        pub fn ownable_unauthorized_account(
            account: alloy::sol_types::private::Address,
        ) -> Self {
            Self::OwnableUnauthorizedAccount(OwnableUnauthorizedAccount {
                account: account,
            })
        }
        /**Creates a [`PlanHashMismatch`] error.

```solidity
error PlanHashMismatch()
```*/
        #[inline]
        pub fn plan_hash_mismatch() -> Self {
            Self::PlanHashMismatch(PlanHashMismatch)
        }
        /**Creates a [`Reentrancy`] error.

```solidity
error Reentrancy()
```*/
        #[inline]
        pub fn reentrancy() -> Self {
            Self::Reentrancy(Reentrancy)
        }
        /**Creates a [`V3SettleShortfall`] error.

```solidity
error V3SettleShortfall(uint256,uint256)
```*/
        #[inline]
        pub fn v_3_settle_shortfall(
            expected: alloy::sol_types::private::primitives::aliases::U256,
            credit: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::V3SettleShortfall(V3SettleShortfall {
                expected: expected,
                credit: credit,
            })
        }
    }
    ///Container for all the [`MevSafe`](self) events.
    #[derive(Clone)]
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub enum MevSafeEvents {
        #[allow(missing_docs)]
        CollateralizationFinanced(CollateralizationFinanced),
        #[allow(missing_docs)]
        Erc6909OperatorSet(Erc6909OperatorSet),
        #[allow(missing_docs)]
        Erc6909Transferred(Erc6909Transferred),
        #[allow(missing_docs)]
        Executed(Executed),
        #[allow(missing_docs)]
        LegacyDelegateeSet(LegacyDelegateeSet),
        #[allow(missing_docs)]
        LpMoved(LpMoved),
        #[allow(missing_docs)]
        OwnershipTransferStarted(OwnershipTransferStarted),
        #[allow(missing_docs)]
        OwnershipTransferred(OwnershipTransferred),
        #[allow(missing_docs)]
        Paused(Paused),
        #[allow(missing_docs)]
        Received(Received),
        #[allow(missing_docs)]
        SignerSet(SignerSet),
        #[allow(missing_docs)]
        ThresholdSet(ThresholdSet),
        #[allow(missing_docs)]
        Unpaused(Unpaused),
        #[allow(missing_docs)]
        V4OperatorSet(V4OperatorSet),
    }
    impl MevSafeEvents {
        /// All the selectors of this enum.
        ///
        /// Note that the selectors might not be in the same order as the variants.
        /// No guarantees are made about the order of the selectors.
        ///
        /// Prefer using `SolInterface` methods instead.
        pub const SELECTORS: &'static [[u8; 32usize]] = &[
            [
                4u8, 53u8, 65u8, 106u8, 95u8, 72u8, 212u8, 29u8, 94u8, 94u8, 222u8, 45u8,
                5u8, 192u8, 225u8, 255u8, 107u8, 31u8, 113u8, 203u8, 87u8, 23u8, 107u8,
                16u8, 17u8, 234u8, 127u8, 189u8, 140u8, 114u8, 90u8, 131u8,
            ],
            [
                53u8, 226u8, 89u8, 193u8, 167u8, 129u8, 220u8, 38u8, 73u8, 231u8, 98u8,
                152u8, 181u8, 164u8, 213u8, 72u8, 199u8, 144u8, 82u8, 135u8, 202u8, 61u8,
                124u8, 67u8, 55u8, 72u8, 12u8, 227u8, 253u8, 5u8, 235u8, 105u8,
            ],
            [
                56u8, 209u8, 107u8, 140u8, 172u8, 34u8, 217u8, 159u8, 199u8, 193u8, 36u8,
                185u8, 205u8, 13u8, 226u8, 211u8, 250u8, 31u8, 174u8, 244u8, 32u8, 191u8,
                231u8, 145u8, 216u8, 195u8, 98u8, 215u8, 101u8, 226u8, 39u8, 0u8,
            ],
            [
                74u8, 148u8, 248u8, 158u8, 19u8, 22u8, 153u8, 237u8, 52u8, 22u8, 103u8,
                12u8, 1u8, 28u8, 230u8, 77u8, 98u8, 229u8, 165u8, 129u8, 164u8, 235u8,
                180u8, 96u8, 59u8, 246u8, 196u8, 165u8, 208u8, 106u8, 6u8, 206u8,
            ],
            [
                83u8, 242u8, 19u8, 51u8, 85u8, 6u8, 59u8, 7u8, 135u8, 190u8, 155u8,
                115u8, 249u8, 242u8, 195u8, 214u8, 225u8, 70u8, 112u8, 226u8, 163u8,
                125u8, 212u8, 98u8, 54u8, 139u8, 93u8, 26u8, 148u8, 232u8, 107u8, 6u8,
            ],
            [
                93u8, 185u8, 238u8, 10u8, 73u8, 91u8, 242u8, 230u8, 255u8, 156u8, 145u8,
                167u8, 131u8, 76u8, 27u8, 164u8, 253u8, 210u8, 68u8, 165u8, 232u8, 170u8,
                78u8, 83u8, 123u8, 211u8, 138u8, 234u8, 228u8, 176u8, 115u8, 170u8,
            ],
            [
                98u8, 231u8, 140u8, 234u8, 1u8, 190u8, 227u8, 32u8, 205u8, 78u8, 66u8,
                2u8, 112u8, 181u8, 234u8, 116u8, 0u8, 13u8, 17u8, 176u8, 201u8, 247u8,
                71u8, 84u8, 235u8, 219u8, 252u8, 84u8, 75u8, 5u8, 162u8, 88u8,
            ],
            [
                110u8, 138u8, 24u8, 125u8, 121u8, 68u8, 153u8, 128u8, 133u8, 219u8,
                209u8, 241u8, 107u8, 132u8, 197u8, 28u8, 144u8, 59u8, 183u8, 39u8, 83u8,
                108u8, 219u8, 168u8, 105u8, 98u8, 67u8, 154u8, 222u8, 210u8, 207u8, 215u8,
            ],
            [
                130u8, 228u8, 49u8, 64u8, 252u8, 65u8, 219u8, 202u8, 182u8, 22u8, 59u8,
                194u8, 189u8, 125u8, 223u8, 64u8, 215u8, 71u8, 114u8, 134u8, 251u8,
                124u8, 149u8, 195u8, 124u8, 29u8, 188u8, 149u8, 119u8, 86u8, 169u8, 186u8,
            ],
            [
                139u8, 224u8, 7u8, 156u8, 83u8, 22u8, 89u8, 20u8, 19u8, 68u8, 205u8,
                31u8, 208u8, 164u8, 242u8, 132u8, 25u8, 73u8, 127u8, 151u8, 34u8, 163u8,
                218u8, 175u8, 227u8, 180u8, 24u8, 111u8, 107u8, 100u8, 87u8, 224u8,
            ],
            [
                156u8, 142u8, 23u8, 250u8, 17u8, 77u8, 36u8, 207u8, 200u8, 246u8, 124u8,
                61u8, 108u8, 230u8, 188u8, 46u8, 36u8, 6u8, 125u8, 190u8, 65u8, 37u8,
                102u8, 64u8, 188u8, 72u8, 253u8, 109u8, 16u8, 102u8, 86u8, 47u8,
            ],
            [
                209u8, 236u8, 3u8, 13u8, 160u8, 249u8, 159u8, 204u8, 173u8, 29u8, 226u8,
                119u8, 77u8, 201u8, 130u8, 119u8, 182u8, 231u8, 105u8, 58u8, 84u8, 159u8,
                71u8, 81u8, 195u8, 25u8, 207u8, 135u8, 55u8, 155u8, 195u8, 12u8,
            ],
            [
                224u8, 143u8, 137u8, 37u8, 244u8, 92u8, 51u8, 125u8, 81u8, 75u8, 7u8,
                175u8, 37u8, 38u8, 225u8, 68u8, 73u8, 188u8, 249u8, 10u8, 253u8, 146u8,
                239u8, 216u8, 182u8, 17u8, 241u8, 126u8, 191u8, 65u8, 157u8, 176u8,
            ],
            [
                252u8, 74u8, 203u8, 73u8, 148u8, 145u8, 205u8, 133u8, 10u8, 138u8, 33u8,
                171u8, 152u8, 199u8, 241u8, 40u8, 133u8, 12u8, 15u8, 14u8, 95u8, 26u8,
                135u8, 90u8, 98u8, 183u8, 250u8, 5u8, 92u8, 46u8, 207u8, 25u8,
            ],
        ];
        /// The names of the variants in the same order as `SELECTORS`.
        pub const VARIANT_NAMES: &'static [&'static str] = &[
            ::core::stringify!(LegacyDelegateeSet),
            ::core::stringify!(LpMoved),
            ::core::stringify!(OwnershipTransferStarted),
            ::core::stringify!(Erc6909Transferred),
            ::core::stringify!(CollateralizationFinanced),
            ::core::stringify!(Unpaused),
            ::core::stringify!(Paused),
            ::core::stringify!(ThresholdSet),
            ::core::stringify!(V4OperatorSet),
            ::core::stringify!(OwnershipTransferred),
            ::core::stringify!(Erc6909OperatorSet),
            ::core::stringify!(Received),
            ::core::stringify!(Executed),
            ::core::stringify!(SignerSet),
        ];
        /// The signatures in the same order as `SELECTORS`.
        pub const SIGNATURES: &'static [&'static str] = &[
            <LegacyDelegateeSet as alloy_sol_types::SolEvent>::SIGNATURE,
            <LpMoved as alloy_sol_types::SolEvent>::SIGNATURE,
            <OwnershipTransferStarted as alloy_sol_types::SolEvent>::SIGNATURE,
            <Erc6909Transferred as alloy_sol_types::SolEvent>::SIGNATURE,
            <CollateralizationFinanced as alloy_sol_types::SolEvent>::SIGNATURE,
            <Unpaused as alloy_sol_types::SolEvent>::SIGNATURE,
            <Paused as alloy_sol_types::SolEvent>::SIGNATURE,
            <ThresholdSet as alloy_sol_types::SolEvent>::SIGNATURE,
            <V4OperatorSet as alloy_sol_types::SolEvent>::SIGNATURE,
            <OwnershipTransferred as alloy_sol_types::SolEvent>::SIGNATURE,
            <Erc6909OperatorSet as alloy_sol_types::SolEvent>::SIGNATURE,
            <Received as alloy_sol_types::SolEvent>::SIGNATURE,
            <Executed as alloy_sol_types::SolEvent>::SIGNATURE,
            <SignerSet as alloy_sol_types::SolEvent>::SIGNATURE,
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
    impl alloy_sol_types::SolEventInterface for MevSafeEvents {
        const NAME: &'static str = "MevSafeEvents";
        const COUNT: usize = 14usize;
        fn decode_raw_log(
            topics: &[alloy_sol_types::Word],
            data: &[u8],
        ) -> alloy_sol_types::Result<Self> {
            match topics.first().copied() {
                Some(
                    <CollateralizationFinanced as alloy_sol_types::SolEvent>::SIGNATURE_HASH,
                ) => {
                    <CollateralizationFinanced as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::CollateralizationFinanced)
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
                Some(<Executed as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <Executed as alloy_sol_types::SolEvent>::decode_raw_log(topics, data)
                        .map(Self::Executed)
                }
                Some(
                    <LegacyDelegateeSet as alloy_sol_types::SolEvent>::SIGNATURE_HASH,
                ) => {
                    <LegacyDelegateeSet as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::LegacyDelegateeSet)
                }
                Some(<LpMoved as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <LpMoved as alloy_sol_types::SolEvent>::decode_raw_log(topics, data)
                        .map(Self::LpMoved)
                }
                Some(
                    <OwnershipTransferStarted as alloy_sol_types::SolEvent>::SIGNATURE_HASH,
                ) => {
                    <OwnershipTransferStarted as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::OwnershipTransferStarted)
                }
                Some(
                    <OwnershipTransferred as alloy_sol_types::SolEvent>::SIGNATURE_HASH,
                ) => {
                    <OwnershipTransferred as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::OwnershipTransferred)
                }
                Some(<Paused as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <Paused as alloy_sol_types::SolEvent>::decode_raw_log(topics, data)
                        .map(Self::Paused)
                }
                Some(<Received as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <Received as alloy_sol_types::SolEvent>::decode_raw_log(topics, data)
                        .map(Self::Received)
                }
                Some(<SignerSet as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <SignerSet as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::SignerSet)
                }
                Some(<ThresholdSet as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <ThresholdSet as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::ThresholdSet)
                }
                Some(<Unpaused as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <Unpaused as alloy_sol_types::SolEvent>::decode_raw_log(topics, data)
                        .map(Self::Unpaused)
                }
                Some(<V4OperatorSet as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <V4OperatorSet as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::V4OperatorSet)
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
    impl alloy_sol_types::private::IntoLogData for MevSafeEvents {
        fn to_log_data(&self) -> alloy_sol_types::private::LogData {
            match self {
                Self::CollateralizationFinanced(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Erc6909OperatorSet(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Erc6909Transferred(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Executed(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::LegacyDelegateeSet(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::LpMoved(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::OwnershipTransferStarted(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::OwnershipTransferred(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Paused(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Received(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::SignerSet(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::ThresholdSet(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Unpaused(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::V4OperatorSet(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
            }
        }
        fn into_log_data(self) -> alloy_sol_types::private::LogData {
            match self {
                Self::CollateralizationFinanced(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Erc6909OperatorSet(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Erc6909Transferred(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Executed(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::LegacyDelegateeSet(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::LpMoved(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::OwnershipTransferStarted(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::OwnershipTransferred(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Paused(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Received(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::SignerSet(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::ThresholdSet(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Unpaused(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::V4OperatorSet(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
            }
        }
    }
    #[automatically_derived]
    impl MevSafeEvents {
        /**Creates a [`CollateralizationFinanced`] event.

```solidity
event CollateralizationFinanced(bytes32,address,uint256,address,uint256,address,uint256,uint256,int256)
```*/
        #[inline]
        pub fn collateralization_financed(
            op_id: alloy::sol_types::private::FixedBytes<32>,
            flash_asset: alloy::sol_types::private::Address,
            flash_amount: alloy::sol_types::private::primitives::aliases::U256,
            collateral_asset: alloy::sol_types::private::Address,
            supply_amount: alloy::sol_types::private::primitives::aliases::U256,
            debt_asset: alloy::sol_types::private::Address,
            borrow_amount: alloy::sol_types::private::primitives::aliases::U256,
            flash_repaid: alloy::sol_types::private::primitives::aliases::U256,
            net_delta_flash: alloy::sol_types::private::primitives::aliases::I256,
        ) -> Self {
            Self::CollateralizationFinanced(CollateralizationFinanced {
                opId: op_id,
                flashAsset: flash_asset,
                flashAmount: flash_amount,
                collateralAsset: collateral_asset,
                supplyAmount: supply_amount,
                debtAsset: debt_asset,
                borrowAmount: borrow_amount,
                flashRepaid: flash_repaid,
                netDeltaFlash: net_delta_flash,
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
        /**Creates a [`Executed`] event.

```solidity
event Executed(address,uint256,bytes4)
```*/
        #[inline]
        pub fn executed(
            target: alloy::sol_types::private::Address,
            value: alloy::sol_types::private::primitives::aliases::U256,
            selector: alloy::sol_types::private::FixedBytes<4>,
        ) -> Self {
            Self::Executed(Executed {
                target: target,
                value: value,
                selector: selector,
            })
        }
        /**Creates a [`LegacyDelegateeSet`] event.

```solidity
event LegacyDelegateeSet(address,bool)
```*/
        #[inline]
        pub fn legacy_delegatee_set(
            account: alloy::sol_types::private::Address,
            allowed: bool,
        ) -> Self {
            Self::LegacyDelegateeSet(LegacyDelegateeSet {
                account: account,
                allowed: allowed,
            })
        }
        /**Creates a [`LpMoved`] event.

```solidity
event LpMoved(uint8,address,uint256,uint256,address)
```*/
        #[inline]
        pub fn lp_moved(
            kind: <LpTransferLib::LpKind as alloy::sol_types::SolType>::RustType,
            pool: alloy::sol_types::private::Address,
            id_or_token_id: alloy::sol_types::private::primitives::aliases::U256,
            amount: alloy::sol_types::private::primitives::aliases::U256,
            to: alloy::sol_types::private::Address,
        ) -> Self {
            Self::LpMoved(LpMoved {
                kind: kind,
                pool: pool,
                idOrTokenId: id_or_token_id,
                amount: amount,
                to: to,
            })
        }
        /**Creates a [`OwnershipTransferStarted`] event.

```solidity
event OwnershipTransferStarted(address,address)
```*/
        #[inline]
        pub fn ownership_transfer_started(
            previous_owner: alloy::sol_types::private::Address,
            new_owner: alloy::sol_types::private::Address,
        ) -> Self {
            Self::OwnershipTransferStarted(OwnershipTransferStarted {
                previousOwner: previous_owner,
                newOwner: new_owner,
            })
        }
        /**Creates a [`OwnershipTransferred`] event.

```solidity
event OwnershipTransferred(address,address)
```*/
        #[inline]
        pub fn ownership_transferred(
            previous_owner: alloy::sol_types::private::Address,
            new_owner: alloy::sol_types::private::Address,
        ) -> Self {
            Self::OwnershipTransferred(OwnershipTransferred {
                previousOwner: previous_owner,
                newOwner: new_owner,
            })
        }
        /**Creates a [`Paused`] event.

```solidity
event Paused(address)
```*/
        #[inline]
        pub fn paused(account: alloy::sol_types::private::Address) -> Self {
            Self::Paused(Paused { account: account })
        }
        /**Creates a [`Received`] event.

```solidity
event Received(bytes4,address,uint256,uint256)
```*/
        #[inline]
        pub fn received(
            standard: alloy::sol_types::private::FixedBytes<4>,
            sender: alloy::sol_types::private::Address,
            token_id: alloy::sol_types::private::primitives::aliases::U256,
            value: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::Received(Received {
                standard: standard,
                sender: sender,
                tokenId: token_id,
                value: value,
            })
        }
        /**Creates a [`SignerSet`] event.

```solidity
event SignerSet(address,bool)
```*/
        #[inline]
        pub fn signer_set(
            signer: alloy::sol_types::private::Address,
            allowed: bool,
        ) -> Self {
            Self::SignerSet(SignerSet {
                signer: signer,
                allowed: allowed,
            })
        }
        /**Creates a [`ThresholdSet`] event.

```solidity
event ThresholdSet(uint256)
```*/
        #[inline]
        pub fn threshold_set(
            new_threshold: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::ThresholdSet(ThresholdSet {
                newThreshold: new_threshold,
            })
        }
        /**Creates a [`Unpaused`] event.

```solidity
event Unpaused(address)
```*/
        #[inline]
        pub fn unpaused(account: alloy::sol_types::private::Address) -> Self {
            Self::Unpaused(Unpaused { account: account })
        }
        /**Creates a [`V4OperatorSet`] event.

```solidity
event V4OperatorSet(address,address,bool)
```*/
        #[inline]
        pub fn v_4_operator_set(
            pool_manager: alloy::sol_types::private::Address,
            operator: alloy::sol_types::private::Address,
            approved: bool,
        ) -> Self {
            Self::V4OperatorSet(V4OperatorSet {
                poolManager: pool_manager,
                operator: operator,
                approved: approved,
            })
        }
    }
    use alloy::contract as alloy_contract;
    /**Creates a new wrapper around an on-chain [`MevSafe`](self) contract instance.

See the [wrapper's documentation](`MevSafeInstance`) for more details.*/
    #[inline]
    pub const fn new<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    >(
        address: alloy_sol_types::private::Address,
        __provider: P,
    ) -> MevSafeInstance<P, N> {
        MevSafeInstance::<P, N>::new(address, __provider)
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
        initialOwner: alloy::sol_types::private::Address,
        permissions: alloy::sol_types::private::Address,
    ) -> impl ::core::future::Future<
        Output = alloy_contract::Result<MevSafeInstance<P, N>>,
    > {
        MevSafeInstance::<P, N>::deploy(__provider, initialOwner, permissions)
    }
    /**Creates a `RawCallBuilder` for deploying this contract using the given `provider`
and constructor arguments, if any.

This is a simple wrapper around creating a `RawCallBuilder` with the data set to
the bytecode concatenated with the constructor's ABI-encoded arguments.*/
    #[inline]
    pub fn deploy_builder<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    >(
        __provider: P,
        initialOwner: alloy::sol_types::private::Address,
        permissions: alloy::sol_types::private::Address,
    ) -> alloy_contract::RawCallBuilder<P, N> {
        MevSafeInstance::<P, N>::deploy_builder(__provider, initialOwner, permissions)
    }
    /**A [`MevSafe`](self) instance.

Contains type-safe methods for interacting with an on-chain instance of the
[`MevSafe`](self) contract located at a given `address`, using a given
provider `P`.

If the contract bytecode is available (see the [`sol!`](alloy_sol_types::sol!)
documentation on how to provide it), the `deploy` and `deploy_builder` methods can
be used to deploy a new instance of the contract.

See the [module-level documentation](self) for all the available methods.*/
    #[derive(Clone)]
    pub struct MevSafeInstance<P, N = alloy_contract::private::Ethereum> {
        address: alloy_sol_types::private::Address,
        provider: P,
        _network: ::core::marker::PhantomData<N>,
    }
    #[automatically_derived]
    impl<P, N> ::core::fmt::Debug for MevSafeInstance<P, N> {
        #[inline]
        fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
            f.debug_tuple("MevSafeInstance").field(&self.address).finish()
        }
    }
    /// Instantiation and getters/setters.
    impl<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    > MevSafeInstance<P, N> {
        /**Creates a new wrapper around an on-chain [`MevSafe`](self) contract instance.

See the [wrapper's documentation](`MevSafeInstance`) for more details.*/
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
            initialOwner: alloy::sol_types::private::Address,
            permissions: alloy::sol_types::private::Address,
        ) -> alloy_contract::Result<MevSafeInstance<P, N>> {
            let call_builder = Self::deploy_builder(
                __provider,
                initialOwner,
                permissions,
            );
            let contract_address = call_builder.deploy().await?;
            Ok(Self::new(contract_address, call_builder.provider))
        }
        /**Creates a `RawCallBuilder` for deploying this contract using the given `provider`
and constructor arguments, if any.

This is a simple wrapper around creating a `RawCallBuilder` with the data set to
the bytecode concatenated with the constructor's ABI-encoded arguments.*/
        #[inline]
        pub fn deploy_builder(
            __provider: P,
            initialOwner: alloy::sol_types::private::Address,
            permissions: alloy::sol_types::private::Address,
        ) -> alloy_contract::RawCallBuilder<P, N> {
            alloy_contract::RawCallBuilder::new_raw_deploy(
                __provider,
                [
                    &BYTECODE[..],
                    &alloy_sol_types::SolConstructor::abi_encode(
                        &constructorCall {
                            initialOwner,
                            permissions,
                        },
                    )[..],
                ]
                    .concat()
                    .into(),
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
    impl<P: ::core::clone::Clone, N> MevSafeInstance<&P, N> {
        /// Clones the provider and returns a new instance with the cloned provider.
        #[inline]
        pub fn with_cloned_provider(self) -> MevSafeInstance<P, N> {
            MevSafeInstance {
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
    > MevSafeInstance<P, N> {
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
        ///Creates a new call builder for the [`AAVE_V3_POOL`] function.
        pub fn AAVE_V3_POOL(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, AAVE_V3_POOLCall, N> {
            self.call_builder(&AAVE_V3_POOLCall)
        }
        ///Creates a new call builder for the [`BALANCER_V2_VAULT`] function.
        pub fn BALANCER_V2_VAULT(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, BALANCER_V2_VAULTCall, N> {
            self.call_builder(&BALANCER_V2_VAULTCall)
        }
        ///Creates a new call builder for the [`BALANCER_V3_VAULT`] function.
        pub fn BALANCER_V3_VAULT(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, BALANCER_V3_VAULTCall, N> {
            self.call_builder(&BALANCER_V3_VAULTCall)
        }
        ///Creates a new call builder for the [`ENTRY_POINT`] function.
        pub fn ENTRY_POINT(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, ENTRY_POINTCall, N> {
            self.call_builder(&ENTRY_POINTCall)
        }
        ///Creates a new call builder for the [`MORPHO_BLUE`] function.
        pub fn MORPHO_BLUE(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, MORPHO_BLUECall, N> {
            self.call_builder(&MORPHO_BLUECall)
        }
        ///Creates a new call builder for the [`PERMISSIONS`] function.
        pub fn PERMISSIONS(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, PERMISSIONSCall, N> {
            self.call_builder(&PERMISSIONSCall)
        }
        ///Creates a new call builder for the [`aaveBorrow`] function.
        pub fn aaveBorrow(
            &self,
            pool: alloy::sol_types::private::Address,
            asset: alloy::sol_types::private::Address,
            amount: alloy::sol_types::private::primitives::aliases::U256,
            interestRateMode: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, aaveBorrowCall, N> {
            self.call_builder(
                &aaveBorrowCall {
                    pool,
                    asset,
                    amount,
                    interestRateMode,
                },
            )
        }
        ///Creates a new call builder for the [`aaveRepay`] function.
        pub fn aaveRepay(
            &self,
            pool: alloy::sol_types::private::Address,
            asset: alloy::sol_types::private::Address,
            amount: alloy::sol_types::private::primitives::aliases::U256,
            interestRateMode: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, aaveRepayCall, N> {
            self.call_builder(
                &aaveRepayCall {
                    pool,
                    asset,
                    amount,
                    interestRateMode,
                },
            )
        }
        ///Creates a new call builder for the [`aaveSupply`] function.
        pub fn aaveSupply(
            &self,
            pool: alloy::sol_types::private::Address,
            asset: alloy::sol_types::private::Address,
            amount: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, aaveSupplyCall, N> {
            self.call_builder(
                &aaveSupplyCall {
                    pool,
                    asset,
                    amount,
                },
            )
        }
        ///Creates a new call builder for the [`aaveWithdraw`] function.
        pub fn aaveWithdraw(
            &self,
            pool: alloy::sol_types::private::Address,
            asset: alloy::sol_types::private::Address,
            amount: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, aaveWithdrawCall, N> {
            self.call_builder(
                &aaveWithdrawCall {
                    pool,
                    asset,
                    amount,
                },
            )
        }
        ///Creates a new call builder for the [`acceptOwnership`] function.
        pub fn acceptOwnership(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, acceptOwnershipCall, N> {
            self.call_builder(&acceptOwnershipCall)
        }
        ///Creates a new call builder for the [`execute`] function.
        pub fn execute(
            &self,
            c: <Call as alloy::sol_types::SolType>::RustType,
        ) -> alloy_contract::SolCallBuilder<&P, executeCall, N> {
            self.call_builder(&executeCall { c })
        }
        ///Creates a new call builder for the [`executeBatch`] function.
        pub fn executeBatch(
            &self,
            calls: alloy::sol_types::private::Vec<
                <Call as alloy::sol_types::SolType>::RustType,
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
        ///Creates a new call builder for the [`executeFinanceUnlock`] function.
        pub fn executeFinanceUnlock(
            &self,
            plan: <FinancePlanV3 as alloy::sol_types::SolType>::RustType,
        ) -> alloy_contract::SolCallBuilder<&P, executeFinanceUnlockCall, N> {
            self.call_builder(&executeFinanceUnlockCall { plan })
        }
        ///Creates a new call builder for the [`flashCollateralize`] function.
        pub fn flashCollateralize(
            &self,
            plan: <FinancePlan as alloy::sol_types::SolType>::RustType,
        ) -> alloy_contract::SolCallBuilder<&P, flashCollateralizeCall, N> {
            self.call_builder(&flashCollateralizeCall { plan })
        }
        ///Creates a new call builder for the [`flashCollateralizeV3`] function.
        pub fn flashCollateralizeV3(
            &self,
            plan: <FinancePlanV3 as alloy::sol_types::SolType>::RustType,
        ) -> alloy_contract::SolCallBuilder<&P, flashCollateralizeV3Call, N> {
            self.call_builder(&flashCollateralizeV3Call { plan })
        }
        ///Creates a new call builder for the [`isAuthorized`] function.
        pub fn isAuthorized(
            &self,
            account: alloy::sol_types::private::Address,
            target: alloy::sol_types::private::Address,
            selector: alloy::sol_types::private::FixedBytes<4>,
        ) -> alloy_contract::SolCallBuilder<&P, isAuthorizedCall, N> {
            self.call_builder(
                &isAuthorizedCall {
                    account,
                    target,
                    selector,
                },
            )
        }
        ///Creates a new call builder for the [`isSigner`] function.
        pub fn isSigner(
            &self,
            _0: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, isSignerCall, N> {
            self.call_builder(&isSignerCall(_0))
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
        ///Creates a new call builder for the [`legacyDelegatees`] function.
        pub fn legacyDelegatees(
            &self,
            _0: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, legacyDelegateesCall, N> {
            self.call_builder(&legacyDelegateesCall(_0))
        }
        ///Creates a new call builder for the [`moveLpV3Nft`] function.
        pub fn moveLpV3Nft(
            &self,
            positionManager: alloy::sol_types::private::Address,
            tokenId: alloy::sol_types::private::primitives::aliases::U256,
            to: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, moveLpV3NftCall, N> {
            self.call_builder(
                &moveLpV3NftCall {
                    positionManager,
                    tokenId,
                    to,
                },
            )
        }
        ///Creates a new call builder for the [`moveLpV4`] function.
        pub fn moveLpV4(
            &self,
            poolManager: alloy::sol_types::private::Address,
            id: alloy::sol_types::private::primitives::aliases::U256,
            amount: alloy::sol_types::private::primitives::aliases::U256,
            to: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, moveLpV4Call, N> {
            self.call_builder(
                &moveLpV4Call {
                    poolManager,
                    id,
                    amount,
                    to,
                },
            )
        }
        ///Creates a new call builder for the [`onERC1155BatchReceived`] function.
        pub fn onERC1155BatchReceived(
            &self,
            operator: alloy::sol_types::private::Address,
            from: alloy::sol_types::private::Address,
            ids: alloy::sol_types::private::Vec<
                alloy::sol_types::private::primitives::aliases::U256,
            >,
            values: alloy::sol_types::private::Vec<
                alloy::sol_types::private::primitives::aliases::U256,
            >,
            data: alloy::sol_types::private::Bytes,
        ) -> alloy_contract::SolCallBuilder<&P, onERC1155BatchReceivedCall, N> {
            self.call_builder(
                &onERC1155BatchReceivedCall {
                    operator,
                    from,
                    ids,
                    values,
                    data,
                },
            )
        }
        ///Creates a new call builder for the [`onERC1155Received`] function.
        pub fn onERC1155Received(
            &self,
            operator: alloy::sol_types::private::Address,
            from: alloy::sol_types::private::Address,
            id: alloy::sol_types::private::primitives::aliases::U256,
            value: alloy::sol_types::private::primitives::aliases::U256,
            data: alloy::sol_types::private::Bytes,
        ) -> alloy_contract::SolCallBuilder<&P, onERC1155ReceivedCall, N> {
            self.call_builder(
                &onERC1155ReceivedCall {
                    operator,
                    from,
                    id,
                    value,
                    data,
                },
            )
        }
        ///Creates a new call builder for the [`onERC721Received`] function.
        pub fn onERC721Received(
            &self,
            operator: alloy::sol_types::private::Address,
            from: alloy::sol_types::private::Address,
            tokenId: alloy::sol_types::private::primitives::aliases::U256,
            data: alloy::sol_types::private::Bytes,
        ) -> alloy_contract::SolCallBuilder<&P, onERC721ReceivedCall, N> {
            self.call_builder(
                &onERC721ReceivedCall {
                    operator,
                    from,
                    tokenId,
                    data,
                },
            )
        }
        ///Creates a new call builder for the [`owner`] function.
        pub fn owner(&self) -> alloy_contract::SolCallBuilder<&P, ownerCall, N> {
            self.call_builder(&ownerCall)
        }
        ///Creates a new call builder for the [`pause`] function.
        pub fn pause(&self) -> alloy_contract::SolCallBuilder<&P, pauseCall, N> {
            self.call_builder(&pauseCall)
        }
        ///Creates a new call builder for the [`paused`] function.
        pub fn paused(&self) -> alloy_contract::SolCallBuilder<&P, pausedCall, N> {
            self.call_builder(&pausedCall)
        }
        ///Creates a new call builder for the [`pendingOwner`] function.
        pub fn pendingOwner(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, pendingOwnerCall, N> {
            self.call_builder(&pendingOwnerCall)
        }
        ///Creates a new call builder for the [`receiveFlashLoan`] function.
        pub fn receiveFlashLoan(
            &self,
            tokens: alloy::sol_types::private::Vec<alloy::sol_types::private::Address>,
            amounts: alloy::sol_types::private::Vec<
                alloy::sol_types::private::primitives::aliases::U256,
            >,
            feeAmounts: alloy::sol_types::private::Vec<
                alloy::sol_types::private::primitives::aliases::U256,
            >,
            userData: alloy::sol_types::private::Bytes,
        ) -> alloy_contract::SolCallBuilder<&P, receiveFlashLoanCall, N> {
            self.call_builder(
                &receiveFlashLoanCall {
                    tokens,
                    amounts,
                    feeAmounts,
                    userData,
                },
            )
        }
        ///Creates a new call builder for the [`renounceOwnership`] function.
        pub fn renounceOwnership(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, renounceOwnershipCall, N> {
            self.call_builder(&renounceOwnershipCall)
        }
        ///Creates a new call builder for the [`setLegacyDelegatee`] function.
        pub fn setLegacyDelegatee(
            &self,
            account: alloy::sol_types::private::Address,
            allowed: bool,
        ) -> alloy_contract::SolCallBuilder<&P, setLegacyDelegateeCall, N> {
            self.call_builder(
                &setLegacyDelegateeCall {
                    account,
                    allowed,
                },
            )
        }
        ///Creates a new call builder for the [`setSigner`] function.
        pub fn setSigner(
            &self,
            signer: alloy::sol_types::private::Address,
            allowed: bool,
        ) -> alloy_contract::SolCallBuilder<&P, setSignerCall, N> {
            self.call_builder(&setSignerCall { signer, allowed })
        }
        ///Creates a new call builder for the [`setThreshold`] function.
        pub fn setThreshold(
            &self,
            t: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, setThresholdCall, N> {
            self.call_builder(&setThresholdCall { t })
        }
        ///Creates a new call builder for the [`setV4Operator`] function.
        pub fn setV4Operator(
            &self,
            poolManager: alloy::sol_types::private::Address,
            operator: alloy::sol_types::private::Address,
            approved: bool,
        ) -> alloy_contract::SolCallBuilder<&P, setV4OperatorCall, N> {
            self.call_builder(
                &setV4OperatorCall {
                    poolManager,
                    operator,
                    approved,
                },
            )
        }
        ///Creates a new call builder for the [`signerCount`] function.
        pub fn signerCount(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, signerCountCall, N> {
            self.call_builder(&signerCountCall)
        }
        ///Creates a new call builder for the [`supportsInterface`] function.
        pub fn supportsInterface(
            &self,
            interfaceId: alloy::sol_types::private::FixedBytes<4>,
        ) -> alloy_contract::SolCallBuilder<&P, supportsInterfaceCall, N> {
            self.call_builder(
                &supportsInterfaceCall {
                    interfaceId,
                },
            )
        }
        ///Creates a new call builder for the [`sweepBatch`] function.
        pub fn sweepBatch(
            &self,
            tokens: alloy::sol_types::private::Vec<alloy::sol_types::private::Address>,
            to: alloy::sol_types::private::Address,
            amounts: alloy::sol_types::private::Vec<
                alloy::sol_types::private::primitives::aliases::U256,
            >,
        ) -> alloy_contract::SolCallBuilder<&P, sweepBatchCall, N> {
            self.call_builder(
                &sweepBatchCall {
                    tokens,
                    to,
                    amounts,
                },
            )
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
        ///Creates a new call builder for the [`threshold`] function.
        pub fn threshold(&self) -> alloy_contract::SolCallBuilder<&P, thresholdCall, N> {
            self.call_builder(&thresholdCall)
        }
        ///Creates a new call builder for the [`transferOwnership`] function.
        pub fn transferOwnership(
            &self,
            newOwner: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, transferOwnershipCall, N> {
            self.call_builder(&transferOwnershipCall { newOwner })
        }
        ///Creates a new call builder for the [`unpause`] function.
        pub fn unpause(&self) -> alloy_contract::SolCallBuilder<&P, unpauseCall, N> {
            self.call_builder(&unpauseCall)
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
    > MevSafeInstance<P, N> {
        /// Creates a new event filter using this contract instance's provider and address.
        ///
        /// Note that the type can be any event, not just those defined in this contract.
        /// Prefer using the other methods for building type-safe event filters.
        pub fn event_filter<E: alloy_sol_types::SolEvent>(
            &self,
        ) -> alloy_contract::Event<&P, E, N> {
            alloy_contract::Event::new_sol(&self.provider, &self.address)
        }
        ///Creates a new event filter for the [`CollateralizationFinanced`] event.
        pub fn CollateralizationFinanced_filter(
            &self,
        ) -> alloy_contract::Event<&P, CollateralizationFinanced, N> {
            self.event_filter::<CollateralizationFinanced>()
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
        ///Creates a new event filter for the [`Executed`] event.
        pub fn Executed_filter(&self) -> alloy_contract::Event<&P, Executed, N> {
            self.event_filter::<Executed>()
        }
        ///Creates a new event filter for the [`LegacyDelegateeSet`] event.
        pub fn LegacyDelegateeSet_filter(
            &self,
        ) -> alloy_contract::Event<&P, LegacyDelegateeSet, N> {
            self.event_filter::<LegacyDelegateeSet>()
        }
        ///Creates a new event filter for the [`LpMoved`] event.
        pub fn LpMoved_filter(&self) -> alloy_contract::Event<&P, LpMoved, N> {
            self.event_filter::<LpMoved>()
        }
        ///Creates a new event filter for the [`OwnershipTransferStarted`] event.
        pub fn OwnershipTransferStarted_filter(
            &self,
        ) -> alloy_contract::Event<&P, OwnershipTransferStarted, N> {
            self.event_filter::<OwnershipTransferStarted>()
        }
        ///Creates a new event filter for the [`OwnershipTransferred`] event.
        pub fn OwnershipTransferred_filter(
            &self,
        ) -> alloy_contract::Event<&P, OwnershipTransferred, N> {
            self.event_filter::<OwnershipTransferred>()
        }
        ///Creates a new event filter for the [`Paused`] event.
        pub fn Paused_filter(&self) -> alloy_contract::Event<&P, Paused, N> {
            self.event_filter::<Paused>()
        }
        ///Creates a new event filter for the [`Received`] event.
        pub fn Received_filter(&self) -> alloy_contract::Event<&P, Received, N> {
            self.event_filter::<Received>()
        }
        ///Creates a new event filter for the [`SignerSet`] event.
        pub fn SignerSet_filter(&self) -> alloy_contract::Event<&P, SignerSet, N> {
            self.event_filter::<SignerSet>()
        }
        ///Creates a new event filter for the [`ThresholdSet`] event.
        pub fn ThresholdSet_filter(&self) -> alloy_contract::Event<&P, ThresholdSet, N> {
            self.event_filter::<ThresholdSet>()
        }
        ///Creates a new event filter for the [`Unpaused`] event.
        pub fn Unpaused_filter(&self) -> alloy_contract::Event<&P, Unpaused, N> {
            self.event_filter::<Unpaused>()
        }
        ///Creates a new event filter for the [`V4OperatorSet`] event.
        pub fn V4OperatorSet_filter(
            &self,
        ) -> alloy_contract::Event<&P, V4OperatorSet, N> {
            self.event_filter::<V4OperatorSet>()
        }
    }
}
