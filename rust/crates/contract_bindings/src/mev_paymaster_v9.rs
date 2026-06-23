///Module containing a contract's types and functions.
/**

```solidity
library IPaymaster {
    type PostOpMode is uint8;
}
```*/
#[allow(
    non_camel_case_types,
    non_snake_case,
    clippy::pub_underscore_fields,
    clippy::style,
    clippy::empty_structs_with_brackets
)]
pub mod IPaymaster {
    use super::*;
    use alloy::sol_types as alloy_sol_types;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct PostOpMode(u8);
    const _: () = {
        use alloy::sol_types as alloy_sol_types;
        #[automatically_derived]
        impl alloy_sol_types::private::SolTypeValue<PostOpMode> for u8 {
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
        impl PostOpMode {
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
        impl From<u8> for PostOpMode {
            fn from(value: u8) -> Self {
                Self::from_underlying(value)
            }
        }
        #[automatically_derived]
        impl From<PostOpMode> for u8 {
            fn from(value: PostOpMode) -> Self {
                value.into_underlying()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolType for PostOpMode {
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
        impl alloy_sol_types::EventTopic for PostOpMode {
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
    /**Creates a new wrapper around an on-chain [`IPaymaster`](self) contract instance.

See the [wrapper's documentation](`IPaymasterInstance`) for more details.*/
    #[inline]
    pub const fn new<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    >(
        address: alloy_sol_types::private::Address,
        __provider: P,
    ) -> IPaymasterInstance<P, N> {
        IPaymasterInstance::<P, N>::new(address, __provider)
    }
    /**A [`IPaymaster`](self) instance.

Contains type-safe methods for interacting with an on-chain instance of the
[`IPaymaster`](self) contract located at a given `address`, using a given
provider `P`.

If the contract bytecode is available (see the [`sol!`](alloy_sol_types::sol!)
documentation on how to provide it), the `deploy` and `deploy_builder` methods can
be used to deploy a new instance of the contract.

See the [module-level documentation](self) for all the available methods.*/
    #[derive(Clone)]
    pub struct IPaymasterInstance<P, N = alloy_contract::private::Ethereum> {
        address: alloy_sol_types::private::Address,
        provider: P,
        _network: ::core::marker::PhantomData<N>,
    }
    #[automatically_derived]
    impl<P, N> ::core::fmt::Debug for IPaymasterInstance<P, N> {
        #[inline]
        fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
            f.debug_tuple("IPaymasterInstance").field(&self.address).finish()
        }
    }
    /// Instantiation and getters/setters.
    impl<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    > IPaymasterInstance<P, N> {
        /**Creates a new wrapper around an on-chain [`IPaymaster`](self) contract instance.

See the [wrapper's documentation](`IPaymasterInstance`) for more details.*/
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
    impl<P: ::core::clone::Clone, N> IPaymasterInstance<&P, N> {
        /// Clones the provider and returns a new instance with the cloned provider.
        #[inline]
        pub fn with_cloned_provider(self) -> IPaymasterInstance<P, N> {
            IPaymasterInstance {
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
    > IPaymasterInstance<P, N> {
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
    > IPaymasterInstance<P, N> {
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
library IPaymaster {
    type PostOpMode is uint8;
}

interface MevPaymasterV9 {
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

    error DivisionByZero();
    error ERC165Error(address entryPoint, bytes4 interfaceId);
    error Eip7702SenderNotDelegate(address sender);
    error Eip7702SenderWithoutCode(address sender);
    error EnforcedPause();
    error EpochBudgetExceeded(uint128 spent, uint128 cap);
    error Erc20InsufficientAllowance(address token, address sender, uint256 requiredAmount, uint256 allowance);
    error Erc20InvalidConfig();
    error Erc20MaxAmountExceeded(uint256 requiredAmount, uint256 maxAmount);
    error Erc20OracleNotSet();
    error Erc20PaymasterDataInvalid();
    error Erc20PriceInvalid(address oracle, int256 answer);
    error Erc20PriceStale(address oracle, uint256 updatedAt, uint256 maxStaleness);
    error Erc20TokenNotEnabled(address token);
    error ExpectedPause();
    error InvalidParams();
    error InvalidPoolId(uint256 poolId);
    error MevPaymaster__SenderNotSponsored(address sender);
    error MevPaymaster__UnexpectedEntryPoint(address actual, address expected);
    error MevPaymaster__UntrustedDelegate(address sender, address delegate);
    error MustOverride();
    error NativeBalanceWithdrawFailed();
    error NotFromEntryPoint(address msgSender, address entity, address entryPoint);
    error OwnableInvalidOwner(address owner);
    error OwnableUnauthorizedAccount(address account);
    error PaymasterPaused();
    error PoolEthBalanceInsufficient(uint256 poolId, uint256 requested, uint256 available);
    error PoolTokenBalanceInsufficient(uint256 poolId, address token, uint256 requested, uint256 available);
    error UnsupportedOperation();

    event Erc20ConfigChanged(address indexed token, bool enabled, address tokenOracle, uint32 maxStaleness, uint16 markupBps, address treasury);
    event Erc20Settled(address indexed sender, address indexed token, uint256 actualGasCost, uint256 actualTokenCharge);
    event Erc20Sponsored(address indexed sender, address indexed token, uint256 maxTokenAmount, uint256 priceWithMarkup);
    event EthOracleChanged(address indexed oldOracle, address indexed newOracle);
    event NativeBalanceWithdrawn(address indexed to, uint256 amount);
    event OwnershipTransferStarted(address indexed previousOwner, address indexed newOwner);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);
    event Paused(address account);
    event PoolErc20Settled(address indexed sender, uint256 indexed poolId, address indexed token, uint256 actualGasCost, uint256 actualCharge);
    event PoolErc20Sponsored(address indexed sender, uint256 indexed poolId, address indexed token, uint256 reservedAmount, uint256 priceWithMarkup);
    event PoolEthSettled(address indexed sender, uint256 indexed poolId, uint256 actualGasCost, uint256 chargedFromPool);
    event PoolEthSponsored(address indexed sender, uint256 indexed poolId, uint256 reserved);
    event SenderPoolChanged(address indexed sender, uint256 indexed oldPoolId, uint256 indexed newPoolId);
    event Settled(address indexed sender, uint256 actualGasCost, uint64 epoch, uint128 spentInEpoch);
    event Sponsored(address indexed sender, uint256 maxCostEstimate, uint64 epoch);
    event SponsoredAccountChanged(address indexed account, bool allowed);
    event Transfer(address caller, address indexed from, address indexed to, uint256 indexed id, uint256 amount);
    event TrustedDelegateChanged(address indexed delegate, bool allowed);
    event TuningUpdated(uint128 maxWeiPerEpoch, uint64 epochLength);
    event Unpaused(address account);

    constructor(address ep, address initialOwner);

    receive() external payable;

    function acceptOwnership() external;
    function addStake(uint32 unstakeDelaySec) external payable;
    function allowance(address, address, uint256) external pure returns (uint256);
    function approve(address, uint256, uint256) external pure returns (bool);
    function balanceOf(address owner, uint256 id) external view returns (uint256);
    function budgets(address) external view returns (uint64 epoch, uint128 spent);
    function burnPoolEth(uint256 poolId, uint256 amount, address payable to) external;
    function burnPoolToken(uint256 poolId, address token, uint256 amount, address to) external;
    function creditPoolEth(uint256 poolId) external payable;
    function creditPoolToken(uint256 poolId, address token, uint256 amount) external;
    function currentEpoch() external view returns (uint64);
    function deposit() external payable;
    function entryPoint() external view returns (address);
    function epochLength() external view returns (uint64);
    function erc20Config(address token) external view returns (bool enabled, uint8 tokenDecimals, uint32 maxStaleness, uint16 markupBps, address tokenOracle, address treasury);
    function ethOracle() external view returns (address);
    function getDeposit() external view returns (uint256);
    function isOperator(address, address) external pure returns (bool);
    function maxWeiPerEpoch() external view returns (uint128);
    function owner() external view returns (address);
    function pause() external;
    function paused() external view returns (bool);
    function pendingOwner() external view returns (address);
    function poolEthBalance(uint256 poolId) external view returns (uint256);
    function poolTokenBalance(uint256 poolId, address token) external view returns (uint256);
    function postOp(IPaymaster.PostOpMode mode, bytes memory context, uint256 actualGasCost, uint256 actualUserOpFeePerGas) external;
    function remainingBudget(address sender) external view returns (uint128);
    function removeErc20Token(address token) external;
    function renounceOwnership() external;
    function senderPool(address sender) external view returns (uint256 poolId);
    function setErc20Config(address token, address tokenOracle, uint32 maxStaleness, uint16 markupBps, address treasury) external;
    function setEthOracle(address newOracle) external;
    function setOperator(address, bool) external pure returns (bool);
    function setSenderPool(address sender, uint256 poolId) external;
    function setSponsored(address account, bool allowed) external;
    function setTrustedDelegate(address delegate, bool allowed) external;
    function setTuning(uint128 newMaxWeiPerEpoch, uint64 newEpochLength) external;
    function sponsoredAccount(address) external view returns (bool);
    function supportsInterface(bytes4 interfaceId) external pure returns (bool);
    function transfer(address, uint256, uint256) external pure returns (bool);
    function transferFrom(address, address, uint256, uint256) external pure returns (bool);
    function transferOwnership(address newOwner) external;
    function trustedDelegate(address) external view returns (bool);
    function unlockStake() external;
    function unpause() external;
    function validatePaymasterUserOp(PackedUserOperation memory userOp, bytes32 userOpHash, uint256 maxCost) external returns (bytes memory context, uint256 validationData);
    function withdrawNativeBalance(address payable to) external;
    function withdrawStake(address payable withdrawAddress) external;
    function withdrawTo(address payable withdrawAddress, uint256 amount) external;
}
```

...which was generated by the following JSON ABI:
```json
[
  {
    "type": "constructor",
    "inputs": [
      {
        "name": "ep",
        "type": "address",
        "internalType": "contract IEntryPoint"
      },
      {
        "name": "initialOwner",
        "type": "address",
        "internalType": "address"
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
    "name": "acceptOwnership",
    "inputs": [],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "addStake",
    "inputs": [
      {
        "name": "unstakeDelaySec",
        "type": "uint32",
        "internalType": "uint32"
      }
    ],
    "outputs": [],
    "stateMutability": "payable"
  },
  {
    "type": "function",
    "name": "allowance",
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
      }
    ],
    "outputs": [
      {
        "name": "",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "stateMutability": "pure"
  },
  {
    "type": "function",
    "name": "approve",
    "inputs": [
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
    "name": "balanceOf",
    "inputs": [
      {
        "name": "owner",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "id",
        "type": "uint256",
        "internalType": "uint256"
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
    "name": "budgets",
    "inputs": [
      {
        "name": "",
        "type": "address",
        "internalType": "address"
      }
    ],
    "outputs": [
      {
        "name": "epoch",
        "type": "uint64",
        "internalType": "uint64"
      },
      {
        "name": "spent",
        "type": "uint128",
        "internalType": "uint128"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "burnPoolEth",
    "inputs": [
      {
        "name": "poolId",
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
        "internalType": "address payable"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "burnPoolToken",
    "inputs": [
      {
        "name": "poolId",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "token",
        "type": "address",
        "internalType": "address"
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
    "name": "creditPoolEth",
    "inputs": [
      {
        "name": "poolId",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "outputs": [],
    "stateMutability": "payable"
  },
  {
    "type": "function",
    "name": "creditPoolToken",
    "inputs": [
      {
        "name": "poolId",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "token",
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
    "name": "currentEpoch",
    "inputs": [],
    "outputs": [
      {
        "name": "",
        "type": "uint64",
        "internalType": "uint64"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "deposit",
    "inputs": [],
    "outputs": [],
    "stateMutability": "payable"
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
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "epochLength",
    "inputs": [],
    "outputs": [
      {
        "name": "",
        "type": "uint64",
        "internalType": "uint64"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "erc20Config",
    "inputs": [
      {
        "name": "token",
        "type": "address",
        "internalType": "address"
      }
    ],
    "outputs": [
      {
        "name": "enabled",
        "type": "bool",
        "internalType": "bool"
      },
      {
        "name": "tokenDecimals",
        "type": "uint8",
        "internalType": "uint8"
      },
      {
        "name": "maxStaleness",
        "type": "uint32",
        "internalType": "uint32"
      },
      {
        "name": "markupBps",
        "type": "uint16",
        "internalType": "uint16"
      },
      {
        "name": "tokenOracle",
        "type": "address",
        "internalType": "contract IAggregatorV3"
      },
      {
        "name": "treasury",
        "type": "address",
        "internalType": "address"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "ethOracle",
    "inputs": [],
    "outputs": [
      {
        "name": "",
        "type": "address",
        "internalType": "contract IAggregatorV3"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "getDeposit",
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
    "name": "isOperator",
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
    "name": "maxWeiPerEpoch",
    "inputs": [],
    "outputs": [
      {
        "name": "",
        "type": "uint128",
        "internalType": "uint128"
      }
    ],
    "stateMutability": "view"
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
    "name": "poolEthBalance",
    "inputs": [
      {
        "name": "poolId",
        "type": "uint256",
        "internalType": "uint256"
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
    "name": "poolTokenBalance",
    "inputs": [
      {
        "name": "poolId",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "token",
        "type": "address",
        "internalType": "address"
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
    "name": "postOp",
    "inputs": [
      {
        "name": "mode",
        "type": "uint8",
        "internalType": "enum IPaymaster.PostOpMode"
      },
      {
        "name": "context",
        "type": "bytes",
        "internalType": "bytes"
      },
      {
        "name": "actualGasCost",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "actualUserOpFeePerGas",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "remainingBudget",
    "inputs": [
      {
        "name": "sender",
        "type": "address",
        "internalType": "address"
      }
    ],
    "outputs": [
      {
        "name": "",
        "type": "uint128",
        "internalType": "uint128"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "removeErc20Token",
    "inputs": [
      {
        "name": "token",
        "type": "address",
        "internalType": "address"
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
    "name": "senderPool",
    "inputs": [
      {
        "name": "sender",
        "type": "address",
        "internalType": "address"
      }
    ],
    "outputs": [
      {
        "name": "poolId",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "stateMutability": "view"
  },
  {
    "type": "function",
    "name": "setErc20Config",
    "inputs": [
      {
        "name": "token",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "tokenOracle",
        "type": "address",
        "internalType": "contract IAggregatorV3"
      },
      {
        "name": "maxStaleness",
        "type": "uint32",
        "internalType": "uint32"
      },
      {
        "name": "markupBps",
        "type": "uint16",
        "internalType": "uint16"
      },
      {
        "name": "treasury",
        "type": "address",
        "internalType": "address"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "setEthOracle",
    "inputs": [
      {
        "name": "newOracle",
        "type": "address",
        "internalType": "contract IAggregatorV3"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "setOperator",
    "inputs": [
      {
        "name": "",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "",
        "type": "bool",
        "internalType": "bool"
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
    "name": "setSenderPool",
    "inputs": [
      {
        "name": "sender",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "poolId",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "setSponsored",
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
    "name": "setTrustedDelegate",
    "inputs": [
      {
        "name": "delegate",
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
    "name": "setTuning",
    "inputs": [
      {
        "name": "newMaxWeiPerEpoch",
        "type": "uint128",
        "internalType": "uint128"
      },
      {
        "name": "newEpochLength",
        "type": "uint64",
        "internalType": "uint64"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "sponsoredAccount",
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
    "stateMutability": "pure"
  },
  {
    "type": "function",
    "name": "transfer",
    "inputs": [
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
    "name": "transferFrom",
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
    "name": "trustedDelegate",
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
    "name": "unlockStake",
    "inputs": [],
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
    "name": "validatePaymasterUserOp",
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
        "name": "maxCost",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "outputs": [
      {
        "name": "context",
        "type": "bytes",
        "internalType": "bytes"
      },
      {
        "name": "validationData",
        "type": "uint256",
        "internalType": "uint256"
      }
    ],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "withdrawNativeBalance",
    "inputs": [
      {
        "name": "to",
        "type": "address",
        "internalType": "address payable"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "withdrawStake",
    "inputs": [
      {
        "name": "withdrawAddress",
        "type": "address",
        "internalType": "address payable"
      }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  },
  {
    "type": "function",
    "name": "withdrawTo",
    "inputs": [
      {
        "name": "withdrawAddress",
        "type": "address",
        "internalType": "address payable"
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
    "type": "event",
    "name": "Erc20ConfigChanged",
    "inputs": [
      {
        "name": "token",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "enabled",
        "type": "bool",
        "indexed": false,
        "internalType": "bool"
      },
      {
        "name": "tokenOracle",
        "type": "address",
        "indexed": false,
        "internalType": "address"
      },
      {
        "name": "maxStaleness",
        "type": "uint32",
        "indexed": false,
        "internalType": "uint32"
      },
      {
        "name": "markupBps",
        "type": "uint16",
        "indexed": false,
        "internalType": "uint16"
      },
      {
        "name": "treasury",
        "type": "address",
        "indexed": false,
        "internalType": "address"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "Erc20Settled",
    "inputs": [
      {
        "name": "sender",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "token",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "actualGasCost",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      },
      {
        "name": "actualTokenCharge",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "Erc20Sponsored",
    "inputs": [
      {
        "name": "sender",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "token",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "maxTokenAmount",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      },
      {
        "name": "priceWithMarkup",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "EthOracleChanged",
    "inputs": [
      {
        "name": "oldOracle",
        "type": "address",
        "indexed": true,
        "internalType": "contract IAggregatorV3"
      },
      {
        "name": "newOracle",
        "type": "address",
        "indexed": true,
        "internalType": "contract IAggregatorV3"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "NativeBalanceWithdrawn",
    "inputs": [
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
    "name": "PoolErc20Settled",
    "inputs": [
      {
        "name": "sender",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "poolId",
        "type": "uint256",
        "indexed": true,
        "internalType": "uint256"
      },
      {
        "name": "token",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "actualGasCost",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      },
      {
        "name": "actualCharge",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "PoolErc20Sponsored",
    "inputs": [
      {
        "name": "sender",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "poolId",
        "type": "uint256",
        "indexed": true,
        "internalType": "uint256"
      },
      {
        "name": "token",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "reservedAmount",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      },
      {
        "name": "priceWithMarkup",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "PoolEthSettled",
    "inputs": [
      {
        "name": "sender",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "poolId",
        "type": "uint256",
        "indexed": true,
        "internalType": "uint256"
      },
      {
        "name": "actualGasCost",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      },
      {
        "name": "chargedFromPool",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "PoolEthSponsored",
    "inputs": [
      {
        "name": "sender",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "poolId",
        "type": "uint256",
        "indexed": true,
        "internalType": "uint256"
      },
      {
        "name": "reserved",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "SenderPoolChanged",
    "inputs": [
      {
        "name": "sender",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "oldPoolId",
        "type": "uint256",
        "indexed": true,
        "internalType": "uint256"
      },
      {
        "name": "newPoolId",
        "type": "uint256",
        "indexed": true,
        "internalType": "uint256"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "Settled",
    "inputs": [
      {
        "name": "sender",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "actualGasCost",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      },
      {
        "name": "epoch",
        "type": "uint64",
        "indexed": false,
        "internalType": "uint64"
      },
      {
        "name": "spentInEpoch",
        "type": "uint128",
        "indexed": false,
        "internalType": "uint128"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "Sponsored",
    "inputs": [
      {
        "name": "sender",
        "type": "address",
        "indexed": true,
        "internalType": "address"
      },
      {
        "name": "maxCostEstimate",
        "type": "uint256",
        "indexed": false,
        "internalType": "uint256"
      },
      {
        "name": "epoch",
        "type": "uint64",
        "indexed": false,
        "internalType": "uint64"
      }
    ],
    "anonymous": false
  },
  {
    "type": "event",
    "name": "SponsoredAccountChanged",
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
    "name": "Transfer",
    "inputs": [
      {
        "name": "caller",
        "type": "address",
        "indexed": false,
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
    "name": "TrustedDelegateChanged",
    "inputs": [
      {
        "name": "delegate",
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
    "name": "TuningUpdated",
    "inputs": [
      {
        "name": "maxWeiPerEpoch",
        "type": "uint128",
        "indexed": false,
        "internalType": "uint128"
      },
      {
        "name": "epochLength",
        "type": "uint64",
        "indexed": false,
        "internalType": "uint64"
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
    "type": "error",
    "name": "DivisionByZero",
    "inputs": []
  },
  {
    "type": "error",
    "name": "ERC165Error",
    "inputs": [
      {
        "name": "entryPoint",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "interfaceId",
        "type": "bytes4",
        "internalType": "bytes4"
      }
    ]
  },
  {
    "type": "error",
    "name": "Eip7702SenderNotDelegate",
    "inputs": [
      {
        "name": "sender",
        "type": "address",
        "internalType": "address"
      }
    ]
  },
  {
    "type": "error",
    "name": "Eip7702SenderWithoutCode",
    "inputs": [
      {
        "name": "sender",
        "type": "address",
        "internalType": "address"
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
    "name": "EpochBudgetExceeded",
    "inputs": [
      {
        "name": "spent",
        "type": "uint128",
        "internalType": "uint128"
      },
      {
        "name": "cap",
        "type": "uint128",
        "internalType": "uint128"
      }
    ]
  },
  {
    "type": "error",
    "name": "Erc20InsufficientAllowance",
    "inputs": [
      {
        "name": "token",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "sender",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "requiredAmount",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "allowance",
        "type": "uint256",
        "internalType": "uint256"
      }
    ]
  },
  {
    "type": "error",
    "name": "Erc20InvalidConfig",
    "inputs": []
  },
  {
    "type": "error",
    "name": "Erc20MaxAmountExceeded",
    "inputs": [
      {
        "name": "requiredAmount",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "maxAmount",
        "type": "uint256",
        "internalType": "uint256"
      }
    ]
  },
  {
    "type": "error",
    "name": "Erc20OracleNotSet",
    "inputs": []
  },
  {
    "type": "error",
    "name": "Erc20PaymasterDataInvalid",
    "inputs": []
  },
  {
    "type": "error",
    "name": "Erc20PriceInvalid",
    "inputs": [
      {
        "name": "oracle",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "answer",
        "type": "int256",
        "internalType": "int256"
      }
    ]
  },
  {
    "type": "error",
    "name": "Erc20PriceStale",
    "inputs": [
      {
        "name": "oracle",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "updatedAt",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "maxStaleness",
        "type": "uint256",
        "internalType": "uint256"
      }
    ]
  },
  {
    "type": "error",
    "name": "Erc20TokenNotEnabled",
    "inputs": [
      {
        "name": "token",
        "type": "address",
        "internalType": "address"
      }
    ]
  },
  {
    "type": "error",
    "name": "ExpectedPause",
    "inputs": []
  },
  {
    "type": "error",
    "name": "InvalidParams",
    "inputs": []
  },
  {
    "type": "error",
    "name": "InvalidPoolId",
    "inputs": [
      {
        "name": "poolId",
        "type": "uint256",
        "internalType": "uint256"
      }
    ]
  },
  {
    "type": "error",
    "name": "MevPaymaster__SenderNotSponsored",
    "inputs": [
      {
        "name": "sender",
        "type": "address",
        "internalType": "address"
      }
    ]
  },
  {
    "type": "error",
    "name": "MevPaymaster__UnexpectedEntryPoint",
    "inputs": [
      {
        "name": "actual",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "expected",
        "type": "address",
        "internalType": "address"
      }
    ]
  },
  {
    "type": "error",
    "name": "MevPaymaster__UntrustedDelegate",
    "inputs": [
      {
        "name": "sender",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "delegate",
        "type": "address",
        "internalType": "address"
      }
    ]
  },
  {
    "type": "error",
    "name": "MustOverride",
    "inputs": []
  },
  {
    "type": "error",
    "name": "NativeBalanceWithdrawFailed",
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
    "name": "PaymasterPaused",
    "inputs": []
  },
  {
    "type": "error",
    "name": "PoolEthBalanceInsufficient",
    "inputs": [
      {
        "name": "poolId",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "requested",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "available",
        "type": "uint256",
        "internalType": "uint256"
      }
    ]
  },
  {
    "type": "error",
    "name": "PoolTokenBalanceInsufficient",
    "inputs": [
      {
        "name": "poolId",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "token",
        "type": "address",
        "internalType": "address"
      },
      {
        "name": "requested",
        "type": "uint256",
        "internalType": "uint256"
      },
      {
        "name": "available",
        "type": "uint256",
        "internalType": "uint256"
      }
    ]
  },
  {
    "type": "error",
    "name": "UnsupportedOperation",
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
pub mod MevPaymasterV9 {
    use super::*;
    use alloy::sol_types as alloy_sol_types;
    /// The creation / init bytecode of the contract.
    ///
    /// ```text
    ///0x60a03461019c57601f61308238819003918201601f19168301916001600160401b038311848410176101a057808492604094855283398101031261019c578051906001600160a01b0382169081830361019c57602001516001600160a01b0381169081900361019c575f8054610100600160c81b03191673015180000000000000000000038d7ea4c6800000179055801561018957600a80546001600160a01b031990811690915560098054918216831790556001600160a01b03167f8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e05f80a3730a630a99df908a81115a3022927be82f9299987e8114801590610180575b6101565750608052604051612ecd90816101b582396080518181816105e70152818161069101528181610735015281816107d30152818161094c0152818161175a015281816119f201528181611b400152611ca90152f35b63163122db60e31b5f52600452730a630a99df908a81115a3022927be82f9299987e60245260445ffd5b50803b156100fe565b631e4fbdf760e01b5f525f60045260245ffd5b5f80fd5b634e487b7160e01b5f52604160045260245ffdfe60806040526004361015610022575b3615610018575f80fd5b610020611b3e565b005b5f5f3560e01c8062fdd58e1461182257806301ffc9a7146117cc5780630396cb6014611732578063095bcdb6146111ea578063110dfd9214611650578063147e7e66146115fe5780631ac3e310146115bf578063205c287814611591578063249112f4146114f75780632a895f351461129a5780633c7bdcea146112555780633f4ba83a146111ef578063426a8493146111ea57806352b7512c1461111a57806354a2b939146110e5578063558a7297146110c557806357d775f81461109c578063598af9e71461107f5780635c975abb1461105d5780636ca18fc614610f8e578063715018a614610f2757806375cbcca714610e225780637667180814610ddf57806379ba509714610d575780637c627b2114610cdf5780638456cb5914610c765780638da5cb5b14610c4d57806390a4450e14610adf5780639800c10514610a5c5780639c01a3ce14610a335780639c8762e114610a0a578063a6cd75dc1461097b578063b0d691fe14610936578063b1c5af77146108ca578063b6363cf21461089b578063b89b6a1e1461078c578063bb9fe6bf14610719578063c23a5cea1461066c578063c399ec88146105ba578063c9b6d2ba146104df578063cb5638d7146104b5578063cdd0c12d14610476578063d0e30db01461045f578063e30c397814610436578063e5c1623e146103fd578063f2fde38b1461038f578063f935d0b014610308578063f9af2bf21461027c5763fe99049a14610247575061000e565b3461027957608036600319011261027957600490610263611851565b5061026c611867565b50639ba6061b60e01b8152fd5b80fd5b50346102795760203660031901126102795760c0906040906001600160a01b036102a4611851565b1681526008602052208054906001808060a01b03910154166040519160ff81161515835260ff8160081c16602084015263ffffffff8160101c16604084015261ffff8160301c16606084015260018060a01b039060401c16608083015260a0820152f35b503461027957610317366118c5565b90610320611b9b565b6001600160a01b03169081156103805760207f71b4b10828d5fe536940fe767ede8c16ba18426108d4628caff576ad45acf395918385526002825261037481604087209060ff801983541691151516179055565b6040519015158152a280f35b635435b28960e11b8352600483fd5b5034610279576020366003190112610279576103a9611851565b6103b1611b9b565b600a80546001600160a01b0319166001600160a01b039283169081179091556009549091167f38d16b8cac22d99fc7c124b9cd0de2d3fa1faef420bfe791d8c362d765e227008380a380f35b5034610279576020366003190112610279576020906040906001600160a01b03610425611851565b168152600683522054604051908152f35b5034610279578060031936011261027957600a546040516001600160a01b039091168152602090f35b508060031936011261027957610473611b3e565b80f35b50346102795760203660031901126102795760209060ff906040906001600160a01b036104a1611851565b168152600284522054166040519015158152f35b50346102795760203660031901126102795760406020916004358152600483522054604051908152f35b5034610279576020366003190112610279576104f9611851565b610501611b9b565b6001600160a01b031680156105ab57478280808084865af13d156105a6573d6001600160401b0381116105925760405190610546601f8201601f1916602001836119ae565b81528460203d92013e5b156105835760207f5ceac9f7036a05e231fa263d6b6731de430460d4af4830160bb6f00d1b957f5091604051908152a280f35b63018f95f560e01b8352600483fd5b634e487b7160e01b85526041600452602485fd5b610550565b635435b28960e11b8252600482fd5b50346102795780600319360112610279576040516370a0823160e01b8152306004820152906020826024817f00000000000000000000000000000000000000000000000000000000000000006001600160a01b03165afa9081156106605790610629575b602090604051908152f35b506020813d602011610658575b81610643602093836119ae565b81010312610654576020905161061e565b5f80fd5b3d9150610636565b604051903d90823e3d90fd5b50346102795760203660031901126102795780610687611851565b61068f611b9b565b7f00000000000000000000000000000000000000000000000000000000000000006001600160a01b031690813b156107155760405163611d2e7560e11b81526001600160a01b0390911660048201529082908290602490829084905af1801561070a576106f95750f35b81610703916119ae565b6102795780f35b6040513d84823e3d90fd5b5050fd5b5034610279578060031936011261027957610732611b9b565b807f00000000000000000000000000000000000000000000000000000000000000006001600160a01b0316803b156107895781809160046040518094819363bb9fe6bf60e01b83525af1801561070a576106f95750f35b50fd5b506020366003190112610279576004356107a4611b9b565b8015801561088b575b6108795734156105ab578082526004602052604082206107ce3482546119cf565b9055817f00000000000000000000000000000000000000000000000000000000000000006001600160a01b0316803b1561087557816024916040519283809263b760faf960e01b825230600483015234905af1801561070a57610860575b50506040805133815234602082015260a09290921b91309184915f516020612ea15f395f51905f5291819081015b0390a480f35b8161086a916119ae565b61087557815f61082c565b5080fd5b63d531737d60e01b8252600452602490fd5b506001600160601b0381116107ad565b5034610279576040366003190112610279576020906108b8611851565b506108c1611867565b50604051908152f35b5034610279576108d9366118c5565b906108e2611b9b565b6001600160a01b03169081156103805760207fb4734c1ff22ef330acc505cb27f93c3ada143d8f2fda8d82edae83e40c2bee16918385526003825261037481604087209060ff801983541691151516179055565b50346102795780600319360112610279576040517f00000000000000000000000000000000000000000000000000000000000000006001600160a01b03168152602090f35b503461027957602036600319011261027957610995611851565b61099d611b9b565b6001600160a01b0381169081156109fb576109b790611c39565b600780546001600160a01b0319811683179091556001600160a01b03167fdd786e7bdbeb989faf1b5c8b448a792709a1104d2222d9dad2aa531b37b56e948380a380f35b6369184b7760e01b8352600483fd5b50346102795780600319360112610279576007546040516001600160a01b039091168152602090f35b50346102795780600319360112610279576001600160801b036020915460081c16604051908152f35b503461027957602036600319011261027957610a76611851565b610a7e611b9b565b60018060a01b031680825260086020528160016040822082815501557f7a787f5cbda9d7705145411afa7377ebc9deb36d640b47f5f39434e624a8884660a0604051848152846020820152846040820152846060820152846080820152a280f35b50346102795760803660031901126102795760043590610afd611867565b91604435906064356001600160a01b0381169190828103610c4957610b20611b9b565b81158015610c39575b610c25576001600160a01b038616928315908115610c1c575b508015610c14575b610c0557818552600560205260408520835f5260205260405f2054848110610bce5793610b96918697879685849952600560205260408820875f526020528360405f2091039055611d4d565b60a01b17915f516020612ea15f395f51905f526040518061085a3094338360209093929193604081019460018060a01b031681520152565b604051633465b76160e21b8152600481018490526001600160a01b0388166024820152604481018690526064810191909152608490fd5b635435b28960e11b8552600485fd5b508315610b4a565b9050155f610b42565b63d531737d60e01b85526004829052602485fd5b506001600160601b038211610b29565b8480fd5b50346102795780600319360112610279576009546040516001600160a01b039091168152602090f35b5034610279578060031936011261027957610c8f611b9b565b805460ff8116610cd05760019060ff19161781557f62e78cea01bee320cd4e420270b5ea74000d11b0c9f74754ebdbfc544b05a2586020604051338152a180f35b63d93c066560e01b8252600482fd5b50346102795760803660031901126102795760036004351015610279576024356001600160401b03811161087557366023820112156108755780600401356001600160401b038111610d53573660248284010111610d535761047391610d43611ca7565b6064359160246044359201612464565b8280fd5b5034610279578060031936011261027957600a54336001600160a01b0390911603610dcc57600a80546001600160a01b0319908116909155600980543392811683179091556001600160a01b03167f8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e08380a380f35b63118cdaa760e01b815233600452602490fd5b5034610279578060031936011261027957610e11610e0c6001600160401b036020935460881c1642611a6b565b611d28565b6001600160401b0360405191168152f35b5034610279576060366003190112610279576024356004356044356001600160a01b038116808203610c4957610e56611b9b565b82158015610f17575b610f0357158015610efb575b610eec5781845260046020526040842054838110610ed05783839281610e9f938896875260046020520360408620556119f0565b60408051338152602081019490945260a09190911b9230915f516020612ea15f395f51905f5291908190810161085a565b635926565160e01b855260048390526024849052604452606484fd5b635435b28960e11b8452600484fd5b508215610e6b565b63d531737d60e01b85526004839052602485fd5b506001600160601b038311610e5f565b5034610279578060031936011261027957610f40611b9b565b600a80546001600160a01b031990811690915560098054918216905581906001600160a01b03167f8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e08280a380f35b5034610279576040366003190112610279576004356001600160801b03811690818103610d5357602435906001600160401b03821691828103610c4957610fd3611b9b565b83158015611055575b610c05578454610100600160c81b03191660089290921b70ffffffffffffffffffffffffffffffff00169190911760889190911b67ffffffffffffffff60881b161783556040805192835260208301919091527f9a649e8ac2e5d06a297ad5c3d5636c2ec800686ba217ec8f17cb11fea9687b2891a180f35b508215610fdc565b503461027957806003193601126102795760ff60209154166040519015158152f35b5034610279576060366003190112610279576020906108b8611851565b50346102795780600319360112610279576001600160401b036020915460881c16604051908152f35b5034610279576004906110d7366118c5565b5050639ba6061b60e01b8152fd5b5034610279576020366003190112610279576020611109611104611851565b611a89565b6001600160801b0360405191168152f35b5034610279576060366003190112610279576004356001600160401b0381116108755780600401906101206003198236030112610d535760e49061115c611ca7565b0160346111698284611cf6565b9050106103805761117a9082611cf6565b9091816034116111e65735906001600160a01b03821682036111e6576060926111af9260443592603319019160340190611d9e565b6020604051938492604084528051928391826040870152018585015e82820184018190526020830152601f01601f19168101030190f35b8380fd5b611891565b5034610279578060031936011261027957611208611b9b565b805460ff8116156112465760ff191681557f5db9ee0a495bf2e6ff9c91a7834c1ba4fdd244a5e8aa4e537bd38aeae4b073aa6020604051338152a180f35b638dfc202b60e01b8252600482fd5b5034610279576040366003190112610279576040611271611867565b9160043581526005602052209060018060a01b03165f52602052602060405f2054604051908152f35b50346102795760a0366003190112610279576112b4611851565b6112bc611867565b6044359063ffffffff82168092036111e6576064359261ffff8416809403610c49576084356001600160a01b03811691908290036114f3576112fc611b9b565b6001600160a01b031693841580156114e2575b80156114da575b80156114d2575b80156114c7575b6114b85790859161133484611c39565b60405163313ce56760e01b8152946020866004818a5afa80156114ad57600160a09689927f7a787f5cbda9d7705145411afa7377ebc9deb36d640b47f5f39434e624a8884699889161147e575b506040519161138f83611964565b83835260ff6020840192168252604083019186835260608401908882526113e660406080870194888f81901b03169c8d86528e8801998d8b52815260086020522095511515869060ff801983541691151516179055565b5167ffff00000000000065ffffffff00008654955160101b16925160301b1692600160401b8760e01b03905160401b169361ff006001600160401b0363ffffffff60e01b019260081b169067ffffffffffffff001916171617171781550190600180881b039051166001600160601b03871b82541617905560405193600185526020850152604084015260608301526080820152a280f35b6114a0915060203d6020116114a6575b61149881836119ae565b810190611c20565b5f611381565b503d61148e565b6040513d86823e3d90fd5b6369184b7760e01b8652600486fd5b506113888111611324565b50831561131d565b508115611316565b506001600160a01b0383161561130f565b8580fd5b503461027957604036600319011261027957611511611851565b6024359061151d611b9b565b6001600160a01b03168015610380576001600160601b03821161157d57808352600660205260408320549080845260066020528260408520557f253774f7af1ea3bc8cf8e4adb437f908edd957cad34d0705f3233a59ea0bf1e78480a480f35b63d531737d60e01b83526004829052602483fd5b5034610279576040366003190112610279576104736115ae611851565b6115b6611b9b565b602435906119f0565b50346102795760203660031901126102795760209060ff906040906001600160a01b036115ea611851565b168152600384522054166040519015158152f35b50346102795760203660031901126102795760409081906001600160a01b03611625611851565b168152600160205220546001600160801b038251916001600160401b0381168352831c166020820152f35b50346102795760603660031901126102795760043561166d611867565b60443591611679611b9b565b80158015611722575b611710576001600160a01b0382169182158015611708575b610c0557836116cf91838752600560205260408720855f5260205260405f206116c48382546119cf565b905530903390611bc2565b60a01b1790825f516020612ea15f395f51905f526040518061085a3095338360209093929193604081019460018060a01b031681520152565b50831561169a565b63d531737d60e01b8452600452602483fd5b506001600160601b038111611682565b5060203660031901126106545760043563ffffffff811680910361065457611758611b9b565b7f00000000000000000000000000000000000000000000000000000000000000006001600160a01b031690813b15610654575f90602460405180948193621cb65b60e51b8352600483015234905af180156117c1576117b5575080f35b61002091505f906119ae565b6040513d5f823e3d90fd5b346106545760203660031901126106545760043563ffffffff60e01b811680910361065457602090630f632fb360e01b8114908115611811575b506040519015158152f35b6301ffc9a760e01b14905082611806565b34610654576040366003190112610654576020611849611840611851565b60243590611908565b604051908152f35b600435906001600160a01b038216820361065457565b602435906001600160a01b038216820361065457565b35906001600160a01b038216820361065457565b34610654576060366003190112610654576004356001600160a01b03811681036106545750639ba6061b60e01b5f5260045ffd5b6040906003190112610654576004356001600160a01b0381168103610654579060243580151581036106545790565b35906001600160801b038216820361065457565b306001600160a01b039091160361195f5760a081901c906001600160a01b03168061193d57505f52600460205260405f205490565b905f52600560205260405f209060018060a01b03165f5260205260405f205490565b505f90565b60c081019081106001600160401b0382111761197f57604052565b634e487b7160e01b5f52604160045260245ffd5b604081019081106001600160401b0382111761197f57604052565b90601f801991011681019081106001600160401b0382111761197f57604052565b919082018092116119dc57565b634e487b7160e01b5f52601160045260245ffd5b7f00000000000000000000000000000000000000000000000000000000000000006001600160a01b031691823b156106545760405163040b850f60e31b81526001600160a01b0390921660048301526024820152905f908290604490829084905af180156117c157611a5f5750565b5f611a69916119ae565b565b8115611a75570490565b634e487b7160e01b5f52601260045260245ffd5b60018060a01b03165f52600160205260405f2060405190611aa982611993565b546001600160401b03811682526001600160801b03602083019160401c1681525f54916001600160401b0380611ae7610e0c828760881c1642611a6b565b925116911603611b2e576001600160801b03809151169160081c16908181118015611b25575b611b1f576001600160801b0391031690565b50505f90565b50818114611b0d565b5060081c6001600160801b031690565b7f00000000000000000000000000000000000000000000000000000000000000006001600160a01b0316803b15610654575f6024916040519283809263b760faf960e01b825230600483015234905af180156117c157611a5f5750565b6009546001600160a01b03163303611baf57565b63118cdaa760e01b5f523360045260245ffd5b916040519360605260405260601b602c526323b872dd60601b600c5260205f6064601c82855af1908160015f51141615611c02575b50505f606052604052565b3b153d171015611c13575f80611bf7565b637939f4245f526004601cfd5b90816020910312610654575160ff811681036106545790565b60405163313ce56760e01b815290602090829060049082906001600160a01b03165afa9081156117c15760089160ff915f91611c88575b501603611c7957565b6369184b7760e01b5f5260045ffd5b611ca1915060203d6020116114a65761149881836119ae565b5f611c70565b7f00000000000000000000000000000000000000000000000000000000000000006001600160a01b031633819003611cdc5750565b63fe34a6d360e01b5f52336004523060245260445260645ffd5b903590601e198136030182121561065457018035906001600160401b0382116106545760200191813603831361065457565b600160401b811015611d40576001600160401b031690565b6335278d125f526004601cfd5b919060145260345263a9059cbb60601b5f5260205f6044601082855af1908160015f51141615611d80575b50505f603452565b3b153d171015611d91575f80611d78565b6390b8ec185f526004601cfd5b926060925f549260ff8416612447576001600160a01b0386165f8181526002602052604090205490969060ff16156124345760175f80833c5f516001600160e81b03191661ef0160f01b146123b1575b5090611df9916127b3565b93865f93929352600660205260405f205495861561207157505060ff1615611fc65760018060a01b03811690815f52600860205260405f2090604051611e3e81611964565b82549260ff841615938415835260ff8160081c16602084015263ffffffff8160101c16604084015261ffff8160301c16606084015260018060a01b039060401c1660808301526001808060a01b03910154169260a08201938452611fb35784611ea691612891565b919095808711611f9d5750865f52600560205260405f20845f5260205260405f205490868210611f655750877f8d0635eb35324b23729a0b8a83ac58cce10dfe45f2b084490770cae0b55b0386604086948a94855f526005602052825f20875f526020528a835f20910390558151908a82526020820152a460018060a01b0390511693604051956003602088015260408701526060860152608085015260a084015260c083015260e082015260e08152611f62610100826119ae565b90565b604051633465b76160e21b8152600481018990526001600160a01b039190911660248201526044810187905260648101829052608490fd5b8663fac7436f60e01b5f5260045260245260445ffd5b83630c42945f60e21b5f5260045260245ffd5b50918091505f52600460205260405f205491808310612058579080847f91ba9c25efc3c1af7905cdb9048fe88e2b7daab7c2d0d0c3f627772b5006e004602085806120196001600160801b039998612823565b97865f52600484520360405f2055604051908152a360405193600260208601526040850152606084015216608082015260808152611f6260a0826119ae565b90635926565160e01b5f5260045260245260445260645ffd5b9160ff9193949650161561224357505060018060a01b031691825f52600860205260405f206040516120a281611964565b81549160ff831615928315835260ff8160081c16602084015263ffffffff8160101c16604084015261ffff8160301c16606084015260018060a01b039060401c1660808301526001808060a01b03910154169160a08201928352612230578261210a91612891565b848295921161221957604051636eb1769f60e11b8152600481018890523060248201526020816044818a5afa9081156117c1575f916121e7575b508581106121bd57507f296caef5f4574ea01a4a23e6dd315c798c4c426a034a07d41babf927188bceae60408793899382519182526020820152a360018060a01b0390511692604051946001602087015260408601526060850152608084015260a083015260c082015260c08152611f6260e0826119ae565b86608491878a604051936386b7d9a960e01b85526004850152602484015260448301526064820152fd5b90506020813d602011612211575b81612202602093836119ae565b8101031261065457515f612144565b3d91506121f5565b508363fac7436f60e01b5f5260045260245260445ffd5b84630c42945f60e21b5f5260045260245ffd5b9150926001600160801b03925061225990612823565b92845f52600160205260405f20936040519461227486611993565b546001600160401b038116865284602087019160401c1681526001600160401b03806122a8610e0c828760881c1642611a6b565b97511696169586145f146123a8578480915116915b1693168301906001600160801b0382116119dc576001600160801b03809160081c169116908082116123925750604051906122f782611993565b84825260208201908152855f5260016020526001600160401b0360405f20925116600160401b600160c01b038354925160401b16916001600160401b0360c01b1617179055837f46c10e67d6d6ef2dff7f3a9a7c4a28f4716171d8808a5063e3c89c12a81e475f60408051858152866020820152a2604051935f60208601526040850152830152608082015260808152611f6260a0826119ae565b90630b636b6b60e11b5f5260045260245260445ffd5b50835f916122bd565b60175f80833c5f51906110ff60f01b6001600160e81b0319831601612408575060481c6001600160a01b03165f8181526003602052604090205460ff16611dee578663e9f2e2f360e01b5f5260045260245260445ffd5b87903b1561242257639f4e4cc960e01b5f5260045260245ffd5b63e5819b9560e01b5f5260045260245ffd5b8663ec0adc3360e01b5f5260045260245ffd5b6326341fb360e21b5f5260045ffd5b359060ff8216820361065457565b925080601f101561279f5782601f81013560f81c8015612749576001811461268e5760021461258b5760e09181010312610654576124a182612456565b506124ae6020830161187d565b6040830135926124c06060820161187d565b9360a0820135936124ec60806124d860c0860161187d565b976001600160a01b03169401358287612e03565b93858511612583575b60407fee8cf41df036875660de15dbd27f5a70c45d889f2e18236da00ae9d724869eb991878798879810612559575b508151938452602084018890526001600160a01b031692a48161254657505050565b611a69926001600160a01b031690611d4d565b855f526005602052825f2060018060a01b0388165f5260205288835f20910381540190555f612524565b8594506124f5565b608091810103126106545761259f82612456565b507faefb7cef608b5381b2c1e06377ec6ac2ee5d19494995d9fa0f4d9fa375b558ed6125cd6020840161187d565b6125de6060604086013595016118f4565b6001600160a01b03909116926125f381612823565b916001600160801b0381166001600160801b03841690808211918215612684575b505061266157855f5260046020526001600160801b038360405f20920316815401905561265c604051928392839092916001600160801b036020916040840195845216910152565b0390a3565b604080519283526001600160801b0390911660208301529091508190810161265c565b1490505f80612614565b5060c09181010312610654576126a382612456565b5061271460406126b56020850161187d565b936126c182820161187d565b7f8ef4ad95023ffbd5418bd9d8db2c67f3e2469ac6189a50d9c0e3cb9b37da5b696126ee60a0840161187d565b966001600160a01b03928316959216938593859390606081013590899060800135612e03565b968151908152876020820152a38261272d575b50505050565b612740936001600160a01b031691611bc2565b5f808080612727565b50608091810103126106545761275e82612456565b5061276b6020830161187d565b91606061277a604083016118f4565b9101356001600160401b038116810361065457611a69936001600160a01b0316612c94565b634e487b7160e01b5f52603260045260245ffd5b91811561281957823560f81c92831561280757600160ff8516036127f857603583036127f8578260151161065457600181013560601c92603511610654576015013590565b632222434f60e11b5f5260045ffd5b5091506001036127f8575f905f905f90565b5f92508291508190565b600160801b811015611d40576001600160801b031690565b519069ffffffffffffffffffff8216820361065457565b908160a0910312610654576128668161283b565b91602082015191604081015191611f6260806060840151930161283b565b919082039182116119dc57565b6007546001600160a01b031692918315612c3c57604051633fabe5a360e21b815260a081600481885afa9081156117c1575f5f925f5f935f92612c12575b505f85128015612c0a575b612bf35715918215612bd9575b5050612bb3576128f78142612884565b90604085019663ffffffff885116809311612b9b57505050608083018051604051633fabe5a360e21b81529193919060a090829060049082906001600160a01b03165afa9384156117c1575f5f955f5f945f92612b5e575b505f88128015612b56575b612b305715918215612b16575b5050612ae85763ffffffff61297c8342612884565b985116809811612ac357505061299493949550612e03565b61ffff606083015116612710019081612710116119dc578181029161271082158284860414170215612a7157505061271090045b60ff60208293015116604d81116119dc57600a0a9081810291670de0b6b3a764000082158284860414170215612a08575050670de0b6b3a7640000900491565b670de0b6b3a7640000905f198184098481108501900392099080670de0b6b3a76400001115612a6457828211900360ee1b910360121c177faccb18165bd6fe31ae1cf318dc5b51eee0e1ba569b88cd74c1773b91fac106690291565b63ae47f7025f526004601cfd5b612710905f1981840984811085019003920990806127101115612a6457828211900360fc1b910360041c177fbc01a36e2eb1c432ca57a786c226809d495182a9930be0ded288ce703afb7e91026129c8565b87925060018060a01b03905116637af2274f60e01b5f5260045260245260445260645ffd5b9063ffffffff889260018060a01b0390511692511691637af2274f60e01b5f5260045260245260445260645ffd5b69ffffffffffffffffffff91925081169116105f80612967565b8351636319d6ab60e01b5f9081526001600160a01b039091166004526024899052604490fd5b50871561295a565b939750505050612b86915060a03d60a011612b94575b612b7e81836119ae565b810190612852565b92969093909290915f61294f565b503d612b74565b637af2274f60e01b5f5260045260245260445260645ffd5b859063ffffffff60408601511691637af2274f60e01b5f5260045260245260445260645ffd5b69ffffffffffffffffffff91925081169116105f806128e7565b8489636319d6ab60e01b5f5260045260245260445ffd5b5084156128da565b9350505050612c30915060a03d60a011612b9457612b7e81836119ae565b9293909290915f6128cf565b631f94d8a360e01b5f5260045ffd5b9081526001600160401b0390911660208201526001600160801b03909116604082015260600190565b906001600160801b03809116911603906001600160801b0382116119dc57565b6001600160a01b03165f8181526001602052604090819020905191947feb43290efc1c9442713347e1d541c9e24d355a30f119510830214559ccd72e8f94909291612cde83611993565b546001600160801b036001600160401b0382169182855260401c166001600160401b036020850196828852168203612dee575050612d1b83612823565b6001600160801b0382166001600160801b03821690808211918215612de4575b5050612dbd57916001600160801b03612d68869593612d62612db89684809a511692612c74565b90612c74565b168452865f5260016020526001600160401b0360405f2091511690805494519482600160401b600160c01b038760401b16916001600160401b0360c01b1617179055604051948594169184612c4b565b0390a2565b50506001600160801b036001600160401b03612db892511693511660405193849384612c4b565b1490505f80612d3b565b91509350612db8915060405193849384612c4b565b9190918115612e9157828102928282158284870414170215612e26575050900490565b82905f1981840985811086019003920990825f0383169281811115612a645783900480600302600218808202600203028082026002030280820260020302808202600203028082026002030280910260020302936001848483030494805f0304019211900302170290565b6323d359a360e01b5f5260045ffdfe1b3d7edb2e9c0b0e7c525b20aaaef0f5940d2ed71663c7d39266ecafac728859a164736f6c6343000822000a
    /// ```
    #[rustfmt::skip]
    #[allow(clippy::all)]
    pub static BYTECODE: alloy_sol_types::private::Bytes = alloy_sol_types::private::Bytes::from_static(
        b"`\xA04a\x01\x9CW`\x1Fa0\x828\x81\x90\x03\x91\x82\x01`\x1F\x19\x16\x83\x01\x91`\x01`\x01`@\x1B\x03\x83\x11\x84\x84\x10\x17a\x01\xA0W\x80\x84\x92`@\x94\x85R\x839\x81\x01\x03\x12a\x01\x9CW\x80Q\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x90\x81\x83\x03a\x01\x9CW` \x01Q`\x01`\x01`\xA0\x1B\x03\x81\x16\x90\x81\x90\x03a\x01\x9CW_\x80Ta\x01\0`\x01`\xC8\x1B\x03\x19\x16s\x01Q\x80\0\0\0\0\0\0\0\0\0\x03\x8D~\xA4\xC6\x80\0\0\x17\x90U\x80\x15a\x01\x89W`\n\x80T`\x01`\x01`\xA0\x1B\x03\x19\x90\x81\x16\x90\x91U`\t\x80T\x91\x82\x16\x83\x17\x90U`\x01`\x01`\xA0\x1B\x03\x16\x7F\x8B\xE0\x07\x9CS\x16Y\x14\x13D\xCD\x1F\xD0\xA4\xF2\x84\x19I\x7F\x97\"\xA3\xDA\xAF\xE3\xB4\x18okdW\xE0_\x80\xA3s\nc\n\x99\xDF\x90\x8A\x81\x11Z0\"\x92{\xE8/\x92\x99\x98~\x81\x14\x80\x15\x90a\x01\x80W[a\x01VWP`\x80R`@Qa.\xCD\x90\x81a\x01\xB5\x829`\x80Q\x81\x81\x81a\x05\xE7\x01R\x81\x81a\x06\x91\x01R\x81\x81a\x075\x01R\x81\x81a\x07\xD3\x01R\x81\x81a\tL\x01R\x81\x81a\x17Z\x01R\x81\x81a\x19\xF2\x01R\x81\x81a\x1B@\x01Ra\x1C\xA9\x01R\xF3[c\x161\"\xDB`\xE3\x1B_R`\x04Rs\nc\n\x99\xDF\x90\x8A\x81\x11Z0\"\x92{\xE8/\x92\x99\x98~`$R`D_\xFD[P\x80;\x15a\0\xFEV[c\x1EO\xBD\xF7`\xE0\x1B_R_`\x04R`$_\xFD[_\x80\xFD[cNH{q`\xE0\x1B_R`A`\x04R`$_\xFD\xFE`\x80`@R`\x046\x10\x15a\0\"W[6\x15a\0\x18W_\x80\xFD[a\0 a\x1B>V[\0[__5`\xE0\x1C\x80b\xFD\xD5\x8E\x14a\x18\"W\x80c\x01\xFF\xC9\xA7\x14a\x17\xCCW\x80c\x03\x96\xCB`\x14a\x172W\x80c\t[\xCD\xB6\x14a\x11\xEAW\x80c\x11\r\xFD\x92\x14a\x16PW\x80c\x14~~f\x14a\x15\xFEW\x80c\x1A\xC3\xE3\x10\x14a\x15\xBFW\x80c \\(x\x14a\x15\x91W\x80c$\x91\x12\xF4\x14a\x14\xF7W\x80c*\x89_5\x14a\x12\x9AW\x80c<{\xDC\xEA\x14a\x12UW\x80c?K\xA8:\x14a\x11\xEFW\x80cBj\x84\x93\x14a\x11\xEAW\x80cR\xB7Q,\x14a\x11\x1AW\x80cT\xA2\xB99\x14a\x10\xE5W\x80cU\x8Ar\x97\x14a\x10\xC5W\x80cW\xD7u\xF8\x14a\x10\x9CW\x80cY\x8A\xF9\xE7\x14a\x10\x7FW\x80c\\\x97Z\xBB\x14a\x10]W\x80cl\xA1\x8F\xC6\x14a\x0F\x8EW\x80cqP\x18\xA6\x14a\x0F'W\x80cu\xCB\xCC\xA7\x14a\x0E\"W\x80cvg\x18\x08\x14a\r\xDFW\x80cy\xBAP\x97\x14a\rWW\x80c|b{!\x14a\x0C\xDFW\x80c\x84V\xCBY\x14a\x0CvW\x80c\x8D\xA5\xCB[\x14a\x0CMW\x80c\x90\xA4E\x0E\x14a\n\xDFW\x80c\x98\0\xC1\x05\x14a\n\\W\x80c\x9C\x01\xA3\xCE\x14a\n3W\x80c\x9C\x87b\xE1\x14a\n\nW\x80c\xA6\xCDu\xDC\x14a\t{W\x80c\xB0\xD6\x91\xFE\x14a\t6W\x80c\xB1\xC5\xAFw\x14a\x08\xCAW\x80c\xB66<\xF2\x14a\x08\x9BW\x80c\xB8\x9Bj\x1E\x14a\x07\x8CW\x80c\xBB\x9F\xE6\xBF\x14a\x07\x19W\x80c\xC2:\\\xEA\x14a\x06lW\x80c\xC3\x99\xEC\x88\x14a\x05\xBAW\x80c\xC9\xB6\xD2\xBA\x14a\x04\xDFW\x80c\xCBV8\xD7\x14a\x04\xB5W\x80c\xCD\xD0\xC1-\x14a\x04vW\x80c\xD0\xE3\r\xB0\x14a\x04_W\x80c\xE3\x0C9x\x14a\x046W\x80c\xE5\xC1b>\x14a\x03\xFDW\x80c\xF2\xFD\xE3\x8B\x14a\x03\x8FW\x80c\xF95\xD0\xB0\x14a\x03\x08W\x80c\xF9\xAF+\xF2\x14a\x02|Wc\xFE\x99\x04\x9A\x14a\x02GWPa\0\x0EV[4a\x02yW`\x806`\x03\x19\x01\x12a\x02yW`\x04\x90a\x02ca\x18QV[Pa\x02la\x18gV[Pc\x9B\xA6\x06\x1B`\xE0\x1B\x81R\xFD[\x80\xFD[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yW`\xC0\x90`@\x90`\x01`\x01`\xA0\x1B\x03a\x02\xA4a\x18QV[\x16\x81R`\x08` R \x80T\x90`\x01\x80\x80`\xA0\x1B\x03\x91\x01T\x16`@Q\x91`\xFF\x81\x16\x15\x15\x83R`\xFF\x81`\x08\x1C\x16` \x84\x01Rc\xFF\xFF\xFF\xFF\x81`\x10\x1C\x16`@\x84\x01Ra\xFF\xFF\x81`0\x1C\x16``\x84\x01R`\x01\x80`\xA0\x1B\x03\x90`@\x1C\x16`\x80\x83\x01R`\xA0\x82\x01R\xF3[P4a\x02yWa\x03\x176a\x18\xC5V[\x90a\x03 a\x1B\x9BV[`\x01`\x01`\xA0\x1B\x03\x16\x90\x81\x15a\x03\x80W` \x7Fq\xB4\xB1\x08(\xD5\xFESi@\xFEv~\xDE\x8C\x16\xBA\x18Ba\x08\xD4b\x8C\xAF\xF5v\xADE\xAC\xF3\x95\x91\x83\x85R`\x02\x82Ra\x03t\x81`@\x87 \x90`\xFF\x80\x19\x83T\x16\x91\x15\x15\x16\x17\x90UV[`@Q\x90\x15\x15\x81R\xA2\x80\xF3[cT5\xB2\x89`\xE1\x1B\x83R`\x04\x83\xFD[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yWa\x03\xA9a\x18QV[a\x03\xB1a\x1B\x9BV[`\n\x80T`\x01`\x01`\xA0\x1B\x03\x19\x16`\x01`\x01`\xA0\x1B\x03\x92\x83\x16\x90\x81\x17\x90\x91U`\tT\x90\x91\x16\x7F8\xD1k\x8C\xAC\"\xD9\x9F\xC7\xC1$\xB9\xCD\r\xE2\xD3\xFA\x1F\xAE\xF4 \xBF\xE7\x91\xD8\xC3b\xD7e\xE2'\0\x83\x80\xA3\x80\xF3[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yW` \x90`@\x90`\x01`\x01`\xA0\x1B\x03a\x04%a\x18QV[\x16\x81R`\x06\x83R T`@Q\x90\x81R\xF3[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`\nT`@Q`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x81R` \x90\xF3[P\x80`\x03\x196\x01\x12a\x02yWa\x04sa\x1B>V[\x80\xF3[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yW` \x90`\xFF\x90`@\x90`\x01`\x01`\xA0\x1B\x03a\x04\xA1a\x18QV[\x16\x81R`\x02\x84R T\x16`@Q\x90\x15\x15\x81R\xF3[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yW`@` \x91`\x045\x81R`\x04\x83R T`@Q\x90\x81R\xF3[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yWa\x04\xF9a\x18QV[a\x05\x01a\x1B\x9BV[`\x01`\x01`\xA0\x1B\x03\x16\x80\x15a\x05\xABWG\x82\x80\x80\x80\x84\x86Z\xF1=\x15a\x05\xA6W=`\x01`\x01`@\x1B\x03\x81\x11a\x05\x92W`@Q\x90a\x05F`\x1F\x82\x01`\x1F\x19\x16` \x01\x83a\x19\xAEV[\x81R\x84` =\x92\x01>[\x15a\x05\x83W` \x7F\\\xEA\xC9\xF7\x03j\x05\xE21\xFA&=kg1\xDEC\x04`\xD4\xAFH0\x16\x0B\xB6\xF0\r\x1B\x95\x7FP\x91`@Q\x90\x81R\xA2\x80\xF3[c\x01\x8F\x95\xF5`\xE0\x1B\x83R`\x04\x83\xFD[cNH{q`\xE0\x1B\x85R`A`\x04R`$\x85\xFD[a\x05PV[cT5\xB2\x89`\xE1\x1B\x82R`\x04\x82\xFD[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`@Qcp\xA0\x821`\xE0\x1B\x81R0`\x04\x82\x01R\x90` \x82`$\x81\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16Z\xFA\x90\x81\x15a\x06`W\x90a\x06)W[` \x90`@Q\x90\x81R\xF3[P` \x81=` \x11a\x06XW[\x81a\x06C` \x93\x83a\x19\xAEV[\x81\x01\x03\x12a\x06TW` \x90Qa\x06\x1EV[_\x80\xFD[=\x91Pa\x066V[`@Q\x90=\x90\x82>=\x90\xFD[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yW\x80a\x06\x87a\x18QV[a\x06\x8Fa\x1B\x9BV[\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16\x90\x81;\x15a\x07\x15W`@Qca\x1D.u`\xE1\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x90\x91\x16`\x04\x82\x01R\x90\x82\x90\x82\x90`$\x90\x82\x90\x84\x90Z\xF1\x80\x15a\x07\nWa\x06\xF9WP\xF3[\x81a\x07\x03\x91a\x19\xAEV[a\x02yW\x80\xF3[`@Q=\x84\x82>=\x90\xFD[PP\xFD[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yWa\x072a\x1B\x9BV[\x80\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16\x80;\x15a\x07\x89W\x81\x80\x91`\x04`@Q\x80\x94\x81\x93c\xBB\x9F\xE6\xBF`\xE0\x1B\x83RZ\xF1\x80\x15a\x07\nWa\x06\xF9WP\xF3[P\xFD[P` 6`\x03\x19\x01\x12a\x02yW`\x045a\x07\xA4a\x1B\x9BV[\x80\x15\x80\x15a\x08\x8BW[a\x08yW4\x15a\x05\xABW\x80\x82R`\x04` R`@\x82 a\x07\xCE4\x82Ta\x19\xCFV[\x90U\x81\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16\x80;\x15a\x08uW\x81`$\x91`@Q\x92\x83\x80\x92c\xB7`\xFA\xF9`\xE0\x1B\x82R0`\x04\x83\x01R4\x90Z\xF1\x80\x15a\x07\nWa\x08`W[PP`@\x80Q3\x81R4` \x82\x01R`\xA0\x92\x90\x92\x1B\x910\x91\x84\x91_Q` a.\xA1_9_Q\x90_R\x91\x81\x90\x81\x01[\x03\x90\xA4\x80\xF3[\x81a\x08j\x91a\x19\xAEV[a\x08uW\x81_a\x08,V[P\x80\xFD[c\xD51s}`\xE0\x1B\x82R`\x04R`$\x90\xFD[P`\x01`\x01``\x1B\x03\x81\x11a\x07\xADV[P4a\x02yW`@6`\x03\x19\x01\x12a\x02yW` \x90a\x08\xB8a\x18QV[Pa\x08\xC1a\x18gV[P`@Q\x90\x81R\xF3[P4a\x02yWa\x08\xD96a\x18\xC5V[\x90a\x08\xE2a\x1B\x9BV[`\x01`\x01`\xA0\x1B\x03\x16\x90\x81\x15a\x03\x80W` \x7F\xB4sL\x1F\xF2.\xF30\xAC\xC5\x05\xCB'\xF9<:\xDA\x14=\x8F/\xDA\x8D\x82\xED\xAE\x83\xE4\x0C+\xEE\x16\x91\x83\x85R`\x03\x82Ra\x03t\x81`@\x87 \x90`\xFF\x80\x19\x83T\x16\x91\x15\x15\x16\x17\x90UV[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`@Q\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16\x81R` \x90\xF3[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yWa\t\x95a\x18QV[a\t\x9Da\x1B\x9BV[`\x01`\x01`\xA0\x1B\x03\x81\x16\x90\x81\x15a\t\xFBWa\t\xB7\x90a\x1C9V[`\x07\x80T`\x01`\x01`\xA0\x1B\x03\x19\x81\x16\x83\x17\x90\x91U`\x01`\x01`\xA0\x1B\x03\x16\x7F\xDDxn{\xDB\xEB\x98\x9F\xAF\x1B\\\x8BD\x8Ay'\t\xA1\x10M\"\"\xD9\xDA\xD2\xAAS\x1B7\xB5n\x94\x83\x80\xA3\x80\xF3[ci\x18Kw`\xE0\x1B\x83R`\x04\x83\xFD[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`\x07T`@Q`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x81R` \x90\xF3[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`\x01`\x01`\x80\x1B\x03` \x91T`\x08\x1C\x16`@Q\x90\x81R\xF3[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yWa\nva\x18QV[a\n~a\x1B\x9BV[`\x01\x80`\xA0\x1B\x03\x16\x80\x82R`\x08` R\x81`\x01`@\x82 \x82\x81U\x01U\x7Fzx\x7F\\\xBD\xA9\xD7pQEA\x1A\xFAsw\xEB\xC9\xDE\xB3md\x0BG\xF5\xF3\x944\xE6$\xA8\x88F`\xA0`@Q\x84\x81R\x84` \x82\x01R\x84`@\x82\x01R\x84``\x82\x01R\x84`\x80\x82\x01R\xA2\x80\xF3[P4a\x02yW`\x806`\x03\x19\x01\x12a\x02yW`\x045\x90a\n\xFDa\x18gV[\x91`D5\x90`d5`\x01`\x01`\xA0\x1B\x03\x81\x16\x91\x90\x82\x81\x03a\x0CIWa\x0B a\x1B\x9BV[\x81\x15\x80\x15a\x0C9W[a\x0C%W`\x01`\x01`\xA0\x1B\x03\x86\x16\x92\x83\x15\x90\x81\x15a\x0C\x1CW[P\x80\x15a\x0C\x14W[a\x0C\x05W\x81\x85R`\x05` R`@\x85 \x83_R` R`@_ T\x84\x81\x10a\x0B\xCEW\x93a\x0B\x96\x91\x86\x97\x87\x96\x85\x84\x99R`\x05` R`@\x88 \x87_R` R\x83`@_ \x91\x03\x90Ua\x1DMV[`\xA0\x1B\x17\x91_Q` a.\xA1_9_Q\x90_R`@Q\x80a\x08Z0\x943\x83` \x90\x93\x92\x91\x93`@\x81\x01\x94`\x01\x80`\xA0\x1B\x03\x16\x81R\x01RV[`@Qc4e\xB7a`\xE2\x1B\x81R`\x04\x81\x01\x84\x90R`\x01`\x01`\xA0\x1B\x03\x88\x16`$\x82\x01R`D\x81\x01\x86\x90R`d\x81\x01\x91\x90\x91R`\x84\x90\xFD[cT5\xB2\x89`\xE1\x1B\x85R`\x04\x85\xFD[P\x83\x15a\x0BJV[\x90P\x15_a\x0BBV[c\xD51s}`\xE0\x1B\x85R`\x04\x82\x90R`$\x85\xFD[P`\x01`\x01``\x1B\x03\x82\x11a\x0B)V[\x84\x80\xFD[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`\tT`@Q`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x81R` \x90\xF3[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yWa\x0C\x8Fa\x1B\x9BV[\x80T`\xFF\x81\x16a\x0C\xD0W`\x01\x90`\xFF\x19\x16\x17\x81U\x7Fb\xE7\x8C\xEA\x01\xBE\xE3 \xCDNB\x02p\xB5\xEAt\0\r\x11\xB0\xC9\xF7GT\xEB\xDB\xFCTK\x05\xA2X` `@Q3\x81R\xA1\x80\xF3[c\xD9<\x06e`\xE0\x1B\x82R`\x04\x82\xFD[P4a\x02yW`\x806`\x03\x19\x01\x12a\x02yW`\x03`\x045\x10\x15a\x02yW`$5`\x01`\x01`@\x1B\x03\x81\x11a\x08uW6`#\x82\x01\x12\x15a\x08uW\x80`\x04\x015`\x01`\x01`@\x1B\x03\x81\x11a\rSW6`$\x82\x84\x01\x01\x11a\rSWa\x04s\x91a\rCa\x1C\xA7V[`d5\x91`$`D5\x92\x01a$dV[\x82\x80\xFD[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`\nT3`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x03a\r\xCCW`\n\x80T`\x01`\x01`\xA0\x1B\x03\x19\x90\x81\x16\x90\x91U`\t\x80T3\x92\x81\x16\x83\x17\x90\x91U`\x01`\x01`\xA0\x1B\x03\x16\x7F\x8B\xE0\x07\x9CS\x16Y\x14\x13D\xCD\x1F\xD0\xA4\xF2\x84\x19I\x7F\x97\"\xA3\xDA\xAF\xE3\xB4\x18okdW\xE0\x83\x80\xA3\x80\xF3[c\x11\x8C\xDA\xA7`\xE0\x1B\x81R3`\x04R`$\x90\xFD[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yWa\x0E\x11a\x0E\x0C`\x01`\x01`@\x1B\x03` \x93T`\x88\x1C\x16Ba\x1AkV[a\x1D(V[`\x01`\x01`@\x1B\x03`@Q\x91\x16\x81R\xF3[P4a\x02yW``6`\x03\x19\x01\x12a\x02yW`$5`\x045`D5`\x01`\x01`\xA0\x1B\x03\x81\x16\x80\x82\x03a\x0CIWa\x0EVa\x1B\x9BV[\x82\x15\x80\x15a\x0F\x17W[a\x0F\x03W\x15\x80\x15a\x0E\xFBW[a\x0E\xECW\x81\x84R`\x04` R`@\x84 T\x83\x81\x10a\x0E\xD0W\x83\x83\x92\x81a\x0E\x9F\x93\x88\x96\x87R`\x04` R\x03`@\x86 Ua\x19\xF0V[`@\x80Q3\x81R` \x81\x01\x94\x90\x94R`\xA0\x91\x90\x91\x1B\x920\x91_Q` a.\xA1_9_Q\x90_R\x91\x90\x81\x90\x81\x01a\x08ZV[cY&VQ`\xE0\x1B\x85R`\x04\x83\x90R`$\x84\x90R`DR`d\x84\xFD[cT5\xB2\x89`\xE1\x1B\x84R`\x04\x84\xFD[P\x82\x15a\x0EkV[c\xD51s}`\xE0\x1B\x85R`\x04\x83\x90R`$\x85\xFD[P`\x01`\x01``\x1B\x03\x83\x11a\x0E_V[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yWa\x0F@a\x1B\x9BV[`\n\x80T`\x01`\x01`\xA0\x1B\x03\x19\x90\x81\x16\x90\x91U`\t\x80T\x91\x82\x16\x90U\x81\x90`\x01`\x01`\xA0\x1B\x03\x16\x7F\x8B\xE0\x07\x9CS\x16Y\x14\x13D\xCD\x1F\xD0\xA4\xF2\x84\x19I\x7F\x97\"\xA3\xDA\xAF\xE3\xB4\x18okdW\xE0\x82\x80\xA3\x80\xF3[P4a\x02yW`@6`\x03\x19\x01\x12a\x02yW`\x045`\x01`\x01`\x80\x1B\x03\x81\x16\x90\x81\x81\x03a\rSW`$5\x90`\x01`\x01`@\x1B\x03\x82\x16\x91\x82\x81\x03a\x0CIWa\x0F\xD3a\x1B\x9BV[\x83\x15\x80\x15a\x10UW[a\x0C\x05W\x84Ta\x01\0`\x01`\xC8\x1B\x03\x19\x16`\x08\x92\x90\x92\x1Bp\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\0\x16\x91\x90\x91\x17`\x88\x91\x90\x91\x1Bg\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF`\x88\x1B\x16\x17\x83U`@\x80Q\x92\x83R` \x83\x01\x91\x90\x91R\x7F\x9Ad\x9E\x8A\xC2\xE5\xD0j)z\xD5\xC3\xD5cl.\xC8\0hk\xA2\x17\xEC\x8F\x17\xCB\x11\xFE\xA9h{(\x91\xA1\x80\xF3[P\x82\x15a\x0F\xDCV[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`\xFF` \x91T\x16`@Q\x90\x15\x15\x81R\xF3[P4a\x02yW``6`\x03\x19\x01\x12a\x02yW` \x90a\x08\xB8a\x18QV[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`\x01`\x01`@\x1B\x03` \x91T`\x88\x1C\x16`@Q\x90\x81R\xF3[P4a\x02yW`\x04\x90a\x10\xD76a\x18\xC5V[PPc\x9B\xA6\x06\x1B`\xE0\x1B\x81R\xFD[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yW` a\x11\ta\x11\x04a\x18QV[a\x1A\x89V[`\x01`\x01`\x80\x1B\x03`@Q\x91\x16\x81R\xF3[P4a\x02yW``6`\x03\x19\x01\x12a\x02yW`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x08uW\x80`\x04\x01\x90a\x01 `\x03\x19\x826\x03\x01\x12a\rSW`\xE4\x90a\x11\\a\x1C\xA7V[\x01`4a\x11i\x82\x84a\x1C\xF6V[\x90P\x10a\x03\x80Wa\x11z\x90\x82a\x1C\xF6V[\x90\x91\x81`4\x11a\x11\xE6W5\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x82\x03a\x11\xE6W``\x92a\x11\xAF\x92`D5\x92`3\x19\x01\x91`4\x01\x90a\x1D\x9EV[` `@Q\x93\x84\x92`@\x84R\x80Q\x92\x83\x91\x82`@\x87\x01R\x01\x85\x85\x01^\x82\x82\x01\x84\x01\x81\x90R` \x83\x01R`\x1F\x01`\x1F\x19\x16\x81\x01\x03\x01\x90\xF3[\x83\x80\xFD[a\x18\x91V[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yWa\x12\x08a\x1B\x9BV[\x80T`\xFF\x81\x16\x15a\x12FW`\xFF\x19\x16\x81U\x7F]\xB9\xEE\nI[\xF2\xE6\xFF\x9C\x91\xA7\x83L\x1B\xA4\xFD\xD2D\xA5\xE8\xAANS{\xD3\x8A\xEA\xE4\xB0s\xAA` `@Q3\x81R\xA1\x80\xF3[c\x8D\xFC +`\xE0\x1B\x82R`\x04\x82\xFD[P4a\x02yW`@6`\x03\x19\x01\x12a\x02yW`@a\x12qa\x18gV[\x91`\x045\x81R`\x05` R \x90`\x01\x80`\xA0\x1B\x03\x16_R` R` `@_ T`@Q\x90\x81R\xF3[P4a\x02yW`\xA06`\x03\x19\x01\x12a\x02yWa\x12\xB4a\x18QV[a\x12\xBCa\x18gV[`D5\x90c\xFF\xFF\xFF\xFF\x82\x16\x80\x92\x03a\x11\xE6W`d5\x92a\xFF\xFF\x84\x16\x80\x94\x03a\x0CIW`\x845`\x01`\x01`\xA0\x1B\x03\x81\x16\x91\x90\x82\x90\x03a\x14\xF3Wa\x12\xFCa\x1B\x9BV[`\x01`\x01`\xA0\x1B\x03\x16\x93\x84\x15\x80\x15a\x14\xE2W[\x80\x15a\x14\xDAW[\x80\x15a\x14\xD2W[\x80\x15a\x14\xC7W[a\x14\xB8W\x90\x85\x91a\x134\x84a\x1C9V[`@Qc1<\xE5g`\xE0\x1B\x81R\x94` \x86`\x04\x81\x8AZ\xFA\x80\x15a\x14\xADW`\x01`\xA0\x96\x89\x92\x7Fzx\x7F\\\xBD\xA9\xD7pQEA\x1A\xFAsw\xEB\xC9\xDE\xB3md\x0BG\xF5\xF3\x944\xE6$\xA8\x88F\x99\x88\x91a\x14~W[P`@Q\x91a\x13\x8F\x83a\x19dV[\x83\x83R`\xFF` \x84\x01\x92\x16\x82R`@\x83\x01\x91\x86\x83R``\x84\x01\x90\x88\x82Ra\x13\xE6`@`\x80\x87\x01\x94\x88\x8F\x81\x90\x1B\x03\x16\x9C\x8D\x86R\x8E\x88\x01\x99\x8D\x8BR\x81R`\x08` R \x95Q\x15\x15\x86\x90`\xFF\x80\x19\x83T\x16\x91\x15\x15\x16\x17\x90UV[Qg\xFF\xFF\0\0\0\0\0\0e\xFF\xFF\xFF\xFF\0\0\x86T\x95Q`\x10\x1B\x16\x92Q`0\x1B\x16\x92`\x01`@\x1B\x87`\xE0\x1B\x03\x90Q`@\x1B\x16\x93a\xFF\0`\x01`\x01`@\x1B\x03c\xFF\xFF\xFF\xFF`\xE0\x1B\x01\x92`\x08\x1B\x16\x90g\xFF\xFF\xFF\xFF\xFF\xFF\xFF\0\x19\x16\x17\x16\x17\x17\x17\x81U\x01\x90`\x01\x80\x88\x1B\x03\x90Q\x16`\x01`\x01``\x1B\x03\x87\x1B\x82T\x16\x17\x90U`@Q\x93`\x01\x85R` \x85\x01R`@\x84\x01R``\x83\x01R`\x80\x82\x01R\xA2\x80\xF3[a\x14\xA0\x91P` =` \x11a\x14\xA6W[a\x14\x98\x81\x83a\x19\xAEV[\x81\x01\x90a\x1C V[_a\x13\x81V[P=a\x14\x8EV[`@Q=\x86\x82>=\x90\xFD[ci\x18Kw`\xE0\x1B\x86R`\x04\x86\xFD[Pa\x13\x88\x81\x11a\x13$V[P\x83\x15a\x13\x1DV[P\x81\x15a\x13\x16V[P`\x01`\x01`\xA0\x1B\x03\x83\x16\x15a\x13\x0FV[\x85\x80\xFD[P4a\x02yW`@6`\x03\x19\x01\x12a\x02yWa\x15\x11a\x18QV[`$5\x90a\x15\x1Da\x1B\x9BV[`\x01`\x01`\xA0\x1B\x03\x16\x80\x15a\x03\x80W`\x01`\x01``\x1B\x03\x82\x11a\x15}W\x80\x83R`\x06` R`@\x83 T\x90\x80\x84R`\x06` R\x82`@\x85 U\x7F%7t\xF7\xAF\x1E\xA3\xBC\x8C\xF8\xE4\xAD\xB47\xF9\x08\xED\xD9W\xCA\xD3M\x07\x05\xF3#:Y\xEA\x0B\xF1\xE7\x84\x80\xA4\x80\xF3[c\xD51s}`\xE0\x1B\x83R`\x04\x82\x90R`$\x83\xFD[P4a\x02yW`@6`\x03\x19\x01\x12a\x02yWa\x04sa\x15\xAEa\x18QV[a\x15\xB6a\x1B\x9BV[`$5\x90a\x19\xF0V[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yW` \x90`\xFF\x90`@\x90`\x01`\x01`\xA0\x1B\x03a\x15\xEAa\x18QV[\x16\x81R`\x03\x84R T\x16`@Q\x90\x15\x15\x81R\xF3[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yW`@\x90\x81\x90`\x01`\x01`\xA0\x1B\x03a\x16%a\x18QV[\x16\x81R`\x01` R T`\x01`\x01`\x80\x1B\x03\x82Q\x91`\x01`\x01`@\x1B\x03\x81\x16\x83R\x83\x1C\x16` \x82\x01R\xF3[P4a\x02yW``6`\x03\x19\x01\x12a\x02yW`\x045a\x16ma\x18gV[`D5\x91a\x16ya\x1B\x9BV[\x80\x15\x80\x15a\x17\"W[a\x17\x10W`\x01`\x01`\xA0\x1B\x03\x82\x16\x91\x82\x15\x80\x15a\x17\x08W[a\x0C\x05W\x83a\x16\xCF\x91\x83\x87R`\x05` R`@\x87 \x85_R` R`@_ a\x16\xC4\x83\x82Ta\x19\xCFV[\x90U0\x903\x90a\x1B\xC2V[`\xA0\x1B\x17\x90\x82_Q` a.\xA1_9_Q\x90_R`@Q\x80a\x08Z0\x953\x83` \x90\x93\x92\x91\x93`@\x81\x01\x94`\x01\x80`\xA0\x1B\x03\x16\x81R\x01RV[P\x83\x15a\x16\x9AV[c\xD51s}`\xE0\x1B\x84R`\x04R`$\x83\xFD[P`\x01`\x01``\x1B\x03\x81\x11a\x16\x82V[P` 6`\x03\x19\x01\x12a\x06TW`\x045c\xFF\xFF\xFF\xFF\x81\x16\x80\x91\x03a\x06TWa\x17Xa\x1B\x9BV[\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16\x90\x81;\x15a\x06TW_\x90`$`@Q\x80\x94\x81\x93b\x1C\xB6[`\xE5\x1B\x83R`\x04\x83\x01R4\x90Z\xF1\x80\x15a\x17\xC1Wa\x17\xB5WP\x80\xF3[a\0 \x91P_\x90a\x19\xAEV[`@Q=_\x82>=\x90\xFD[4a\x06TW` 6`\x03\x19\x01\x12a\x06TW`\x045c\xFF\xFF\xFF\xFF`\xE0\x1B\x81\x16\x80\x91\x03a\x06TW` \x90c\x0Fc/\xB3`\xE0\x1B\x81\x14\x90\x81\x15a\x18\x11W[P`@Q\x90\x15\x15\x81R\xF3[c\x01\xFF\xC9\xA7`\xE0\x1B\x14\x90P\x82a\x18\x06V[4a\x06TW`@6`\x03\x19\x01\x12a\x06TW` a\x18Ia\x18@a\x18QV[`$5\x90a\x19\x08V[`@Q\x90\x81R\xF3[`\x045\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x82\x03a\x06TWV[`$5\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x82\x03a\x06TWV[5\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x82\x03a\x06TWV[4a\x06TW``6`\x03\x19\x01\x12a\x06TW`\x045`\x01`\x01`\xA0\x1B\x03\x81\x16\x81\x03a\x06TWPc\x9B\xA6\x06\x1B`\xE0\x1B_R`\x04_\xFD[`@\x90`\x03\x19\x01\x12a\x06TW`\x045`\x01`\x01`\xA0\x1B\x03\x81\x16\x81\x03a\x06TW\x90`$5\x80\x15\x15\x81\x03a\x06TW\x90V[5\x90`\x01`\x01`\x80\x1B\x03\x82\x16\x82\x03a\x06TWV[0`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x03a\x19_W`\xA0\x81\x90\x1C\x90`\x01`\x01`\xA0\x1B\x03\x16\x80a\x19=WP_R`\x04` R`@_ T\x90V[\x90_R`\x05` R`@_ \x90`\x01\x80`\xA0\x1B\x03\x16_R` R`@_ T\x90V[P_\x90V[`\xC0\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x19\x7FW`@RV[cNH{q`\xE0\x1B_R`A`\x04R`$_\xFD[`@\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x19\x7FW`@RV[\x90`\x1F\x80\x19\x91\x01\x16\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x19\x7FW`@RV[\x91\x90\x82\x01\x80\x92\x11a\x19\xDCWV[cNH{q`\xE0\x1B_R`\x11`\x04R`$_\xFD[\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16\x91\x82;\x15a\x06TW`@Qc\x04\x0B\x85\x0F`\xE3\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x90\x92\x16`\x04\x83\x01R`$\x82\x01R\x90_\x90\x82\x90`D\x90\x82\x90\x84\x90Z\xF1\x80\x15a\x17\xC1Wa\x1A_WPV[_a\x1Ai\x91a\x19\xAEV[V[\x81\x15a\x1AuW\x04\x90V[cNH{q`\xE0\x1B_R`\x12`\x04R`$_\xFD[`\x01\x80`\xA0\x1B\x03\x16_R`\x01` R`@_ `@Q\x90a\x1A\xA9\x82a\x19\x93V[T`\x01`\x01`@\x1B\x03\x81\x16\x82R`\x01`\x01`\x80\x1B\x03` \x83\x01\x91`@\x1C\x16\x81R_T\x91`\x01`\x01`@\x1B\x03\x80a\x1A\xE7a\x0E\x0C\x82\x87`\x88\x1C\x16Ba\x1AkV[\x92Q\x16\x91\x16\x03a\x1B.W`\x01`\x01`\x80\x1B\x03\x80\x91Q\x16\x91`\x08\x1C\x16\x90\x81\x81\x11\x80\x15a\x1B%W[a\x1B\x1FW`\x01`\x01`\x80\x1B\x03\x91\x03\x16\x90V[PP_\x90V[P\x81\x81\x14a\x1B\rV[P`\x08\x1C`\x01`\x01`\x80\x1B\x03\x16\x90V[\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16\x80;\x15a\x06TW_`$\x91`@Q\x92\x83\x80\x92c\xB7`\xFA\xF9`\xE0\x1B\x82R0`\x04\x83\x01R4\x90Z\xF1\x80\x15a\x17\xC1Wa\x1A_WPV[`\tT`\x01`\x01`\xA0\x1B\x03\x163\x03a\x1B\xAFWV[c\x11\x8C\xDA\xA7`\xE0\x1B_R3`\x04R`$_\xFD[\x91`@Q\x93``R`@R``\x1B`,Rc#\xB8r\xDD``\x1B`\x0CR` _`d`\x1C\x82\x85Z\xF1\x90\x81`\x01_Q\x14\x16\x15a\x1C\x02W[PP_``R`@RV[;\x15=\x17\x10\x15a\x1C\x13W_\x80a\x1B\xF7V[cy9\xF4$_R`\x04`\x1C\xFD[\x90\x81` \x91\x03\x12a\x06TWQ`\xFF\x81\x16\x81\x03a\x06TW\x90V[`@Qc1<\xE5g`\xE0\x1B\x81R\x90` \x90\x82\x90`\x04\x90\x82\x90`\x01`\x01`\xA0\x1B\x03\x16Z\xFA\x90\x81\x15a\x17\xC1W`\x08\x91`\xFF\x91_\x91a\x1C\x88W[P\x16\x03a\x1CyWV[ci\x18Kw`\xE0\x1B_R`\x04_\xFD[a\x1C\xA1\x91P` =` \x11a\x14\xA6Wa\x14\x98\x81\x83a\x19\xAEV[_a\x1CpV[\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x163\x81\x90\x03a\x1C\xDCWPV[c\xFE4\xA6\xD3`\xE0\x1B_R3`\x04R0`$R`DR`d_\xFD[\x905\x90`\x1E\x19\x816\x03\x01\x82\x12\x15a\x06TW\x01\x805\x90`\x01`\x01`@\x1B\x03\x82\x11a\x06TW` \x01\x91\x816\x03\x83\x13a\x06TWV[`\x01`@\x1B\x81\x10\x15a\x1D@W`\x01`\x01`@\x1B\x03\x16\x90V[c5'\x8D\x12_R`\x04`\x1C\xFD[\x91\x90`\x14R`4Rc\xA9\x05\x9C\xBB``\x1B_R` _`D`\x10\x82\x85Z\xF1\x90\x81`\x01_Q\x14\x16\x15a\x1D\x80W[PP_`4RV[;\x15=\x17\x10\x15a\x1D\x91W_\x80a\x1DxV[c\x90\xB8\xEC\x18_R`\x04`\x1C\xFD[\x92``\x92_T\x92`\xFF\x84\x16a$GW`\x01`\x01`\xA0\x1B\x03\x86\x16_\x81\x81R`\x02` R`@\x90 T\x90\x96\x90`\xFF\x16\x15a$4W`\x17_\x80\x83<_Q`\x01`\x01`\xE8\x1B\x03\x19\x16a\xEF\x01`\xF0\x1B\x14a#\xB1W[P\x90a\x1D\xF9\x91a'\xB3V[\x93\x86_\x93\x92\x93R`\x06` R`@_ T\x95\x86\x15a qWPP`\xFF\x16\x15a\x1F\xC6W`\x01\x80`\xA0\x1B\x03\x81\x16\x90\x81_R`\x08` R`@_ \x90`@Qa\x1E>\x81a\x19dV[\x82T\x92`\xFF\x84\x16\x15\x93\x84\x15\x83R`\xFF\x81`\x08\x1C\x16` \x84\x01Rc\xFF\xFF\xFF\xFF\x81`\x10\x1C\x16`@\x84\x01Ra\xFF\xFF\x81`0\x1C\x16``\x84\x01R`\x01\x80`\xA0\x1B\x03\x90`@\x1C\x16`\x80\x83\x01R`\x01\x80\x80`\xA0\x1B\x03\x91\x01T\x16\x92`\xA0\x82\x01\x93\x84Ra\x1F\xB3W\x84a\x1E\xA6\x91a(\x91V[\x91\x90\x95\x80\x87\x11a\x1F\x9DWP\x86_R`\x05` R`@_ \x84_R` R`@_ T\x90\x86\x82\x10a\x1FeWP\x87\x7F\x8D\x065\xEB52K#r\x9A\x0B\x8A\x83\xACX\xCC\xE1\r\xFEE\xF2\xB0\x84I\x07p\xCA\xE0\xB5[\x03\x86`@\x86\x94\x8A\x94\x85_R`\x05` R\x82_ \x87_R` R\x8A\x83_ \x91\x03\x90U\x81Q\x90\x8A\x82R` \x82\x01R\xA4`\x01\x80`\xA0\x1B\x03\x90Q\x16\x93`@Q\x95`\x03` \x88\x01R`@\x87\x01R``\x86\x01R`\x80\x85\x01R`\xA0\x84\x01R`\xC0\x83\x01R`\xE0\x82\x01R`\xE0\x81Ra\x1Fba\x01\0\x82a\x19\xAEV[\x90V[`@Qc4e\xB7a`\xE2\x1B\x81R`\x04\x81\x01\x89\x90R`\x01`\x01`\xA0\x1B\x03\x91\x90\x91\x16`$\x82\x01R`D\x81\x01\x87\x90R`d\x81\x01\x82\x90R`\x84\x90\xFD[\x86c\xFA\xC7Co`\xE0\x1B_R`\x04R`$R`D_\xFD[\x83c\x0CB\x94_`\xE2\x1B_R`\x04R`$_\xFD[P\x91\x80\x91P_R`\x04` R`@_ T\x91\x80\x83\x10a XW\x90\x80\x84\x7F\x91\xBA\x9C%\xEF\xC3\xC1\xAFy\x05\xCD\xB9\x04\x8F\xE8\x8E+}\xAA\xB7\xC2\xD0\xD0\xC3\xF6'w+P\x06\xE0\x04` \x85\x80a \x19`\x01`\x01`\x80\x1B\x03\x99\x98a(#V[\x97\x86_R`\x04\x84R\x03`@_ U`@Q\x90\x81R\xA3`@Q\x93`\x02` \x86\x01R`@\x85\x01R``\x84\x01R\x16`\x80\x82\x01R`\x80\x81Ra\x1Fb`\xA0\x82a\x19\xAEV[\x90cY&VQ`\xE0\x1B_R`\x04R`$R`DR`d_\xFD[\x91`\xFF\x91\x93\x94\x96P\x16\x15a\"CWPP`\x01\x80`\xA0\x1B\x03\x16\x91\x82_R`\x08` R`@_ `@Qa \xA2\x81a\x19dV[\x81T\x91`\xFF\x83\x16\x15\x92\x83\x15\x83R`\xFF\x81`\x08\x1C\x16` \x84\x01Rc\xFF\xFF\xFF\xFF\x81`\x10\x1C\x16`@\x84\x01Ra\xFF\xFF\x81`0\x1C\x16``\x84\x01R`\x01\x80`\xA0\x1B\x03\x90`@\x1C\x16`\x80\x83\x01R`\x01\x80\x80`\xA0\x1B\x03\x91\x01T\x16\x91`\xA0\x82\x01\x92\x83Ra\"0W\x82a!\n\x91a(\x91V[\x84\x82\x95\x92\x11a\"\x19W`@Qcn\xB1v\x9F`\xE1\x1B\x81R`\x04\x81\x01\x88\x90R0`$\x82\x01R` \x81`D\x81\x8AZ\xFA\x90\x81\x15a\x17\xC1W_\x91a!\xE7W[P\x85\x81\x10a!\xBDWP\x7F)l\xAE\xF5\xF4WN\xA0\x1AJ#\xE6\xDD1\\y\x8CLBj\x03J\x07\xD4\x1B\xAB\xF9'\x18\x8B\xCE\xAE`@\x87\x93\x89\x93\x82Q\x91\x82R` \x82\x01R\xA3`\x01\x80`\xA0\x1B\x03\x90Q\x16\x92`@Q\x94`\x01` \x87\x01R`@\x86\x01R``\x85\x01R`\x80\x84\x01R`\xA0\x83\x01R`\xC0\x82\x01R`\xC0\x81Ra\x1Fb`\xE0\x82a\x19\xAEV[\x86`\x84\x91\x87\x8A`@Q\x93c\x86\xB7\xD9\xA9`\xE0\x1B\x85R`\x04\x85\x01R`$\x84\x01R`D\x83\x01R`d\x82\x01R\xFD[\x90P` \x81=` \x11a\"\x11W[\x81a\"\x02` \x93\x83a\x19\xAEV[\x81\x01\x03\x12a\x06TWQ_a!DV[=\x91Pa!\xF5V[P\x83c\xFA\xC7Co`\xE0\x1B_R`\x04R`$R`D_\xFD[\x84c\x0CB\x94_`\xE2\x1B_R`\x04R`$_\xFD[\x91P\x92`\x01`\x01`\x80\x1B\x03\x92Pa\"Y\x90a(#V[\x92\x84_R`\x01` R`@_ \x93`@Q\x94a\"t\x86a\x19\x93V[T`\x01`\x01`@\x1B\x03\x81\x16\x86R\x84` \x87\x01\x91`@\x1C\x16\x81R`\x01`\x01`@\x1B\x03\x80a\"\xA8a\x0E\x0C\x82\x87`\x88\x1C\x16Ba\x1AkV[\x97Q\x16\x96\x16\x95\x86\x14_\x14a#\xA8W\x84\x80\x91Q\x16\x91[\x16\x93\x16\x83\x01\x90`\x01`\x01`\x80\x1B\x03\x82\x11a\x19\xDCW`\x01`\x01`\x80\x1B\x03\x80\x91`\x08\x1C\x16\x91\x16\x90\x80\x82\x11a#\x92WP`@Q\x90a\"\xF7\x82a\x19\x93V[\x84\x82R` \x82\x01\x90\x81R\x85_R`\x01` R`\x01`\x01`@\x1B\x03`@_ \x92Q\x16`\x01`@\x1B`\x01`\xC0\x1B\x03\x83T\x92Q`@\x1B\x16\x91`\x01`\x01`@\x1B\x03`\xC0\x1B\x16\x17\x17\x90U\x83\x7FF\xC1\x0Eg\xD6\xD6\xEF-\xFF\x7F:\x9A|J(\xF4qaq\xD8\x80\x8APc\xE3\xC8\x9C\x12\xA8\x1EG_`@\x80Q\x85\x81R\x86` \x82\x01R\xA2`@Q\x93_` \x86\x01R`@\x85\x01R\x83\x01R`\x80\x82\x01R`\x80\x81Ra\x1Fb`\xA0\x82a\x19\xAEV[\x90c\x0Bckk`\xE1\x1B_R`\x04R`$R`D_\xFD[P\x83_\x91a\"\xBDV[`\x17_\x80\x83<_Q\x90a\x10\xFF`\xF0\x1B`\x01`\x01`\xE8\x1B\x03\x19\x83\x16\x01a$\x08WP`H\x1C`\x01`\x01`\xA0\x1B\x03\x16_\x81\x81R`\x03` R`@\x90 T`\xFF\x16a\x1D\xEEW\x86c\xE9\xF2\xE2\xF3`\xE0\x1B_R`\x04R`$R`D_\xFD[\x87\x90;\x15a$\"Wc\x9FNL\xC9`\xE0\x1B_R`\x04R`$_\xFD[c\xE5\x81\x9B\x95`\xE0\x1B_R`\x04R`$_\xFD[\x86c\xEC\n\xDC3`\xE0\x1B_R`\x04R`$_\xFD[c&4\x1F\xB3`\xE2\x1B_R`\x04_\xFD[5\x90`\xFF\x82\x16\x82\x03a\x06TWV[\x92P\x80`\x1F\x10\x15a'\x9FW\x82`\x1F\x81\x015`\xF8\x1C\x80\x15a'IW`\x01\x81\x14a&\x8EW`\x02\x14a%\x8BW`\xE0\x91\x81\x01\x03\x12a\x06TWa$\xA1\x82a$VV[Pa$\xAE` \x83\x01a\x18}V[`@\x83\x015\x92a$\xC0``\x82\x01a\x18}V[\x93`\xA0\x82\x015\x93a$\xEC`\x80a$\xD8`\xC0\x86\x01a\x18}V[\x97`\x01`\x01`\xA0\x1B\x03\x16\x94\x015\x82\x87a.\x03V[\x93\x85\x85\x11a%\x83W[`@\x7F\xEE\x8C\xF4\x1D\xF06\x87V`\xDE\x15\xDB\xD2\x7FZp\xC4]\x88\x9F.\x18#m\xA0\n\xE9\xD7$\x86\x9E\xB9\x91\x87\x87\x98\x87\x98\x10a%YW[P\x81Q\x93\x84R` \x84\x01\x88\x90R`\x01`\x01`\xA0\x1B\x03\x16\x92\xA4\x81a%FWPPPV[a\x1Ai\x92`\x01`\x01`\xA0\x1B\x03\x16\x90a\x1DMV[\x85_R`\x05` R\x82_ `\x01\x80`\xA0\x1B\x03\x88\x16_R` R\x88\x83_ \x91\x03\x81T\x01\x90U_a%$V[\x85\x94Pa$\xF5V[`\x80\x91\x81\x01\x03\x12a\x06TWa%\x9F\x82a$VV[P\x7F\xAE\xFB|\xEF`\x8BS\x81\xB2\xC1\xE0cw\xECj\xC2\xEE]\x19II\x95\xD9\xFA\x0FM\x9F\xA3u\xB5X\xEDa%\xCD` \x84\x01a\x18}V[a%\xDE```@\x86\x015\x95\x01a\x18\xF4V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x92a%\xF3\x81a(#V[\x91`\x01`\x01`\x80\x1B\x03\x81\x16`\x01`\x01`\x80\x1B\x03\x84\x16\x90\x80\x82\x11\x91\x82\x15a&\x84W[PPa&aW\x85_R`\x04` R`\x01`\x01`\x80\x1B\x03\x83`@_ \x92\x03\x16\x81T\x01\x90Ua&\\`@Q\x92\x83\x92\x83\x90\x92\x91`\x01`\x01`\x80\x1B\x03` \x91`@\x84\x01\x95\x84R\x16\x91\x01RV[\x03\x90\xA3V[`@\x80Q\x92\x83R`\x01`\x01`\x80\x1B\x03\x90\x91\x16` \x83\x01R\x90\x91P\x81\x90\x81\x01a&\\V[\x14\x90P_\x80a&\x14V[P`\xC0\x91\x81\x01\x03\x12a\x06TWa&\xA3\x82a$VV[Pa'\x14`@a&\xB5` \x85\x01a\x18}V[\x93a&\xC1\x82\x82\x01a\x18}V[\x7F\x8E\xF4\xAD\x95\x02?\xFB\xD5A\x8B\xD9\xD8\xDB,g\xF3\xE2F\x9A\xC6\x18\x9AP\xD9\xC0\xE3\xCB\x9B7\xDA[ia&\xEE`\xA0\x84\x01a\x18}V[\x96`\x01`\x01`\xA0\x1B\x03\x92\x83\x16\x95\x92\x16\x93\x85\x93\x85\x93\x90``\x81\x015\x90\x89\x90`\x80\x015a.\x03V[\x96\x81Q\x90\x81R\x87` \x82\x01R\xA3\x82a'-W[PPPPV[a'@\x93`\x01`\x01`\xA0\x1B\x03\x16\x91a\x1B\xC2V[_\x80\x80\x80a''V[P`\x80\x91\x81\x01\x03\x12a\x06TWa'^\x82a$VV[Pa'k` \x83\x01a\x18}V[\x91``a'z`@\x83\x01a\x18\xF4V[\x91\x015`\x01`\x01`@\x1B\x03\x81\x16\x81\x03a\x06TWa\x1Ai\x93`\x01`\x01`\xA0\x1B\x03\x16a,\x94V[cNH{q`\xE0\x1B_R`2`\x04R`$_\xFD[\x91\x81\x15a(\x19W\x825`\xF8\x1C\x92\x83\x15a(\x07W`\x01`\xFF\x85\x16\x03a'\xF8W`5\x83\x03a'\xF8W\x82`\x15\x11a\x06TW`\x01\x81\x015``\x1C\x92`5\x11a\x06TW`\x15\x015\x90V[c\"\"CO`\xE1\x1B_R`\x04_\xFD[P\x91P`\x01\x03a'\xF8W_\x90_\x90_\x90V[_\x92P\x82\x91P\x81\x90V[`\x01`\x80\x1B\x81\x10\x15a\x1D@W`\x01`\x01`\x80\x1B\x03\x16\x90V[Q\x90i\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\x82\x16\x82\x03a\x06TWV[\x90\x81`\xA0\x91\x03\x12a\x06TWa(f\x81a(;V[\x91` \x82\x01Q\x91`@\x81\x01Q\x91a\x1Fb`\x80``\x84\x01Q\x93\x01a(;V[\x91\x90\x82\x03\x91\x82\x11a\x19\xDCWV[`\x07T`\x01`\x01`\xA0\x1B\x03\x16\x92\x91\x83\x15a,<W`@Qc?\xAB\xE5\xA3`\xE2\x1B\x81R`\xA0\x81`\x04\x81\x88Z\xFA\x90\x81\x15a\x17\xC1W__\x92__\x93_\x92a,\x12W[P_\x85\x12\x80\x15a,\nW[a+\xF3W\x15\x91\x82\x15a+\xD9W[PPa+\xB3Wa(\xF7\x81Ba(\x84V[\x90`@\x85\x01\x96c\xFF\xFF\xFF\xFF\x88Q\x16\x80\x93\x11a+\x9BWPPP`\x80\x83\x01\x80Q`@Qc?\xAB\xE5\xA3`\xE2\x1B\x81R\x91\x93\x91\x90`\xA0\x90\x82\x90`\x04\x90\x82\x90`\x01`\x01`\xA0\x1B\x03\x16Z\xFA\x93\x84\x15a\x17\xC1W__\x95__\x94_\x92a+^W[P_\x88\x12\x80\x15a+VW[a+0W\x15\x91\x82\x15a+\x16W[PPa*\xE8Wc\xFF\xFF\xFF\xFFa)|\x83Ba(\x84V[\x98Q\x16\x80\x98\x11a*\xC3WPPa)\x94\x93\x94\x95Pa.\x03V[a\xFF\xFF``\x83\x01Q\x16a'\x10\x01\x90\x81a'\x10\x11a\x19\xDCW\x81\x81\x02\x91a'\x10\x82\x15\x82\x84\x86\x04\x14\x17\x02\x15a*qWPPa'\x10\x90\x04[`\xFF` \x82\x93\x01Q\x16`M\x81\x11a\x19\xDCW`\n\n\x90\x81\x81\x02\x91g\r\xE0\xB6\xB3\xA7d\0\0\x82\x15\x82\x84\x86\x04\x14\x17\x02\x15a*\x08WPPg\r\xE0\xB6\xB3\xA7d\0\0\x90\x04\x91V[g\r\xE0\xB6\xB3\xA7d\0\0\x90_\x19\x81\x84\t\x84\x81\x10\x85\x01\x90\x03\x92\t\x90\x80g\r\xE0\xB6\xB3\xA7d\0\0\x11\x15a*dW\x82\x82\x11\x90\x03`\xEE\x1B\x91\x03`\x12\x1C\x17\x7F\xAC\xCB\x18\x16[\xD6\xFE1\xAE\x1C\xF3\x18\xDC[Q\xEE\xE0\xE1\xBAV\x9B\x88\xCDt\xC1w;\x91\xFA\xC1\x06i\x02\x91V[c\xAEG\xF7\x02_R`\x04`\x1C\xFD[a'\x10\x90_\x19\x81\x84\t\x84\x81\x10\x85\x01\x90\x03\x92\t\x90\x80a'\x10\x11\x15a*dW\x82\x82\x11\x90\x03`\xFC\x1B\x91\x03`\x04\x1C\x17\x7F\xBC\x01\xA3n.\xB1\xC42\xCAW\xA7\x86\xC2&\x80\x9DIQ\x82\xA9\x93\x0B\xE0\xDE\xD2\x88\xCEp:\xFB~\x91\x02a)\xC8V[\x87\x92P`\x01\x80`\xA0\x1B\x03\x90Q\x16cz\xF2'O`\xE0\x1B_R`\x04R`$R`DR`d_\xFD[\x90c\xFF\xFF\xFF\xFF\x88\x92`\x01\x80`\xA0\x1B\x03\x90Q\x16\x92Q\x16\x91cz\xF2'O`\xE0\x1B_R`\x04R`$R`DR`d_\xFD[i\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\x91\x92P\x81\x16\x91\x16\x10_\x80a)gV[\x83Qcc\x19\xD6\xAB`\xE0\x1B_\x90\x81R`\x01`\x01`\xA0\x1B\x03\x90\x91\x16`\x04R`$\x89\x90R`D\x90\xFD[P\x87\x15a)ZV[\x93\x97PPPPa+\x86\x91P`\xA0=`\xA0\x11a+\x94W[a+~\x81\x83a\x19\xAEV[\x81\x01\x90a(RV[\x92\x96\x90\x93\x90\x92\x90\x91_a)OV[P=a+tV[cz\xF2'O`\xE0\x1B_R`\x04R`$R`DR`d_\xFD[\x85\x90c\xFF\xFF\xFF\xFF`@\x86\x01Q\x16\x91cz\xF2'O`\xE0\x1B_R`\x04R`$R`DR`d_\xFD[i\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\x91\x92P\x81\x16\x91\x16\x10_\x80a(\xE7V[\x84\x89cc\x19\xD6\xAB`\xE0\x1B_R`\x04R`$R`D_\xFD[P\x84\x15a(\xDAV[\x93PPPPa,0\x91P`\xA0=`\xA0\x11a+\x94Wa+~\x81\x83a\x19\xAEV[\x92\x93\x90\x92\x90\x91_a(\xCFV[c\x1F\x94\xD8\xA3`\xE0\x1B_R`\x04_\xFD[\x90\x81R`\x01`\x01`@\x1B\x03\x90\x91\x16` \x82\x01R`\x01`\x01`\x80\x1B\x03\x90\x91\x16`@\x82\x01R``\x01\x90V[\x90`\x01`\x01`\x80\x1B\x03\x80\x91\x16\x91\x16\x03\x90`\x01`\x01`\x80\x1B\x03\x82\x11a\x19\xDCWV[`\x01`\x01`\xA0\x1B\x03\x16_\x81\x81R`\x01` R`@\x90\x81\x90 \x90Q\x91\x94\x7F\xEBC)\x0E\xFC\x1C\x94Bq3G\xE1\xD5A\xC9\xE2M5Z0\xF1\x19Q\x080!EY\xCC\xD7.\x8F\x94\x90\x92\x91a,\xDE\x83a\x19\x93V[T`\x01`\x01`\x80\x1B\x03`\x01`\x01`@\x1B\x03\x82\x16\x91\x82\x85R`@\x1C\x16`\x01`\x01`@\x1B\x03` \x85\x01\x96\x82\x88R\x16\x82\x03a-\xEEWPPa-\x1B\x83a(#V[`\x01`\x01`\x80\x1B\x03\x82\x16`\x01`\x01`\x80\x1B\x03\x82\x16\x90\x80\x82\x11\x91\x82\x15a-\xE4W[PPa-\xBDW\x91`\x01`\x01`\x80\x1B\x03a-h\x86\x95\x93a-ba-\xB8\x96\x84\x80\x9AQ\x16\x92a,tV[\x90a,tV[\x16\x84R\x86_R`\x01` R`\x01`\x01`@\x1B\x03`@_ \x91Q\x16\x90\x80T\x94Q\x94\x82`\x01`@\x1B`\x01`\xC0\x1B\x03\x87`@\x1B\x16\x91`\x01`\x01`@\x1B\x03`\xC0\x1B\x16\x17\x17\x90U`@Q\x94\x85\x94\x16\x91\x84a,KV[\x03\x90\xA2V[PP`\x01`\x01`\x80\x1B\x03`\x01`\x01`@\x1B\x03a-\xB8\x92Q\x16\x93Q\x16`@Q\x93\x84\x93\x84a,KV[\x14\x90P_\x80a-;V[\x91P\x93Pa-\xB8\x91P`@Q\x93\x84\x93\x84a,KV[\x91\x90\x91\x81\x15a.\x91W\x82\x81\x02\x92\x82\x82\x15\x82\x84\x87\x04\x14\x17\x02\x15a.&WPP\x90\x04\x90V[\x82\x90_\x19\x81\x84\t\x85\x81\x10\x86\x01\x90\x03\x92\t\x90\x82_\x03\x83\x16\x92\x81\x81\x11\x15a*dW\x83\x90\x04\x80`\x03\x02`\x02\x18\x80\x82\x02`\x02\x03\x02\x80\x82\x02`\x02\x03\x02\x80\x82\x02`\x02\x03\x02\x80\x82\x02`\x02\x03\x02\x80\x82\x02`\x02\x03\x02\x80\x91\x02`\x02\x03\x02\x93`\x01\x84\x84\x83\x03\x04\x94\x80_\x03\x04\x01\x92\x11\x90\x03\x02\x17\x02\x90V[c#\xD3Y\xA3`\xE0\x1B_R`\x04_\xFD\xFE\x1B=~\xDB.\x9C\x0B\x0E|R[ \xAA\xAE\xF0\xF5\x94\r.\xD7\x16c\xC7\xD3\x92f\xEC\xAF\xACr\x88Y\xA1dsolcC\0\x08\"\0\n",
    );
    /// The runtime bytecode of the contract, as deployed on the network.
    ///
    /// ```text
    ///0x60806040526004361015610022575b3615610018575f80fd5b610020611b3e565b005b5f5f3560e01c8062fdd58e1461182257806301ffc9a7146117cc5780630396cb6014611732578063095bcdb6146111ea578063110dfd9214611650578063147e7e66146115fe5780631ac3e310146115bf578063205c287814611591578063249112f4146114f75780632a895f351461129a5780633c7bdcea146112555780633f4ba83a146111ef578063426a8493146111ea57806352b7512c1461111a57806354a2b939146110e5578063558a7297146110c557806357d775f81461109c578063598af9e71461107f5780635c975abb1461105d5780636ca18fc614610f8e578063715018a614610f2757806375cbcca714610e225780637667180814610ddf57806379ba509714610d575780637c627b2114610cdf5780638456cb5914610c765780638da5cb5b14610c4d57806390a4450e14610adf5780639800c10514610a5c5780639c01a3ce14610a335780639c8762e114610a0a578063a6cd75dc1461097b578063b0d691fe14610936578063b1c5af77146108ca578063b6363cf21461089b578063b89b6a1e1461078c578063bb9fe6bf14610719578063c23a5cea1461066c578063c399ec88146105ba578063c9b6d2ba146104df578063cb5638d7146104b5578063cdd0c12d14610476578063d0e30db01461045f578063e30c397814610436578063e5c1623e146103fd578063f2fde38b1461038f578063f935d0b014610308578063f9af2bf21461027c5763fe99049a14610247575061000e565b3461027957608036600319011261027957600490610263611851565b5061026c611867565b50639ba6061b60e01b8152fd5b80fd5b50346102795760203660031901126102795760c0906040906001600160a01b036102a4611851565b1681526008602052208054906001808060a01b03910154166040519160ff81161515835260ff8160081c16602084015263ffffffff8160101c16604084015261ffff8160301c16606084015260018060a01b039060401c16608083015260a0820152f35b503461027957610317366118c5565b90610320611b9b565b6001600160a01b03169081156103805760207f71b4b10828d5fe536940fe767ede8c16ba18426108d4628caff576ad45acf395918385526002825261037481604087209060ff801983541691151516179055565b6040519015158152a280f35b635435b28960e11b8352600483fd5b5034610279576020366003190112610279576103a9611851565b6103b1611b9b565b600a80546001600160a01b0319166001600160a01b039283169081179091556009549091167f38d16b8cac22d99fc7c124b9cd0de2d3fa1faef420bfe791d8c362d765e227008380a380f35b5034610279576020366003190112610279576020906040906001600160a01b03610425611851565b168152600683522054604051908152f35b5034610279578060031936011261027957600a546040516001600160a01b039091168152602090f35b508060031936011261027957610473611b3e565b80f35b50346102795760203660031901126102795760209060ff906040906001600160a01b036104a1611851565b168152600284522054166040519015158152f35b50346102795760203660031901126102795760406020916004358152600483522054604051908152f35b5034610279576020366003190112610279576104f9611851565b610501611b9b565b6001600160a01b031680156105ab57478280808084865af13d156105a6573d6001600160401b0381116105925760405190610546601f8201601f1916602001836119ae565b81528460203d92013e5b156105835760207f5ceac9f7036a05e231fa263d6b6731de430460d4af4830160bb6f00d1b957f5091604051908152a280f35b63018f95f560e01b8352600483fd5b634e487b7160e01b85526041600452602485fd5b610550565b635435b28960e11b8252600482fd5b50346102795780600319360112610279576040516370a0823160e01b8152306004820152906020826024817f00000000000000000000000000000000000000000000000000000000000000006001600160a01b03165afa9081156106605790610629575b602090604051908152f35b506020813d602011610658575b81610643602093836119ae565b81010312610654576020905161061e565b5f80fd5b3d9150610636565b604051903d90823e3d90fd5b50346102795760203660031901126102795780610687611851565b61068f611b9b565b7f00000000000000000000000000000000000000000000000000000000000000006001600160a01b031690813b156107155760405163611d2e7560e11b81526001600160a01b0390911660048201529082908290602490829084905af1801561070a576106f95750f35b81610703916119ae565b6102795780f35b6040513d84823e3d90fd5b5050fd5b5034610279578060031936011261027957610732611b9b565b807f00000000000000000000000000000000000000000000000000000000000000006001600160a01b0316803b156107895781809160046040518094819363bb9fe6bf60e01b83525af1801561070a576106f95750f35b50fd5b506020366003190112610279576004356107a4611b9b565b8015801561088b575b6108795734156105ab578082526004602052604082206107ce3482546119cf565b9055817f00000000000000000000000000000000000000000000000000000000000000006001600160a01b0316803b1561087557816024916040519283809263b760faf960e01b825230600483015234905af1801561070a57610860575b50506040805133815234602082015260a09290921b91309184915f516020612ea15f395f51905f5291819081015b0390a480f35b8161086a916119ae565b61087557815f61082c565b5080fd5b63d531737d60e01b8252600452602490fd5b506001600160601b0381116107ad565b5034610279576040366003190112610279576020906108b8611851565b506108c1611867565b50604051908152f35b5034610279576108d9366118c5565b906108e2611b9b565b6001600160a01b03169081156103805760207fb4734c1ff22ef330acc505cb27f93c3ada143d8f2fda8d82edae83e40c2bee16918385526003825261037481604087209060ff801983541691151516179055565b50346102795780600319360112610279576040517f00000000000000000000000000000000000000000000000000000000000000006001600160a01b03168152602090f35b503461027957602036600319011261027957610995611851565b61099d611b9b565b6001600160a01b0381169081156109fb576109b790611c39565b600780546001600160a01b0319811683179091556001600160a01b03167fdd786e7bdbeb989faf1b5c8b448a792709a1104d2222d9dad2aa531b37b56e948380a380f35b6369184b7760e01b8352600483fd5b50346102795780600319360112610279576007546040516001600160a01b039091168152602090f35b50346102795780600319360112610279576001600160801b036020915460081c16604051908152f35b503461027957602036600319011261027957610a76611851565b610a7e611b9b565b60018060a01b031680825260086020528160016040822082815501557f7a787f5cbda9d7705145411afa7377ebc9deb36d640b47f5f39434e624a8884660a0604051848152846020820152846040820152846060820152846080820152a280f35b50346102795760803660031901126102795760043590610afd611867565b91604435906064356001600160a01b0381169190828103610c4957610b20611b9b565b81158015610c39575b610c25576001600160a01b038616928315908115610c1c575b508015610c14575b610c0557818552600560205260408520835f5260205260405f2054848110610bce5793610b96918697879685849952600560205260408820875f526020528360405f2091039055611d4d565b60a01b17915f516020612ea15f395f51905f526040518061085a3094338360209093929193604081019460018060a01b031681520152565b604051633465b76160e21b8152600481018490526001600160a01b0388166024820152604481018690526064810191909152608490fd5b635435b28960e11b8552600485fd5b508315610b4a565b9050155f610b42565b63d531737d60e01b85526004829052602485fd5b506001600160601b038211610b29565b8480fd5b50346102795780600319360112610279576009546040516001600160a01b039091168152602090f35b5034610279578060031936011261027957610c8f611b9b565b805460ff8116610cd05760019060ff19161781557f62e78cea01bee320cd4e420270b5ea74000d11b0c9f74754ebdbfc544b05a2586020604051338152a180f35b63d93c066560e01b8252600482fd5b50346102795760803660031901126102795760036004351015610279576024356001600160401b03811161087557366023820112156108755780600401356001600160401b038111610d53573660248284010111610d535761047391610d43611ca7565b6064359160246044359201612464565b8280fd5b5034610279578060031936011261027957600a54336001600160a01b0390911603610dcc57600a80546001600160a01b0319908116909155600980543392811683179091556001600160a01b03167f8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e08380a380f35b63118cdaa760e01b815233600452602490fd5b5034610279578060031936011261027957610e11610e0c6001600160401b036020935460881c1642611a6b565b611d28565b6001600160401b0360405191168152f35b5034610279576060366003190112610279576024356004356044356001600160a01b038116808203610c4957610e56611b9b565b82158015610f17575b610f0357158015610efb575b610eec5781845260046020526040842054838110610ed05783839281610e9f938896875260046020520360408620556119f0565b60408051338152602081019490945260a09190911b9230915f516020612ea15f395f51905f5291908190810161085a565b635926565160e01b855260048390526024849052604452606484fd5b635435b28960e11b8452600484fd5b508215610e6b565b63d531737d60e01b85526004839052602485fd5b506001600160601b038311610e5f565b5034610279578060031936011261027957610f40611b9b565b600a80546001600160a01b031990811690915560098054918216905581906001600160a01b03167f8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e08280a380f35b5034610279576040366003190112610279576004356001600160801b03811690818103610d5357602435906001600160401b03821691828103610c4957610fd3611b9b565b83158015611055575b610c05578454610100600160c81b03191660089290921b70ffffffffffffffffffffffffffffffff00169190911760889190911b67ffffffffffffffff60881b161783556040805192835260208301919091527f9a649e8ac2e5d06a297ad5c3d5636c2ec800686ba217ec8f17cb11fea9687b2891a180f35b508215610fdc565b503461027957806003193601126102795760ff60209154166040519015158152f35b5034610279576060366003190112610279576020906108b8611851565b50346102795780600319360112610279576001600160401b036020915460881c16604051908152f35b5034610279576004906110d7366118c5565b5050639ba6061b60e01b8152fd5b5034610279576020366003190112610279576020611109611104611851565b611a89565b6001600160801b0360405191168152f35b5034610279576060366003190112610279576004356001600160401b0381116108755780600401906101206003198236030112610d535760e49061115c611ca7565b0160346111698284611cf6565b9050106103805761117a9082611cf6565b9091816034116111e65735906001600160a01b03821682036111e6576060926111af9260443592603319019160340190611d9e565b6020604051938492604084528051928391826040870152018585015e82820184018190526020830152601f01601f19168101030190f35b8380fd5b611891565b5034610279578060031936011261027957611208611b9b565b805460ff8116156112465760ff191681557f5db9ee0a495bf2e6ff9c91a7834c1ba4fdd244a5e8aa4e537bd38aeae4b073aa6020604051338152a180f35b638dfc202b60e01b8252600482fd5b5034610279576040366003190112610279576040611271611867565b9160043581526005602052209060018060a01b03165f52602052602060405f2054604051908152f35b50346102795760a0366003190112610279576112b4611851565b6112bc611867565b6044359063ffffffff82168092036111e6576064359261ffff8416809403610c49576084356001600160a01b03811691908290036114f3576112fc611b9b565b6001600160a01b031693841580156114e2575b80156114da575b80156114d2575b80156114c7575b6114b85790859161133484611c39565b60405163313ce56760e01b8152946020866004818a5afa80156114ad57600160a09689927f7a787f5cbda9d7705145411afa7377ebc9deb36d640b47f5f39434e624a8884699889161147e575b506040519161138f83611964565b83835260ff6020840192168252604083019186835260608401908882526113e660406080870194888f81901b03169c8d86528e8801998d8b52815260086020522095511515869060ff801983541691151516179055565b5167ffff00000000000065ffffffff00008654955160101b16925160301b1692600160401b8760e01b03905160401b169361ff006001600160401b0363ffffffff60e01b019260081b169067ffffffffffffff001916171617171781550190600180881b039051166001600160601b03871b82541617905560405193600185526020850152604084015260608301526080820152a280f35b6114a0915060203d6020116114a6575b61149881836119ae565b810190611c20565b5f611381565b503d61148e565b6040513d86823e3d90fd5b6369184b7760e01b8652600486fd5b506113888111611324565b50831561131d565b508115611316565b506001600160a01b0383161561130f565b8580fd5b503461027957604036600319011261027957611511611851565b6024359061151d611b9b565b6001600160a01b03168015610380576001600160601b03821161157d57808352600660205260408320549080845260066020528260408520557f253774f7af1ea3bc8cf8e4adb437f908edd957cad34d0705f3233a59ea0bf1e78480a480f35b63d531737d60e01b83526004829052602483fd5b5034610279576040366003190112610279576104736115ae611851565b6115b6611b9b565b602435906119f0565b50346102795760203660031901126102795760209060ff906040906001600160a01b036115ea611851565b168152600384522054166040519015158152f35b50346102795760203660031901126102795760409081906001600160a01b03611625611851565b168152600160205220546001600160801b038251916001600160401b0381168352831c166020820152f35b50346102795760603660031901126102795760043561166d611867565b60443591611679611b9b565b80158015611722575b611710576001600160a01b0382169182158015611708575b610c0557836116cf91838752600560205260408720855f5260205260405f206116c48382546119cf565b905530903390611bc2565b60a01b1790825f516020612ea15f395f51905f526040518061085a3095338360209093929193604081019460018060a01b031681520152565b50831561169a565b63d531737d60e01b8452600452602483fd5b506001600160601b038111611682565b5060203660031901126106545760043563ffffffff811680910361065457611758611b9b565b7f00000000000000000000000000000000000000000000000000000000000000006001600160a01b031690813b15610654575f90602460405180948193621cb65b60e51b8352600483015234905af180156117c1576117b5575080f35b61002091505f906119ae565b6040513d5f823e3d90fd5b346106545760203660031901126106545760043563ffffffff60e01b811680910361065457602090630f632fb360e01b8114908115611811575b506040519015158152f35b6301ffc9a760e01b14905082611806565b34610654576040366003190112610654576020611849611840611851565b60243590611908565b604051908152f35b600435906001600160a01b038216820361065457565b602435906001600160a01b038216820361065457565b35906001600160a01b038216820361065457565b34610654576060366003190112610654576004356001600160a01b03811681036106545750639ba6061b60e01b5f5260045ffd5b6040906003190112610654576004356001600160a01b0381168103610654579060243580151581036106545790565b35906001600160801b038216820361065457565b306001600160a01b039091160361195f5760a081901c906001600160a01b03168061193d57505f52600460205260405f205490565b905f52600560205260405f209060018060a01b03165f5260205260405f205490565b505f90565b60c081019081106001600160401b0382111761197f57604052565b634e487b7160e01b5f52604160045260245ffd5b604081019081106001600160401b0382111761197f57604052565b90601f801991011681019081106001600160401b0382111761197f57604052565b919082018092116119dc57565b634e487b7160e01b5f52601160045260245ffd5b7f00000000000000000000000000000000000000000000000000000000000000006001600160a01b031691823b156106545760405163040b850f60e31b81526001600160a01b0390921660048301526024820152905f908290604490829084905af180156117c157611a5f5750565b5f611a69916119ae565b565b8115611a75570490565b634e487b7160e01b5f52601260045260245ffd5b60018060a01b03165f52600160205260405f2060405190611aa982611993565b546001600160401b03811682526001600160801b03602083019160401c1681525f54916001600160401b0380611ae7610e0c828760881c1642611a6b565b925116911603611b2e576001600160801b03809151169160081c16908181118015611b25575b611b1f576001600160801b0391031690565b50505f90565b50818114611b0d565b5060081c6001600160801b031690565b7f00000000000000000000000000000000000000000000000000000000000000006001600160a01b0316803b15610654575f6024916040519283809263b760faf960e01b825230600483015234905af180156117c157611a5f5750565b6009546001600160a01b03163303611baf57565b63118cdaa760e01b5f523360045260245ffd5b916040519360605260405260601b602c526323b872dd60601b600c5260205f6064601c82855af1908160015f51141615611c02575b50505f606052604052565b3b153d171015611c13575f80611bf7565b637939f4245f526004601cfd5b90816020910312610654575160ff811681036106545790565b60405163313ce56760e01b815290602090829060049082906001600160a01b03165afa9081156117c15760089160ff915f91611c88575b501603611c7957565b6369184b7760e01b5f5260045ffd5b611ca1915060203d6020116114a65761149881836119ae565b5f611c70565b7f00000000000000000000000000000000000000000000000000000000000000006001600160a01b031633819003611cdc5750565b63fe34a6d360e01b5f52336004523060245260445260645ffd5b903590601e198136030182121561065457018035906001600160401b0382116106545760200191813603831361065457565b600160401b811015611d40576001600160401b031690565b6335278d125f526004601cfd5b919060145260345263a9059cbb60601b5f5260205f6044601082855af1908160015f51141615611d80575b50505f603452565b3b153d171015611d91575f80611d78565b6390b8ec185f526004601cfd5b926060925f549260ff8416612447576001600160a01b0386165f8181526002602052604090205490969060ff16156124345760175f80833c5f516001600160e81b03191661ef0160f01b146123b1575b5090611df9916127b3565b93865f93929352600660205260405f205495861561207157505060ff1615611fc65760018060a01b03811690815f52600860205260405f2090604051611e3e81611964565b82549260ff841615938415835260ff8160081c16602084015263ffffffff8160101c16604084015261ffff8160301c16606084015260018060a01b039060401c1660808301526001808060a01b03910154169260a08201938452611fb35784611ea691612891565b919095808711611f9d5750865f52600560205260405f20845f5260205260405f205490868210611f655750877f8d0635eb35324b23729a0b8a83ac58cce10dfe45f2b084490770cae0b55b0386604086948a94855f526005602052825f20875f526020528a835f20910390558151908a82526020820152a460018060a01b0390511693604051956003602088015260408701526060860152608085015260a084015260c083015260e082015260e08152611f62610100826119ae565b90565b604051633465b76160e21b8152600481018990526001600160a01b039190911660248201526044810187905260648101829052608490fd5b8663fac7436f60e01b5f5260045260245260445ffd5b83630c42945f60e21b5f5260045260245ffd5b50918091505f52600460205260405f205491808310612058579080847f91ba9c25efc3c1af7905cdb9048fe88e2b7daab7c2d0d0c3f627772b5006e004602085806120196001600160801b039998612823565b97865f52600484520360405f2055604051908152a360405193600260208601526040850152606084015216608082015260808152611f6260a0826119ae565b90635926565160e01b5f5260045260245260445260645ffd5b9160ff9193949650161561224357505060018060a01b031691825f52600860205260405f206040516120a281611964565b81549160ff831615928315835260ff8160081c16602084015263ffffffff8160101c16604084015261ffff8160301c16606084015260018060a01b039060401c1660808301526001808060a01b03910154169160a08201928352612230578261210a91612891565b848295921161221957604051636eb1769f60e11b8152600481018890523060248201526020816044818a5afa9081156117c1575f916121e7575b508581106121bd57507f296caef5f4574ea01a4a23e6dd315c798c4c426a034a07d41babf927188bceae60408793899382519182526020820152a360018060a01b0390511692604051946001602087015260408601526060850152608084015260a083015260c082015260c08152611f6260e0826119ae565b86608491878a604051936386b7d9a960e01b85526004850152602484015260448301526064820152fd5b90506020813d602011612211575b81612202602093836119ae565b8101031261065457515f612144565b3d91506121f5565b508363fac7436f60e01b5f5260045260245260445ffd5b84630c42945f60e21b5f5260045260245ffd5b9150926001600160801b03925061225990612823565b92845f52600160205260405f20936040519461227486611993565b546001600160401b038116865284602087019160401c1681526001600160401b03806122a8610e0c828760881c1642611a6b565b97511696169586145f146123a8578480915116915b1693168301906001600160801b0382116119dc576001600160801b03809160081c169116908082116123925750604051906122f782611993565b84825260208201908152855f5260016020526001600160401b0360405f20925116600160401b600160c01b038354925160401b16916001600160401b0360c01b1617179055837f46c10e67d6d6ef2dff7f3a9a7c4a28f4716171d8808a5063e3c89c12a81e475f60408051858152866020820152a2604051935f60208601526040850152830152608082015260808152611f6260a0826119ae565b90630b636b6b60e11b5f5260045260245260445ffd5b50835f916122bd565b60175f80833c5f51906110ff60f01b6001600160e81b0319831601612408575060481c6001600160a01b03165f8181526003602052604090205460ff16611dee578663e9f2e2f360e01b5f5260045260245260445ffd5b87903b1561242257639f4e4cc960e01b5f5260045260245ffd5b63e5819b9560e01b5f5260045260245ffd5b8663ec0adc3360e01b5f5260045260245ffd5b6326341fb360e21b5f5260045ffd5b359060ff8216820361065457565b925080601f101561279f5782601f81013560f81c8015612749576001811461268e5760021461258b5760e09181010312610654576124a182612456565b506124ae6020830161187d565b6040830135926124c06060820161187d565b9360a0820135936124ec60806124d860c0860161187d565b976001600160a01b03169401358287612e03565b93858511612583575b60407fee8cf41df036875660de15dbd27f5a70c45d889f2e18236da00ae9d724869eb991878798879810612559575b508151938452602084018890526001600160a01b031692a48161254657505050565b611a69926001600160a01b031690611d4d565b855f526005602052825f2060018060a01b0388165f5260205288835f20910381540190555f612524565b8594506124f5565b608091810103126106545761259f82612456565b507faefb7cef608b5381b2c1e06377ec6ac2ee5d19494995d9fa0f4d9fa375b558ed6125cd6020840161187d565b6125de6060604086013595016118f4565b6001600160a01b03909116926125f381612823565b916001600160801b0381166001600160801b03841690808211918215612684575b505061266157855f5260046020526001600160801b038360405f20920316815401905561265c604051928392839092916001600160801b036020916040840195845216910152565b0390a3565b604080519283526001600160801b0390911660208301529091508190810161265c565b1490505f80612614565b5060c09181010312610654576126a382612456565b5061271460406126b56020850161187d565b936126c182820161187d565b7f8ef4ad95023ffbd5418bd9d8db2c67f3e2469ac6189a50d9c0e3cb9b37da5b696126ee60a0840161187d565b966001600160a01b03928316959216938593859390606081013590899060800135612e03565b968151908152876020820152a38261272d575b50505050565b612740936001600160a01b031691611bc2565b5f808080612727565b50608091810103126106545761275e82612456565b5061276b6020830161187d565b91606061277a604083016118f4565b9101356001600160401b038116810361065457611a69936001600160a01b0316612c94565b634e487b7160e01b5f52603260045260245ffd5b91811561281957823560f81c92831561280757600160ff8516036127f857603583036127f8578260151161065457600181013560601c92603511610654576015013590565b632222434f60e11b5f5260045ffd5b5091506001036127f8575f905f905f90565b5f92508291508190565b600160801b811015611d40576001600160801b031690565b519069ffffffffffffffffffff8216820361065457565b908160a0910312610654576128668161283b565b91602082015191604081015191611f6260806060840151930161283b565b919082039182116119dc57565b6007546001600160a01b031692918315612c3c57604051633fabe5a360e21b815260a081600481885afa9081156117c1575f5f925f5f935f92612c12575b505f85128015612c0a575b612bf35715918215612bd9575b5050612bb3576128f78142612884565b90604085019663ffffffff885116809311612b9b57505050608083018051604051633fabe5a360e21b81529193919060a090829060049082906001600160a01b03165afa9384156117c1575f5f955f5f945f92612b5e575b505f88128015612b56575b612b305715918215612b16575b5050612ae85763ffffffff61297c8342612884565b985116809811612ac357505061299493949550612e03565b61ffff606083015116612710019081612710116119dc578181029161271082158284860414170215612a7157505061271090045b60ff60208293015116604d81116119dc57600a0a9081810291670de0b6b3a764000082158284860414170215612a08575050670de0b6b3a7640000900491565b670de0b6b3a7640000905f198184098481108501900392099080670de0b6b3a76400001115612a6457828211900360ee1b910360121c177faccb18165bd6fe31ae1cf318dc5b51eee0e1ba569b88cd74c1773b91fac106690291565b63ae47f7025f526004601cfd5b612710905f1981840984811085019003920990806127101115612a6457828211900360fc1b910360041c177fbc01a36e2eb1c432ca57a786c226809d495182a9930be0ded288ce703afb7e91026129c8565b87925060018060a01b03905116637af2274f60e01b5f5260045260245260445260645ffd5b9063ffffffff889260018060a01b0390511692511691637af2274f60e01b5f5260045260245260445260645ffd5b69ffffffffffffffffffff91925081169116105f80612967565b8351636319d6ab60e01b5f9081526001600160a01b039091166004526024899052604490fd5b50871561295a565b939750505050612b86915060a03d60a011612b94575b612b7e81836119ae565b810190612852565b92969093909290915f61294f565b503d612b74565b637af2274f60e01b5f5260045260245260445260645ffd5b859063ffffffff60408601511691637af2274f60e01b5f5260045260245260445260645ffd5b69ffffffffffffffffffff91925081169116105f806128e7565b8489636319d6ab60e01b5f5260045260245260445ffd5b5084156128da565b9350505050612c30915060a03d60a011612b9457612b7e81836119ae565b9293909290915f6128cf565b631f94d8a360e01b5f5260045ffd5b9081526001600160401b0390911660208201526001600160801b03909116604082015260600190565b906001600160801b03809116911603906001600160801b0382116119dc57565b6001600160a01b03165f8181526001602052604090819020905191947feb43290efc1c9442713347e1d541c9e24d355a30f119510830214559ccd72e8f94909291612cde83611993565b546001600160801b036001600160401b0382169182855260401c166001600160401b036020850196828852168203612dee575050612d1b83612823565b6001600160801b0382166001600160801b03821690808211918215612de4575b5050612dbd57916001600160801b03612d68869593612d62612db89684809a511692612c74565b90612c74565b168452865f5260016020526001600160401b0360405f2091511690805494519482600160401b600160c01b038760401b16916001600160401b0360c01b1617179055604051948594169184612c4b565b0390a2565b50506001600160801b036001600160401b03612db892511693511660405193849384612c4b565b1490505f80612d3b565b91509350612db8915060405193849384612c4b565b9190918115612e9157828102928282158284870414170215612e26575050900490565b82905f1981840985811086019003920990825f0383169281811115612a645783900480600302600218808202600203028082026002030280820260020302808202600203028082026002030280910260020302936001848483030494805f0304019211900302170290565b6323d359a360e01b5f5260045ffdfe1b3d7edb2e9c0b0e7c525b20aaaef0f5940d2ed71663c7d39266ecafac728859a164736f6c6343000822000a
    /// ```
    #[rustfmt::skip]
    #[allow(clippy::all)]
    pub static DEPLOYED_BYTECODE: alloy_sol_types::private::Bytes = alloy_sol_types::private::Bytes::from_static(
        b"`\x80`@R`\x046\x10\x15a\0\"W[6\x15a\0\x18W_\x80\xFD[a\0 a\x1B>V[\0[__5`\xE0\x1C\x80b\xFD\xD5\x8E\x14a\x18\"W\x80c\x01\xFF\xC9\xA7\x14a\x17\xCCW\x80c\x03\x96\xCB`\x14a\x172W\x80c\t[\xCD\xB6\x14a\x11\xEAW\x80c\x11\r\xFD\x92\x14a\x16PW\x80c\x14~~f\x14a\x15\xFEW\x80c\x1A\xC3\xE3\x10\x14a\x15\xBFW\x80c \\(x\x14a\x15\x91W\x80c$\x91\x12\xF4\x14a\x14\xF7W\x80c*\x89_5\x14a\x12\x9AW\x80c<{\xDC\xEA\x14a\x12UW\x80c?K\xA8:\x14a\x11\xEFW\x80cBj\x84\x93\x14a\x11\xEAW\x80cR\xB7Q,\x14a\x11\x1AW\x80cT\xA2\xB99\x14a\x10\xE5W\x80cU\x8Ar\x97\x14a\x10\xC5W\x80cW\xD7u\xF8\x14a\x10\x9CW\x80cY\x8A\xF9\xE7\x14a\x10\x7FW\x80c\\\x97Z\xBB\x14a\x10]W\x80cl\xA1\x8F\xC6\x14a\x0F\x8EW\x80cqP\x18\xA6\x14a\x0F'W\x80cu\xCB\xCC\xA7\x14a\x0E\"W\x80cvg\x18\x08\x14a\r\xDFW\x80cy\xBAP\x97\x14a\rWW\x80c|b{!\x14a\x0C\xDFW\x80c\x84V\xCBY\x14a\x0CvW\x80c\x8D\xA5\xCB[\x14a\x0CMW\x80c\x90\xA4E\x0E\x14a\n\xDFW\x80c\x98\0\xC1\x05\x14a\n\\W\x80c\x9C\x01\xA3\xCE\x14a\n3W\x80c\x9C\x87b\xE1\x14a\n\nW\x80c\xA6\xCDu\xDC\x14a\t{W\x80c\xB0\xD6\x91\xFE\x14a\t6W\x80c\xB1\xC5\xAFw\x14a\x08\xCAW\x80c\xB66<\xF2\x14a\x08\x9BW\x80c\xB8\x9Bj\x1E\x14a\x07\x8CW\x80c\xBB\x9F\xE6\xBF\x14a\x07\x19W\x80c\xC2:\\\xEA\x14a\x06lW\x80c\xC3\x99\xEC\x88\x14a\x05\xBAW\x80c\xC9\xB6\xD2\xBA\x14a\x04\xDFW\x80c\xCBV8\xD7\x14a\x04\xB5W\x80c\xCD\xD0\xC1-\x14a\x04vW\x80c\xD0\xE3\r\xB0\x14a\x04_W\x80c\xE3\x0C9x\x14a\x046W\x80c\xE5\xC1b>\x14a\x03\xFDW\x80c\xF2\xFD\xE3\x8B\x14a\x03\x8FW\x80c\xF95\xD0\xB0\x14a\x03\x08W\x80c\xF9\xAF+\xF2\x14a\x02|Wc\xFE\x99\x04\x9A\x14a\x02GWPa\0\x0EV[4a\x02yW`\x806`\x03\x19\x01\x12a\x02yW`\x04\x90a\x02ca\x18QV[Pa\x02la\x18gV[Pc\x9B\xA6\x06\x1B`\xE0\x1B\x81R\xFD[\x80\xFD[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yW`\xC0\x90`@\x90`\x01`\x01`\xA0\x1B\x03a\x02\xA4a\x18QV[\x16\x81R`\x08` R \x80T\x90`\x01\x80\x80`\xA0\x1B\x03\x91\x01T\x16`@Q\x91`\xFF\x81\x16\x15\x15\x83R`\xFF\x81`\x08\x1C\x16` \x84\x01Rc\xFF\xFF\xFF\xFF\x81`\x10\x1C\x16`@\x84\x01Ra\xFF\xFF\x81`0\x1C\x16``\x84\x01R`\x01\x80`\xA0\x1B\x03\x90`@\x1C\x16`\x80\x83\x01R`\xA0\x82\x01R\xF3[P4a\x02yWa\x03\x176a\x18\xC5V[\x90a\x03 a\x1B\x9BV[`\x01`\x01`\xA0\x1B\x03\x16\x90\x81\x15a\x03\x80W` \x7Fq\xB4\xB1\x08(\xD5\xFESi@\xFEv~\xDE\x8C\x16\xBA\x18Ba\x08\xD4b\x8C\xAF\xF5v\xADE\xAC\xF3\x95\x91\x83\x85R`\x02\x82Ra\x03t\x81`@\x87 \x90`\xFF\x80\x19\x83T\x16\x91\x15\x15\x16\x17\x90UV[`@Q\x90\x15\x15\x81R\xA2\x80\xF3[cT5\xB2\x89`\xE1\x1B\x83R`\x04\x83\xFD[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yWa\x03\xA9a\x18QV[a\x03\xB1a\x1B\x9BV[`\n\x80T`\x01`\x01`\xA0\x1B\x03\x19\x16`\x01`\x01`\xA0\x1B\x03\x92\x83\x16\x90\x81\x17\x90\x91U`\tT\x90\x91\x16\x7F8\xD1k\x8C\xAC\"\xD9\x9F\xC7\xC1$\xB9\xCD\r\xE2\xD3\xFA\x1F\xAE\xF4 \xBF\xE7\x91\xD8\xC3b\xD7e\xE2'\0\x83\x80\xA3\x80\xF3[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yW` \x90`@\x90`\x01`\x01`\xA0\x1B\x03a\x04%a\x18QV[\x16\x81R`\x06\x83R T`@Q\x90\x81R\xF3[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`\nT`@Q`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x81R` \x90\xF3[P\x80`\x03\x196\x01\x12a\x02yWa\x04sa\x1B>V[\x80\xF3[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yW` \x90`\xFF\x90`@\x90`\x01`\x01`\xA0\x1B\x03a\x04\xA1a\x18QV[\x16\x81R`\x02\x84R T\x16`@Q\x90\x15\x15\x81R\xF3[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yW`@` \x91`\x045\x81R`\x04\x83R T`@Q\x90\x81R\xF3[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yWa\x04\xF9a\x18QV[a\x05\x01a\x1B\x9BV[`\x01`\x01`\xA0\x1B\x03\x16\x80\x15a\x05\xABWG\x82\x80\x80\x80\x84\x86Z\xF1=\x15a\x05\xA6W=`\x01`\x01`@\x1B\x03\x81\x11a\x05\x92W`@Q\x90a\x05F`\x1F\x82\x01`\x1F\x19\x16` \x01\x83a\x19\xAEV[\x81R\x84` =\x92\x01>[\x15a\x05\x83W` \x7F\\\xEA\xC9\xF7\x03j\x05\xE21\xFA&=kg1\xDEC\x04`\xD4\xAFH0\x16\x0B\xB6\xF0\r\x1B\x95\x7FP\x91`@Q\x90\x81R\xA2\x80\xF3[c\x01\x8F\x95\xF5`\xE0\x1B\x83R`\x04\x83\xFD[cNH{q`\xE0\x1B\x85R`A`\x04R`$\x85\xFD[a\x05PV[cT5\xB2\x89`\xE1\x1B\x82R`\x04\x82\xFD[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`@Qcp\xA0\x821`\xE0\x1B\x81R0`\x04\x82\x01R\x90` \x82`$\x81\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16Z\xFA\x90\x81\x15a\x06`W\x90a\x06)W[` \x90`@Q\x90\x81R\xF3[P` \x81=` \x11a\x06XW[\x81a\x06C` \x93\x83a\x19\xAEV[\x81\x01\x03\x12a\x06TW` \x90Qa\x06\x1EV[_\x80\xFD[=\x91Pa\x066V[`@Q\x90=\x90\x82>=\x90\xFD[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yW\x80a\x06\x87a\x18QV[a\x06\x8Fa\x1B\x9BV[\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16\x90\x81;\x15a\x07\x15W`@Qca\x1D.u`\xE1\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x90\x91\x16`\x04\x82\x01R\x90\x82\x90\x82\x90`$\x90\x82\x90\x84\x90Z\xF1\x80\x15a\x07\nWa\x06\xF9WP\xF3[\x81a\x07\x03\x91a\x19\xAEV[a\x02yW\x80\xF3[`@Q=\x84\x82>=\x90\xFD[PP\xFD[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yWa\x072a\x1B\x9BV[\x80\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16\x80;\x15a\x07\x89W\x81\x80\x91`\x04`@Q\x80\x94\x81\x93c\xBB\x9F\xE6\xBF`\xE0\x1B\x83RZ\xF1\x80\x15a\x07\nWa\x06\xF9WP\xF3[P\xFD[P` 6`\x03\x19\x01\x12a\x02yW`\x045a\x07\xA4a\x1B\x9BV[\x80\x15\x80\x15a\x08\x8BW[a\x08yW4\x15a\x05\xABW\x80\x82R`\x04` R`@\x82 a\x07\xCE4\x82Ta\x19\xCFV[\x90U\x81\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16\x80;\x15a\x08uW\x81`$\x91`@Q\x92\x83\x80\x92c\xB7`\xFA\xF9`\xE0\x1B\x82R0`\x04\x83\x01R4\x90Z\xF1\x80\x15a\x07\nWa\x08`W[PP`@\x80Q3\x81R4` \x82\x01R`\xA0\x92\x90\x92\x1B\x910\x91\x84\x91_Q` a.\xA1_9_Q\x90_R\x91\x81\x90\x81\x01[\x03\x90\xA4\x80\xF3[\x81a\x08j\x91a\x19\xAEV[a\x08uW\x81_a\x08,V[P\x80\xFD[c\xD51s}`\xE0\x1B\x82R`\x04R`$\x90\xFD[P`\x01`\x01``\x1B\x03\x81\x11a\x07\xADV[P4a\x02yW`@6`\x03\x19\x01\x12a\x02yW` \x90a\x08\xB8a\x18QV[Pa\x08\xC1a\x18gV[P`@Q\x90\x81R\xF3[P4a\x02yWa\x08\xD96a\x18\xC5V[\x90a\x08\xE2a\x1B\x9BV[`\x01`\x01`\xA0\x1B\x03\x16\x90\x81\x15a\x03\x80W` \x7F\xB4sL\x1F\xF2.\xF30\xAC\xC5\x05\xCB'\xF9<:\xDA\x14=\x8F/\xDA\x8D\x82\xED\xAE\x83\xE4\x0C+\xEE\x16\x91\x83\x85R`\x03\x82Ra\x03t\x81`@\x87 \x90`\xFF\x80\x19\x83T\x16\x91\x15\x15\x16\x17\x90UV[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`@Q\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16\x81R` \x90\xF3[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yWa\t\x95a\x18QV[a\t\x9Da\x1B\x9BV[`\x01`\x01`\xA0\x1B\x03\x81\x16\x90\x81\x15a\t\xFBWa\t\xB7\x90a\x1C9V[`\x07\x80T`\x01`\x01`\xA0\x1B\x03\x19\x81\x16\x83\x17\x90\x91U`\x01`\x01`\xA0\x1B\x03\x16\x7F\xDDxn{\xDB\xEB\x98\x9F\xAF\x1B\\\x8BD\x8Ay'\t\xA1\x10M\"\"\xD9\xDA\xD2\xAAS\x1B7\xB5n\x94\x83\x80\xA3\x80\xF3[ci\x18Kw`\xE0\x1B\x83R`\x04\x83\xFD[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`\x07T`@Q`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x81R` \x90\xF3[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`\x01`\x01`\x80\x1B\x03` \x91T`\x08\x1C\x16`@Q\x90\x81R\xF3[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yWa\nva\x18QV[a\n~a\x1B\x9BV[`\x01\x80`\xA0\x1B\x03\x16\x80\x82R`\x08` R\x81`\x01`@\x82 \x82\x81U\x01U\x7Fzx\x7F\\\xBD\xA9\xD7pQEA\x1A\xFAsw\xEB\xC9\xDE\xB3md\x0BG\xF5\xF3\x944\xE6$\xA8\x88F`\xA0`@Q\x84\x81R\x84` \x82\x01R\x84`@\x82\x01R\x84``\x82\x01R\x84`\x80\x82\x01R\xA2\x80\xF3[P4a\x02yW`\x806`\x03\x19\x01\x12a\x02yW`\x045\x90a\n\xFDa\x18gV[\x91`D5\x90`d5`\x01`\x01`\xA0\x1B\x03\x81\x16\x91\x90\x82\x81\x03a\x0CIWa\x0B a\x1B\x9BV[\x81\x15\x80\x15a\x0C9W[a\x0C%W`\x01`\x01`\xA0\x1B\x03\x86\x16\x92\x83\x15\x90\x81\x15a\x0C\x1CW[P\x80\x15a\x0C\x14W[a\x0C\x05W\x81\x85R`\x05` R`@\x85 \x83_R` R`@_ T\x84\x81\x10a\x0B\xCEW\x93a\x0B\x96\x91\x86\x97\x87\x96\x85\x84\x99R`\x05` R`@\x88 \x87_R` R\x83`@_ \x91\x03\x90Ua\x1DMV[`\xA0\x1B\x17\x91_Q` a.\xA1_9_Q\x90_R`@Q\x80a\x08Z0\x943\x83` \x90\x93\x92\x91\x93`@\x81\x01\x94`\x01\x80`\xA0\x1B\x03\x16\x81R\x01RV[`@Qc4e\xB7a`\xE2\x1B\x81R`\x04\x81\x01\x84\x90R`\x01`\x01`\xA0\x1B\x03\x88\x16`$\x82\x01R`D\x81\x01\x86\x90R`d\x81\x01\x91\x90\x91R`\x84\x90\xFD[cT5\xB2\x89`\xE1\x1B\x85R`\x04\x85\xFD[P\x83\x15a\x0BJV[\x90P\x15_a\x0BBV[c\xD51s}`\xE0\x1B\x85R`\x04\x82\x90R`$\x85\xFD[P`\x01`\x01``\x1B\x03\x82\x11a\x0B)V[\x84\x80\xFD[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`\tT`@Q`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x81R` \x90\xF3[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yWa\x0C\x8Fa\x1B\x9BV[\x80T`\xFF\x81\x16a\x0C\xD0W`\x01\x90`\xFF\x19\x16\x17\x81U\x7Fb\xE7\x8C\xEA\x01\xBE\xE3 \xCDNB\x02p\xB5\xEAt\0\r\x11\xB0\xC9\xF7GT\xEB\xDB\xFCTK\x05\xA2X` `@Q3\x81R\xA1\x80\xF3[c\xD9<\x06e`\xE0\x1B\x82R`\x04\x82\xFD[P4a\x02yW`\x806`\x03\x19\x01\x12a\x02yW`\x03`\x045\x10\x15a\x02yW`$5`\x01`\x01`@\x1B\x03\x81\x11a\x08uW6`#\x82\x01\x12\x15a\x08uW\x80`\x04\x015`\x01`\x01`@\x1B\x03\x81\x11a\rSW6`$\x82\x84\x01\x01\x11a\rSWa\x04s\x91a\rCa\x1C\xA7V[`d5\x91`$`D5\x92\x01a$dV[\x82\x80\xFD[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`\nT3`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x03a\r\xCCW`\n\x80T`\x01`\x01`\xA0\x1B\x03\x19\x90\x81\x16\x90\x91U`\t\x80T3\x92\x81\x16\x83\x17\x90\x91U`\x01`\x01`\xA0\x1B\x03\x16\x7F\x8B\xE0\x07\x9CS\x16Y\x14\x13D\xCD\x1F\xD0\xA4\xF2\x84\x19I\x7F\x97\"\xA3\xDA\xAF\xE3\xB4\x18okdW\xE0\x83\x80\xA3\x80\xF3[c\x11\x8C\xDA\xA7`\xE0\x1B\x81R3`\x04R`$\x90\xFD[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yWa\x0E\x11a\x0E\x0C`\x01`\x01`@\x1B\x03` \x93T`\x88\x1C\x16Ba\x1AkV[a\x1D(V[`\x01`\x01`@\x1B\x03`@Q\x91\x16\x81R\xF3[P4a\x02yW``6`\x03\x19\x01\x12a\x02yW`$5`\x045`D5`\x01`\x01`\xA0\x1B\x03\x81\x16\x80\x82\x03a\x0CIWa\x0EVa\x1B\x9BV[\x82\x15\x80\x15a\x0F\x17W[a\x0F\x03W\x15\x80\x15a\x0E\xFBW[a\x0E\xECW\x81\x84R`\x04` R`@\x84 T\x83\x81\x10a\x0E\xD0W\x83\x83\x92\x81a\x0E\x9F\x93\x88\x96\x87R`\x04` R\x03`@\x86 Ua\x19\xF0V[`@\x80Q3\x81R` \x81\x01\x94\x90\x94R`\xA0\x91\x90\x91\x1B\x920\x91_Q` a.\xA1_9_Q\x90_R\x91\x90\x81\x90\x81\x01a\x08ZV[cY&VQ`\xE0\x1B\x85R`\x04\x83\x90R`$\x84\x90R`DR`d\x84\xFD[cT5\xB2\x89`\xE1\x1B\x84R`\x04\x84\xFD[P\x82\x15a\x0EkV[c\xD51s}`\xE0\x1B\x85R`\x04\x83\x90R`$\x85\xFD[P`\x01`\x01``\x1B\x03\x83\x11a\x0E_V[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yWa\x0F@a\x1B\x9BV[`\n\x80T`\x01`\x01`\xA0\x1B\x03\x19\x90\x81\x16\x90\x91U`\t\x80T\x91\x82\x16\x90U\x81\x90`\x01`\x01`\xA0\x1B\x03\x16\x7F\x8B\xE0\x07\x9CS\x16Y\x14\x13D\xCD\x1F\xD0\xA4\xF2\x84\x19I\x7F\x97\"\xA3\xDA\xAF\xE3\xB4\x18okdW\xE0\x82\x80\xA3\x80\xF3[P4a\x02yW`@6`\x03\x19\x01\x12a\x02yW`\x045`\x01`\x01`\x80\x1B\x03\x81\x16\x90\x81\x81\x03a\rSW`$5\x90`\x01`\x01`@\x1B\x03\x82\x16\x91\x82\x81\x03a\x0CIWa\x0F\xD3a\x1B\x9BV[\x83\x15\x80\x15a\x10UW[a\x0C\x05W\x84Ta\x01\0`\x01`\xC8\x1B\x03\x19\x16`\x08\x92\x90\x92\x1Bp\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\0\x16\x91\x90\x91\x17`\x88\x91\x90\x91\x1Bg\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF`\x88\x1B\x16\x17\x83U`@\x80Q\x92\x83R` \x83\x01\x91\x90\x91R\x7F\x9Ad\x9E\x8A\xC2\xE5\xD0j)z\xD5\xC3\xD5cl.\xC8\0hk\xA2\x17\xEC\x8F\x17\xCB\x11\xFE\xA9h{(\x91\xA1\x80\xF3[P\x82\x15a\x0F\xDCV[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`\xFF` \x91T\x16`@Q\x90\x15\x15\x81R\xF3[P4a\x02yW``6`\x03\x19\x01\x12a\x02yW` \x90a\x08\xB8a\x18QV[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yW`\x01`\x01`@\x1B\x03` \x91T`\x88\x1C\x16`@Q\x90\x81R\xF3[P4a\x02yW`\x04\x90a\x10\xD76a\x18\xC5V[PPc\x9B\xA6\x06\x1B`\xE0\x1B\x81R\xFD[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yW` a\x11\ta\x11\x04a\x18QV[a\x1A\x89V[`\x01`\x01`\x80\x1B\x03`@Q\x91\x16\x81R\xF3[P4a\x02yW``6`\x03\x19\x01\x12a\x02yW`\x045`\x01`\x01`@\x1B\x03\x81\x11a\x08uW\x80`\x04\x01\x90a\x01 `\x03\x19\x826\x03\x01\x12a\rSW`\xE4\x90a\x11\\a\x1C\xA7V[\x01`4a\x11i\x82\x84a\x1C\xF6V[\x90P\x10a\x03\x80Wa\x11z\x90\x82a\x1C\xF6V[\x90\x91\x81`4\x11a\x11\xE6W5\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x82\x03a\x11\xE6W``\x92a\x11\xAF\x92`D5\x92`3\x19\x01\x91`4\x01\x90a\x1D\x9EV[` `@Q\x93\x84\x92`@\x84R\x80Q\x92\x83\x91\x82`@\x87\x01R\x01\x85\x85\x01^\x82\x82\x01\x84\x01\x81\x90R` \x83\x01R`\x1F\x01`\x1F\x19\x16\x81\x01\x03\x01\x90\xF3[\x83\x80\xFD[a\x18\x91V[P4a\x02yW\x80`\x03\x196\x01\x12a\x02yWa\x12\x08a\x1B\x9BV[\x80T`\xFF\x81\x16\x15a\x12FW`\xFF\x19\x16\x81U\x7F]\xB9\xEE\nI[\xF2\xE6\xFF\x9C\x91\xA7\x83L\x1B\xA4\xFD\xD2D\xA5\xE8\xAANS{\xD3\x8A\xEA\xE4\xB0s\xAA` `@Q3\x81R\xA1\x80\xF3[c\x8D\xFC +`\xE0\x1B\x82R`\x04\x82\xFD[P4a\x02yW`@6`\x03\x19\x01\x12a\x02yW`@a\x12qa\x18gV[\x91`\x045\x81R`\x05` R \x90`\x01\x80`\xA0\x1B\x03\x16_R` R` `@_ T`@Q\x90\x81R\xF3[P4a\x02yW`\xA06`\x03\x19\x01\x12a\x02yWa\x12\xB4a\x18QV[a\x12\xBCa\x18gV[`D5\x90c\xFF\xFF\xFF\xFF\x82\x16\x80\x92\x03a\x11\xE6W`d5\x92a\xFF\xFF\x84\x16\x80\x94\x03a\x0CIW`\x845`\x01`\x01`\xA0\x1B\x03\x81\x16\x91\x90\x82\x90\x03a\x14\xF3Wa\x12\xFCa\x1B\x9BV[`\x01`\x01`\xA0\x1B\x03\x16\x93\x84\x15\x80\x15a\x14\xE2W[\x80\x15a\x14\xDAW[\x80\x15a\x14\xD2W[\x80\x15a\x14\xC7W[a\x14\xB8W\x90\x85\x91a\x134\x84a\x1C9V[`@Qc1<\xE5g`\xE0\x1B\x81R\x94` \x86`\x04\x81\x8AZ\xFA\x80\x15a\x14\xADW`\x01`\xA0\x96\x89\x92\x7Fzx\x7F\\\xBD\xA9\xD7pQEA\x1A\xFAsw\xEB\xC9\xDE\xB3md\x0BG\xF5\xF3\x944\xE6$\xA8\x88F\x99\x88\x91a\x14~W[P`@Q\x91a\x13\x8F\x83a\x19dV[\x83\x83R`\xFF` \x84\x01\x92\x16\x82R`@\x83\x01\x91\x86\x83R``\x84\x01\x90\x88\x82Ra\x13\xE6`@`\x80\x87\x01\x94\x88\x8F\x81\x90\x1B\x03\x16\x9C\x8D\x86R\x8E\x88\x01\x99\x8D\x8BR\x81R`\x08` R \x95Q\x15\x15\x86\x90`\xFF\x80\x19\x83T\x16\x91\x15\x15\x16\x17\x90UV[Qg\xFF\xFF\0\0\0\0\0\0e\xFF\xFF\xFF\xFF\0\0\x86T\x95Q`\x10\x1B\x16\x92Q`0\x1B\x16\x92`\x01`@\x1B\x87`\xE0\x1B\x03\x90Q`@\x1B\x16\x93a\xFF\0`\x01`\x01`@\x1B\x03c\xFF\xFF\xFF\xFF`\xE0\x1B\x01\x92`\x08\x1B\x16\x90g\xFF\xFF\xFF\xFF\xFF\xFF\xFF\0\x19\x16\x17\x16\x17\x17\x17\x81U\x01\x90`\x01\x80\x88\x1B\x03\x90Q\x16`\x01`\x01``\x1B\x03\x87\x1B\x82T\x16\x17\x90U`@Q\x93`\x01\x85R` \x85\x01R`@\x84\x01R``\x83\x01R`\x80\x82\x01R\xA2\x80\xF3[a\x14\xA0\x91P` =` \x11a\x14\xA6W[a\x14\x98\x81\x83a\x19\xAEV[\x81\x01\x90a\x1C V[_a\x13\x81V[P=a\x14\x8EV[`@Q=\x86\x82>=\x90\xFD[ci\x18Kw`\xE0\x1B\x86R`\x04\x86\xFD[Pa\x13\x88\x81\x11a\x13$V[P\x83\x15a\x13\x1DV[P\x81\x15a\x13\x16V[P`\x01`\x01`\xA0\x1B\x03\x83\x16\x15a\x13\x0FV[\x85\x80\xFD[P4a\x02yW`@6`\x03\x19\x01\x12a\x02yWa\x15\x11a\x18QV[`$5\x90a\x15\x1Da\x1B\x9BV[`\x01`\x01`\xA0\x1B\x03\x16\x80\x15a\x03\x80W`\x01`\x01``\x1B\x03\x82\x11a\x15}W\x80\x83R`\x06` R`@\x83 T\x90\x80\x84R`\x06` R\x82`@\x85 U\x7F%7t\xF7\xAF\x1E\xA3\xBC\x8C\xF8\xE4\xAD\xB47\xF9\x08\xED\xD9W\xCA\xD3M\x07\x05\xF3#:Y\xEA\x0B\xF1\xE7\x84\x80\xA4\x80\xF3[c\xD51s}`\xE0\x1B\x83R`\x04\x82\x90R`$\x83\xFD[P4a\x02yW`@6`\x03\x19\x01\x12a\x02yWa\x04sa\x15\xAEa\x18QV[a\x15\xB6a\x1B\x9BV[`$5\x90a\x19\xF0V[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yW` \x90`\xFF\x90`@\x90`\x01`\x01`\xA0\x1B\x03a\x15\xEAa\x18QV[\x16\x81R`\x03\x84R T\x16`@Q\x90\x15\x15\x81R\xF3[P4a\x02yW` 6`\x03\x19\x01\x12a\x02yW`@\x90\x81\x90`\x01`\x01`\xA0\x1B\x03a\x16%a\x18QV[\x16\x81R`\x01` R T`\x01`\x01`\x80\x1B\x03\x82Q\x91`\x01`\x01`@\x1B\x03\x81\x16\x83R\x83\x1C\x16` \x82\x01R\xF3[P4a\x02yW``6`\x03\x19\x01\x12a\x02yW`\x045a\x16ma\x18gV[`D5\x91a\x16ya\x1B\x9BV[\x80\x15\x80\x15a\x17\"W[a\x17\x10W`\x01`\x01`\xA0\x1B\x03\x82\x16\x91\x82\x15\x80\x15a\x17\x08W[a\x0C\x05W\x83a\x16\xCF\x91\x83\x87R`\x05` R`@\x87 \x85_R` R`@_ a\x16\xC4\x83\x82Ta\x19\xCFV[\x90U0\x903\x90a\x1B\xC2V[`\xA0\x1B\x17\x90\x82_Q` a.\xA1_9_Q\x90_R`@Q\x80a\x08Z0\x953\x83` \x90\x93\x92\x91\x93`@\x81\x01\x94`\x01\x80`\xA0\x1B\x03\x16\x81R\x01RV[P\x83\x15a\x16\x9AV[c\xD51s}`\xE0\x1B\x84R`\x04R`$\x83\xFD[P`\x01`\x01``\x1B\x03\x81\x11a\x16\x82V[P` 6`\x03\x19\x01\x12a\x06TW`\x045c\xFF\xFF\xFF\xFF\x81\x16\x80\x91\x03a\x06TWa\x17Xa\x1B\x9BV[\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16\x90\x81;\x15a\x06TW_\x90`$`@Q\x80\x94\x81\x93b\x1C\xB6[`\xE5\x1B\x83R`\x04\x83\x01R4\x90Z\xF1\x80\x15a\x17\xC1Wa\x17\xB5WP\x80\xF3[a\0 \x91P_\x90a\x19\xAEV[`@Q=_\x82>=\x90\xFD[4a\x06TW` 6`\x03\x19\x01\x12a\x06TW`\x045c\xFF\xFF\xFF\xFF`\xE0\x1B\x81\x16\x80\x91\x03a\x06TW` \x90c\x0Fc/\xB3`\xE0\x1B\x81\x14\x90\x81\x15a\x18\x11W[P`@Q\x90\x15\x15\x81R\xF3[c\x01\xFF\xC9\xA7`\xE0\x1B\x14\x90P\x82a\x18\x06V[4a\x06TW`@6`\x03\x19\x01\x12a\x06TW` a\x18Ia\x18@a\x18QV[`$5\x90a\x19\x08V[`@Q\x90\x81R\xF3[`\x045\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x82\x03a\x06TWV[`$5\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x82\x03a\x06TWV[5\x90`\x01`\x01`\xA0\x1B\x03\x82\x16\x82\x03a\x06TWV[4a\x06TW``6`\x03\x19\x01\x12a\x06TW`\x045`\x01`\x01`\xA0\x1B\x03\x81\x16\x81\x03a\x06TWPc\x9B\xA6\x06\x1B`\xE0\x1B_R`\x04_\xFD[`@\x90`\x03\x19\x01\x12a\x06TW`\x045`\x01`\x01`\xA0\x1B\x03\x81\x16\x81\x03a\x06TW\x90`$5\x80\x15\x15\x81\x03a\x06TW\x90V[5\x90`\x01`\x01`\x80\x1B\x03\x82\x16\x82\x03a\x06TWV[0`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x03a\x19_W`\xA0\x81\x90\x1C\x90`\x01`\x01`\xA0\x1B\x03\x16\x80a\x19=WP_R`\x04` R`@_ T\x90V[\x90_R`\x05` R`@_ \x90`\x01\x80`\xA0\x1B\x03\x16_R` R`@_ T\x90V[P_\x90V[`\xC0\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x19\x7FW`@RV[cNH{q`\xE0\x1B_R`A`\x04R`$_\xFD[`@\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x19\x7FW`@RV[\x90`\x1F\x80\x19\x91\x01\x16\x81\x01\x90\x81\x10`\x01`\x01`@\x1B\x03\x82\x11\x17a\x19\x7FW`@RV[\x91\x90\x82\x01\x80\x92\x11a\x19\xDCWV[cNH{q`\xE0\x1B_R`\x11`\x04R`$_\xFD[\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16\x91\x82;\x15a\x06TW`@Qc\x04\x0B\x85\x0F`\xE3\x1B\x81R`\x01`\x01`\xA0\x1B\x03\x90\x92\x16`\x04\x83\x01R`$\x82\x01R\x90_\x90\x82\x90`D\x90\x82\x90\x84\x90Z\xF1\x80\x15a\x17\xC1Wa\x1A_WPV[_a\x1Ai\x91a\x19\xAEV[V[\x81\x15a\x1AuW\x04\x90V[cNH{q`\xE0\x1B_R`\x12`\x04R`$_\xFD[`\x01\x80`\xA0\x1B\x03\x16_R`\x01` R`@_ `@Q\x90a\x1A\xA9\x82a\x19\x93V[T`\x01`\x01`@\x1B\x03\x81\x16\x82R`\x01`\x01`\x80\x1B\x03` \x83\x01\x91`@\x1C\x16\x81R_T\x91`\x01`\x01`@\x1B\x03\x80a\x1A\xE7a\x0E\x0C\x82\x87`\x88\x1C\x16Ba\x1AkV[\x92Q\x16\x91\x16\x03a\x1B.W`\x01`\x01`\x80\x1B\x03\x80\x91Q\x16\x91`\x08\x1C\x16\x90\x81\x81\x11\x80\x15a\x1B%W[a\x1B\x1FW`\x01`\x01`\x80\x1B\x03\x91\x03\x16\x90V[PP_\x90V[P\x81\x81\x14a\x1B\rV[P`\x08\x1C`\x01`\x01`\x80\x1B\x03\x16\x90V[\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x16\x80;\x15a\x06TW_`$\x91`@Q\x92\x83\x80\x92c\xB7`\xFA\xF9`\xE0\x1B\x82R0`\x04\x83\x01R4\x90Z\xF1\x80\x15a\x17\xC1Wa\x1A_WPV[`\tT`\x01`\x01`\xA0\x1B\x03\x163\x03a\x1B\xAFWV[c\x11\x8C\xDA\xA7`\xE0\x1B_R3`\x04R`$_\xFD[\x91`@Q\x93``R`@R``\x1B`,Rc#\xB8r\xDD``\x1B`\x0CR` _`d`\x1C\x82\x85Z\xF1\x90\x81`\x01_Q\x14\x16\x15a\x1C\x02W[PP_``R`@RV[;\x15=\x17\x10\x15a\x1C\x13W_\x80a\x1B\xF7V[cy9\xF4$_R`\x04`\x1C\xFD[\x90\x81` \x91\x03\x12a\x06TWQ`\xFF\x81\x16\x81\x03a\x06TW\x90V[`@Qc1<\xE5g`\xE0\x1B\x81R\x90` \x90\x82\x90`\x04\x90\x82\x90`\x01`\x01`\xA0\x1B\x03\x16Z\xFA\x90\x81\x15a\x17\xC1W`\x08\x91`\xFF\x91_\x91a\x1C\x88W[P\x16\x03a\x1CyWV[ci\x18Kw`\xE0\x1B_R`\x04_\xFD[a\x1C\xA1\x91P` =` \x11a\x14\xA6Wa\x14\x98\x81\x83a\x19\xAEV[_a\x1CpV[\x7F\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0`\x01`\x01`\xA0\x1B\x03\x163\x81\x90\x03a\x1C\xDCWPV[c\xFE4\xA6\xD3`\xE0\x1B_R3`\x04R0`$R`DR`d_\xFD[\x905\x90`\x1E\x19\x816\x03\x01\x82\x12\x15a\x06TW\x01\x805\x90`\x01`\x01`@\x1B\x03\x82\x11a\x06TW` \x01\x91\x816\x03\x83\x13a\x06TWV[`\x01`@\x1B\x81\x10\x15a\x1D@W`\x01`\x01`@\x1B\x03\x16\x90V[c5'\x8D\x12_R`\x04`\x1C\xFD[\x91\x90`\x14R`4Rc\xA9\x05\x9C\xBB``\x1B_R` _`D`\x10\x82\x85Z\xF1\x90\x81`\x01_Q\x14\x16\x15a\x1D\x80W[PP_`4RV[;\x15=\x17\x10\x15a\x1D\x91W_\x80a\x1DxV[c\x90\xB8\xEC\x18_R`\x04`\x1C\xFD[\x92``\x92_T\x92`\xFF\x84\x16a$GW`\x01`\x01`\xA0\x1B\x03\x86\x16_\x81\x81R`\x02` R`@\x90 T\x90\x96\x90`\xFF\x16\x15a$4W`\x17_\x80\x83<_Q`\x01`\x01`\xE8\x1B\x03\x19\x16a\xEF\x01`\xF0\x1B\x14a#\xB1W[P\x90a\x1D\xF9\x91a'\xB3V[\x93\x86_\x93\x92\x93R`\x06` R`@_ T\x95\x86\x15a qWPP`\xFF\x16\x15a\x1F\xC6W`\x01\x80`\xA0\x1B\x03\x81\x16\x90\x81_R`\x08` R`@_ \x90`@Qa\x1E>\x81a\x19dV[\x82T\x92`\xFF\x84\x16\x15\x93\x84\x15\x83R`\xFF\x81`\x08\x1C\x16` \x84\x01Rc\xFF\xFF\xFF\xFF\x81`\x10\x1C\x16`@\x84\x01Ra\xFF\xFF\x81`0\x1C\x16``\x84\x01R`\x01\x80`\xA0\x1B\x03\x90`@\x1C\x16`\x80\x83\x01R`\x01\x80\x80`\xA0\x1B\x03\x91\x01T\x16\x92`\xA0\x82\x01\x93\x84Ra\x1F\xB3W\x84a\x1E\xA6\x91a(\x91V[\x91\x90\x95\x80\x87\x11a\x1F\x9DWP\x86_R`\x05` R`@_ \x84_R` R`@_ T\x90\x86\x82\x10a\x1FeWP\x87\x7F\x8D\x065\xEB52K#r\x9A\x0B\x8A\x83\xACX\xCC\xE1\r\xFEE\xF2\xB0\x84I\x07p\xCA\xE0\xB5[\x03\x86`@\x86\x94\x8A\x94\x85_R`\x05` R\x82_ \x87_R` R\x8A\x83_ \x91\x03\x90U\x81Q\x90\x8A\x82R` \x82\x01R\xA4`\x01\x80`\xA0\x1B\x03\x90Q\x16\x93`@Q\x95`\x03` \x88\x01R`@\x87\x01R``\x86\x01R`\x80\x85\x01R`\xA0\x84\x01R`\xC0\x83\x01R`\xE0\x82\x01R`\xE0\x81Ra\x1Fba\x01\0\x82a\x19\xAEV[\x90V[`@Qc4e\xB7a`\xE2\x1B\x81R`\x04\x81\x01\x89\x90R`\x01`\x01`\xA0\x1B\x03\x91\x90\x91\x16`$\x82\x01R`D\x81\x01\x87\x90R`d\x81\x01\x82\x90R`\x84\x90\xFD[\x86c\xFA\xC7Co`\xE0\x1B_R`\x04R`$R`D_\xFD[\x83c\x0CB\x94_`\xE2\x1B_R`\x04R`$_\xFD[P\x91\x80\x91P_R`\x04` R`@_ T\x91\x80\x83\x10a XW\x90\x80\x84\x7F\x91\xBA\x9C%\xEF\xC3\xC1\xAFy\x05\xCD\xB9\x04\x8F\xE8\x8E+}\xAA\xB7\xC2\xD0\xD0\xC3\xF6'w+P\x06\xE0\x04` \x85\x80a \x19`\x01`\x01`\x80\x1B\x03\x99\x98a(#V[\x97\x86_R`\x04\x84R\x03`@_ U`@Q\x90\x81R\xA3`@Q\x93`\x02` \x86\x01R`@\x85\x01R``\x84\x01R\x16`\x80\x82\x01R`\x80\x81Ra\x1Fb`\xA0\x82a\x19\xAEV[\x90cY&VQ`\xE0\x1B_R`\x04R`$R`DR`d_\xFD[\x91`\xFF\x91\x93\x94\x96P\x16\x15a\"CWPP`\x01\x80`\xA0\x1B\x03\x16\x91\x82_R`\x08` R`@_ `@Qa \xA2\x81a\x19dV[\x81T\x91`\xFF\x83\x16\x15\x92\x83\x15\x83R`\xFF\x81`\x08\x1C\x16` \x84\x01Rc\xFF\xFF\xFF\xFF\x81`\x10\x1C\x16`@\x84\x01Ra\xFF\xFF\x81`0\x1C\x16``\x84\x01R`\x01\x80`\xA0\x1B\x03\x90`@\x1C\x16`\x80\x83\x01R`\x01\x80\x80`\xA0\x1B\x03\x91\x01T\x16\x91`\xA0\x82\x01\x92\x83Ra\"0W\x82a!\n\x91a(\x91V[\x84\x82\x95\x92\x11a\"\x19W`@Qcn\xB1v\x9F`\xE1\x1B\x81R`\x04\x81\x01\x88\x90R0`$\x82\x01R` \x81`D\x81\x8AZ\xFA\x90\x81\x15a\x17\xC1W_\x91a!\xE7W[P\x85\x81\x10a!\xBDWP\x7F)l\xAE\xF5\xF4WN\xA0\x1AJ#\xE6\xDD1\\y\x8CLBj\x03J\x07\xD4\x1B\xAB\xF9'\x18\x8B\xCE\xAE`@\x87\x93\x89\x93\x82Q\x91\x82R` \x82\x01R\xA3`\x01\x80`\xA0\x1B\x03\x90Q\x16\x92`@Q\x94`\x01` \x87\x01R`@\x86\x01R``\x85\x01R`\x80\x84\x01R`\xA0\x83\x01R`\xC0\x82\x01R`\xC0\x81Ra\x1Fb`\xE0\x82a\x19\xAEV[\x86`\x84\x91\x87\x8A`@Q\x93c\x86\xB7\xD9\xA9`\xE0\x1B\x85R`\x04\x85\x01R`$\x84\x01R`D\x83\x01R`d\x82\x01R\xFD[\x90P` \x81=` \x11a\"\x11W[\x81a\"\x02` \x93\x83a\x19\xAEV[\x81\x01\x03\x12a\x06TWQ_a!DV[=\x91Pa!\xF5V[P\x83c\xFA\xC7Co`\xE0\x1B_R`\x04R`$R`D_\xFD[\x84c\x0CB\x94_`\xE2\x1B_R`\x04R`$_\xFD[\x91P\x92`\x01`\x01`\x80\x1B\x03\x92Pa\"Y\x90a(#V[\x92\x84_R`\x01` R`@_ \x93`@Q\x94a\"t\x86a\x19\x93V[T`\x01`\x01`@\x1B\x03\x81\x16\x86R\x84` \x87\x01\x91`@\x1C\x16\x81R`\x01`\x01`@\x1B\x03\x80a\"\xA8a\x0E\x0C\x82\x87`\x88\x1C\x16Ba\x1AkV[\x97Q\x16\x96\x16\x95\x86\x14_\x14a#\xA8W\x84\x80\x91Q\x16\x91[\x16\x93\x16\x83\x01\x90`\x01`\x01`\x80\x1B\x03\x82\x11a\x19\xDCW`\x01`\x01`\x80\x1B\x03\x80\x91`\x08\x1C\x16\x91\x16\x90\x80\x82\x11a#\x92WP`@Q\x90a\"\xF7\x82a\x19\x93V[\x84\x82R` \x82\x01\x90\x81R\x85_R`\x01` R`\x01`\x01`@\x1B\x03`@_ \x92Q\x16`\x01`@\x1B`\x01`\xC0\x1B\x03\x83T\x92Q`@\x1B\x16\x91`\x01`\x01`@\x1B\x03`\xC0\x1B\x16\x17\x17\x90U\x83\x7FF\xC1\x0Eg\xD6\xD6\xEF-\xFF\x7F:\x9A|J(\xF4qaq\xD8\x80\x8APc\xE3\xC8\x9C\x12\xA8\x1EG_`@\x80Q\x85\x81R\x86` \x82\x01R\xA2`@Q\x93_` \x86\x01R`@\x85\x01R\x83\x01R`\x80\x82\x01R`\x80\x81Ra\x1Fb`\xA0\x82a\x19\xAEV[\x90c\x0Bckk`\xE1\x1B_R`\x04R`$R`D_\xFD[P\x83_\x91a\"\xBDV[`\x17_\x80\x83<_Q\x90a\x10\xFF`\xF0\x1B`\x01`\x01`\xE8\x1B\x03\x19\x83\x16\x01a$\x08WP`H\x1C`\x01`\x01`\xA0\x1B\x03\x16_\x81\x81R`\x03` R`@\x90 T`\xFF\x16a\x1D\xEEW\x86c\xE9\xF2\xE2\xF3`\xE0\x1B_R`\x04R`$R`D_\xFD[\x87\x90;\x15a$\"Wc\x9FNL\xC9`\xE0\x1B_R`\x04R`$_\xFD[c\xE5\x81\x9B\x95`\xE0\x1B_R`\x04R`$_\xFD[\x86c\xEC\n\xDC3`\xE0\x1B_R`\x04R`$_\xFD[c&4\x1F\xB3`\xE2\x1B_R`\x04_\xFD[5\x90`\xFF\x82\x16\x82\x03a\x06TWV[\x92P\x80`\x1F\x10\x15a'\x9FW\x82`\x1F\x81\x015`\xF8\x1C\x80\x15a'IW`\x01\x81\x14a&\x8EW`\x02\x14a%\x8BW`\xE0\x91\x81\x01\x03\x12a\x06TWa$\xA1\x82a$VV[Pa$\xAE` \x83\x01a\x18}V[`@\x83\x015\x92a$\xC0``\x82\x01a\x18}V[\x93`\xA0\x82\x015\x93a$\xEC`\x80a$\xD8`\xC0\x86\x01a\x18}V[\x97`\x01`\x01`\xA0\x1B\x03\x16\x94\x015\x82\x87a.\x03V[\x93\x85\x85\x11a%\x83W[`@\x7F\xEE\x8C\xF4\x1D\xF06\x87V`\xDE\x15\xDB\xD2\x7FZp\xC4]\x88\x9F.\x18#m\xA0\n\xE9\xD7$\x86\x9E\xB9\x91\x87\x87\x98\x87\x98\x10a%YW[P\x81Q\x93\x84R` \x84\x01\x88\x90R`\x01`\x01`\xA0\x1B\x03\x16\x92\xA4\x81a%FWPPPV[a\x1Ai\x92`\x01`\x01`\xA0\x1B\x03\x16\x90a\x1DMV[\x85_R`\x05` R\x82_ `\x01\x80`\xA0\x1B\x03\x88\x16_R` R\x88\x83_ \x91\x03\x81T\x01\x90U_a%$V[\x85\x94Pa$\xF5V[`\x80\x91\x81\x01\x03\x12a\x06TWa%\x9F\x82a$VV[P\x7F\xAE\xFB|\xEF`\x8BS\x81\xB2\xC1\xE0cw\xECj\xC2\xEE]\x19II\x95\xD9\xFA\x0FM\x9F\xA3u\xB5X\xEDa%\xCD` \x84\x01a\x18}V[a%\xDE```@\x86\x015\x95\x01a\x18\xF4V[`\x01`\x01`\xA0\x1B\x03\x90\x91\x16\x92a%\xF3\x81a(#V[\x91`\x01`\x01`\x80\x1B\x03\x81\x16`\x01`\x01`\x80\x1B\x03\x84\x16\x90\x80\x82\x11\x91\x82\x15a&\x84W[PPa&aW\x85_R`\x04` R`\x01`\x01`\x80\x1B\x03\x83`@_ \x92\x03\x16\x81T\x01\x90Ua&\\`@Q\x92\x83\x92\x83\x90\x92\x91`\x01`\x01`\x80\x1B\x03` \x91`@\x84\x01\x95\x84R\x16\x91\x01RV[\x03\x90\xA3V[`@\x80Q\x92\x83R`\x01`\x01`\x80\x1B\x03\x90\x91\x16` \x83\x01R\x90\x91P\x81\x90\x81\x01a&\\V[\x14\x90P_\x80a&\x14V[P`\xC0\x91\x81\x01\x03\x12a\x06TWa&\xA3\x82a$VV[Pa'\x14`@a&\xB5` \x85\x01a\x18}V[\x93a&\xC1\x82\x82\x01a\x18}V[\x7F\x8E\xF4\xAD\x95\x02?\xFB\xD5A\x8B\xD9\xD8\xDB,g\xF3\xE2F\x9A\xC6\x18\x9AP\xD9\xC0\xE3\xCB\x9B7\xDA[ia&\xEE`\xA0\x84\x01a\x18}V[\x96`\x01`\x01`\xA0\x1B\x03\x92\x83\x16\x95\x92\x16\x93\x85\x93\x85\x93\x90``\x81\x015\x90\x89\x90`\x80\x015a.\x03V[\x96\x81Q\x90\x81R\x87` \x82\x01R\xA3\x82a'-W[PPPPV[a'@\x93`\x01`\x01`\xA0\x1B\x03\x16\x91a\x1B\xC2V[_\x80\x80\x80a''V[P`\x80\x91\x81\x01\x03\x12a\x06TWa'^\x82a$VV[Pa'k` \x83\x01a\x18}V[\x91``a'z`@\x83\x01a\x18\xF4V[\x91\x015`\x01`\x01`@\x1B\x03\x81\x16\x81\x03a\x06TWa\x1Ai\x93`\x01`\x01`\xA0\x1B\x03\x16a,\x94V[cNH{q`\xE0\x1B_R`2`\x04R`$_\xFD[\x91\x81\x15a(\x19W\x825`\xF8\x1C\x92\x83\x15a(\x07W`\x01`\xFF\x85\x16\x03a'\xF8W`5\x83\x03a'\xF8W\x82`\x15\x11a\x06TW`\x01\x81\x015``\x1C\x92`5\x11a\x06TW`\x15\x015\x90V[c\"\"CO`\xE1\x1B_R`\x04_\xFD[P\x91P`\x01\x03a'\xF8W_\x90_\x90_\x90V[_\x92P\x82\x91P\x81\x90V[`\x01`\x80\x1B\x81\x10\x15a\x1D@W`\x01`\x01`\x80\x1B\x03\x16\x90V[Q\x90i\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\x82\x16\x82\x03a\x06TWV[\x90\x81`\xA0\x91\x03\x12a\x06TWa(f\x81a(;V[\x91` \x82\x01Q\x91`@\x81\x01Q\x91a\x1Fb`\x80``\x84\x01Q\x93\x01a(;V[\x91\x90\x82\x03\x91\x82\x11a\x19\xDCWV[`\x07T`\x01`\x01`\xA0\x1B\x03\x16\x92\x91\x83\x15a,<W`@Qc?\xAB\xE5\xA3`\xE2\x1B\x81R`\xA0\x81`\x04\x81\x88Z\xFA\x90\x81\x15a\x17\xC1W__\x92__\x93_\x92a,\x12W[P_\x85\x12\x80\x15a,\nW[a+\xF3W\x15\x91\x82\x15a+\xD9W[PPa+\xB3Wa(\xF7\x81Ba(\x84V[\x90`@\x85\x01\x96c\xFF\xFF\xFF\xFF\x88Q\x16\x80\x93\x11a+\x9BWPPP`\x80\x83\x01\x80Q`@Qc?\xAB\xE5\xA3`\xE2\x1B\x81R\x91\x93\x91\x90`\xA0\x90\x82\x90`\x04\x90\x82\x90`\x01`\x01`\xA0\x1B\x03\x16Z\xFA\x93\x84\x15a\x17\xC1W__\x95__\x94_\x92a+^W[P_\x88\x12\x80\x15a+VW[a+0W\x15\x91\x82\x15a+\x16W[PPa*\xE8Wc\xFF\xFF\xFF\xFFa)|\x83Ba(\x84V[\x98Q\x16\x80\x98\x11a*\xC3WPPa)\x94\x93\x94\x95Pa.\x03V[a\xFF\xFF``\x83\x01Q\x16a'\x10\x01\x90\x81a'\x10\x11a\x19\xDCW\x81\x81\x02\x91a'\x10\x82\x15\x82\x84\x86\x04\x14\x17\x02\x15a*qWPPa'\x10\x90\x04[`\xFF` \x82\x93\x01Q\x16`M\x81\x11a\x19\xDCW`\n\n\x90\x81\x81\x02\x91g\r\xE0\xB6\xB3\xA7d\0\0\x82\x15\x82\x84\x86\x04\x14\x17\x02\x15a*\x08WPPg\r\xE0\xB6\xB3\xA7d\0\0\x90\x04\x91V[g\r\xE0\xB6\xB3\xA7d\0\0\x90_\x19\x81\x84\t\x84\x81\x10\x85\x01\x90\x03\x92\t\x90\x80g\r\xE0\xB6\xB3\xA7d\0\0\x11\x15a*dW\x82\x82\x11\x90\x03`\xEE\x1B\x91\x03`\x12\x1C\x17\x7F\xAC\xCB\x18\x16[\xD6\xFE1\xAE\x1C\xF3\x18\xDC[Q\xEE\xE0\xE1\xBAV\x9B\x88\xCDt\xC1w;\x91\xFA\xC1\x06i\x02\x91V[c\xAEG\xF7\x02_R`\x04`\x1C\xFD[a'\x10\x90_\x19\x81\x84\t\x84\x81\x10\x85\x01\x90\x03\x92\t\x90\x80a'\x10\x11\x15a*dW\x82\x82\x11\x90\x03`\xFC\x1B\x91\x03`\x04\x1C\x17\x7F\xBC\x01\xA3n.\xB1\xC42\xCAW\xA7\x86\xC2&\x80\x9DIQ\x82\xA9\x93\x0B\xE0\xDE\xD2\x88\xCEp:\xFB~\x91\x02a)\xC8V[\x87\x92P`\x01\x80`\xA0\x1B\x03\x90Q\x16cz\xF2'O`\xE0\x1B_R`\x04R`$R`DR`d_\xFD[\x90c\xFF\xFF\xFF\xFF\x88\x92`\x01\x80`\xA0\x1B\x03\x90Q\x16\x92Q\x16\x91cz\xF2'O`\xE0\x1B_R`\x04R`$R`DR`d_\xFD[i\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\x91\x92P\x81\x16\x91\x16\x10_\x80a)gV[\x83Qcc\x19\xD6\xAB`\xE0\x1B_\x90\x81R`\x01`\x01`\xA0\x1B\x03\x90\x91\x16`\x04R`$\x89\x90R`D\x90\xFD[P\x87\x15a)ZV[\x93\x97PPPPa+\x86\x91P`\xA0=`\xA0\x11a+\x94W[a+~\x81\x83a\x19\xAEV[\x81\x01\x90a(RV[\x92\x96\x90\x93\x90\x92\x90\x91_a)OV[P=a+tV[cz\xF2'O`\xE0\x1B_R`\x04R`$R`DR`d_\xFD[\x85\x90c\xFF\xFF\xFF\xFF`@\x86\x01Q\x16\x91cz\xF2'O`\xE0\x1B_R`\x04R`$R`DR`d_\xFD[i\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\x91\x92P\x81\x16\x91\x16\x10_\x80a(\xE7V[\x84\x89cc\x19\xD6\xAB`\xE0\x1B_R`\x04R`$R`D_\xFD[P\x84\x15a(\xDAV[\x93PPPPa,0\x91P`\xA0=`\xA0\x11a+\x94Wa+~\x81\x83a\x19\xAEV[\x92\x93\x90\x92\x90\x91_a(\xCFV[c\x1F\x94\xD8\xA3`\xE0\x1B_R`\x04_\xFD[\x90\x81R`\x01`\x01`@\x1B\x03\x90\x91\x16` \x82\x01R`\x01`\x01`\x80\x1B\x03\x90\x91\x16`@\x82\x01R``\x01\x90V[\x90`\x01`\x01`\x80\x1B\x03\x80\x91\x16\x91\x16\x03\x90`\x01`\x01`\x80\x1B\x03\x82\x11a\x19\xDCWV[`\x01`\x01`\xA0\x1B\x03\x16_\x81\x81R`\x01` R`@\x90\x81\x90 \x90Q\x91\x94\x7F\xEBC)\x0E\xFC\x1C\x94Bq3G\xE1\xD5A\xC9\xE2M5Z0\xF1\x19Q\x080!EY\xCC\xD7.\x8F\x94\x90\x92\x91a,\xDE\x83a\x19\x93V[T`\x01`\x01`\x80\x1B\x03`\x01`\x01`@\x1B\x03\x82\x16\x91\x82\x85R`@\x1C\x16`\x01`\x01`@\x1B\x03` \x85\x01\x96\x82\x88R\x16\x82\x03a-\xEEWPPa-\x1B\x83a(#V[`\x01`\x01`\x80\x1B\x03\x82\x16`\x01`\x01`\x80\x1B\x03\x82\x16\x90\x80\x82\x11\x91\x82\x15a-\xE4W[PPa-\xBDW\x91`\x01`\x01`\x80\x1B\x03a-h\x86\x95\x93a-ba-\xB8\x96\x84\x80\x9AQ\x16\x92a,tV[\x90a,tV[\x16\x84R\x86_R`\x01` R`\x01`\x01`@\x1B\x03`@_ \x91Q\x16\x90\x80T\x94Q\x94\x82`\x01`@\x1B`\x01`\xC0\x1B\x03\x87`@\x1B\x16\x91`\x01`\x01`@\x1B\x03`\xC0\x1B\x16\x17\x17\x90U`@Q\x94\x85\x94\x16\x91\x84a,KV[\x03\x90\xA2V[PP`\x01`\x01`\x80\x1B\x03`\x01`\x01`@\x1B\x03a-\xB8\x92Q\x16\x93Q\x16`@Q\x93\x84\x93\x84a,KV[\x14\x90P_\x80a-;V[\x91P\x93Pa-\xB8\x91P`@Q\x93\x84\x93\x84a,KV[\x91\x90\x91\x81\x15a.\x91W\x82\x81\x02\x92\x82\x82\x15\x82\x84\x87\x04\x14\x17\x02\x15a.&WPP\x90\x04\x90V[\x82\x90_\x19\x81\x84\t\x85\x81\x10\x86\x01\x90\x03\x92\t\x90\x82_\x03\x83\x16\x92\x81\x81\x11\x15a*dW\x83\x90\x04\x80`\x03\x02`\x02\x18\x80\x82\x02`\x02\x03\x02\x80\x82\x02`\x02\x03\x02\x80\x82\x02`\x02\x03\x02\x80\x82\x02`\x02\x03\x02\x80\x82\x02`\x02\x03\x02\x80\x91\x02`\x02\x03\x02\x93`\x01\x84\x84\x83\x03\x04\x94\x80_\x03\x04\x01\x92\x11\x90\x03\x02\x17\x02\x90V[c#\xD3Y\xA3`\xE0\x1B_R`\x04_\xFD\xFE\x1B=~\xDB.\x9C\x0B\x0E|R[ \xAA\xAE\xF0\xF5\x94\r.\xD7\x16c\xC7\xD3\x92f\xEC\xAF\xACr\x88Y\xA1dsolcC\0\x08\"\0\n",
    );
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
    /**Custom error with signature `ERC165Error(address,bytes4)` and selector `0x65d25c71`.
```solidity
error ERC165Error(address entryPoint, bytes4 interfaceId);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ERC165Error {
        #[allow(missing_docs)]
        pub entryPoint: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub interfaceId: alloy::sol_types::private::FixedBytes<4>,
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
            alloy::sol_types::sol_data::FixedBytes<4>,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
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
        impl ::core::convert::From<ERC165Error> for UnderlyingRustTuple<'_> {
            fn from(value: ERC165Error) -> Self {
                (value.entryPoint, value.interfaceId)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for ERC165Error {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    entryPoint: tuple.0,
                    interfaceId: tuple.1,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for ERC165Error {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "ERC165Error(address,bytes4)";
            const SELECTOR: [u8; 4] = [101u8, 210u8, 92u8, 113u8];
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
                        &self.entryPoint,
                    ),
                    <alloy::sol_types::sol_data::FixedBytes<
                        4,
                    > as alloy_sol_types::SolType>::tokenize(&self.interfaceId),
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
    /**Custom error with signature `Eip7702SenderNotDelegate(address)` and selector `0x9f4e4cc9`.
```solidity
error Eip7702SenderNotDelegate(address sender);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Eip7702SenderNotDelegate {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
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
        impl ::core::convert::From<Eip7702SenderNotDelegate>
        for UnderlyingRustTuple<'_> {
            fn from(value: Eip7702SenderNotDelegate) -> Self {
                (value.sender,)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for Eip7702SenderNotDelegate {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self { sender: tuple.0 }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for Eip7702SenderNotDelegate {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "Eip7702SenderNotDelegate(address)";
            const SELECTOR: [u8; 4] = [159u8, 78u8, 76u8, 201u8];
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
                        &self.sender,
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
    /**Custom error with signature `Eip7702SenderWithoutCode(address)` and selector `0xe5819b95`.
```solidity
error Eip7702SenderWithoutCode(address sender);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Eip7702SenderWithoutCode {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
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
        impl ::core::convert::From<Eip7702SenderWithoutCode>
        for UnderlyingRustTuple<'_> {
            fn from(value: Eip7702SenderWithoutCode) -> Self {
                (value.sender,)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for Eip7702SenderWithoutCode {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self { sender: tuple.0 }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for Eip7702SenderWithoutCode {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "Eip7702SenderWithoutCode(address)";
            const SELECTOR: [u8; 4] = [229u8, 129u8, 155u8, 149u8];
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
                        &self.sender,
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
    /**Custom error with signature `EpochBudgetExceeded(uint128,uint128)` and selector `0x16c6d6d6`.
```solidity
error EpochBudgetExceeded(uint128 spent, uint128 cap);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct EpochBudgetExceeded {
        #[allow(missing_docs)]
        pub spent: u128,
        #[allow(missing_docs)]
        pub cap: u128,
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
            alloy::sol_types::sol_data::Uint<128>,
            alloy::sol_types::sol_data::Uint<128>,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (u128, u128);
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
        impl ::core::convert::From<EpochBudgetExceeded> for UnderlyingRustTuple<'_> {
            fn from(value: EpochBudgetExceeded) -> Self {
                (value.spent, value.cap)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for EpochBudgetExceeded {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    spent: tuple.0,
                    cap: tuple.1,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for EpochBudgetExceeded {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "EpochBudgetExceeded(uint128,uint128)";
            const SELECTOR: [u8; 4] = [22u8, 198u8, 214u8, 214u8];
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
                        128,
                    > as alloy_sol_types::SolType>::tokenize(&self.spent),
                    <alloy::sol_types::sol_data::Uint<
                        128,
                    > as alloy_sol_types::SolType>::tokenize(&self.cap),
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
    /**Custom error with signature `Erc20InsufficientAllowance(address,address,uint256,uint256)` and selector `0x86b7d9a9`.
```solidity
error Erc20InsufficientAllowance(address token, address sender, uint256 requiredAmount, uint256 allowance);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Erc20InsufficientAllowance {
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub requiredAmount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub allowance: alloy::sol_types::private::primitives::aliases::U256,
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
        impl ::core::convert::From<Erc20InsufficientAllowance>
        for UnderlyingRustTuple<'_> {
            fn from(value: Erc20InsufficientAllowance) -> Self {
                (value.token, value.sender, value.requiredAmount, value.allowance)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for Erc20InsufficientAllowance {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    token: tuple.0,
                    sender: tuple.1,
                    requiredAmount: tuple.2,
                    allowance: tuple.3,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for Erc20InsufficientAllowance {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "Erc20InsufficientAllowance(address,address,uint256,uint256)";
            const SELECTOR: [u8; 4] = [134u8, 183u8, 217u8, 169u8];
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
                        &self.sender,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.requiredAmount),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.allowance),
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
    /**Custom error with signature `Erc20InvalidConfig()` and selector `0x69184b77`.
```solidity
error Erc20InvalidConfig();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Erc20InvalidConfig;
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
        impl ::core::convert::From<Erc20InvalidConfig> for UnderlyingRustTuple<'_> {
            fn from(value: Erc20InvalidConfig) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for Erc20InvalidConfig {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for Erc20InvalidConfig {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "Erc20InvalidConfig()";
            const SELECTOR: [u8; 4] = [105u8, 24u8, 75u8, 119u8];
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
    /**Custom error with signature `Erc20MaxAmountExceeded(uint256,uint256)` and selector `0xfac7436f`.
```solidity
error Erc20MaxAmountExceeded(uint256 requiredAmount, uint256 maxAmount);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Erc20MaxAmountExceeded {
        #[allow(missing_docs)]
        pub requiredAmount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub maxAmount: alloy::sol_types::private::primitives::aliases::U256,
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
        impl ::core::convert::From<Erc20MaxAmountExceeded> for UnderlyingRustTuple<'_> {
            fn from(value: Erc20MaxAmountExceeded) -> Self {
                (value.requiredAmount, value.maxAmount)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for Erc20MaxAmountExceeded {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    requiredAmount: tuple.0,
                    maxAmount: tuple.1,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for Erc20MaxAmountExceeded {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "Erc20MaxAmountExceeded(uint256,uint256)";
            const SELECTOR: [u8; 4] = [250u8, 199u8, 67u8, 111u8];
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
                    > as alloy_sol_types::SolType>::tokenize(&self.requiredAmount),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.maxAmount),
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
    /**Custom error with signature `Erc20OracleNotSet()` and selector `0x1f94d8a3`.
```solidity
error Erc20OracleNotSet();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Erc20OracleNotSet;
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
        impl ::core::convert::From<Erc20OracleNotSet> for UnderlyingRustTuple<'_> {
            fn from(value: Erc20OracleNotSet) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for Erc20OracleNotSet {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for Erc20OracleNotSet {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "Erc20OracleNotSet()";
            const SELECTOR: [u8; 4] = [31u8, 148u8, 216u8, 163u8];
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
    /**Custom error with signature `Erc20PaymasterDataInvalid()` and selector `0x4444869e`.
```solidity
error Erc20PaymasterDataInvalid();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Erc20PaymasterDataInvalid;
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
        impl ::core::convert::From<Erc20PaymasterDataInvalid>
        for UnderlyingRustTuple<'_> {
            fn from(value: Erc20PaymasterDataInvalid) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for Erc20PaymasterDataInvalid {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for Erc20PaymasterDataInvalid {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "Erc20PaymasterDataInvalid()";
            const SELECTOR: [u8; 4] = [68u8, 68u8, 134u8, 158u8];
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
    /**Custom error with signature `Erc20PriceInvalid(address,int256)` and selector `0x6319d6ab`.
```solidity
error Erc20PriceInvalid(address oracle, int256 answer);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Erc20PriceInvalid {
        #[allow(missing_docs)]
        pub oracle: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub answer: alloy::sol_types::private::primitives::aliases::I256,
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
            alloy::sol_types::sol_data::Int<256>,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
            alloy::sol_types::private::Address,
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
        impl ::core::convert::From<Erc20PriceInvalid> for UnderlyingRustTuple<'_> {
            fn from(value: Erc20PriceInvalid) -> Self {
                (value.oracle, value.answer)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for Erc20PriceInvalid {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    oracle: tuple.0,
                    answer: tuple.1,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for Erc20PriceInvalid {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "Erc20PriceInvalid(address,int256)";
            const SELECTOR: [u8; 4] = [99u8, 25u8, 214u8, 171u8];
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
                        &self.oracle,
                    ),
                    <alloy::sol_types::sol_data::Int<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.answer),
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
    /**Custom error with signature `Erc20PriceStale(address,uint256,uint256)` and selector `0x7af2274f`.
```solidity
error Erc20PriceStale(address oracle, uint256 updatedAt, uint256 maxStaleness);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Erc20PriceStale {
        #[allow(missing_docs)]
        pub oracle: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub updatedAt: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub maxStaleness: alloy::sol_types::private::primitives::aliases::U256,
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
            alloy::sol_types::sol_data::Uint<256>,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
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
        impl ::core::convert::From<Erc20PriceStale> for UnderlyingRustTuple<'_> {
            fn from(value: Erc20PriceStale) -> Self {
                (value.oracle, value.updatedAt, value.maxStaleness)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for Erc20PriceStale {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    oracle: tuple.0,
                    updatedAt: tuple.1,
                    maxStaleness: tuple.2,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for Erc20PriceStale {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "Erc20PriceStale(address,uint256,uint256)";
            const SELECTOR: [u8; 4] = [122u8, 242u8, 39u8, 79u8];
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
                        &self.oracle,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.updatedAt),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.maxStaleness),
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
    /**Custom error with signature `Erc20TokenNotEnabled(address)` and selector `0x310a517c`.
```solidity
error Erc20TokenNotEnabled(address token);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct Erc20TokenNotEnabled {
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
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
        impl ::core::convert::From<Erc20TokenNotEnabled> for UnderlyingRustTuple<'_> {
            fn from(value: Erc20TokenNotEnabled) -> Self {
                (value.token,)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for Erc20TokenNotEnabled {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self { token: tuple.0 }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for Erc20TokenNotEnabled {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "Erc20TokenNotEnabled(address)";
            const SELECTOR: [u8; 4] = [49u8, 10u8, 81u8, 124u8];
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
    /**Custom error with signature `InvalidPoolId(uint256)` and selector `0xd531737d`.
```solidity
error InvalidPoolId(uint256 poolId);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct InvalidPoolId {
        #[allow(missing_docs)]
        pub poolId: alloy::sol_types::private::primitives::aliases::U256,
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
        impl ::core::convert::From<InvalidPoolId> for UnderlyingRustTuple<'_> {
            fn from(value: InvalidPoolId) -> Self {
                (value.poolId,)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for InvalidPoolId {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self { poolId: tuple.0 }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for InvalidPoolId {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "InvalidPoolId(uint256)";
            const SELECTOR: [u8; 4] = [213u8, 49u8, 115u8, 125u8];
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
                    > as alloy_sol_types::SolType>::tokenize(&self.poolId),
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
    /**Custom error with signature `MevPaymaster__SenderNotSponsored(address)` and selector `0xec0adc33`.
```solidity
error MevPaymaster__SenderNotSponsored(address sender);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MevPaymaster__SenderNotSponsored {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
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
        impl ::core::convert::From<MevPaymaster__SenderNotSponsored>
        for UnderlyingRustTuple<'_> {
            fn from(value: MevPaymaster__SenderNotSponsored) -> Self {
                (value.sender,)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for MevPaymaster__SenderNotSponsored {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self { sender: tuple.0 }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for MevPaymaster__SenderNotSponsored {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "MevPaymaster__SenderNotSponsored(address)";
            const SELECTOR: [u8; 4] = [236u8, 10u8, 220u8, 51u8];
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
                        &self.sender,
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
    /**Custom error with signature `MevPaymaster__UnexpectedEntryPoint(address,address)` and selector `0xb18916d8`.
```solidity
error MevPaymaster__UnexpectedEntryPoint(address actual, address expected);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MevPaymaster__UnexpectedEntryPoint {
        #[allow(missing_docs)]
        pub actual: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub expected: alloy::sol_types::private::Address,
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
        impl ::core::convert::From<MevPaymaster__UnexpectedEntryPoint>
        for UnderlyingRustTuple<'_> {
            fn from(value: MevPaymaster__UnexpectedEntryPoint) -> Self {
                (value.actual, value.expected)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for MevPaymaster__UnexpectedEntryPoint {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    actual: tuple.0,
                    expected: tuple.1,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for MevPaymaster__UnexpectedEntryPoint {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "MevPaymaster__UnexpectedEntryPoint(address,address)";
            const SELECTOR: [u8; 4] = [177u8, 137u8, 22u8, 216u8];
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
                        &self.actual,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.expected,
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
    /**Custom error with signature `MevPaymaster__UntrustedDelegate(address,address)` and selector `0xe9f2e2f3`.
```solidity
error MevPaymaster__UntrustedDelegate(address sender, address delegate);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MevPaymaster__UntrustedDelegate {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub delegate: alloy::sol_types::private::Address,
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
        impl ::core::convert::From<MevPaymaster__UntrustedDelegate>
        for UnderlyingRustTuple<'_> {
            fn from(value: MevPaymaster__UntrustedDelegate) -> Self {
                (value.sender, value.delegate)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for MevPaymaster__UntrustedDelegate {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    sender: tuple.0,
                    delegate: tuple.1,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for MevPaymaster__UntrustedDelegate {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "MevPaymaster__UntrustedDelegate(address,address)";
            const SELECTOR: [u8; 4] = [233u8, 242u8, 226u8, 243u8];
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
                        &self.sender,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.delegate,
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
    /**Custom error with signature `MustOverride()` and selector `0x25ad501f`.
```solidity
error MustOverride();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct MustOverride;
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
        impl ::core::convert::From<MustOverride> for UnderlyingRustTuple<'_> {
            fn from(value: MustOverride) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for MustOverride {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for MustOverride {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "MustOverride()";
            const SELECTOR: [u8; 4] = [37u8, 173u8, 80u8, 31u8];
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
    /**Custom error with signature `NativeBalanceWithdrawFailed()` and selector `0x018f95f5`.
```solidity
error NativeBalanceWithdrawFailed();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct NativeBalanceWithdrawFailed;
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
        impl ::core::convert::From<NativeBalanceWithdrawFailed>
        for UnderlyingRustTuple<'_> {
            fn from(value: NativeBalanceWithdrawFailed) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for NativeBalanceWithdrawFailed {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for NativeBalanceWithdrawFailed {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "NativeBalanceWithdrawFailed()";
            const SELECTOR: [u8; 4] = [1u8, 143u8, 149u8, 245u8];
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
    /**Custom error with signature `PaymasterPaused()` and selector `0x98d07ecc`.
```solidity
error PaymasterPaused();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct PaymasterPaused;
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
        impl ::core::convert::From<PaymasterPaused> for UnderlyingRustTuple<'_> {
            fn from(value: PaymasterPaused) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for PaymasterPaused {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for PaymasterPaused {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "PaymasterPaused()";
            const SELECTOR: [u8; 4] = [152u8, 208u8, 126u8, 204u8];
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
    /**Custom error with signature `PoolEthBalanceInsufficient(uint256,uint256,uint256)` and selector `0x59265651`.
```solidity
error PoolEthBalanceInsufficient(uint256 poolId, uint256 requested, uint256 available);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct PoolEthBalanceInsufficient {
        #[allow(missing_docs)]
        pub poolId: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub requested: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub available: alloy::sol_types::private::primitives::aliases::U256,
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
            alloy::sol_types::sol_data::Uint<256>,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
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
        impl ::core::convert::From<PoolEthBalanceInsufficient>
        for UnderlyingRustTuple<'_> {
            fn from(value: PoolEthBalanceInsufficient) -> Self {
                (value.poolId, value.requested, value.available)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for PoolEthBalanceInsufficient {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    poolId: tuple.0,
                    requested: tuple.1,
                    available: tuple.2,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for PoolEthBalanceInsufficient {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "PoolEthBalanceInsufficient(uint256,uint256,uint256)";
            const SELECTOR: [u8; 4] = [89u8, 38u8, 86u8, 81u8];
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
                    > as alloy_sol_types::SolType>::tokenize(&self.poolId),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.requested),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.available),
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
    /**Custom error with signature `PoolTokenBalanceInsufficient(uint256,address,uint256,uint256)` and selector `0xd196dd84`.
```solidity
error PoolTokenBalanceInsufficient(uint256 poolId, address token, uint256 requested, uint256 available);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct PoolTokenBalanceInsufficient {
        #[allow(missing_docs)]
        pub poolId: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub requested: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub available: alloy::sol_types::private::primitives::aliases::U256,
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
            alloy::sol_types::sol_data::Address,
            alloy::sol_types::sol_data::Uint<256>,
            alloy::sol_types::sol_data::Uint<256>,
        );
        #[doc(hidden)]
        type UnderlyingRustTuple<'a> = (
            alloy::sol_types::private::primitives::aliases::U256,
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
        impl ::core::convert::From<PoolTokenBalanceInsufficient>
        for UnderlyingRustTuple<'_> {
            fn from(value: PoolTokenBalanceInsufficient) -> Self {
                (value.poolId, value.token, value.requested, value.available)
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>>
        for PoolTokenBalanceInsufficient {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self {
                    poolId: tuple.0,
                    token: tuple.1,
                    requested: tuple.2,
                    available: tuple.3,
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for PoolTokenBalanceInsufficient {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "PoolTokenBalanceInsufficient(uint256,address,uint256,uint256)";
            const SELECTOR: [u8; 4] = [209u8, 150u8, 221u8, 132u8];
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
                    > as alloy_sol_types::SolType>::tokenize(&self.poolId),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.token,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.requested),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.available),
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
    /**Custom error with signature `UnsupportedOperation()` and selector `0x9ba6061b`.
```solidity
error UnsupportedOperation();
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct UnsupportedOperation;
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
        impl ::core::convert::From<UnsupportedOperation> for UnderlyingRustTuple<'_> {
            fn from(value: UnsupportedOperation) -> Self {
                ()
            }
        }
        #[automatically_derived]
        #[doc(hidden)]
        impl ::core::convert::From<UnderlyingRustTuple<'_>> for UnsupportedOperation {
            fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                Self
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolError for UnsupportedOperation {
            type Parameters<'a> = UnderlyingSolTuple<'a>;
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "UnsupportedOperation()";
            const SELECTOR: [u8; 4] = [155u8, 166u8, 6u8, 27u8];
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
    /**Event with signature `Erc20ConfigChanged(address,bool,address,uint32,uint16,address)` and selector `0x7a787f5cbda9d7705145411afa7377ebc9deb36d640b47f5f39434e624a88846`.
```solidity
event Erc20ConfigChanged(address indexed token, bool enabled, address tokenOracle, uint32 maxStaleness, uint16 markupBps, address treasury);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct Erc20ConfigChanged {
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub enabled: bool,
        #[allow(missing_docs)]
        pub tokenOracle: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub maxStaleness: u32,
        #[allow(missing_docs)]
        pub markupBps: u16,
        #[allow(missing_docs)]
        pub treasury: alloy::sol_types::private::Address,
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
        impl alloy_sol_types::SolEvent for Erc20ConfigChanged {
            type DataTuple<'a> = (
                alloy::sol_types::sol_data::Bool,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<32>,
                alloy::sol_types::sol_data::Uint<16>,
                alloy::sol_types::sol_data::Address,
            );
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "Erc20ConfigChanged(address,bool,address,uint32,uint16,address)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                122u8, 120u8, 127u8, 92u8, 189u8, 169u8, 215u8, 112u8, 81u8, 69u8, 65u8,
                26u8, 250u8, 115u8, 119u8, 235u8, 201u8, 222u8, 179u8, 109u8, 100u8,
                11u8, 71u8, 245u8, 243u8, 148u8, 52u8, 230u8, 36u8, 168u8, 136u8, 70u8,
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
                    enabled: data.0,
                    tokenOracle: data.1,
                    maxStaleness: data.2,
                    markupBps: data.3,
                    treasury: data.4,
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
                        &self.enabled,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.tokenOracle,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.maxStaleness),
                    <alloy::sol_types::sol_data::Uint<
                        16,
                    > as alloy_sol_types::SolType>::tokenize(&self.markupBps),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.treasury,
                    ),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(), self.token.clone())
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
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for Erc20ConfigChanged {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&Erc20ConfigChanged> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &Erc20ConfigChanged) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `Erc20Settled(address,address,uint256,uint256)` and selector `0x8ef4ad95023ffbd5418bd9d8db2c67f3e2469ac6189a50d9c0e3cb9b37da5b69`.
```solidity
event Erc20Settled(address indexed sender, address indexed token, uint256 actualGasCost, uint256 actualTokenCharge);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct Erc20Settled {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub actualGasCost: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub actualTokenCharge: alloy::sol_types::private::primitives::aliases::U256,
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
        impl alloy_sol_types::SolEvent for Erc20Settled {
            type DataTuple<'a> = (
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "Erc20Settled(address,address,uint256,uint256)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                142u8, 244u8, 173u8, 149u8, 2u8, 63u8, 251u8, 213u8, 65u8, 139u8, 217u8,
                216u8, 219u8, 44u8, 103u8, 243u8, 226u8, 70u8, 154u8, 198u8, 24u8, 154u8,
                80u8, 217u8, 192u8, 227u8, 203u8, 155u8, 55u8, 218u8, 91u8, 105u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    sender: topics.1,
                    token: topics.2,
                    actualGasCost: data.0,
                    actualTokenCharge: data.1,
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
                    > as alloy_sol_types::SolType>::tokenize(&self.actualGasCost),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.actualTokenCharge),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(), self.sender.clone(), self.token.clone())
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
                    &self.sender,
                );
                out[2usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.token,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for Erc20Settled {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&Erc20Settled> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &Erc20Settled) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `Erc20Sponsored(address,address,uint256,uint256)` and selector `0x296caef5f4574ea01a4a23e6dd315c798c4c426a034a07d41babf927188bceae`.
```solidity
event Erc20Sponsored(address indexed sender, address indexed token, uint256 maxTokenAmount, uint256 priceWithMarkup);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct Erc20Sponsored {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub maxTokenAmount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub priceWithMarkup: alloy::sol_types::private::primitives::aliases::U256,
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
        impl alloy_sol_types::SolEvent for Erc20Sponsored {
            type DataTuple<'a> = (
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "Erc20Sponsored(address,address,uint256,uint256)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                41u8, 108u8, 174u8, 245u8, 244u8, 87u8, 78u8, 160u8, 26u8, 74u8, 35u8,
                230u8, 221u8, 49u8, 92u8, 121u8, 140u8, 76u8, 66u8, 106u8, 3u8, 74u8,
                7u8, 212u8, 27u8, 171u8, 249u8, 39u8, 24u8, 139u8, 206u8, 174u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    sender: topics.1,
                    token: topics.2,
                    maxTokenAmount: data.0,
                    priceWithMarkup: data.1,
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
                    > as alloy_sol_types::SolType>::tokenize(&self.maxTokenAmount),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.priceWithMarkup),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(), self.sender.clone(), self.token.clone())
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
                    &self.sender,
                );
                out[2usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.token,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for Erc20Sponsored {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&Erc20Sponsored> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &Erc20Sponsored) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `EthOracleChanged(address,address)` and selector `0xdd786e7bdbeb989faf1b5c8b448a792709a1104d2222d9dad2aa531b37b56e94`.
```solidity
event EthOracleChanged(address indexed oldOracle, address indexed newOracle);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct EthOracleChanged {
        #[allow(missing_docs)]
        pub oldOracle: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub newOracle: alloy::sol_types::private::Address,
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
        impl alloy_sol_types::SolEvent for EthOracleChanged {
            type DataTuple<'a> = ();
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "EthOracleChanged(address,address)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                221u8, 120u8, 110u8, 123u8, 219u8, 235u8, 152u8, 159u8, 175u8, 27u8,
                92u8, 139u8, 68u8, 138u8, 121u8, 39u8, 9u8, 161u8, 16u8, 77u8, 34u8,
                34u8, 217u8, 218u8, 210u8, 170u8, 83u8, 27u8, 55u8, 181u8, 110u8, 148u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    oldOracle: topics.1,
                    newOracle: topics.2,
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
                    self.oldOracle.clone(),
                    self.newOracle.clone(),
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
                    &self.oldOracle,
                );
                out[2usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.newOracle,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for EthOracleChanged {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&EthOracleChanged> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &EthOracleChanged) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `NativeBalanceWithdrawn(address,uint256)` and selector `0x5ceac9f7036a05e231fa263d6b6731de430460d4af4830160bb6f00d1b957f50`.
```solidity
event NativeBalanceWithdrawn(address indexed to, uint256 amount);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct NativeBalanceWithdrawn {
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
        impl alloy_sol_types::SolEvent for NativeBalanceWithdrawn {
            type DataTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "NativeBalanceWithdrawn(address,uint256)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                92u8, 234u8, 201u8, 247u8, 3u8, 106u8, 5u8, 226u8, 49u8, 250u8, 38u8,
                61u8, 107u8, 103u8, 49u8, 222u8, 67u8, 4u8, 96u8, 212u8, 175u8, 72u8,
                48u8, 22u8, 11u8, 182u8, 240u8, 13u8, 27u8, 149u8, 127u8, 80u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    to: topics.1,
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
                (Self::SIGNATURE_HASH.into(), self.to.clone())
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
                    &self.to,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for NativeBalanceWithdrawn {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&NativeBalanceWithdrawn> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &NativeBalanceWithdrawn) -> alloy_sol_types::private::LogData {
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
    /**Event with signature `PoolErc20Settled(address,uint256,address,uint256,uint256)` and selector `0xee8cf41df036875660de15dbd27f5a70c45d889f2e18236da00ae9d724869eb9`.
```solidity
event PoolErc20Settled(address indexed sender, uint256 indexed poolId, address indexed token, uint256 actualGasCost, uint256 actualCharge);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct PoolErc20Settled {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub poolId: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub actualGasCost: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub actualCharge: alloy::sol_types::private::primitives::aliases::U256,
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
        impl alloy_sol_types::SolEvent for PoolErc20Settled {
            type DataTuple<'a> = (
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "PoolErc20Settled(address,uint256,address,uint256,uint256)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                238u8, 140u8, 244u8, 29u8, 240u8, 54u8, 135u8, 86u8, 96u8, 222u8, 21u8,
                219u8, 210u8, 127u8, 90u8, 112u8, 196u8, 93u8, 136u8, 159u8, 46u8, 24u8,
                35u8, 109u8, 160u8, 10u8, 233u8, 215u8, 36u8, 134u8, 158u8, 185u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    sender: topics.1,
                    poolId: topics.2,
                    token: topics.3,
                    actualGasCost: data.0,
                    actualCharge: data.1,
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
                    > as alloy_sol_types::SolType>::tokenize(&self.actualGasCost),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.actualCharge),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (
                    Self::SIGNATURE_HASH.into(),
                    self.sender.clone(),
                    self.poolId.clone(),
                    self.token.clone(),
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
                    &self.sender,
                );
                out[2usize] = <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic(&self.poolId);
                out[3usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.token,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for PoolErc20Settled {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&PoolErc20Settled> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &PoolErc20Settled) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `PoolErc20Sponsored(address,uint256,address,uint256,uint256)` and selector `0x8d0635eb35324b23729a0b8a83ac58cce10dfe45f2b084490770cae0b55b0386`.
```solidity
event PoolErc20Sponsored(address indexed sender, uint256 indexed poolId, address indexed token, uint256 reservedAmount, uint256 priceWithMarkup);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct PoolErc20Sponsored {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub poolId: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub reservedAmount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub priceWithMarkup: alloy::sol_types::private::primitives::aliases::U256,
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
        impl alloy_sol_types::SolEvent for PoolErc20Sponsored {
            type DataTuple<'a> = (
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "PoolErc20Sponsored(address,uint256,address,uint256,uint256)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                141u8, 6u8, 53u8, 235u8, 53u8, 50u8, 75u8, 35u8, 114u8, 154u8, 11u8,
                138u8, 131u8, 172u8, 88u8, 204u8, 225u8, 13u8, 254u8, 69u8, 242u8, 176u8,
                132u8, 73u8, 7u8, 112u8, 202u8, 224u8, 181u8, 91u8, 3u8, 134u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    sender: topics.1,
                    poolId: topics.2,
                    token: topics.3,
                    reservedAmount: data.0,
                    priceWithMarkup: data.1,
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
                    > as alloy_sol_types::SolType>::tokenize(&self.reservedAmount),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.priceWithMarkup),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (
                    Self::SIGNATURE_HASH.into(),
                    self.sender.clone(),
                    self.poolId.clone(),
                    self.token.clone(),
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
                    &self.sender,
                );
                out[2usize] = <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic(&self.poolId);
                out[3usize] = <alloy::sol_types::sol_data::Address as alloy_sol_types::EventTopic>::encode_topic(
                    &self.token,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for PoolErc20Sponsored {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&PoolErc20Sponsored> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &PoolErc20Sponsored) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `PoolEthSettled(address,uint256,uint256,uint256)` and selector `0xaefb7cef608b5381b2c1e06377ec6ac2ee5d19494995d9fa0f4d9fa375b558ed`.
```solidity
event PoolEthSettled(address indexed sender, uint256 indexed poolId, uint256 actualGasCost, uint256 chargedFromPool);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct PoolEthSettled {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub poolId: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub actualGasCost: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub chargedFromPool: alloy::sol_types::private::primitives::aliases::U256,
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
        impl alloy_sol_types::SolEvent for PoolEthSettled {
            type DataTuple<'a> = (
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
            );
            const SIGNATURE: &'static str = "PoolEthSettled(address,uint256,uint256,uint256)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                174u8, 251u8, 124u8, 239u8, 96u8, 139u8, 83u8, 129u8, 178u8, 193u8,
                224u8, 99u8, 119u8, 236u8, 106u8, 194u8, 238u8, 93u8, 25u8, 73u8, 73u8,
                149u8, 217u8, 250u8, 15u8, 77u8, 159u8, 163u8, 117u8, 181u8, 88u8, 237u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    sender: topics.1,
                    poolId: topics.2,
                    actualGasCost: data.0,
                    chargedFromPool: data.1,
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
                    > as alloy_sol_types::SolType>::tokenize(&self.actualGasCost),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.chargedFromPool),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(), self.sender.clone(), self.poolId.clone())
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
                    &self.sender,
                );
                out[2usize] = <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic(&self.poolId);
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for PoolEthSettled {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&PoolEthSettled> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &PoolEthSettled) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `PoolEthSponsored(address,uint256,uint256)` and selector `0x91ba9c25efc3c1af7905cdb9048fe88e2b7daab7c2d0d0c3f627772b5006e004`.
```solidity
event PoolEthSponsored(address indexed sender, uint256 indexed poolId, uint256 reserved);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct PoolEthSponsored {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub poolId: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub reserved: alloy::sol_types::private::primitives::aliases::U256,
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
        impl alloy_sol_types::SolEvent for PoolEthSponsored {
            type DataTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
            );
            const SIGNATURE: &'static str = "PoolEthSponsored(address,uint256,uint256)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                145u8, 186u8, 156u8, 37u8, 239u8, 195u8, 193u8, 175u8, 121u8, 5u8, 205u8,
                185u8, 4u8, 143u8, 232u8, 142u8, 43u8, 125u8, 170u8, 183u8, 194u8, 208u8,
                208u8, 195u8, 246u8, 39u8, 119u8, 43u8, 80u8, 6u8, 224u8, 4u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    sender: topics.1,
                    poolId: topics.2,
                    reserved: data.0,
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
                    > as alloy_sol_types::SolType>::tokenize(&self.reserved),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(), self.sender.clone(), self.poolId.clone())
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
                    &self.sender,
                );
                out[2usize] = <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic(&self.poolId);
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for PoolEthSponsored {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&PoolEthSponsored> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &PoolEthSponsored) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `SenderPoolChanged(address,uint256,uint256)` and selector `0x253774f7af1ea3bc8cf8e4adb437f908edd957cad34d0705f3233a59ea0bf1e7`.
```solidity
event SenderPoolChanged(address indexed sender, uint256 indexed oldPoolId, uint256 indexed newPoolId);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct SenderPoolChanged {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub oldPoolId: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub newPoolId: alloy::sol_types::private::primitives::aliases::U256,
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
        impl alloy_sol_types::SolEvent for SenderPoolChanged {
            type DataTuple<'a> = ();
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            const SIGNATURE: &'static str = "SenderPoolChanged(address,uint256,uint256)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                37u8, 55u8, 116u8, 247u8, 175u8, 30u8, 163u8, 188u8, 140u8, 248u8, 228u8,
                173u8, 180u8, 55u8, 249u8, 8u8, 237u8, 217u8, 87u8, 202u8, 211u8, 77u8,
                7u8, 5u8, 243u8, 35u8, 58u8, 89u8, 234u8, 11u8, 241u8, 231u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    sender: topics.1,
                    oldPoolId: topics.2,
                    newPoolId: topics.3,
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
                    self.sender.clone(),
                    self.oldPoolId.clone(),
                    self.newPoolId.clone(),
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
                    &self.sender,
                );
                out[2usize] = <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic(&self.oldPoolId);
                out[3usize] = <alloy::sol_types::sol_data::Uint<
                    256,
                > as alloy_sol_types::EventTopic>::encode_topic(&self.newPoolId);
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for SenderPoolChanged {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&SenderPoolChanged> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &SenderPoolChanged) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `Settled(address,uint256,uint64,uint128)` and selector `0xeb43290efc1c9442713347e1d541c9e24d355a30f119510830214559ccd72e8f`.
```solidity
event Settled(address indexed sender, uint256 actualGasCost, uint64 epoch, uint128 spentInEpoch);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct Settled {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub actualGasCost: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub epoch: u64,
        #[allow(missing_docs)]
        pub spentInEpoch: u128,
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
        impl alloy_sol_types::SolEvent for Settled {
            type DataTuple<'a> = (
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<64>,
                alloy::sol_types::sol_data::Uint<128>,
            );
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "Settled(address,uint256,uint64,uint128)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                235u8, 67u8, 41u8, 14u8, 252u8, 28u8, 148u8, 66u8, 113u8, 51u8, 71u8,
                225u8, 213u8, 65u8, 201u8, 226u8, 77u8, 53u8, 90u8, 48u8, 241u8, 25u8,
                81u8, 8u8, 48u8, 33u8, 69u8, 89u8, 204u8, 215u8, 46u8, 143u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    sender: topics.1,
                    actualGasCost: data.0,
                    epoch: data.1,
                    spentInEpoch: data.2,
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
                    > as alloy_sol_types::SolType>::tokenize(&self.actualGasCost),
                    <alloy::sol_types::sol_data::Uint<
                        64,
                    > as alloy_sol_types::SolType>::tokenize(&self.epoch),
                    <alloy::sol_types::sol_data::Uint<
                        128,
                    > as alloy_sol_types::SolType>::tokenize(&self.spentInEpoch),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(), self.sender.clone())
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
                    &self.sender,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for Settled {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&Settled> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &Settled) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `Sponsored(address,uint256,uint64)` and selector `0x46c10e67d6d6ef2dff7f3a9a7c4a28f4716171d8808a5063e3c89c12a81e475f`.
```solidity
event Sponsored(address indexed sender, uint256 maxCostEstimate, uint64 epoch);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct Sponsored {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub maxCostEstimate: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub epoch: u64,
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
        impl alloy_sol_types::SolEvent for Sponsored {
            type DataTuple<'a> = (
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<64>,
            );
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "Sponsored(address,uint256,uint64)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                70u8, 193u8, 14u8, 103u8, 214u8, 214u8, 239u8, 45u8, 255u8, 127u8, 58u8,
                154u8, 124u8, 74u8, 40u8, 244u8, 113u8, 97u8, 113u8, 216u8, 128u8, 138u8,
                80u8, 99u8, 227u8, 200u8, 156u8, 18u8, 168u8, 30u8, 71u8, 95u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    sender: topics.1,
                    maxCostEstimate: data.0,
                    epoch: data.1,
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
                    > as alloy_sol_types::SolType>::tokenize(&self.maxCostEstimate),
                    <alloy::sol_types::sol_data::Uint<
                        64,
                    > as alloy_sol_types::SolType>::tokenize(&self.epoch),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (Self::SIGNATURE_HASH.into(), self.sender.clone())
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
                    &self.sender,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for Sponsored {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&Sponsored> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &Sponsored) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `SponsoredAccountChanged(address,bool)` and selector `0x71b4b10828d5fe536940fe767ede8c16ba18426108d4628caff576ad45acf395`.
```solidity
event SponsoredAccountChanged(address indexed account, bool allowed);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct SponsoredAccountChanged {
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
        impl alloy_sol_types::SolEvent for SponsoredAccountChanged {
            type DataTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "SponsoredAccountChanged(address,bool)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                113u8, 180u8, 177u8, 8u8, 40u8, 213u8, 254u8, 83u8, 105u8, 64u8, 254u8,
                118u8, 126u8, 222u8, 140u8, 22u8, 186u8, 24u8, 66u8, 97u8, 8u8, 212u8,
                98u8, 140u8, 175u8, 245u8, 118u8, 173u8, 69u8, 172u8, 243u8, 149u8,
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
        impl alloy_sol_types::private::IntoLogData for SponsoredAccountChanged {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&SponsoredAccountChanged> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(
                this: &SponsoredAccountChanged,
            ) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `Transfer(address,address,address,uint256,uint256)` and selector `0x1b3d7edb2e9c0b0e7c525b20aaaef0f5940d2ed71663c7d39266ecafac728859`.
```solidity
event Transfer(address caller, address indexed from, address indexed to, uint256 indexed id, uint256 amount);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct Transfer {
        #[allow(missing_docs)]
        pub caller: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub from: alloy::sol_types::private::Address,
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
        impl alloy_sol_types::SolEvent for Transfer {
            type DataTuple<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
            );
            const SIGNATURE: &'static str = "Transfer(address,address,address,uint256,uint256)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                27u8, 61u8, 126u8, 219u8, 46u8, 156u8, 11u8, 14u8, 124u8, 82u8, 91u8,
                32u8, 170u8, 174u8, 240u8, 245u8, 148u8, 13u8, 46u8, 215u8, 22u8, 99u8,
                199u8, 211u8, 146u8, 102u8, 236u8, 175u8, 172u8, 114u8, 136u8, 89u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    caller: data.0,
                    from: topics.1,
                    to: topics.2,
                    id: topics.3,
                    amount: data.1,
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
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.caller,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amount),
                )
            }
            #[inline]
            fn topics(&self) -> <Self::TopicList as alloy_sol_types::SolType>::RustType {
                (
                    Self::SIGNATURE_HASH.into(),
                    self.from.clone(),
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
                    &self.from,
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
        impl alloy_sol_types::private::IntoLogData for Transfer {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&Transfer> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &Transfer) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `TrustedDelegateChanged(address,bool)` and selector `0xb4734c1ff22ef330acc505cb27f93c3ada143d8f2fda8d82edae83e40c2bee16`.
```solidity
event TrustedDelegateChanged(address indexed delegate, bool allowed);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct TrustedDelegateChanged {
        #[allow(missing_docs)]
        pub delegate: alloy::sol_types::private::Address,
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
        impl alloy_sol_types::SolEvent for TrustedDelegateChanged {
            type DataTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (
                alloy_sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Address,
            );
            const SIGNATURE: &'static str = "TrustedDelegateChanged(address,bool)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                180u8, 115u8, 76u8, 31u8, 242u8, 46u8, 243u8, 48u8, 172u8, 197u8, 5u8,
                203u8, 39u8, 249u8, 60u8, 58u8, 218u8, 20u8, 61u8, 143u8, 47u8, 218u8,
                141u8, 130u8, 237u8, 174u8, 131u8, 228u8, 12u8, 43u8, 238u8, 22u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    delegate: topics.1,
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
                (Self::SIGNATURE_HASH.into(), self.delegate.clone())
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
                    &self.delegate,
                );
                Ok(())
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::private::IntoLogData for TrustedDelegateChanged {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&TrustedDelegateChanged> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &TrustedDelegateChanged) -> alloy_sol_types::private::LogData {
                alloy_sol_types::SolEvent::encode_log_data(this)
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Event with signature `TuningUpdated(uint128,uint64)` and selector `0x9a649e8ac2e5d06a297ad5c3d5636c2ec800686ba217ec8f17cb11fea9687b28`.
```solidity
event TuningUpdated(uint128 maxWeiPerEpoch, uint64 epochLength);
```*/
    #[allow(
        non_camel_case_types,
        non_snake_case,
        clippy::pub_underscore_fields,
        clippy::style
    )]
    #[derive(Clone)]
    pub struct TuningUpdated {
        #[allow(missing_docs)]
        pub maxWeiPerEpoch: u128,
        #[allow(missing_docs)]
        pub epochLength: u64,
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
        impl alloy_sol_types::SolEvent for TuningUpdated {
            type DataTuple<'a> = (
                alloy::sol_types::sol_data::Uint<128>,
                alloy::sol_types::sol_data::Uint<64>,
            );
            type DataToken<'a> = <Self::DataTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type TopicList = (alloy_sol_types::sol_data::FixedBytes<32>,);
            const SIGNATURE: &'static str = "TuningUpdated(uint128,uint64)";
            const SIGNATURE_HASH: alloy_sol_types::private::B256 = alloy_sol_types::private::B256::new([
                154u8, 100u8, 158u8, 138u8, 194u8, 229u8, 208u8, 106u8, 41u8, 122u8,
                213u8, 195u8, 213u8, 99u8, 108u8, 46u8, 200u8, 0u8, 104u8, 107u8, 162u8,
                23u8, 236u8, 143u8, 23u8, 203u8, 17u8, 254u8, 169u8, 104u8, 123u8, 40u8,
            ]);
            const ANONYMOUS: bool = false;
            #[allow(unused_variables)]
            #[inline]
            fn new(
                topics: <Self::TopicList as alloy_sol_types::SolType>::RustType,
                data: <Self::DataTuple<'_> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                Self {
                    maxWeiPerEpoch: data.0,
                    epochLength: data.1,
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
                        128,
                    > as alloy_sol_types::SolType>::tokenize(&self.maxWeiPerEpoch),
                    <alloy::sol_types::sol_data::Uint<
                        64,
                    > as alloy_sol_types::SolType>::tokenize(&self.epochLength),
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
        impl alloy_sol_types::private::IntoLogData for TuningUpdated {
            fn to_log_data(&self) -> alloy_sol_types::private::LogData {
                From::from(self)
            }
            fn into_log_data(self) -> alloy_sol_types::private::LogData {
                From::from(&self)
            }
        }
        #[automatically_derived]
        impl From<&TuningUpdated> for alloy_sol_types::private::LogData {
            #[inline]
            fn from(this: &TuningUpdated) -> alloy_sol_types::private::LogData {
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
    /**Constructor`.
```solidity
constructor(address ep, address initialOwner);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct constructorCall {
        #[allow(missing_docs)]
        pub ep: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub initialOwner: alloy::sol_types::private::Address,
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
                    (value.ep, value.initialOwner)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for constructorCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        ep: tuple.0,
                        initialOwner: tuple.1,
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
                        &self.ep,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.initialOwner,
                    ),
                )
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
    /**Function with signature `addStake(uint32)` and selector `0x0396cb60`.
```solidity
function addStake(uint32 unstakeDelaySec) external payable;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct addStakeCall {
        #[allow(missing_docs)]
        pub unstakeDelaySec: u32,
    }
    ///Container type for the return parameters of the [`addStake(uint32)`](addStakeCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct addStakeReturn {}
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
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Uint<32>,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (u32,);
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
            impl ::core::convert::From<addStakeCall> for UnderlyingRustTuple<'_> {
                fn from(value: addStakeCall) -> Self {
                    (value.unstakeDelaySec,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for addStakeCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { unstakeDelaySec: tuple.0 }
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
            impl ::core::convert::From<addStakeReturn> for UnderlyingRustTuple<'_> {
                fn from(value: addStakeReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for addStakeReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl addStakeReturn {
            fn _tokenize(
                &self,
            ) -> <addStakeCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for addStakeCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Uint<32>,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = addStakeReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "addStake(uint32)";
            const SELECTOR: [u8; 4] = [3u8, 150u8, 203u8, 96u8];
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
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.unstakeDelaySec),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                addStakeReturn::_tokenize(ret)
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
    /**Function with signature `allowance(address,address,uint256)` and selector `0x598af9e7`.
```solidity
function allowance(address, address, uint256) external pure returns (uint256);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct allowanceCall {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub _1: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub _2: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`allowance(address,address,uint256)`](allowanceCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct allowanceReturn {
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
            impl ::core::convert::From<allowanceCall> for UnderlyingRustTuple<'_> {
                fn from(value: allowanceCall) -> Self {
                    (value._0, value._1, value._2)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for allowanceCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        _0: tuple.0,
                        _1: tuple.1,
                        _2: tuple.2,
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
            impl ::core::convert::From<allowanceReturn> for UnderlyingRustTuple<'_> {
                fn from(value: allowanceReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for allowanceReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for allowanceCall {
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
            const SIGNATURE: &'static str = "allowance(address,address,uint256)";
            const SELECTOR: [u8; 4] = [89u8, 138u8, 249u8, 231u8];
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
                        let r: allowanceReturn = r.into();
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
                        let r: allowanceReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `approve(address,uint256,uint256)` and selector `0x426a8493`.
```solidity
function approve(address, uint256, uint256) external pure returns (bool);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct approveCall {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub _1: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub _2: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`approve(address,uint256,uint256)`](approveCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct approveReturn {
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
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
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
            impl ::core::convert::From<approveCall> for UnderlyingRustTuple<'_> {
                fn from(value: approveCall) -> Self {
                    (value._0, value._1, value._2)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for approveCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        _0: tuple.0,
                        _1: tuple.1,
                        _2: tuple.2,
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
            impl ::core::convert::From<approveReturn> for UnderlyingRustTuple<'_> {
                fn from(value: approveReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for approveReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for approveCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = bool;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "approve(address,uint256,uint256)";
            const SELECTOR: [u8; 4] = [66u8, 106u8, 132u8, 147u8];
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
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self._1),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self._2),
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
                        let r: approveReturn = r.into();
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
                        let r: approveReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `balanceOf(address,uint256)` and selector `0x00fdd58e`.
```solidity
function balanceOf(address owner, uint256 id) external view returns (uint256);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct balanceOfCall {
        #[allow(missing_docs)]
        pub owner: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub id: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`balanceOf(address,uint256)`](balanceOfCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct balanceOfReturn {
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
                alloy::sol_types::sol_data::Uint<256>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
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
            impl ::core::convert::From<balanceOfCall> for UnderlyingRustTuple<'_> {
                fn from(value: balanceOfCall) -> Self {
                    (value.owner, value.id)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for balanceOfCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        owner: tuple.0,
                        id: tuple.1,
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
            impl ::core::convert::From<balanceOfReturn> for UnderlyingRustTuple<'_> {
                fn from(value: balanceOfReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for balanceOfReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for balanceOfCall {
            type Parameters<'a> = (
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
            const SIGNATURE: &'static str = "balanceOf(address,uint256)";
            const SELECTOR: [u8; 4] = [0u8, 253u8, 213u8, 142u8];
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
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.id),
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
                        let r: balanceOfReturn = r.into();
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
                        let r: balanceOfReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `budgets(address)` and selector `0x147e7e66`.
```solidity
function budgets(address) external view returns (uint64 epoch, uint128 spent);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct budgetsCall(pub alloy::sol_types::private::Address);
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`budgets(address)`](budgetsCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct budgetsReturn {
        #[allow(missing_docs)]
        pub epoch: u64,
        #[allow(missing_docs)]
        pub spent: u128,
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
            impl ::core::convert::From<budgetsCall> for UnderlyingRustTuple<'_> {
                fn from(value: budgetsCall) -> Self {
                    (value.0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for budgetsCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self(tuple.0)
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (
                alloy::sol_types::sol_data::Uint<64>,
                alloy::sol_types::sol_data::Uint<128>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (u64, u128);
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
            impl ::core::convert::From<budgetsReturn> for UnderlyingRustTuple<'_> {
                fn from(value: budgetsReturn) -> Self {
                    (value.epoch, value.spent)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for budgetsReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        epoch: tuple.0,
                        spent: tuple.1,
                    }
                }
            }
        }
        impl budgetsReturn {
            fn _tokenize(
                &self,
            ) -> <budgetsCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::Uint<
                        64,
                    > as alloy_sol_types::SolType>::tokenize(&self.epoch),
                    <alloy::sol_types::sol_data::Uint<
                        128,
                    > as alloy_sol_types::SolType>::tokenize(&self.spent),
                )
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for budgetsCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Address,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = budgetsReturn;
            type ReturnTuple<'a> = (
                alloy::sol_types::sol_data::Uint<64>,
                alloy::sol_types::sol_data::Uint<128>,
            );
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "budgets(address)";
            const SELECTOR: [u8; 4] = [20u8, 126u8, 126u8, 102u8];
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
                budgetsReturn::_tokenize(ret)
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
    /**Function with signature `burnPoolEth(uint256,uint256,address)` and selector `0x75cbcca7`.
```solidity
function burnPoolEth(uint256 poolId, uint256 amount, address to) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct burnPoolEthCall {
        #[allow(missing_docs)]
        pub poolId: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub amount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub to: alloy::sol_types::private::Address,
    }
    ///Container type for the return parameters of the [`burnPoolEth(uint256,uint256,address)`](burnPoolEthCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct burnPoolEthReturn {}
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
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Address,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
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
            impl ::core::convert::From<burnPoolEthCall> for UnderlyingRustTuple<'_> {
                fn from(value: burnPoolEthCall) -> Self {
                    (value.poolId, value.amount, value.to)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for burnPoolEthCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        poolId: tuple.0,
                        amount: tuple.1,
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
            impl ::core::convert::From<burnPoolEthReturn> for UnderlyingRustTuple<'_> {
                fn from(value: burnPoolEthReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for burnPoolEthReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl burnPoolEthReturn {
            fn _tokenize(
                &self,
            ) -> <burnPoolEthCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for burnPoolEthCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Address,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = burnPoolEthReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "burnPoolEth(uint256,uint256,address)";
            const SELECTOR: [u8; 4] = [117u8, 203u8, 204u8, 167u8];
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
                    > as alloy_sol_types::SolType>::tokenize(&self.poolId),
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
                burnPoolEthReturn::_tokenize(ret)
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
    /**Function with signature `burnPoolToken(uint256,address,uint256,address)` and selector `0x90a4450e`.
```solidity
function burnPoolToken(uint256 poolId, address token, uint256 amount, address to) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct burnPoolTokenCall {
        #[allow(missing_docs)]
        pub poolId: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub amount: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub to: alloy::sol_types::private::Address,
    }
    ///Container type for the return parameters of the [`burnPoolToken(uint256,address,uint256,address)`](burnPoolTokenCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct burnPoolTokenReturn {}
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
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Address,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::primitives::aliases::U256,
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
            impl ::core::convert::From<burnPoolTokenCall> for UnderlyingRustTuple<'_> {
                fn from(value: burnPoolTokenCall) -> Self {
                    (value.poolId, value.token, value.amount, value.to)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for burnPoolTokenCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        poolId: tuple.0,
                        token: tuple.1,
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
            impl ::core::convert::From<burnPoolTokenReturn> for UnderlyingRustTuple<'_> {
                fn from(value: burnPoolTokenReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for burnPoolTokenReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl burnPoolTokenReturn {
            fn _tokenize(
                &self,
            ) -> <burnPoolTokenCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for burnPoolTokenCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Address,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = burnPoolTokenReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "burnPoolToken(uint256,address,uint256,address)";
            const SELECTOR: [u8; 4] = [144u8, 164u8, 69u8, 14u8];
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
                    > as alloy_sol_types::SolType>::tokenize(&self.poolId),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.token,
                    ),
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
                burnPoolTokenReturn::_tokenize(ret)
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
    /**Function with signature `creditPoolEth(uint256)` and selector `0xb89b6a1e`.
```solidity
function creditPoolEth(uint256 poolId) external payable;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct creditPoolEthCall {
        #[allow(missing_docs)]
        pub poolId: alloy::sol_types::private::primitives::aliases::U256,
    }
    ///Container type for the return parameters of the [`creditPoolEth(uint256)`](creditPoolEthCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct creditPoolEthReturn {}
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
            impl ::core::convert::From<creditPoolEthCall> for UnderlyingRustTuple<'_> {
                fn from(value: creditPoolEthCall) -> Self {
                    (value.poolId,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for creditPoolEthCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { poolId: tuple.0 }
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
            impl ::core::convert::From<creditPoolEthReturn> for UnderlyingRustTuple<'_> {
                fn from(value: creditPoolEthReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for creditPoolEthReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl creditPoolEthReturn {
            fn _tokenize(
                &self,
            ) -> <creditPoolEthCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for creditPoolEthCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = creditPoolEthReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "creditPoolEth(uint256)";
            const SELECTOR: [u8; 4] = [184u8, 155u8, 106u8, 30u8];
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
                    > as alloy_sol_types::SolType>::tokenize(&self.poolId),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                creditPoolEthReturn::_tokenize(ret)
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
    /**Function with signature `creditPoolToken(uint256,address,uint256)` and selector `0x110dfd92`.
```solidity
function creditPoolToken(uint256 poolId, address token, uint256 amount) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct creditPoolTokenCall {
        #[allow(missing_docs)]
        pub poolId: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub amount: alloy::sol_types::private::primitives::aliases::U256,
    }
    ///Container type for the return parameters of the [`creditPoolToken(uint256,address,uint256)`](creditPoolTokenCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct creditPoolTokenReturn {}
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
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::primitives::aliases::U256,
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
            impl ::core::convert::From<creditPoolTokenCall> for UnderlyingRustTuple<'_> {
                fn from(value: creditPoolTokenCall) -> Self {
                    (value.poolId, value.token, value.amount)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for creditPoolTokenCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        poolId: tuple.0,
                        token: tuple.1,
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
            impl ::core::convert::From<creditPoolTokenReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: creditPoolTokenReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for creditPoolTokenReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl creditPoolTokenReturn {
            fn _tokenize(
                &self,
            ) -> <creditPoolTokenCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for creditPoolTokenCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = creditPoolTokenReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "creditPoolToken(uint256,address,uint256)";
            const SELECTOR: [u8; 4] = [17u8, 13u8, 253u8, 146u8];
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
                    > as alloy_sol_types::SolType>::tokenize(&self.poolId),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.token,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amount),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                creditPoolTokenReturn::_tokenize(ret)
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
    /**Function with signature `currentEpoch()` and selector `0x76671808`.
```solidity
function currentEpoch() external view returns (uint64);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct currentEpochCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`currentEpoch()`](currentEpochCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct currentEpochReturn {
        #[allow(missing_docs)]
        pub _0: u64,
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
            impl ::core::convert::From<currentEpochCall> for UnderlyingRustTuple<'_> {
                fn from(value: currentEpochCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for currentEpochCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Uint<64>,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (u64,);
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
            impl ::core::convert::From<currentEpochReturn> for UnderlyingRustTuple<'_> {
                fn from(value: currentEpochReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for currentEpochReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for currentEpochCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = u64;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Uint<64>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "currentEpoch()";
            const SELECTOR: [u8; 4] = [118u8, 103u8, 24u8, 8u8];
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
                        64,
                    > as alloy_sol_types::SolType>::tokenize(ret),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: currentEpochReturn = r.into();
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
                        let r: currentEpochReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `deposit()` and selector `0xd0e30db0`.
```solidity
function deposit() external payable;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct depositCall;
    ///Container type for the return parameters of the [`deposit()`](depositCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct depositReturn {}
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
            impl ::core::convert::From<depositCall> for UnderlyingRustTuple<'_> {
                fn from(value: depositCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for depositCall {
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
            impl ::core::convert::From<depositReturn> for UnderlyingRustTuple<'_> {
                fn from(value: depositReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for depositReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl depositReturn {
            fn _tokenize(
                &self,
            ) -> <depositCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for depositCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = depositReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "deposit()";
            const SELECTOR: [u8; 4] = [208u8, 227u8, 13u8, 176u8];
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
                depositReturn::_tokenize(ret)
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
    /**Function with signature `entryPoint()` and selector `0xb0d691fe`.
```solidity
function entryPoint() external view returns (address);
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
    /**Function with signature `epochLength()` and selector `0x57d775f8`.
```solidity
function epochLength() external view returns (uint64);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct epochLengthCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`epochLength()`](epochLengthCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct epochLengthReturn {
        #[allow(missing_docs)]
        pub _0: u64,
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
            impl ::core::convert::From<epochLengthCall> for UnderlyingRustTuple<'_> {
                fn from(value: epochLengthCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for epochLengthCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Uint<64>,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (u64,);
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
            impl ::core::convert::From<epochLengthReturn> for UnderlyingRustTuple<'_> {
                fn from(value: epochLengthReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for epochLengthReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for epochLengthCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = u64;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Uint<64>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "epochLength()";
            const SELECTOR: [u8; 4] = [87u8, 215u8, 117u8, 248u8];
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
                        64,
                    > as alloy_sol_types::SolType>::tokenize(ret),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: epochLengthReturn = r.into();
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
                        let r: epochLengthReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `erc20Config(address)` and selector `0xf9af2bf2`.
```solidity
function erc20Config(address token) external view returns (bool enabled, uint8 tokenDecimals, uint32 maxStaleness, uint16 markupBps, address tokenOracle, address treasury);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct erc20ConfigCall {
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`erc20Config(address)`](erc20ConfigCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct erc20ConfigReturn {
        #[allow(missing_docs)]
        pub enabled: bool,
        #[allow(missing_docs)]
        pub tokenDecimals: u8,
        #[allow(missing_docs)]
        pub maxStaleness: u32,
        #[allow(missing_docs)]
        pub markupBps: u16,
        #[allow(missing_docs)]
        pub tokenOracle: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub treasury: alloy::sol_types::private::Address,
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
            impl ::core::convert::From<erc20ConfigCall> for UnderlyingRustTuple<'_> {
                fn from(value: erc20ConfigCall) -> Self {
                    (value.token,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for erc20ConfigCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { token: tuple.0 }
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (
                alloy::sol_types::sol_data::Bool,
                alloy::sol_types::sol_data::Uint<8>,
                alloy::sol_types::sol_data::Uint<32>,
                alloy::sol_types::sol_data::Uint<16>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                bool,
                u8,
                u32,
                u16,
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
            impl ::core::convert::From<erc20ConfigReturn> for UnderlyingRustTuple<'_> {
                fn from(value: erc20ConfigReturn) -> Self {
                    (
                        value.enabled,
                        value.tokenDecimals,
                        value.maxStaleness,
                        value.markupBps,
                        value.tokenOracle,
                        value.treasury,
                    )
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for erc20ConfigReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        enabled: tuple.0,
                        tokenDecimals: tuple.1,
                        maxStaleness: tuple.2,
                        markupBps: tuple.3,
                        tokenOracle: tuple.4,
                        treasury: tuple.5,
                    }
                }
            }
        }
        impl erc20ConfigReturn {
            fn _tokenize(
                &self,
            ) -> <erc20ConfigCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::Bool as alloy_sol_types::SolType>::tokenize(
                        &self.enabled,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        8,
                    > as alloy_sol_types::SolType>::tokenize(&self.tokenDecimals),
                    <alloy::sol_types::sol_data::Uint<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.maxStaleness),
                    <alloy::sol_types::sol_data::Uint<
                        16,
                    > as alloy_sol_types::SolType>::tokenize(&self.markupBps),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.tokenOracle,
                    ),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.treasury,
                    ),
                )
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for erc20ConfigCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Address,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = erc20ConfigReturn;
            type ReturnTuple<'a> = (
                alloy::sol_types::sol_data::Bool,
                alloy::sol_types::sol_data::Uint<8>,
                alloy::sol_types::sol_data::Uint<32>,
                alloy::sol_types::sol_data::Uint<16>,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "erc20Config(address)";
            const SELECTOR: [u8; 4] = [249u8, 175u8, 43u8, 242u8];
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
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                erc20ConfigReturn::_tokenize(ret)
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
    /**Function with signature `ethOracle()` and selector `0x9c8762e1`.
```solidity
function ethOracle() external view returns (address);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ethOracleCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`ethOracle()`](ethOracleCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct ethOracleReturn {
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
            impl ::core::convert::From<ethOracleCall> for UnderlyingRustTuple<'_> {
                fn from(value: ethOracleCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for ethOracleCall {
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
            impl ::core::convert::From<ethOracleReturn> for UnderlyingRustTuple<'_> {
                fn from(value: ethOracleReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for ethOracleReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for ethOracleCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::Address;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Address,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "ethOracle()";
            const SELECTOR: [u8; 4] = [156u8, 135u8, 98u8, 225u8];
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
                        let r: ethOracleReturn = r.into();
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
                        let r: ethOracleReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `getDeposit()` and selector `0xc399ec88`.
```solidity
function getDeposit() external view returns (uint256);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct getDepositCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`getDeposit()`](getDepositCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct getDepositReturn {
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
            impl ::core::convert::From<getDepositCall> for UnderlyingRustTuple<'_> {
                fn from(value: getDepositCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for getDepositCall {
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
            impl ::core::convert::From<getDepositReturn> for UnderlyingRustTuple<'_> {
                fn from(value: getDepositReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for getDepositReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for getDepositCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::primitives::aliases::U256;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "getDeposit()";
            const SELECTOR: [u8; 4] = [195u8, 153u8, 236u8, 136u8];
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
                        let r: getDepositReturn = r.into();
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
                        let r: getDepositReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `isOperator(address,address)` and selector `0xb6363cf2`.
```solidity
function isOperator(address, address) external pure returns (bool);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct isOperatorCall {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub _1: alloy::sol_types::private::Address,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`isOperator(address,address)`](isOperatorCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct isOperatorReturn {
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
            impl ::core::convert::From<isOperatorCall> for UnderlyingRustTuple<'_> {
                fn from(value: isOperatorCall) -> Self {
                    (value._0, value._1)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for isOperatorCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0, _1: tuple.1 }
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
            impl ::core::convert::From<isOperatorReturn> for UnderlyingRustTuple<'_> {
                fn from(value: isOperatorReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for isOperatorReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for isOperatorCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = bool;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "isOperator(address,address)";
            const SELECTOR: [u8; 4] = [182u8, 54u8, 60u8, 242u8];
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
                        let r: isOperatorReturn = r.into();
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
                        let r: isOperatorReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `maxWeiPerEpoch()` and selector `0x9c01a3ce`.
```solidity
function maxWeiPerEpoch() external view returns (uint128);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct maxWeiPerEpochCall;
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`maxWeiPerEpoch()`](maxWeiPerEpochCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct maxWeiPerEpochReturn {
        #[allow(missing_docs)]
        pub _0: u128,
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
            impl ::core::convert::From<maxWeiPerEpochCall> for UnderlyingRustTuple<'_> {
                fn from(value: maxWeiPerEpochCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for maxWeiPerEpochCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Uint<128>,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (u128,);
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
            impl ::core::convert::From<maxWeiPerEpochReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: maxWeiPerEpochReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for maxWeiPerEpochReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for maxWeiPerEpochCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = u128;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Uint<128>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "maxWeiPerEpoch()";
            const SELECTOR: [u8; 4] = [156u8, 1u8, 163u8, 206u8];
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
                        128,
                    > as alloy_sol_types::SolType>::tokenize(ret),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: maxWeiPerEpochReturn = r.into();
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
                        let r: maxWeiPerEpochReturn = r.into();
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
    /**Function with signature `poolEthBalance(uint256)` and selector `0xcb5638d7`.
```solidity
function poolEthBalance(uint256 poolId) external view returns (uint256);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct poolEthBalanceCall {
        #[allow(missing_docs)]
        pub poolId: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`poolEthBalance(uint256)`](poolEthBalanceCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct poolEthBalanceReturn {
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
            impl ::core::convert::From<poolEthBalanceCall> for UnderlyingRustTuple<'_> {
                fn from(value: poolEthBalanceCall) -> Self {
                    (value.poolId,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for poolEthBalanceCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { poolId: tuple.0 }
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
            impl ::core::convert::From<poolEthBalanceReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: poolEthBalanceReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for poolEthBalanceReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for poolEthBalanceCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::primitives::aliases::U256;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "poolEthBalance(uint256)";
            const SELECTOR: [u8; 4] = [203u8, 86u8, 56u8, 215u8];
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
                    > as alloy_sol_types::SolType>::tokenize(&self.poolId),
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
                        let r: poolEthBalanceReturn = r.into();
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
                        let r: poolEthBalanceReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `poolTokenBalance(uint256,address)` and selector `0x3c7bdcea`.
```solidity
function poolTokenBalance(uint256 poolId, address token) external view returns (uint256);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct poolTokenBalanceCall {
        #[allow(missing_docs)]
        pub poolId: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`poolTokenBalance(uint256,address)`](poolTokenBalanceCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct poolTokenBalanceReturn {
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
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Address,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
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
            impl ::core::convert::From<poolTokenBalanceCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: poolTokenBalanceCall) -> Self {
                    (value.poolId, value.token)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for poolTokenBalanceCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        poolId: tuple.0,
                        token: tuple.1,
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
            impl ::core::convert::From<poolTokenBalanceReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: poolTokenBalanceReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for poolTokenBalanceReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for poolTokenBalanceCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Address,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::primitives::aliases::U256;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "poolTokenBalance(uint256,address)";
            const SELECTOR: [u8; 4] = [60u8, 123u8, 220u8, 234u8];
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
                    > as alloy_sol_types::SolType>::tokenize(&self.poolId),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.token,
                    ),
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
                        let r: poolTokenBalanceReturn = r.into();
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
                        let r: poolTokenBalanceReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `postOp(uint8,bytes,uint256,uint256)` and selector `0x7c627b21`.
```solidity
function postOp(IPaymaster.PostOpMode mode, bytes memory context, uint256 actualGasCost, uint256 actualUserOpFeePerGas) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct postOpCall {
        #[allow(missing_docs)]
        pub mode: <IPaymaster::PostOpMode as alloy::sol_types::SolType>::RustType,
        #[allow(missing_docs)]
        pub context: alloy::sol_types::private::Bytes,
        #[allow(missing_docs)]
        pub actualGasCost: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub actualUserOpFeePerGas: alloy::sol_types::private::primitives::aliases::U256,
    }
    ///Container type for the return parameters of the [`postOp(uint8,bytes,uint256,uint256)`](postOpCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct postOpReturn {}
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
                IPaymaster::PostOpMode,
                alloy::sol_types::sol_data::Bytes,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                <IPaymaster::PostOpMode as alloy::sol_types::SolType>::RustType,
                alloy::sol_types::private::Bytes,
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
            impl ::core::convert::From<postOpCall> for UnderlyingRustTuple<'_> {
                fn from(value: postOpCall) -> Self {
                    (
                        value.mode,
                        value.context,
                        value.actualGasCost,
                        value.actualUserOpFeePerGas,
                    )
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for postOpCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        mode: tuple.0,
                        context: tuple.1,
                        actualGasCost: tuple.2,
                        actualUserOpFeePerGas: tuple.3,
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
            impl ::core::convert::From<postOpReturn> for UnderlyingRustTuple<'_> {
                fn from(value: postOpReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for postOpReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl postOpReturn {
            fn _tokenize(
                &self,
            ) -> <postOpCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for postOpCall {
            type Parameters<'a> = (
                IPaymaster::PostOpMode,
                alloy::sol_types::sol_data::Bytes,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = postOpReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "postOp(uint8,bytes,uint256,uint256)";
            const SELECTOR: [u8; 4] = [124u8, 98u8, 123u8, 33u8];
            #[inline]
            fn new<'a>(
                tuple: <Self::Parameters<'a> as alloy_sol_types::SolType>::RustType,
            ) -> Self {
                tuple.into()
            }
            #[inline]
            fn tokenize(&self) -> Self::Token<'_> {
                (
                    <IPaymaster::PostOpMode as alloy_sol_types::SolType>::tokenize(
                        &self.mode,
                    ),
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self.context,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.actualGasCost),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.actualUserOpFeePerGas),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                postOpReturn::_tokenize(ret)
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
    /**Function with signature `remainingBudget(address)` and selector `0x54a2b939`.
```solidity
function remainingBudget(address sender) external view returns (uint128);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct remainingBudgetCall {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`remainingBudget(address)`](remainingBudgetCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct remainingBudgetReturn {
        #[allow(missing_docs)]
        pub _0: u128,
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
            impl ::core::convert::From<remainingBudgetCall> for UnderlyingRustTuple<'_> {
                fn from(value: remainingBudgetCall) -> Self {
                    (value.sender,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for remainingBudgetCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { sender: tuple.0 }
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (alloy::sol_types::sol_data::Uint<128>,);
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (u128,);
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
            impl ::core::convert::From<remainingBudgetReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: remainingBudgetReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for remainingBudgetReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for remainingBudgetCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Address,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = u128;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Uint<128>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "remainingBudget(address)";
            const SELECTOR: [u8; 4] = [84u8, 162u8, 185u8, 57u8];
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
                        &self.sender,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                (
                    <alloy::sol_types::sol_data::Uint<
                        128,
                    > as alloy_sol_types::SolType>::tokenize(ret),
                )
            }
            #[inline]
            fn abi_decode_returns(data: &[u8]) -> alloy_sol_types::Result<Self::Return> {
                <Self::ReturnTuple<
                    '_,
                > as alloy_sol_types::SolType>::abi_decode_sequence(data)
                    .map(|r| {
                        let r: remainingBudgetReturn = r.into();
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
                        let r: remainingBudgetReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `removeErc20Token(address)` and selector `0x9800c105`.
```solidity
function removeErc20Token(address token) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct removeErc20TokenCall {
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
    }
    ///Container type for the return parameters of the [`removeErc20Token(address)`](removeErc20TokenCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct removeErc20TokenReturn {}
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
            impl ::core::convert::From<removeErc20TokenCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: removeErc20TokenCall) -> Self {
                    (value.token,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for removeErc20TokenCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { token: tuple.0 }
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
            impl ::core::convert::From<removeErc20TokenReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: removeErc20TokenReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for removeErc20TokenReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl removeErc20TokenReturn {
            fn _tokenize(
                &self,
            ) -> <removeErc20TokenCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for removeErc20TokenCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Address,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = removeErc20TokenReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "removeErc20Token(address)";
            const SELECTOR: [u8; 4] = [152u8, 0u8, 193u8, 5u8];
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
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                removeErc20TokenReturn::_tokenize(ret)
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
    /**Function with signature `senderPool(address)` and selector `0xe5c1623e`.
```solidity
function senderPool(address sender) external view returns (uint256 poolId);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct senderPoolCall {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`senderPool(address)`](senderPoolCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct senderPoolReturn {
        #[allow(missing_docs)]
        pub poolId: alloy::sol_types::private::primitives::aliases::U256,
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
            impl ::core::convert::From<senderPoolCall> for UnderlyingRustTuple<'_> {
                fn from(value: senderPoolCall) -> Self {
                    (value.sender,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for senderPoolCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { sender: tuple.0 }
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
            impl ::core::convert::From<senderPoolReturn> for UnderlyingRustTuple<'_> {
                fn from(value: senderPoolReturn) -> Self {
                    (value.poolId,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for senderPoolReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { poolId: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for senderPoolCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Address,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = alloy::sol_types::private::primitives::aliases::U256;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Uint<256>,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "senderPool(address)";
            const SELECTOR: [u8; 4] = [229u8, 193u8, 98u8, 62u8];
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
                        &self.sender,
                    ),
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
                        let r: senderPoolReturn = r.into();
                        r.poolId
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
                        let r: senderPoolReturn = r.into();
                        r.poolId
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `setErc20Config(address,address,uint32,uint16,address)` and selector `0x2a895f35`.
```solidity
function setErc20Config(address token, address tokenOracle, uint32 maxStaleness, uint16 markupBps, address treasury) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setErc20ConfigCall {
        #[allow(missing_docs)]
        pub token: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub tokenOracle: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub maxStaleness: u32,
        #[allow(missing_docs)]
        pub markupBps: u16,
        #[allow(missing_docs)]
        pub treasury: alloy::sol_types::private::Address,
    }
    ///Container type for the return parameters of the [`setErc20Config(address,address,uint32,uint16,address)`](setErc20ConfigCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setErc20ConfigReturn {}
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
                alloy::sol_types::sol_data::Uint<32>,
                alloy::sol_types::sol_data::Uint<16>,
                alloy::sol_types::sol_data::Address,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Address,
                alloy::sol_types::private::Address,
                u32,
                u16,
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
            impl ::core::convert::From<setErc20ConfigCall> for UnderlyingRustTuple<'_> {
                fn from(value: setErc20ConfigCall) -> Self {
                    (
                        value.token,
                        value.tokenOracle,
                        value.maxStaleness,
                        value.markupBps,
                        value.treasury,
                    )
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setErc20ConfigCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        token: tuple.0,
                        tokenOracle: tuple.1,
                        maxStaleness: tuple.2,
                        markupBps: tuple.3,
                        treasury: tuple.4,
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
            impl ::core::convert::From<setErc20ConfigReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: setErc20ConfigReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for setErc20ConfigReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl setErc20ConfigReturn {
            fn _tokenize(
                &self,
            ) -> <setErc20ConfigCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for setErc20ConfigCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<32>,
                alloy::sol_types::sol_data::Uint<16>,
                alloy::sol_types::sol_data::Address,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = setErc20ConfigReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "setErc20Config(address,address,uint32,uint16,address)";
            const SELECTOR: [u8; 4] = [42u8, 137u8, 95u8, 53u8];
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
                        &self.tokenOracle,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        32,
                    > as alloy_sol_types::SolType>::tokenize(&self.maxStaleness),
                    <alloy::sol_types::sol_data::Uint<
                        16,
                    > as alloy_sol_types::SolType>::tokenize(&self.markupBps),
                    <alloy::sol_types::sol_data::Address as alloy_sol_types::SolType>::tokenize(
                        &self.treasury,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                setErc20ConfigReturn::_tokenize(ret)
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
    /**Function with signature `setEthOracle(address)` and selector `0xa6cd75dc`.
```solidity
function setEthOracle(address newOracle) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setEthOracleCall {
        #[allow(missing_docs)]
        pub newOracle: alloy::sol_types::private::Address,
    }
    ///Container type for the return parameters of the [`setEthOracle(address)`](setEthOracleCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setEthOracleReturn {}
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
            impl ::core::convert::From<setEthOracleCall> for UnderlyingRustTuple<'_> {
                fn from(value: setEthOracleCall) -> Self {
                    (value.newOracle,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setEthOracleCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { newOracle: tuple.0 }
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
            impl ::core::convert::From<setEthOracleReturn> for UnderlyingRustTuple<'_> {
                fn from(value: setEthOracleReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setEthOracleReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl setEthOracleReturn {
            fn _tokenize(
                &self,
            ) -> <setEthOracleCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for setEthOracleCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Address,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = setEthOracleReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "setEthOracle(address)";
            const SELECTOR: [u8; 4] = [166u8, 205u8, 117u8, 220u8];
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
                        &self.newOracle,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                setEthOracleReturn::_tokenize(ret)
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
    /**Function with signature `setOperator(address,bool)` and selector `0x558a7297`.
```solidity
function setOperator(address, bool) external pure returns (bool);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setOperatorCall {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub _1: bool,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`setOperator(address,bool)`](setOperatorCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setOperatorReturn {
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
            impl ::core::convert::From<setOperatorCall> for UnderlyingRustTuple<'_> {
                fn from(value: setOperatorCall) -> Self {
                    (value._0, value._1)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setOperatorCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0, _1: tuple.1 }
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
            impl ::core::convert::From<setOperatorReturn> for UnderlyingRustTuple<'_> {
                fn from(value: setOperatorReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setOperatorReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for setOperatorCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Bool,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = bool;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "setOperator(address,bool)";
            const SELECTOR: [u8; 4] = [85u8, 138u8, 114u8, 151u8];
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
                    <alloy::sol_types::sol_data::Bool as alloy_sol_types::SolType>::tokenize(
                        &self._1,
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
                        let r: setOperatorReturn = r.into();
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
                        let r: setOperatorReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `setSenderPool(address,uint256)` and selector `0x249112f4`.
```solidity
function setSenderPool(address sender, uint256 poolId) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setSenderPoolCall {
        #[allow(missing_docs)]
        pub sender: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub poolId: alloy::sol_types::private::primitives::aliases::U256,
    }
    ///Container type for the return parameters of the [`setSenderPool(address,uint256)`](setSenderPoolCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setSenderPoolReturn {}
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
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
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
            impl ::core::convert::From<setSenderPoolCall> for UnderlyingRustTuple<'_> {
                fn from(value: setSenderPoolCall) -> Self {
                    (value.sender, value.poolId)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setSenderPoolCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        sender: tuple.0,
                        poolId: tuple.1,
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
            impl ::core::convert::From<setSenderPoolReturn> for UnderlyingRustTuple<'_> {
                fn from(value: setSenderPoolReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setSenderPoolReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl setSenderPoolReturn {
            fn _tokenize(
                &self,
            ) -> <setSenderPoolCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for setSenderPoolCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = setSenderPoolReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "setSenderPool(address,uint256)";
            const SELECTOR: [u8; 4] = [36u8, 145u8, 18u8, 244u8];
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
                        &self.sender,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.poolId),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                setSenderPoolReturn::_tokenize(ret)
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
    /**Function with signature `setSponsored(address,bool)` and selector `0xf935d0b0`.
```solidity
function setSponsored(address account, bool allowed) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setSponsoredCall {
        #[allow(missing_docs)]
        pub account: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub allowed: bool,
    }
    ///Container type for the return parameters of the [`setSponsored(address,bool)`](setSponsoredCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setSponsoredReturn {}
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
            impl ::core::convert::From<setSponsoredCall> for UnderlyingRustTuple<'_> {
                fn from(value: setSponsoredCall) -> Self {
                    (value.account, value.allowed)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setSponsoredCall {
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
            impl ::core::convert::From<setSponsoredReturn> for UnderlyingRustTuple<'_> {
                fn from(value: setSponsoredReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setSponsoredReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl setSponsoredReturn {
            fn _tokenize(
                &self,
            ) -> <setSponsoredCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for setSponsoredCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Bool,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = setSponsoredReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "setSponsored(address,bool)";
            const SELECTOR: [u8; 4] = [249u8, 53u8, 208u8, 176u8];
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
                setSponsoredReturn::_tokenize(ret)
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
    /**Function with signature `setTrustedDelegate(address,bool)` and selector `0xb1c5af77`.
```solidity
function setTrustedDelegate(address delegate, bool allowed) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setTrustedDelegateCall {
        #[allow(missing_docs)]
        pub delegate: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub allowed: bool,
    }
    ///Container type for the return parameters of the [`setTrustedDelegate(address,bool)`](setTrustedDelegateCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setTrustedDelegateReturn {}
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
            impl ::core::convert::From<setTrustedDelegateCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: setTrustedDelegateCall) -> Self {
                    (value.delegate, value.allowed)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for setTrustedDelegateCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        delegate: tuple.0,
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
            impl ::core::convert::From<setTrustedDelegateReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: setTrustedDelegateReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for setTrustedDelegateReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl setTrustedDelegateReturn {
            fn _tokenize(
                &self,
            ) -> <setTrustedDelegateCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for setTrustedDelegateCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Bool,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = setTrustedDelegateReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "setTrustedDelegate(address,bool)";
            const SELECTOR: [u8; 4] = [177u8, 197u8, 175u8, 119u8];
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
                        &self.delegate,
                    ),
                    <alloy::sol_types::sol_data::Bool as alloy_sol_types::SolType>::tokenize(
                        &self.allowed,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                setTrustedDelegateReturn::_tokenize(ret)
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
    /**Function with signature `setTuning(uint128,uint64)` and selector `0x6ca18fc6`.
```solidity
function setTuning(uint128 newMaxWeiPerEpoch, uint64 newEpochLength) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setTuningCall {
        #[allow(missing_docs)]
        pub newMaxWeiPerEpoch: u128,
        #[allow(missing_docs)]
        pub newEpochLength: u64,
    }
    ///Container type for the return parameters of the [`setTuning(uint128,uint64)`](setTuningCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct setTuningReturn {}
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
                alloy::sol_types::sol_data::Uint<128>,
                alloy::sol_types::sol_data::Uint<64>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (u128, u64);
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
            impl ::core::convert::From<setTuningCall> for UnderlyingRustTuple<'_> {
                fn from(value: setTuningCall) -> Self {
                    (value.newMaxWeiPerEpoch, value.newEpochLength)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setTuningCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        newMaxWeiPerEpoch: tuple.0,
                        newEpochLength: tuple.1,
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
            impl ::core::convert::From<setTuningReturn> for UnderlyingRustTuple<'_> {
                fn from(value: setTuningReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for setTuningReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl setTuningReturn {
            fn _tokenize(
                &self,
            ) -> <setTuningCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for setTuningCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Uint<128>,
                alloy::sol_types::sol_data::Uint<64>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = setTuningReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "setTuning(uint128,uint64)";
            const SELECTOR: [u8; 4] = [108u8, 161u8, 143u8, 198u8];
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
                        128,
                    > as alloy_sol_types::SolType>::tokenize(&self.newMaxWeiPerEpoch),
                    <alloy::sol_types::sol_data::Uint<
                        64,
                    > as alloy_sol_types::SolType>::tokenize(&self.newEpochLength),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                setTuningReturn::_tokenize(ret)
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
    /**Function with signature `sponsoredAccount(address)` and selector `0xcdd0c12d`.
```solidity
function sponsoredAccount(address) external view returns (bool);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct sponsoredAccountCall(pub alloy::sol_types::private::Address);
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`sponsoredAccount(address)`](sponsoredAccountCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct sponsoredAccountReturn {
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
            impl ::core::convert::From<sponsoredAccountCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: sponsoredAccountCall) -> Self {
                    (value.0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for sponsoredAccountCall {
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
            impl ::core::convert::From<sponsoredAccountReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: sponsoredAccountReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for sponsoredAccountReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for sponsoredAccountCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Address,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = bool;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "sponsoredAccount(address)";
            const SELECTOR: [u8; 4] = [205u8, 208u8, 193u8, 45u8];
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
                        let r: sponsoredAccountReturn = r.into();
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
                        let r: sponsoredAccountReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `supportsInterface(bytes4)` and selector `0x01ffc9a7`.
```solidity
function supportsInterface(bytes4 interfaceId) external pure returns (bool);
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
    /**Function with signature `transfer(address,uint256,uint256)` and selector `0x095bcdb6`.
```solidity
function transfer(address, uint256, uint256) external pure returns (bool);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct transferCall {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub _1: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub _2: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`transfer(address,uint256,uint256)`](transferCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct transferReturn {
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
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
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
            impl ::core::convert::From<transferCall> for UnderlyingRustTuple<'_> {
                fn from(value: transferCall) -> Self {
                    (value._0, value._1, value._2)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for transferCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        _0: tuple.0,
                        _1: tuple.1,
                        _2: tuple.2,
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
            impl ::core::convert::From<transferReturn> for UnderlyingRustTuple<'_> {
                fn from(value: transferReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for transferReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for transferCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = bool;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "transfer(address,uint256,uint256)";
            const SELECTOR: [u8; 4] = [9u8, 91u8, 205u8, 182u8];
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
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self._1),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self._2),
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
                        let r: transferReturn = r.into();
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
                        let r: transferReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `transferFrom(address,address,uint256,uint256)` and selector `0xfe99049a`.
```solidity
function transferFrom(address, address, uint256, uint256) external pure returns (bool);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct transferFromCall {
        #[allow(missing_docs)]
        pub _0: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub _1: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub _2: alloy::sol_types::private::primitives::aliases::U256,
        #[allow(missing_docs)]
        pub _3: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`transferFrom(address,address,uint256,uint256)`](transferFromCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct transferFromReturn {
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
            impl ::core::convert::From<transferFromCall> for UnderlyingRustTuple<'_> {
                fn from(value: transferFromCall) -> Self {
                    (value._0, value._1, value._2, value._3)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for transferFromCall {
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
            impl ::core::convert::From<transferFromReturn> for UnderlyingRustTuple<'_> {
                fn from(value: transferFromReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for transferFromReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for transferFromCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = bool;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "transferFrom(address,address,uint256,uint256)";
            const SELECTOR: [u8; 4] = [254u8, 153u8, 4u8, 154u8];
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
                        let r: transferFromReturn = r.into();
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
                        let r: transferFromReturn = r.into();
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
    /**Function with signature `trustedDelegate(address)` and selector `0x1ac3e310`.
```solidity
function trustedDelegate(address) external view returns (bool);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct trustedDelegateCall(pub alloy::sol_types::private::Address);
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`trustedDelegate(address)`](trustedDelegateCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct trustedDelegateReturn {
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
            impl ::core::convert::From<trustedDelegateCall> for UnderlyingRustTuple<'_> {
                fn from(value: trustedDelegateCall) -> Self {
                    (value.0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for trustedDelegateCall {
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
            impl ::core::convert::From<trustedDelegateReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: trustedDelegateReturn) -> Self {
                    (value._0,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for trustedDelegateReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { _0: tuple.0 }
                }
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for trustedDelegateCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Address,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = bool;
            type ReturnTuple<'a> = (alloy::sol_types::sol_data::Bool,);
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "trustedDelegate(address)";
            const SELECTOR: [u8; 4] = [26u8, 195u8, 227u8, 16u8];
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
                        let r: trustedDelegateReturn = r.into();
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
                        let r: trustedDelegateReturn = r.into();
                        r._0
                    })
            }
        }
    };
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    /**Function with signature `unlockStake()` and selector `0xbb9fe6bf`.
```solidity
function unlockStake() external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct unlockStakeCall;
    ///Container type for the return parameters of the [`unlockStake()`](unlockStakeCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct unlockStakeReturn {}
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
            impl ::core::convert::From<unlockStakeCall> for UnderlyingRustTuple<'_> {
                fn from(value: unlockStakeCall) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for unlockStakeCall {
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
            impl ::core::convert::From<unlockStakeReturn> for UnderlyingRustTuple<'_> {
                fn from(value: unlockStakeReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for unlockStakeReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl unlockStakeReturn {
            fn _tokenize(
                &self,
            ) -> <unlockStakeCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for unlockStakeCall {
            type Parameters<'a> = ();
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = unlockStakeReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "unlockStake()";
            const SELECTOR: [u8; 4] = [187u8, 159u8, 230u8, 191u8];
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
                unlockStakeReturn::_tokenize(ret)
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
    /**Function with signature `validatePaymasterUserOp((address,uint256,bytes,bytes,bytes32,uint256,bytes32,bytes,bytes),bytes32,uint256)` and selector `0x52b7512c`.
```solidity
function validatePaymasterUserOp(PackedUserOperation memory userOp, bytes32 userOpHash, uint256 maxCost) external returns (bytes memory context, uint256 validationData);
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct validatePaymasterUserOpCall {
        #[allow(missing_docs)]
        pub userOp: <PackedUserOperation as alloy::sol_types::SolType>::RustType,
        #[allow(missing_docs)]
        pub userOpHash: alloy::sol_types::private::FixedBytes<32>,
        #[allow(missing_docs)]
        pub maxCost: alloy::sol_types::private::primitives::aliases::U256,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Default, Debug, PartialEq, Eq, Hash)]
    ///Container type for the return parameters of the [`validatePaymasterUserOp((address,uint256,bytes,bytes,bytes32,uint256,bytes32,bytes,bytes),bytes32,uint256)`](validatePaymasterUserOpCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct validatePaymasterUserOpReturn {
        #[allow(missing_docs)]
        pub context: alloy::sol_types::private::Bytes,
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
            impl ::core::convert::From<validatePaymasterUserOpCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: validatePaymasterUserOpCall) -> Self {
                    (value.userOp, value.userOpHash, value.maxCost)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for validatePaymasterUserOpCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        userOp: tuple.0,
                        userOpHash: tuple.1,
                        maxCost: tuple.2,
                    }
                }
            }
        }
        {
            #[doc(hidden)]
            #[allow(dead_code)]
            type UnderlyingSolTuple<'a> = (
                alloy::sol_types::sol_data::Bytes,
                alloy::sol_types::sol_data::Uint<256>,
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
                alloy::sol_types::private::Bytes,
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
            impl ::core::convert::From<validatePaymasterUserOpReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: validatePaymasterUserOpReturn) -> Self {
                    (value.context, value.validationData)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for validatePaymasterUserOpReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        context: tuple.0,
                        validationData: tuple.1,
                    }
                }
            }
        }
        impl validatePaymasterUserOpReturn {
            fn _tokenize(
                &self,
            ) -> <validatePaymasterUserOpCall as alloy_sol_types::SolCall>::ReturnToken<
                '_,
            > {
                (
                    <alloy::sol_types::sol_data::Bytes as alloy_sol_types::SolType>::tokenize(
                        &self.context,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.validationData),
                )
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for validatePaymasterUserOpCall {
            type Parameters<'a> = (
                PackedUserOperation,
                alloy::sol_types::sol_data::FixedBytes<32>,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = validatePaymasterUserOpReturn;
            type ReturnTuple<'a> = (
                alloy::sol_types::sol_data::Bytes,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "validatePaymasterUserOp((address,uint256,bytes,bytes,bytes32,uint256,bytes32,bytes,bytes),bytes32,uint256)";
            const SELECTOR: [u8; 4] = [82u8, 183u8, 81u8, 44u8];
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
                    > as alloy_sol_types::SolType>::tokenize(&self.maxCost),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                validatePaymasterUserOpReturn::_tokenize(ret)
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
    /**Function with signature `withdrawNativeBalance(address)` and selector `0xc9b6d2ba`.
```solidity
function withdrawNativeBalance(address to) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct withdrawNativeBalanceCall {
        #[allow(missing_docs)]
        pub to: alloy::sol_types::private::Address,
    }
    ///Container type for the return parameters of the [`withdrawNativeBalance(address)`](withdrawNativeBalanceCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct withdrawNativeBalanceReturn {}
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
            impl ::core::convert::From<withdrawNativeBalanceCall>
            for UnderlyingRustTuple<'_> {
                fn from(value: withdrawNativeBalanceCall) -> Self {
                    (value.to,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for withdrawNativeBalanceCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { to: tuple.0 }
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
            impl ::core::convert::From<withdrawNativeBalanceReturn>
            for UnderlyingRustTuple<'_> {
                fn from(value: withdrawNativeBalanceReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>>
            for withdrawNativeBalanceReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl withdrawNativeBalanceReturn {
            fn _tokenize(
                &self,
            ) -> <withdrawNativeBalanceCall as alloy_sol_types::SolCall>::ReturnToken<
                '_,
            > {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for withdrawNativeBalanceCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Address,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = withdrawNativeBalanceReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "withdrawNativeBalance(address)";
            const SELECTOR: [u8; 4] = [201u8, 182u8, 210u8, 186u8];
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
                        &self.to,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                withdrawNativeBalanceReturn::_tokenize(ret)
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
    /**Function with signature `withdrawStake(address)` and selector `0xc23a5cea`.
```solidity
function withdrawStake(address withdrawAddress) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct withdrawStakeCall {
        #[allow(missing_docs)]
        pub withdrawAddress: alloy::sol_types::private::Address,
    }
    ///Container type for the return parameters of the [`withdrawStake(address)`](withdrawStakeCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct withdrawStakeReturn {}
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
            impl ::core::convert::From<withdrawStakeCall> for UnderlyingRustTuple<'_> {
                fn from(value: withdrawStakeCall) -> Self {
                    (value.withdrawAddress,)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for withdrawStakeCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self { withdrawAddress: tuple.0 }
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
            impl ::core::convert::From<withdrawStakeReturn> for UnderlyingRustTuple<'_> {
                fn from(value: withdrawStakeReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for withdrawStakeReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl withdrawStakeReturn {
            fn _tokenize(
                &self,
            ) -> <withdrawStakeCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for withdrawStakeCall {
            type Parameters<'a> = (alloy::sol_types::sol_data::Address,);
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = withdrawStakeReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "withdrawStake(address)";
            const SELECTOR: [u8; 4] = [194u8, 58u8, 92u8, 234u8];
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
                        &self.withdrawAddress,
                    ),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                withdrawStakeReturn::_tokenize(ret)
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
    /**Function with signature `withdrawTo(address,uint256)` and selector `0x205c2878`.
```solidity
function withdrawTo(address withdrawAddress, uint256 amount) external;
```*/
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct withdrawToCall {
        #[allow(missing_docs)]
        pub withdrawAddress: alloy::sol_types::private::Address,
        #[allow(missing_docs)]
        pub amount: alloy::sol_types::private::primitives::aliases::U256,
    }
    ///Container type for the return parameters of the [`withdrawTo(address,uint256)`](withdrawToCall) function.
    #[allow(non_camel_case_types, non_snake_case, clippy::pub_underscore_fields)]
    #[derive(Clone)]
    pub struct withdrawToReturn {}
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
            );
            #[doc(hidden)]
            type UnderlyingRustTuple<'a> = (
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
            impl ::core::convert::From<withdrawToCall> for UnderlyingRustTuple<'_> {
                fn from(value: withdrawToCall) -> Self {
                    (value.withdrawAddress, value.amount)
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for withdrawToCall {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {
                        withdrawAddress: tuple.0,
                        amount: tuple.1,
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
            impl ::core::convert::From<withdrawToReturn> for UnderlyingRustTuple<'_> {
                fn from(value: withdrawToReturn) -> Self {
                    ()
                }
            }
            #[automatically_derived]
            #[doc(hidden)]
            impl ::core::convert::From<UnderlyingRustTuple<'_>> for withdrawToReturn {
                fn from(tuple: UnderlyingRustTuple<'_>) -> Self {
                    Self {}
                }
            }
        }
        impl withdrawToReturn {
            fn _tokenize(
                &self,
            ) -> <withdrawToCall as alloy_sol_types::SolCall>::ReturnToken<'_> {
                ()
            }
        }
        #[automatically_derived]
        impl alloy_sol_types::SolCall for withdrawToCall {
            type Parameters<'a> = (
                alloy::sol_types::sol_data::Address,
                alloy::sol_types::sol_data::Uint<256>,
            );
            type Token<'a> = <Self::Parameters<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            type Return = withdrawToReturn;
            type ReturnTuple<'a> = ();
            type ReturnToken<'a> = <Self::ReturnTuple<
                'a,
            > as alloy_sol_types::SolType>::Token<'a>;
            const SIGNATURE: &'static str = "withdrawTo(address,uint256)";
            const SELECTOR: [u8; 4] = [32u8, 92u8, 40u8, 120u8];
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
                        &self.withdrawAddress,
                    ),
                    <alloy::sol_types::sol_data::Uint<
                        256,
                    > as alloy_sol_types::SolType>::tokenize(&self.amount),
                )
            }
            #[inline]
            fn tokenize_returns(ret: &Self::Return) -> Self::ReturnToken<'_> {
                withdrawToReturn::_tokenize(ret)
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
    ///Container for all the [`MevPaymasterV9`](self) function calls.
    #[derive(Clone)]
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive()]
    pub enum MevPaymasterV9Calls {
        #[allow(missing_docs)]
        acceptOwnership(acceptOwnershipCall),
        #[allow(missing_docs)]
        addStake(addStakeCall),
        #[allow(missing_docs)]
        allowance(allowanceCall),
        #[allow(missing_docs)]
        approve(approveCall),
        #[allow(missing_docs)]
        balanceOf(balanceOfCall),
        #[allow(missing_docs)]
        budgets(budgetsCall),
        #[allow(missing_docs)]
        burnPoolEth(burnPoolEthCall),
        #[allow(missing_docs)]
        burnPoolToken(burnPoolTokenCall),
        #[allow(missing_docs)]
        creditPoolEth(creditPoolEthCall),
        #[allow(missing_docs)]
        creditPoolToken(creditPoolTokenCall),
        #[allow(missing_docs)]
        currentEpoch(currentEpochCall),
        #[allow(missing_docs)]
        deposit(depositCall),
        #[allow(missing_docs)]
        entryPoint(entryPointCall),
        #[allow(missing_docs)]
        epochLength(epochLengthCall),
        #[allow(missing_docs)]
        erc20Config(erc20ConfigCall),
        #[allow(missing_docs)]
        ethOracle(ethOracleCall),
        #[allow(missing_docs)]
        getDeposit(getDepositCall),
        #[allow(missing_docs)]
        isOperator(isOperatorCall),
        #[allow(missing_docs)]
        maxWeiPerEpoch(maxWeiPerEpochCall),
        #[allow(missing_docs)]
        owner(ownerCall),
        #[allow(missing_docs)]
        pause(pauseCall),
        #[allow(missing_docs)]
        paused(pausedCall),
        #[allow(missing_docs)]
        pendingOwner(pendingOwnerCall),
        #[allow(missing_docs)]
        poolEthBalance(poolEthBalanceCall),
        #[allow(missing_docs)]
        poolTokenBalance(poolTokenBalanceCall),
        #[allow(missing_docs)]
        postOp(postOpCall),
        #[allow(missing_docs)]
        remainingBudget(remainingBudgetCall),
        #[allow(missing_docs)]
        removeErc20Token(removeErc20TokenCall),
        #[allow(missing_docs)]
        renounceOwnership(renounceOwnershipCall),
        #[allow(missing_docs)]
        senderPool(senderPoolCall),
        #[allow(missing_docs)]
        setErc20Config(setErc20ConfigCall),
        #[allow(missing_docs)]
        setEthOracle(setEthOracleCall),
        #[allow(missing_docs)]
        setOperator(setOperatorCall),
        #[allow(missing_docs)]
        setSenderPool(setSenderPoolCall),
        #[allow(missing_docs)]
        setSponsored(setSponsoredCall),
        #[allow(missing_docs)]
        setTrustedDelegate(setTrustedDelegateCall),
        #[allow(missing_docs)]
        setTuning(setTuningCall),
        #[allow(missing_docs)]
        sponsoredAccount(sponsoredAccountCall),
        #[allow(missing_docs)]
        supportsInterface(supportsInterfaceCall),
        #[allow(missing_docs)]
        transfer(transferCall),
        #[allow(missing_docs)]
        transferFrom(transferFromCall),
        #[allow(missing_docs)]
        transferOwnership(transferOwnershipCall),
        #[allow(missing_docs)]
        trustedDelegate(trustedDelegateCall),
        #[allow(missing_docs)]
        unlockStake(unlockStakeCall),
        #[allow(missing_docs)]
        unpause(unpauseCall),
        #[allow(missing_docs)]
        validatePaymasterUserOp(validatePaymasterUserOpCall),
        #[allow(missing_docs)]
        withdrawNativeBalance(withdrawNativeBalanceCall),
        #[allow(missing_docs)]
        withdrawStake(withdrawStakeCall),
        #[allow(missing_docs)]
        withdrawTo(withdrawToCall),
    }
    impl MevPaymasterV9Calls {
        /// All the selectors of this enum.
        ///
        /// Note that the selectors might not be in the same order as the variants.
        /// No guarantees are made about the order of the selectors.
        ///
        /// Prefer using `SolInterface` methods instead.
        pub const SELECTORS: &'static [[u8; 4usize]] = &[
            [0u8, 253u8, 213u8, 142u8],
            [1u8, 255u8, 201u8, 167u8],
            [3u8, 150u8, 203u8, 96u8],
            [9u8, 91u8, 205u8, 182u8],
            [17u8, 13u8, 253u8, 146u8],
            [20u8, 126u8, 126u8, 102u8],
            [26u8, 195u8, 227u8, 16u8],
            [32u8, 92u8, 40u8, 120u8],
            [36u8, 145u8, 18u8, 244u8],
            [42u8, 137u8, 95u8, 53u8],
            [60u8, 123u8, 220u8, 234u8],
            [63u8, 75u8, 168u8, 58u8],
            [66u8, 106u8, 132u8, 147u8],
            [82u8, 183u8, 81u8, 44u8],
            [84u8, 162u8, 185u8, 57u8],
            [85u8, 138u8, 114u8, 151u8],
            [87u8, 215u8, 117u8, 248u8],
            [89u8, 138u8, 249u8, 231u8],
            [92u8, 151u8, 90u8, 187u8],
            [108u8, 161u8, 143u8, 198u8],
            [113u8, 80u8, 24u8, 166u8],
            [117u8, 203u8, 204u8, 167u8],
            [118u8, 103u8, 24u8, 8u8],
            [121u8, 186u8, 80u8, 151u8],
            [124u8, 98u8, 123u8, 33u8],
            [132u8, 86u8, 203u8, 89u8],
            [141u8, 165u8, 203u8, 91u8],
            [144u8, 164u8, 69u8, 14u8],
            [152u8, 0u8, 193u8, 5u8],
            [156u8, 1u8, 163u8, 206u8],
            [156u8, 135u8, 98u8, 225u8],
            [166u8, 205u8, 117u8, 220u8],
            [176u8, 214u8, 145u8, 254u8],
            [177u8, 197u8, 175u8, 119u8],
            [182u8, 54u8, 60u8, 242u8],
            [184u8, 155u8, 106u8, 30u8],
            [187u8, 159u8, 230u8, 191u8],
            [194u8, 58u8, 92u8, 234u8],
            [195u8, 153u8, 236u8, 136u8],
            [201u8, 182u8, 210u8, 186u8],
            [203u8, 86u8, 56u8, 215u8],
            [205u8, 208u8, 193u8, 45u8],
            [208u8, 227u8, 13u8, 176u8],
            [227u8, 12u8, 57u8, 120u8],
            [229u8, 193u8, 98u8, 62u8],
            [242u8, 253u8, 227u8, 139u8],
            [249u8, 53u8, 208u8, 176u8],
            [249u8, 175u8, 43u8, 242u8],
            [254u8, 153u8, 4u8, 154u8],
        ];
        /// The names of the variants in the same order as `SELECTORS`.
        pub const VARIANT_NAMES: &'static [&'static str] = &[
            ::core::stringify!(balanceOf),
            ::core::stringify!(supportsInterface),
            ::core::stringify!(addStake),
            ::core::stringify!(transfer),
            ::core::stringify!(creditPoolToken),
            ::core::stringify!(budgets),
            ::core::stringify!(trustedDelegate),
            ::core::stringify!(withdrawTo),
            ::core::stringify!(setSenderPool),
            ::core::stringify!(setErc20Config),
            ::core::stringify!(poolTokenBalance),
            ::core::stringify!(unpause),
            ::core::stringify!(approve),
            ::core::stringify!(validatePaymasterUserOp),
            ::core::stringify!(remainingBudget),
            ::core::stringify!(setOperator),
            ::core::stringify!(epochLength),
            ::core::stringify!(allowance),
            ::core::stringify!(paused),
            ::core::stringify!(setTuning),
            ::core::stringify!(renounceOwnership),
            ::core::stringify!(burnPoolEth),
            ::core::stringify!(currentEpoch),
            ::core::stringify!(acceptOwnership),
            ::core::stringify!(postOp),
            ::core::stringify!(pause),
            ::core::stringify!(owner),
            ::core::stringify!(burnPoolToken),
            ::core::stringify!(removeErc20Token),
            ::core::stringify!(maxWeiPerEpoch),
            ::core::stringify!(ethOracle),
            ::core::stringify!(setEthOracle),
            ::core::stringify!(entryPoint),
            ::core::stringify!(setTrustedDelegate),
            ::core::stringify!(isOperator),
            ::core::stringify!(creditPoolEth),
            ::core::stringify!(unlockStake),
            ::core::stringify!(withdrawStake),
            ::core::stringify!(getDeposit),
            ::core::stringify!(withdrawNativeBalance),
            ::core::stringify!(poolEthBalance),
            ::core::stringify!(sponsoredAccount),
            ::core::stringify!(deposit),
            ::core::stringify!(pendingOwner),
            ::core::stringify!(senderPool),
            ::core::stringify!(transferOwnership),
            ::core::stringify!(setSponsored),
            ::core::stringify!(erc20Config),
            ::core::stringify!(transferFrom),
        ];
        /// The signatures in the same order as `SELECTORS`.
        pub const SIGNATURES: &'static [&'static str] = &[
            <balanceOfCall as alloy_sol_types::SolCall>::SIGNATURE,
            <supportsInterfaceCall as alloy_sol_types::SolCall>::SIGNATURE,
            <addStakeCall as alloy_sol_types::SolCall>::SIGNATURE,
            <transferCall as alloy_sol_types::SolCall>::SIGNATURE,
            <creditPoolTokenCall as alloy_sol_types::SolCall>::SIGNATURE,
            <budgetsCall as alloy_sol_types::SolCall>::SIGNATURE,
            <trustedDelegateCall as alloy_sol_types::SolCall>::SIGNATURE,
            <withdrawToCall as alloy_sol_types::SolCall>::SIGNATURE,
            <setSenderPoolCall as alloy_sol_types::SolCall>::SIGNATURE,
            <setErc20ConfigCall as alloy_sol_types::SolCall>::SIGNATURE,
            <poolTokenBalanceCall as alloy_sol_types::SolCall>::SIGNATURE,
            <unpauseCall as alloy_sol_types::SolCall>::SIGNATURE,
            <approveCall as alloy_sol_types::SolCall>::SIGNATURE,
            <validatePaymasterUserOpCall as alloy_sol_types::SolCall>::SIGNATURE,
            <remainingBudgetCall as alloy_sol_types::SolCall>::SIGNATURE,
            <setOperatorCall as alloy_sol_types::SolCall>::SIGNATURE,
            <epochLengthCall as alloy_sol_types::SolCall>::SIGNATURE,
            <allowanceCall as alloy_sol_types::SolCall>::SIGNATURE,
            <pausedCall as alloy_sol_types::SolCall>::SIGNATURE,
            <setTuningCall as alloy_sol_types::SolCall>::SIGNATURE,
            <renounceOwnershipCall as alloy_sol_types::SolCall>::SIGNATURE,
            <burnPoolEthCall as alloy_sol_types::SolCall>::SIGNATURE,
            <currentEpochCall as alloy_sol_types::SolCall>::SIGNATURE,
            <acceptOwnershipCall as alloy_sol_types::SolCall>::SIGNATURE,
            <postOpCall as alloy_sol_types::SolCall>::SIGNATURE,
            <pauseCall as alloy_sol_types::SolCall>::SIGNATURE,
            <ownerCall as alloy_sol_types::SolCall>::SIGNATURE,
            <burnPoolTokenCall as alloy_sol_types::SolCall>::SIGNATURE,
            <removeErc20TokenCall as alloy_sol_types::SolCall>::SIGNATURE,
            <maxWeiPerEpochCall as alloy_sol_types::SolCall>::SIGNATURE,
            <ethOracleCall as alloy_sol_types::SolCall>::SIGNATURE,
            <setEthOracleCall as alloy_sol_types::SolCall>::SIGNATURE,
            <entryPointCall as alloy_sol_types::SolCall>::SIGNATURE,
            <setTrustedDelegateCall as alloy_sol_types::SolCall>::SIGNATURE,
            <isOperatorCall as alloy_sol_types::SolCall>::SIGNATURE,
            <creditPoolEthCall as alloy_sol_types::SolCall>::SIGNATURE,
            <unlockStakeCall as alloy_sol_types::SolCall>::SIGNATURE,
            <withdrawStakeCall as alloy_sol_types::SolCall>::SIGNATURE,
            <getDepositCall as alloy_sol_types::SolCall>::SIGNATURE,
            <withdrawNativeBalanceCall as alloy_sol_types::SolCall>::SIGNATURE,
            <poolEthBalanceCall as alloy_sol_types::SolCall>::SIGNATURE,
            <sponsoredAccountCall as alloy_sol_types::SolCall>::SIGNATURE,
            <depositCall as alloy_sol_types::SolCall>::SIGNATURE,
            <pendingOwnerCall as alloy_sol_types::SolCall>::SIGNATURE,
            <senderPoolCall as alloy_sol_types::SolCall>::SIGNATURE,
            <transferOwnershipCall as alloy_sol_types::SolCall>::SIGNATURE,
            <setSponsoredCall as alloy_sol_types::SolCall>::SIGNATURE,
            <erc20ConfigCall as alloy_sol_types::SolCall>::SIGNATURE,
            <transferFromCall as alloy_sol_types::SolCall>::SIGNATURE,
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
    impl alloy_sol_types::SolInterface for MevPaymasterV9Calls {
        const NAME: &'static str = "MevPaymasterV9Calls";
        const MIN_DATA_LENGTH: usize = 0usize;
        const COUNT: usize = 49usize;
        #[inline]
        fn selector(&self) -> [u8; 4] {
            match self {
                Self::acceptOwnership(_) => {
                    <acceptOwnershipCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::addStake(_) => <addStakeCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::allowance(_) => {
                    <allowanceCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::approve(_) => <approveCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::balanceOf(_) => {
                    <balanceOfCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::budgets(_) => <budgetsCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::burnPoolEth(_) => {
                    <burnPoolEthCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::burnPoolToken(_) => {
                    <burnPoolTokenCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::creditPoolEth(_) => {
                    <creditPoolEthCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::creditPoolToken(_) => {
                    <creditPoolTokenCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::currentEpoch(_) => {
                    <currentEpochCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::deposit(_) => <depositCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::entryPoint(_) => {
                    <entryPointCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::epochLength(_) => {
                    <epochLengthCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::erc20Config(_) => {
                    <erc20ConfigCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::ethOracle(_) => {
                    <ethOracleCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::getDeposit(_) => {
                    <getDepositCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::isOperator(_) => {
                    <isOperatorCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::maxWeiPerEpoch(_) => {
                    <maxWeiPerEpochCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::owner(_) => <ownerCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::pause(_) => <pauseCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::paused(_) => <pausedCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::pendingOwner(_) => {
                    <pendingOwnerCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::poolEthBalance(_) => {
                    <poolEthBalanceCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::poolTokenBalance(_) => {
                    <poolTokenBalanceCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::postOp(_) => <postOpCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::remainingBudget(_) => {
                    <remainingBudgetCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::removeErc20Token(_) => {
                    <removeErc20TokenCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::renounceOwnership(_) => {
                    <renounceOwnershipCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::senderPool(_) => {
                    <senderPoolCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::setErc20Config(_) => {
                    <setErc20ConfigCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::setEthOracle(_) => {
                    <setEthOracleCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::setOperator(_) => {
                    <setOperatorCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::setSenderPool(_) => {
                    <setSenderPoolCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::setSponsored(_) => {
                    <setSponsoredCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::setTrustedDelegate(_) => {
                    <setTrustedDelegateCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::setTuning(_) => {
                    <setTuningCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::sponsoredAccount(_) => {
                    <sponsoredAccountCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::supportsInterface(_) => {
                    <supportsInterfaceCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::transfer(_) => <transferCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::transferFrom(_) => {
                    <transferFromCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::transferOwnership(_) => {
                    <transferOwnershipCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::trustedDelegate(_) => {
                    <trustedDelegateCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::unlockStake(_) => {
                    <unlockStakeCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::unpause(_) => <unpauseCall as alloy_sol_types::SolCall>::SELECTOR,
                Self::validatePaymasterUserOp(_) => {
                    <validatePaymasterUserOpCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::withdrawNativeBalance(_) => {
                    <withdrawNativeBalanceCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::withdrawStake(_) => {
                    <withdrawStakeCall as alloy_sol_types::SolCall>::SELECTOR
                }
                Self::withdrawTo(_) => {
                    <withdrawToCall as alloy_sol_types::SolCall>::SELECTOR
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
            ) -> alloy_sol_types::Result<MevPaymasterV9Calls>] = &[
                {
                    fn balanceOf(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <balanceOfCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevPaymasterV9Calls::balanceOf)
                    }
                    balanceOf
                },
                {
                    fn supportsInterface(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <supportsInterfaceCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::supportsInterface)
                    }
                    supportsInterface
                },
                {
                    fn addStake(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <addStakeCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevPaymasterV9Calls::addStake)
                    }
                    addStake
                },
                {
                    fn transfer(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <transferCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevPaymasterV9Calls::transfer)
                    }
                    transfer
                },
                {
                    fn creditPoolToken(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <creditPoolTokenCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::creditPoolToken)
                    }
                    creditPoolToken
                },
                {
                    fn budgets(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <budgetsCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevPaymasterV9Calls::budgets)
                    }
                    budgets
                },
                {
                    fn trustedDelegate(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <trustedDelegateCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::trustedDelegate)
                    }
                    trustedDelegate
                },
                {
                    fn withdrawTo(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <withdrawToCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::withdrawTo)
                    }
                    withdrawTo
                },
                {
                    fn setSenderPool(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <setSenderPoolCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::setSenderPool)
                    }
                    setSenderPool
                },
                {
                    fn setErc20Config(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <setErc20ConfigCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::setErc20Config)
                    }
                    setErc20Config
                },
                {
                    fn poolTokenBalance(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <poolTokenBalanceCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::poolTokenBalance)
                    }
                    poolTokenBalance
                },
                {
                    fn unpause(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <unpauseCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevPaymasterV9Calls::unpause)
                    }
                    unpause
                },
                {
                    fn approve(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <approveCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevPaymasterV9Calls::approve)
                    }
                    approve
                },
                {
                    fn validatePaymasterUserOp(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <validatePaymasterUserOpCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::validatePaymasterUserOp)
                    }
                    validatePaymasterUserOp
                },
                {
                    fn remainingBudget(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <remainingBudgetCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::remainingBudget)
                    }
                    remainingBudget
                },
                {
                    fn setOperator(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <setOperatorCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::setOperator)
                    }
                    setOperator
                },
                {
                    fn epochLength(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <epochLengthCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::epochLength)
                    }
                    epochLength
                },
                {
                    fn allowance(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <allowanceCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevPaymasterV9Calls::allowance)
                    }
                    allowance
                },
                {
                    fn paused(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <pausedCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevPaymasterV9Calls::paused)
                    }
                    paused
                },
                {
                    fn setTuning(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <setTuningCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevPaymasterV9Calls::setTuning)
                    }
                    setTuning
                },
                {
                    fn renounceOwnership(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <renounceOwnershipCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::renounceOwnership)
                    }
                    renounceOwnership
                },
                {
                    fn burnPoolEth(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <burnPoolEthCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::burnPoolEth)
                    }
                    burnPoolEth
                },
                {
                    fn currentEpoch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <currentEpochCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::currentEpoch)
                    }
                    currentEpoch
                },
                {
                    fn acceptOwnership(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <acceptOwnershipCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::acceptOwnership)
                    }
                    acceptOwnership
                },
                {
                    fn postOp(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <postOpCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevPaymasterV9Calls::postOp)
                    }
                    postOp
                },
                {
                    fn pause(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <pauseCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevPaymasterV9Calls::pause)
                    }
                    pause
                },
                {
                    fn owner(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <ownerCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevPaymasterV9Calls::owner)
                    }
                    owner
                },
                {
                    fn burnPoolToken(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <burnPoolTokenCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::burnPoolToken)
                    }
                    burnPoolToken
                },
                {
                    fn removeErc20Token(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <removeErc20TokenCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::removeErc20Token)
                    }
                    removeErc20Token
                },
                {
                    fn maxWeiPerEpoch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <maxWeiPerEpochCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::maxWeiPerEpoch)
                    }
                    maxWeiPerEpoch
                },
                {
                    fn ethOracle(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <ethOracleCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevPaymasterV9Calls::ethOracle)
                    }
                    ethOracle
                },
                {
                    fn setEthOracle(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <setEthOracleCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::setEthOracle)
                    }
                    setEthOracle
                },
                {
                    fn entryPoint(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <entryPointCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::entryPoint)
                    }
                    entryPoint
                },
                {
                    fn setTrustedDelegate(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <setTrustedDelegateCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::setTrustedDelegate)
                    }
                    setTrustedDelegate
                },
                {
                    fn isOperator(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <isOperatorCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::isOperator)
                    }
                    isOperator
                },
                {
                    fn creditPoolEth(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <creditPoolEthCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::creditPoolEth)
                    }
                    creditPoolEth
                },
                {
                    fn unlockStake(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <unlockStakeCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::unlockStake)
                    }
                    unlockStake
                },
                {
                    fn withdrawStake(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <withdrawStakeCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::withdrawStake)
                    }
                    withdrawStake
                },
                {
                    fn getDeposit(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <getDepositCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::getDeposit)
                    }
                    getDeposit
                },
                {
                    fn withdrawNativeBalance(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <withdrawNativeBalanceCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::withdrawNativeBalance)
                    }
                    withdrawNativeBalance
                },
                {
                    fn poolEthBalance(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <poolEthBalanceCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::poolEthBalance)
                    }
                    poolEthBalance
                },
                {
                    fn sponsoredAccount(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <sponsoredAccountCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::sponsoredAccount)
                    }
                    sponsoredAccount
                },
                {
                    fn deposit(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <depositCall as alloy_sol_types::SolCall>::abi_decode_raw(data)
                            .map(MevPaymasterV9Calls::deposit)
                    }
                    deposit
                },
                {
                    fn pendingOwner(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <pendingOwnerCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::pendingOwner)
                    }
                    pendingOwner
                },
                {
                    fn senderPool(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <senderPoolCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::senderPool)
                    }
                    senderPool
                },
                {
                    fn transferOwnership(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <transferOwnershipCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::transferOwnership)
                    }
                    transferOwnership
                },
                {
                    fn setSponsored(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <setSponsoredCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::setSponsored)
                    }
                    setSponsored
                },
                {
                    fn erc20Config(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <erc20ConfigCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::erc20Config)
                    }
                    erc20Config
                },
                {
                    fn transferFrom(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <transferFromCall as alloy_sol_types::SolCall>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Calls::transferFrom)
                    }
                    transferFrom
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
            ) -> alloy_sol_types::Result<MevPaymasterV9Calls>] = &[
                {
                    fn balanceOf(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <balanceOfCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::balanceOf)
                    }
                    balanceOf
                },
                {
                    fn supportsInterface(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <supportsInterfaceCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::supportsInterface)
                    }
                    supportsInterface
                },
                {
                    fn addStake(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <addStakeCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::addStake)
                    }
                    addStake
                },
                {
                    fn transfer(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <transferCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::transfer)
                    }
                    transfer
                },
                {
                    fn creditPoolToken(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <creditPoolTokenCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::creditPoolToken)
                    }
                    creditPoolToken
                },
                {
                    fn budgets(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <budgetsCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::budgets)
                    }
                    budgets
                },
                {
                    fn trustedDelegate(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <trustedDelegateCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::trustedDelegate)
                    }
                    trustedDelegate
                },
                {
                    fn withdrawTo(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <withdrawToCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::withdrawTo)
                    }
                    withdrawTo
                },
                {
                    fn setSenderPool(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <setSenderPoolCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::setSenderPool)
                    }
                    setSenderPool
                },
                {
                    fn setErc20Config(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <setErc20ConfigCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::setErc20Config)
                    }
                    setErc20Config
                },
                {
                    fn poolTokenBalance(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <poolTokenBalanceCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::poolTokenBalance)
                    }
                    poolTokenBalance
                },
                {
                    fn unpause(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <unpauseCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::unpause)
                    }
                    unpause
                },
                {
                    fn approve(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <approveCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::approve)
                    }
                    approve
                },
                {
                    fn validatePaymasterUserOp(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <validatePaymasterUserOpCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::validatePaymasterUserOp)
                    }
                    validatePaymasterUserOp
                },
                {
                    fn remainingBudget(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <remainingBudgetCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::remainingBudget)
                    }
                    remainingBudget
                },
                {
                    fn setOperator(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <setOperatorCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::setOperator)
                    }
                    setOperator
                },
                {
                    fn epochLength(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <epochLengthCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::epochLength)
                    }
                    epochLength
                },
                {
                    fn allowance(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <allowanceCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::allowance)
                    }
                    allowance
                },
                {
                    fn paused(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <pausedCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::paused)
                    }
                    paused
                },
                {
                    fn setTuning(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <setTuningCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::setTuning)
                    }
                    setTuning
                },
                {
                    fn renounceOwnership(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <renounceOwnershipCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::renounceOwnership)
                    }
                    renounceOwnership
                },
                {
                    fn burnPoolEth(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <burnPoolEthCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::burnPoolEth)
                    }
                    burnPoolEth
                },
                {
                    fn currentEpoch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <currentEpochCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::currentEpoch)
                    }
                    currentEpoch
                },
                {
                    fn acceptOwnership(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <acceptOwnershipCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::acceptOwnership)
                    }
                    acceptOwnership
                },
                {
                    fn postOp(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <postOpCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::postOp)
                    }
                    postOp
                },
                {
                    fn pause(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <pauseCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::pause)
                    }
                    pause
                },
                {
                    fn owner(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <ownerCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::owner)
                    }
                    owner
                },
                {
                    fn burnPoolToken(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <burnPoolTokenCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::burnPoolToken)
                    }
                    burnPoolToken
                },
                {
                    fn removeErc20Token(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <removeErc20TokenCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::removeErc20Token)
                    }
                    removeErc20Token
                },
                {
                    fn maxWeiPerEpoch(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <maxWeiPerEpochCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::maxWeiPerEpoch)
                    }
                    maxWeiPerEpoch
                },
                {
                    fn ethOracle(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <ethOracleCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::ethOracle)
                    }
                    ethOracle
                },
                {
                    fn setEthOracle(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <setEthOracleCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::setEthOracle)
                    }
                    setEthOracle
                },
                {
                    fn entryPoint(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <entryPointCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::entryPoint)
                    }
                    entryPoint
                },
                {
                    fn setTrustedDelegate(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <setTrustedDelegateCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::setTrustedDelegate)
                    }
                    setTrustedDelegate
                },
                {
                    fn isOperator(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <isOperatorCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::isOperator)
                    }
                    isOperator
                },
                {
                    fn creditPoolEth(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <creditPoolEthCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::creditPoolEth)
                    }
                    creditPoolEth
                },
                {
                    fn unlockStake(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <unlockStakeCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::unlockStake)
                    }
                    unlockStake
                },
                {
                    fn withdrawStake(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <withdrawStakeCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::withdrawStake)
                    }
                    withdrawStake
                },
                {
                    fn getDeposit(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <getDepositCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::getDeposit)
                    }
                    getDeposit
                },
                {
                    fn withdrawNativeBalance(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <withdrawNativeBalanceCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::withdrawNativeBalance)
                    }
                    withdrawNativeBalance
                },
                {
                    fn poolEthBalance(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <poolEthBalanceCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::poolEthBalance)
                    }
                    poolEthBalance
                },
                {
                    fn sponsoredAccount(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <sponsoredAccountCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::sponsoredAccount)
                    }
                    sponsoredAccount
                },
                {
                    fn deposit(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <depositCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::deposit)
                    }
                    deposit
                },
                {
                    fn pendingOwner(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <pendingOwnerCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::pendingOwner)
                    }
                    pendingOwner
                },
                {
                    fn senderPool(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <senderPoolCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::senderPool)
                    }
                    senderPool
                },
                {
                    fn transferOwnership(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <transferOwnershipCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::transferOwnership)
                    }
                    transferOwnership
                },
                {
                    fn setSponsored(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <setSponsoredCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::setSponsored)
                    }
                    setSponsored
                },
                {
                    fn erc20Config(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <erc20ConfigCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::erc20Config)
                    }
                    erc20Config
                },
                {
                    fn transferFrom(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Calls> {
                        <transferFromCall as alloy_sol_types::SolCall>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Calls::transferFrom)
                    }
                    transferFrom
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
                Self::acceptOwnership(inner) => {
                    <acceptOwnershipCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::addStake(inner) => {
                    <addStakeCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::allowance(inner) => {
                    <allowanceCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::approve(inner) => {
                    <approveCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::balanceOf(inner) => {
                    <balanceOfCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::budgets(inner) => {
                    <budgetsCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::burnPoolEth(inner) => {
                    <burnPoolEthCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::burnPoolToken(inner) => {
                    <burnPoolTokenCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::creditPoolEth(inner) => {
                    <creditPoolEthCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::creditPoolToken(inner) => {
                    <creditPoolTokenCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::currentEpoch(inner) => {
                    <currentEpochCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::deposit(inner) => {
                    <depositCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::entryPoint(inner) => {
                    <entryPointCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::epochLength(inner) => {
                    <epochLengthCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::erc20Config(inner) => {
                    <erc20ConfigCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::ethOracle(inner) => {
                    <ethOracleCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::getDeposit(inner) => {
                    <getDepositCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::isOperator(inner) => {
                    <isOperatorCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::maxWeiPerEpoch(inner) => {
                    <maxWeiPerEpochCall as alloy_sol_types::SolCall>::abi_encoded_size(
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
                Self::poolEthBalance(inner) => {
                    <poolEthBalanceCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::poolTokenBalance(inner) => {
                    <poolTokenBalanceCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::postOp(inner) => {
                    <postOpCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::remainingBudget(inner) => {
                    <remainingBudgetCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::removeErc20Token(inner) => {
                    <removeErc20TokenCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::renounceOwnership(inner) => {
                    <renounceOwnershipCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::senderPool(inner) => {
                    <senderPoolCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::setErc20Config(inner) => {
                    <setErc20ConfigCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::setEthOracle(inner) => {
                    <setEthOracleCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::setOperator(inner) => {
                    <setOperatorCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::setSenderPool(inner) => {
                    <setSenderPoolCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::setSponsored(inner) => {
                    <setSponsoredCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::setTrustedDelegate(inner) => {
                    <setTrustedDelegateCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::setTuning(inner) => {
                    <setTuningCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::sponsoredAccount(inner) => {
                    <sponsoredAccountCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::supportsInterface(inner) => {
                    <supportsInterfaceCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::transfer(inner) => {
                    <transferCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::transferFrom(inner) => {
                    <transferFromCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::transferOwnership(inner) => {
                    <transferOwnershipCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::trustedDelegate(inner) => {
                    <trustedDelegateCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::unlockStake(inner) => {
                    <unlockStakeCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::unpause(inner) => {
                    <unpauseCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
                Self::validatePaymasterUserOp(inner) => {
                    <validatePaymasterUserOpCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::withdrawNativeBalance(inner) => {
                    <withdrawNativeBalanceCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::withdrawStake(inner) => {
                    <withdrawStakeCall as alloy_sol_types::SolCall>::abi_encoded_size(
                        inner,
                    )
                }
                Self::withdrawTo(inner) => {
                    <withdrawToCall as alloy_sol_types::SolCall>::abi_encoded_size(inner)
                }
            }
        }
        #[inline]
        fn abi_encode_raw(&self, out: &mut alloy_sol_types::private::Vec<u8>) {
            match self {
                Self::acceptOwnership(inner) => {
                    <acceptOwnershipCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::addStake(inner) => {
                    <addStakeCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::allowance(inner) => {
                    <allowanceCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::approve(inner) => {
                    <approveCall as alloy_sol_types::SolCall>::abi_encode_raw(inner, out)
                }
                Self::balanceOf(inner) => {
                    <balanceOfCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::budgets(inner) => {
                    <budgetsCall as alloy_sol_types::SolCall>::abi_encode_raw(inner, out)
                }
                Self::burnPoolEth(inner) => {
                    <burnPoolEthCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::burnPoolToken(inner) => {
                    <burnPoolTokenCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::creditPoolEth(inner) => {
                    <creditPoolEthCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::creditPoolToken(inner) => {
                    <creditPoolTokenCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::currentEpoch(inner) => {
                    <currentEpochCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::deposit(inner) => {
                    <depositCall as alloy_sol_types::SolCall>::abi_encode_raw(inner, out)
                }
                Self::entryPoint(inner) => {
                    <entryPointCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::epochLength(inner) => {
                    <epochLengthCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::erc20Config(inner) => {
                    <erc20ConfigCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::ethOracle(inner) => {
                    <ethOracleCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::getDeposit(inner) => {
                    <getDepositCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::isOperator(inner) => {
                    <isOperatorCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::maxWeiPerEpoch(inner) => {
                    <maxWeiPerEpochCall as alloy_sol_types::SolCall>::abi_encode_raw(
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
                Self::poolEthBalance(inner) => {
                    <poolEthBalanceCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::poolTokenBalance(inner) => {
                    <poolTokenBalanceCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::postOp(inner) => {
                    <postOpCall as alloy_sol_types::SolCall>::abi_encode_raw(inner, out)
                }
                Self::remainingBudget(inner) => {
                    <remainingBudgetCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::removeErc20Token(inner) => {
                    <removeErc20TokenCall as alloy_sol_types::SolCall>::abi_encode_raw(
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
                Self::senderPool(inner) => {
                    <senderPoolCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::setErc20Config(inner) => {
                    <setErc20ConfigCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::setEthOracle(inner) => {
                    <setEthOracleCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::setOperator(inner) => {
                    <setOperatorCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::setSenderPool(inner) => {
                    <setSenderPoolCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::setSponsored(inner) => {
                    <setSponsoredCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::setTrustedDelegate(inner) => {
                    <setTrustedDelegateCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::setTuning(inner) => {
                    <setTuningCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::sponsoredAccount(inner) => {
                    <sponsoredAccountCall as alloy_sol_types::SolCall>::abi_encode_raw(
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
                Self::transfer(inner) => {
                    <transferCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::transferFrom(inner) => {
                    <transferFromCall as alloy_sol_types::SolCall>::abi_encode_raw(
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
                Self::trustedDelegate(inner) => {
                    <trustedDelegateCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::unlockStake(inner) => {
                    <unlockStakeCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::unpause(inner) => {
                    <unpauseCall as alloy_sol_types::SolCall>::abi_encode_raw(inner, out)
                }
                Self::validatePaymasterUserOp(inner) => {
                    <validatePaymasterUserOpCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::withdrawNativeBalance(inner) => {
                    <withdrawNativeBalanceCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::withdrawStake(inner) => {
                    <withdrawStakeCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::withdrawTo(inner) => {
                    <withdrawToCall as alloy_sol_types::SolCall>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
            }
        }
    }
    ///Container for all the [`MevPaymasterV9`](self) custom errors.
    #[derive(Clone)]
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub enum MevPaymasterV9Errors {
        #[allow(missing_docs)]
        DivisionByZero(DivisionByZero),
        #[allow(missing_docs)]
        ERC165Error(ERC165Error),
        #[allow(missing_docs)]
        Eip7702SenderNotDelegate(Eip7702SenderNotDelegate),
        #[allow(missing_docs)]
        Eip7702SenderWithoutCode(Eip7702SenderWithoutCode),
        #[allow(missing_docs)]
        EnforcedPause(EnforcedPause),
        #[allow(missing_docs)]
        EpochBudgetExceeded(EpochBudgetExceeded),
        #[allow(missing_docs)]
        Erc20InsufficientAllowance(Erc20InsufficientAllowance),
        #[allow(missing_docs)]
        Erc20InvalidConfig(Erc20InvalidConfig),
        #[allow(missing_docs)]
        Erc20MaxAmountExceeded(Erc20MaxAmountExceeded),
        #[allow(missing_docs)]
        Erc20OracleNotSet(Erc20OracleNotSet),
        #[allow(missing_docs)]
        Erc20PaymasterDataInvalid(Erc20PaymasterDataInvalid),
        #[allow(missing_docs)]
        Erc20PriceInvalid(Erc20PriceInvalid),
        #[allow(missing_docs)]
        Erc20PriceStale(Erc20PriceStale),
        #[allow(missing_docs)]
        Erc20TokenNotEnabled(Erc20TokenNotEnabled),
        #[allow(missing_docs)]
        ExpectedPause(ExpectedPause),
        #[allow(missing_docs)]
        InvalidParams(InvalidParams),
        #[allow(missing_docs)]
        InvalidPoolId(InvalidPoolId),
        #[allow(missing_docs)]
        MevPaymaster__SenderNotSponsored(MevPaymaster__SenderNotSponsored),
        #[allow(missing_docs)]
        MevPaymaster__UnexpectedEntryPoint(MevPaymaster__UnexpectedEntryPoint),
        #[allow(missing_docs)]
        MevPaymaster__UntrustedDelegate(MevPaymaster__UntrustedDelegate),
        #[allow(missing_docs)]
        MustOverride(MustOverride),
        #[allow(missing_docs)]
        NativeBalanceWithdrawFailed(NativeBalanceWithdrawFailed),
        #[allow(missing_docs)]
        NotFromEntryPoint(NotFromEntryPoint),
        #[allow(missing_docs)]
        OwnableInvalidOwner(OwnableInvalidOwner),
        #[allow(missing_docs)]
        OwnableUnauthorizedAccount(OwnableUnauthorizedAccount),
        #[allow(missing_docs)]
        PaymasterPaused(PaymasterPaused),
        #[allow(missing_docs)]
        PoolEthBalanceInsufficient(PoolEthBalanceInsufficient),
        #[allow(missing_docs)]
        PoolTokenBalanceInsufficient(PoolTokenBalanceInsufficient),
        #[allow(missing_docs)]
        UnsupportedOperation(UnsupportedOperation),
    }
    impl MevPaymasterV9Errors {
        /// All the selectors of this enum.
        ///
        /// Note that the selectors might not be in the same order as the variants.
        /// No guarantees are made about the order of the selectors.
        ///
        /// Prefer using `SolInterface` methods instead.
        pub const SELECTORS: &'static [[u8; 4usize]] = &[
            [1u8, 143u8, 149u8, 245u8],
            [17u8, 140u8, 218u8, 167u8],
            [22u8, 198u8, 214u8, 214u8],
            [30u8, 79u8, 189u8, 247u8],
            [31u8, 148u8, 216u8, 163u8],
            [35u8, 211u8, 89u8, 163u8],
            [37u8, 173u8, 80u8, 31u8],
            [49u8, 10u8, 81u8, 124u8],
            [68u8, 68u8, 134u8, 158u8],
            [89u8, 38u8, 86u8, 81u8],
            [99u8, 25u8, 214u8, 171u8],
            [101u8, 210u8, 92u8, 113u8],
            [105u8, 24u8, 75u8, 119u8],
            [122u8, 242u8, 39u8, 79u8],
            [134u8, 183u8, 217u8, 169u8],
            [141u8, 252u8, 32u8, 43u8],
            [152u8, 208u8, 126u8, 204u8],
            [155u8, 166u8, 6u8, 27u8],
            [159u8, 78u8, 76u8, 201u8],
            [168u8, 107u8, 101u8, 18u8],
            [177u8, 137u8, 22u8, 216u8],
            [209u8, 150u8, 221u8, 132u8],
            [213u8, 49u8, 115u8, 125u8],
            [217u8, 60u8, 6u8, 101u8],
            [229u8, 129u8, 155u8, 149u8],
            [233u8, 242u8, 226u8, 243u8],
            [236u8, 10u8, 220u8, 51u8],
            [250u8, 199u8, 67u8, 111u8],
            [254u8, 52u8, 166u8, 211u8],
        ];
        /// The names of the variants in the same order as `SELECTORS`.
        pub const VARIANT_NAMES: &'static [&'static str] = &[
            ::core::stringify!(NativeBalanceWithdrawFailed),
            ::core::stringify!(OwnableUnauthorizedAccount),
            ::core::stringify!(EpochBudgetExceeded),
            ::core::stringify!(OwnableInvalidOwner),
            ::core::stringify!(Erc20OracleNotSet),
            ::core::stringify!(DivisionByZero),
            ::core::stringify!(MustOverride),
            ::core::stringify!(Erc20TokenNotEnabled),
            ::core::stringify!(Erc20PaymasterDataInvalid),
            ::core::stringify!(PoolEthBalanceInsufficient),
            ::core::stringify!(Erc20PriceInvalid),
            ::core::stringify!(ERC165Error),
            ::core::stringify!(Erc20InvalidConfig),
            ::core::stringify!(Erc20PriceStale),
            ::core::stringify!(Erc20InsufficientAllowance),
            ::core::stringify!(ExpectedPause),
            ::core::stringify!(PaymasterPaused),
            ::core::stringify!(UnsupportedOperation),
            ::core::stringify!(Eip7702SenderNotDelegate),
            ::core::stringify!(InvalidParams),
            ::core::stringify!(MevPaymaster__UnexpectedEntryPoint),
            ::core::stringify!(PoolTokenBalanceInsufficient),
            ::core::stringify!(InvalidPoolId),
            ::core::stringify!(EnforcedPause),
            ::core::stringify!(Eip7702SenderWithoutCode),
            ::core::stringify!(MevPaymaster__UntrustedDelegate),
            ::core::stringify!(MevPaymaster__SenderNotSponsored),
            ::core::stringify!(Erc20MaxAmountExceeded),
            ::core::stringify!(NotFromEntryPoint),
        ];
        /// The signatures in the same order as `SELECTORS`.
        pub const SIGNATURES: &'static [&'static str] = &[
            <NativeBalanceWithdrawFailed as alloy_sol_types::SolError>::SIGNATURE,
            <OwnableUnauthorizedAccount as alloy_sol_types::SolError>::SIGNATURE,
            <EpochBudgetExceeded as alloy_sol_types::SolError>::SIGNATURE,
            <OwnableInvalidOwner as alloy_sol_types::SolError>::SIGNATURE,
            <Erc20OracleNotSet as alloy_sol_types::SolError>::SIGNATURE,
            <DivisionByZero as alloy_sol_types::SolError>::SIGNATURE,
            <MustOverride as alloy_sol_types::SolError>::SIGNATURE,
            <Erc20TokenNotEnabled as alloy_sol_types::SolError>::SIGNATURE,
            <Erc20PaymasterDataInvalid as alloy_sol_types::SolError>::SIGNATURE,
            <PoolEthBalanceInsufficient as alloy_sol_types::SolError>::SIGNATURE,
            <Erc20PriceInvalid as alloy_sol_types::SolError>::SIGNATURE,
            <ERC165Error as alloy_sol_types::SolError>::SIGNATURE,
            <Erc20InvalidConfig as alloy_sol_types::SolError>::SIGNATURE,
            <Erc20PriceStale as alloy_sol_types::SolError>::SIGNATURE,
            <Erc20InsufficientAllowance as alloy_sol_types::SolError>::SIGNATURE,
            <ExpectedPause as alloy_sol_types::SolError>::SIGNATURE,
            <PaymasterPaused as alloy_sol_types::SolError>::SIGNATURE,
            <UnsupportedOperation as alloy_sol_types::SolError>::SIGNATURE,
            <Eip7702SenderNotDelegate as alloy_sol_types::SolError>::SIGNATURE,
            <InvalidParams as alloy_sol_types::SolError>::SIGNATURE,
            <MevPaymaster__UnexpectedEntryPoint as alloy_sol_types::SolError>::SIGNATURE,
            <PoolTokenBalanceInsufficient as alloy_sol_types::SolError>::SIGNATURE,
            <InvalidPoolId as alloy_sol_types::SolError>::SIGNATURE,
            <EnforcedPause as alloy_sol_types::SolError>::SIGNATURE,
            <Eip7702SenderWithoutCode as alloy_sol_types::SolError>::SIGNATURE,
            <MevPaymaster__UntrustedDelegate as alloy_sol_types::SolError>::SIGNATURE,
            <MevPaymaster__SenderNotSponsored as alloy_sol_types::SolError>::SIGNATURE,
            <Erc20MaxAmountExceeded as alloy_sol_types::SolError>::SIGNATURE,
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
    impl alloy_sol_types::SolInterface for MevPaymasterV9Errors {
        const NAME: &'static str = "MevPaymasterV9Errors";
        const MIN_DATA_LENGTH: usize = 0usize;
        const COUNT: usize = 29usize;
        #[inline]
        fn selector(&self) -> [u8; 4] {
            match self {
                Self::DivisionByZero(_) => {
                    <DivisionByZero as alloy_sol_types::SolError>::SELECTOR
                }
                Self::ERC165Error(_) => {
                    <ERC165Error as alloy_sol_types::SolError>::SELECTOR
                }
                Self::Eip7702SenderNotDelegate(_) => {
                    <Eip7702SenderNotDelegate as alloy_sol_types::SolError>::SELECTOR
                }
                Self::Eip7702SenderWithoutCode(_) => {
                    <Eip7702SenderWithoutCode as alloy_sol_types::SolError>::SELECTOR
                }
                Self::EnforcedPause(_) => {
                    <EnforcedPause as alloy_sol_types::SolError>::SELECTOR
                }
                Self::EpochBudgetExceeded(_) => {
                    <EpochBudgetExceeded as alloy_sol_types::SolError>::SELECTOR
                }
                Self::Erc20InsufficientAllowance(_) => {
                    <Erc20InsufficientAllowance as alloy_sol_types::SolError>::SELECTOR
                }
                Self::Erc20InvalidConfig(_) => {
                    <Erc20InvalidConfig as alloy_sol_types::SolError>::SELECTOR
                }
                Self::Erc20MaxAmountExceeded(_) => {
                    <Erc20MaxAmountExceeded as alloy_sol_types::SolError>::SELECTOR
                }
                Self::Erc20OracleNotSet(_) => {
                    <Erc20OracleNotSet as alloy_sol_types::SolError>::SELECTOR
                }
                Self::Erc20PaymasterDataInvalid(_) => {
                    <Erc20PaymasterDataInvalid as alloy_sol_types::SolError>::SELECTOR
                }
                Self::Erc20PriceInvalid(_) => {
                    <Erc20PriceInvalid as alloy_sol_types::SolError>::SELECTOR
                }
                Self::Erc20PriceStale(_) => {
                    <Erc20PriceStale as alloy_sol_types::SolError>::SELECTOR
                }
                Self::Erc20TokenNotEnabled(_) => {
                    <Erc20TokenNotEnabled as alloy_sol_types::SolError>::SELECTOR
                }
                Self::ExpectedPause(_) => {
                    <ExpectedPause as alloy_sol_types::SolError>::SELECTOR
                }
                Self::InvalidParams(_) => {
                    <InvalidParams as alloy_sol_types::SolError>::SELECTOR
                }
                Self::InvalidPoolId(_) => {
                    <InvalidPoolId as alloy_sol_types::SolError>::SELECTOR
                }
                Self::MevPaymaster__SenderNotSponsored(_) => {
                    <MevPaymaster__SenderNotSponsored as alloy_sol_types::SolError>::SELECTOR
                }
                Self::MevPaymaster__UnexpectedEntryPoint(_) => {
                    <MevPaymaster__UnexpectedEntryPoint as alloy_sol_types::SolError>::SELECTOR
                }
                Self::MevPaymaster__UntrustedDelegate(_) => {
                    <MevPaymaster__UntrustedDelegate as alloy_sol_types::SolError>::SELECTOR
                }
                Self::MustOverride(_) => {
                    <MustOverride as alloy_sol_types::SolError>::SELECTOR
                }
                Self::NativeBalanceWithdrawFailed(_) => {
                    <NativeBalanceWithdrawFailed as alloy_sol_types::SolError>::SELECTOR
                }
                Self::NotFromEntryPoint(_) => {
                    <NotFromEntryPoint as alloy_sol_types::SolError>::SELECTOR
                }
                Self::OwnableInvalidOwner(_) => {
                    <OwnableInvalidOwner as alloy_sol_types::SolError>::SELECTOR
                }
                Self::OwnableUnauthorizedAccount(_) => {
                    <OwnableUnauthorizedAccount as alloy_sol_types::SolError>::SELECTOR
                }
                Self::PaymasterPaused(_) => {
                    <PaymasterPaused as alloy_sol_types::SolError>::SELECTOR
                }
                Self::PoolEthBalanceInsufficient(_) => {
                    <PoolEthBalanceInsufficient as alloy_sol_types::SolError>::SELECTOR
                }
                Self::PoolTokenBalanceInsufficient(_) => {
                    <PoolTokenBalanceInsufficient as alloy_sol_types::SolError>::SELECTOR
                }
                Self::UnsupportedOperation(_) => {
                    <UnsupportedOperation as alloy_sol_types::SolError>::SELECTOR
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
            ) -> alloy_sol_types::Result<MevPaymasterV9Errors>] = &[
                {
                    fn NativeBalanceWithdrawFailed(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <NativeBalanceWithdrawFailed as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::NativeBalanceWithdrawFailed)
                    }
                    NativeBalanceWithdrawFailed
                },
                {
                    fn OwnableUnauthorizedAccount(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <OwnableUnauthorizedAccount as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::OwnableUnauthorizedAccount)
                    }
                    OwnableUnauthorizedAccount
                },
                {
                    fn EpochBudgetExceeded(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <EpochBudgetExceeded as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::EpochBudgetExceeded)
                    }
                    EpochBudgetExceeded
                },
                {
                    fn OwnableInvalidOwner(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <OwnableInvalidOwner as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::OwnableInvalidOwner)
                    }
                    OwnableInvalidOwner
                },
                {
                    fn Erc20OracleNotSet(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Erc20OracleNotSet as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Erc20OracleNotSet)
                    }
                    Erc20OracleNotSet
                },
                {
                    fn DivisionByZero(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <DivisionByZero as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::DivisionByZero)
                    }
                    DivisionByZero
                },
                {
                    fn MustOverride(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <MustOverride as alloy_sol_types::SolError>::abi_decode_raw(data)
                            .map(MevPaymasterV9Errors::MustOverride)
                    }
                    MustOverride
                },
                {
                    fn Erc20TokenNotEnabled(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Erc20TokenNotEnabled as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Erc20TokenNotEnabled)
                    }
                    Erc20TokenNotEnabled
                },
                {
                    fn Erc20PaymasterDataInvalid(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Erc20PaymasterDataInvalid as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Erc20PaymasterDataInvalid)
                    }
                    Erc20PaymasterDataInvalid
                },
                {
                    fn PoolEthBalanceInsufficient(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <PoolEthBalanceInsufficient as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::PoolEthBalanceInsufficient)
                    }
                    PoolEthBalanceInsufficient
                },
                {
                    fn Erc20PriceInvalid(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Erc20PriceInvalid as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Erc20PriceInvalid)
                    }
                    Erc20PriceInvalid
                },
                {
                    fn ERC165Error(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <ERC165Error as alloy_sol_types::SolError>::abi_decode_raw(data)
                            .map(MevPaymasterV9Errors::ERC165Error)
                    }
                    ERC165Error
                },
                {
                    fn Erc20InvalidConfig(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Erc20InvalidConfig as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Erc20InvalidConfig)
                    }
                    Erc20InvalidConfig
                },
                {
                    fn Erc20PriceStale(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Erc20PriceStale as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Erc20PriceStale)
                    }
                    Erc20PriceStale
                },
                {
                    fn Erc20InsufficientAllowance(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Erc20InsufficientAllowance as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Erc20InsufficientAllowance)
                    }
                    Erc20InsufficientAllowance
                },
                {
                    fn ExpectedPause(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <ExpectedPause as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::ExpectedPause)
                    }
                    ExpectedPause
                },
                {
                    fn PaymasterPaused(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <PaymasterPaused as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::PaymasterPaused)
                    }
                    PaymasterPaused
                },
                {
                    fn UnsupportedOperation(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <UnsupportedOperation as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::UnsupportedOperation)
                    }
                    UnsupportedOperation
                },
                {
                    fn Eip7702SenderNotDelegate(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Eip7702SenderNotDelegate as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Eip7702SenderNotDelegate)
                    }
                    Eip7702SenderNotDelegate
                },
                {
                    fn InvalidParams(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <InvalidParams as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::InvalidParams)
                    }
                    InvalidParams
                },
                {
                    fn MevPaymaster__UnexpectedEntryPoint(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <MevPaymaster__UnexpectedEntryPoint as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(
                                MevPaymasterV9Errors::MevPaymaster__UnexpectedEntryPoint,
                            )
                    }
                    MevPaymaster__UnexpectedEntryPoint
                },
                {
                    fn PoolTokenBalanceInsufficient(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <PoolTokenBalanceInsufficient as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::PoolTokenBalanceInsufficient)
                    }
                    PoolTokenBalanceInsufficient
                },
                {
                    fn InvalidPoolId(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <InvalidPoolId as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::InvalidPoolId)
                    }
                    InvalidPoolId
                },
                {
                    fn EnforcedPause(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <EnforcedPause as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::EnforcedPause)
                    }
                    EnforcedPause
                },
                {
                    fn Eip7702SenderWithoutCode(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Eip7702SenderWithoutCode as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Eip7702SenderWithoutCode)
                    }
                    Eip7702SenderWithoutCode
                },
                {
                    fn MevPaymaster__UntrustedDelegate(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <MevPaymaster__UntrustedDelegate as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::MevPaymaster__UntrustedDelegate)
                    }
                    MevPaymaster__UntrustedDelegate
                },
                {
                    fn MevPaymaster__SenderNotSponsored(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <MevPaymaster__SenderNotSponsored as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::MevPaymaster__SenderNotSponsored)
                    }
                    MevPaymaster__SenderNotSponsored
                },
                {
                    fn Erc20MaxAmountExceeded(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Erc20MaxAmountExceeded as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Erc20MaxAmountExceeded)
                    }
                    Erc20MaxAmountExceeded
                },
                {
                    fn NotFromEntryPoint(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <NotFromEntryPoint as alloy_sol_types::SolError>::abi_decode_raw(
                                data,
                            )
                            .map(MevPaymasterV9Errors::NotFromEntryPoint)
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
            ) -> alloy_sol_types::Result<MevPaymasterV9Errors>] = &[
                {
                    fn NativeBalanceWithdrawFailed(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <NativeBalanceWithdrawFailed as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::NativeBalanceWithdrawFailed)
                    }
                    NativeBalanceWithdrawFailed
                },
                {
                    fn OwnableUnauthorizedAccount(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <OwnableUnauthorizedAccount as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::OwnableUnauthorizedAccount)
                    }
                    OwnableUnauthorizedAccount
                },
                {
                    fn EpochBudgetExceeded(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <EpochBudgetExceeded as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::EpochBudgetExceeded)
                    }
                    EpochBudgetExceeded
                },
                {
                    fn OwnableInvalidOwner(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <OwnableInvalidOwner as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::OwnableInvalidOwner)
                    }
                    OwnableInvalidOwner
                },
                {
                    fn Erc20OracleNotSet(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Erc20OracleNotSet as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Erc20OracleNotSet)
                    }
                    Erc20OracleNotSet
                },
                {
                    fn DivisionByZero(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <DivisionByZero as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::DivisionByZero)
                    }
                    DivisionByZero
                },
                {
                    fn MustOverride(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <MustOverride as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::MustOverride)
                    }
                    MustOverride
                },
                {
                    fn Erc20TokenNotEnabled(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Erc20TokenNotEnabled as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Erc20TokenNotEnabled)
                    }
                    Erc20TokenNotEnabled
                },
                {
                    fn Erc20PaymasterDataInvalid(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Erc20PaymasterDataInvalid as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Erc20PaymasterDataInvalid)
                    }
                    Erc20PaymasterDataInvalid
                },
                {
                    fn PoolEthBalanceInsufficient(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <PoolEthBalanceInsufficient as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::PoolEthBalanceInsufficient)
                    }
                    PoolEthBalanceInsufficient
                },
                {
                    fn Erc20PriceInvalid(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Erc20PriceInvalid as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Erc20PriceInvalid)
                    }
                    Erc20PriceInvalid
                },
                {
                    fn ERC165Error(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <ERC165Error as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::ERC165Error)
                    }
                    ERC165Error
                },
                {
                    fn Erc20InvalidConfig(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Erc20InvalidConfig as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Erc20InvalidConfig)
                    }
                    Erc20InvalidConfig
                },
                {
                    fn Erc20PriceStale(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Erc20PriceStale as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Erc20PriceStale)
                    }
                    Erc20PriceStale
                },
                {
                    fn Erc20InsufficientAllowance(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Erc20InsufficientAllowance as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Erc20InsufficientAllowance)
                    }
                    Erc20InsufficientAllowance
                },
                {
                    fn ExpectedPause(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <ExpectedPause as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::ExpectedPause)
                    }
                    ExpectedPause
                },
                {
                    fn PaymasterPaused(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <PaymasterPaused as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::PaymasterPaused)
                    }
                    PaymasterPaused
                },
                {
                    fn UnsupportedOperation(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <UnsupportedOperation as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::UnsupportedOperation)
                    }
                    UnsupportedOperation
                },
                {
                    fn Eip7702SenderNotDelegate(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Eip7702SenderNotDelegate as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Eip7702SenderNotDelegate)
                    }
                    Eip7702SenderNotDelegate
                },
                {
                    fn InvalidParams(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <InvalidParams as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::InvalidParams)
                    }
                    InvalidParams
                },
                {
                    fn MevPaymaster__UnexpectedEntryPoint(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <MevPaymaster__UnexpectedEntryPoint as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(
                                MevPaymasterV9Errors::MevPaymaster__UnexpectedEntryPoint,
                            )
                    }
                    MevPaymaster__UnexpectedEntryPoint
                },
                {
                    fn PoolTokenBalanceInsufficient(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <PoolTokenBalanceInsufficient as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::PoolTokenBalanceInsufficient)
                    }
                    PoolTokenBalanceInsufficient
                },
                {
                    fn InvalidPoolId(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <InvalidPoolId as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::InvalidPoolId)
                    }
                    InvalidPoolId
                },
                {
                    fn EnforcedPause(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <EnforcedPause as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::EnforcedPause)
                    }
                    EnforcedPause
                },
                {
                    fn Eip7702SenderWithoutCode(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Eip7702SenderWithoutCode as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Eip7702SenderWithoutCode)
                    }
                    Eip7702SenderWithoutCode
                },
                {
                    fn MevPaymaster__UntrustedDelegate(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <MevPaymaster__UntrustedDelegate as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::MevPaymaster__UntrustedDelegate)
                    }
                    MevPaymaster__UntrustedDelegate
                },
                {
                    fn MevPaymaster__SenderNotSponsored(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <MevPaymaster__SenderNotSponsored as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::MevPaymaster__SenderNotSponsored)
                    }
                    MevPaymaster__SenderNotSponsored
                },
                {
                    fn Erc20MaxAmountExceeded(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <Erc20MaxAmountExceeded as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::Erc20MaxAmountExceeded)
                    }
                    Erc20MaxAmountExceeded
                },
                {
                    fn NotFromEntryPoint(
                        data: &[u8],
                    ) -> alloy_sol_types::Result<MevPaymasterV9Errors> {
                        <NotFromEntryPoint as alloy_sol_types::SolError>::abi_decode_raw_validate(
                                data,
                            )
                            .map(MevPaymasterV9Errors::NotFromEntryPoint)
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
                Self::DivisionByZero(inner) => {
                    <DivisionByZero as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::ERC165Error(inner) => {
                    <ERC165Error as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
                Self::Eip7702SenderNotDelegate(inner) => {
                    <Eip7702SenderNotDelegate as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::Eip7702SenderWithoutCode(inner) => {
                    <Eip7702SenderWithoutCode as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::EnforcedPause(inner) => {
                    <EnforcedPause as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
                Self::EpochBudgetExceeded(inner) => {
                    <EpochBudgetExceeded as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::Erc20InsufficientAllowance(inner) => {
                    <Erc20InsufficientAllowance as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::Erc20InvalidConfig(inner) => {
                    <Erc20InvalidConfig as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::Erc20MaxAmountExceeded(inner) => {
                    <Erc20MaxAmountExceeded as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::Erc20OracleNotSet(inner) => {
                    <Erc20OracleNotSet as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::Erc20PaymasterDataInvalid(inner) => {
                    <Erc20PaymasterDataInvalid as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::Erc20PriceInvalid(inner) => {
                    <Erc20PriceInvalid as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::Erc20PriceStale(inner) => {
                    <Erc20PriceStale as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::Erc20TokenNotEnabled(inner) => {
                    <Erc20TokenNotEnabled as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::ExpectedPause(inner) => {
                    <ExpectedPause as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
                Self::InvalidParams(inner) => {
                    <InvalidParams as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
                Self::InvalidPoolId(inner) => {
                    <InvalidPoolId as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
                Self::MevPaymaster__SenderNotSponsored(inner) => {
                    <MevPaymaster__SenderNotSponsored as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::MevPaymaster__UnexpectedEntryPoint(inner) => {
                    <MevPaymaster__UnexpectedEntryPoint as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::MevPaymaster__UntrustedDelegate(inner) => {
                    <MevPaymaster__UntrustedDelegate as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::MustOverride(inner) => {
                    <MustOverride as alloy_sol_types::SolError>::abi_encoded_size(inner)
                }
                Self::NativeBalanceWithdrawFailed(inner) => {
                    <NativeBalanceWithdrawFailed as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::NotFromEntryPoint(inner) => {
                    <NotFromEntryPoint as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
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
                Self::PaymasterPaused(inner) => {
                    <PaymasterPaused as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::PoolEthBalanceInsufficient(inner) => {
                    <PoolEthBalanceInsufficient as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::PoolTokenBalanceInsufficient(inner) => {
                    <PoolTokenBalanceInsufficient as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
                }
                Self::UnsupportedOperation(inner) => {
                    <UnsupportedOperation as alloy_sol_types::SolError>::abi_encoded_size(
                        inner,
                    )
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
                Self::ERC165Error(inner) => {
                    <ERC165Error as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::Eip7702SenderNotDelegate(inner) => {
                    <Eip7702SenderNotDelegate as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::Eip7702SenderWithoutCode(inner) => {
                    <Eip7702SenderWithoutCode as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::EnforcedPause(inner) => {
                    <EnforcedPause as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::EpochBudgetExceeded(inner) => {
                    <EpochBudgetExceeded as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::Erc20InsufficientAllowance(inner) => {
                    <Erc20InsufficientAllowance as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::Erc20InvalidConfig(inner) => {
                    <Erc20InvalidConfig as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::Erc20MaxAmountExceeded(inner) => {
                    <Erc20MaxAmountExceeded as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::Erc20OracleNotSet(inner) => {
                    <Erc20OracleNotSet as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::Erc20PaymasterDataInvalid(inner) => {
                    <Erc20PaymasterDataInvalid as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::Erc20PriceInvalid(inner) => {
                    <Erc20PriceInvalid as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::Erc20PriceStale(inner) => {
                    <Erc20PriceStale as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::Erc20TokenNotEnabled(inner) => {
                    <Erc20TokenNotEnabled as alloy_sol_types::SolError>::abi_encode_raw(
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
                Self::InvalidParams(inner) => {
                    <InvalidParams as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::InvalidPoolId(inner) => {
                    <InvalidPoolId as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::MevPaymaster__SenderNotSponsored(inner) => {
                    <MevPaymaster__SenderNotSponsored as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::MevPaymaster__UnexpectedEntryPoint(inner) => {
                    <MevPaymaster__UnexpectedEntryPoint as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::MevPaymaster__UntrustedDelegate(inner) => {
                    <MevPaymaster__UntrustedDelegate as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::MustOverride(inner) => {
                    <MustOverride as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::NativeBalanceWithdrawFailed(inner) => {
                    <NativeBalanceWithdrawFailed as alloy_sol_types::SolError>::abi_encode_raw(
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
                Self::PaymasterPaused(inner) => {
                    <PaymasterPaused as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::PoolEthBalanceInsufficient(inner) => {
                    <PoolEthBalanceInsufficient as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::PoolTokenBalanceInsufficient(inner) => {
                    <PoolTokenBalanceInsufficient as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
                Self::UnsupportedOperation(inner) => {
                    <UnsupportedOperation as alloy_sol_types::SolError>::abi_encode_raw(
                        inner,
                        out,
                    )
                }
            }
        }
    }
    #[automatically_derived]
    impl MevPaymasterV9Errors {
        /**Creates a [`DivisionByZero`] error.

```solidity
error DivisionByZero()
```*/
        #[inline]
        pub fn division_by_zero() -> Self {
            Self::DivisionByZero(DivisionByZero)
        }
        /**Creates a [`ERC165Error`] error.

```solidity
error ERC165Error(address,bytes4)
```*/
        #[inline]
        pub fn erc_165_error(
            entry_point: alloy::sol_types::private::Address,
            interface_id: alloy::sol_types::private::FixedBytes<4>,
        ) -> Self {
            Self::ERC165Error(ERC165Error {
                entryPoint: entry_point,
                interfaceId: interface_id,
            })
        }
        /**Creates a [`Eip7702SenderNotDelegate`] error.

```solidity
error Eip7702SenderNotDelegate(address)
```*/
        #[inline]
        pub fn eip_7702_sender_not_delegate(
            sender: alloy::sol_types::private::Address,
        ) -> Self {
            Self::Eip7702SenderNotDelegate(Eip7702SenderNotDelegate {
                sender: sender,
            })
        }
        /**Creates a [`Eip7702SenderWithoutCode`] error.

```solidity
error Eip7702SenderWithoutCode(address)
```*/
        #[inline]
        pub fn eip_7702_sender_without_code(
            sender: alloy::sol_types::private::Address,
        ) -> Self {
            Self::Eip7702SenderWithoutCode(Eip7702SenderWithoutCode {
                sender: sender,
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
        /**Creates a [`EpochBudgetExceeded`] error.

```solidity
error EpochBudgetExceeded(uint128,uint128)
```*/
        #[inline]
        pub fn epoch_budget_exceeded(spent: u128, cap: u128) -> Self {
            Self::EpochBudgetExceeded(EpochBudgetExceeded {
                spent: spent,
                cap: cap,
            })
        }
        /**Creates a [`Erc20InsufficientAllowance`] error.

```solidity
error Erc20InsufficientAllowance(address,address,uint256,uint256)
```*/
        #[inline]
        pub fn erc_20_insufficient_allowance(
            token: alloy::sol_types::private::Address,
            sender: alloy::sol_types::private::Address,
            required_amount: alloy::sol_types::private::primitives::aliases::U256,
            allowance: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::Erc20InsufficientAllowance(Erc20InsufficientAllowance {
                token: token,
                sender: sender,
                requiredAmount: required_amount,
                allowance: allowance,
            })
        }
        /**Creates a [`Erc20InvalidConfig`] error.

```solidity
error Erc20InvalidConfig()
```*/
        #[inline]
        pub fn erc_20_invalid_config() -> Self {
            Self::Erc20InvalidConfig(Erc20InvalidConfig)
        }
        /**Creates a [`Erc20MaxAmountExceeded`] error.

```solidity
error Erc20MaxAmountExceeded(uint256,uint256)
```*/
        #[inline]
        pub fn erc_20_max_amount_exceeded(
            required_amount: alloy::sol_types::private::primitives::aliases::U256,
            max_amount: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::Erc20MaxAmountExceeded(Erc20MaxAmountExceeded {
                requiredAmount: required_amount,
                maxAmount: max_amount,
            })
        }
        /**Creates a [`Erc20OracleNotSet`] error.

```solidity
error Erc20OracleNotSet()
```*/
        #[inline]
        pub fn erc_20_oracle_not_set() -> Self {
            Self::Erc20OracleNotSet(Erc20OracleNotSet)
        }
        /**Creates a [`Erc20PaymasterDataInvalid`] error.

```solidity
error Erc20PaymasterDataInvalid()
```*/
        #[inline]
        pub fn erc_20_paymaster_data_invalid() -> Self {
            Self::Erc20PaymasterDataInvalid(Erc20PaymasterDataInvalid)
        }
        /**Creates a [`Erc20PriceInvalid`] error.

```solidity
error Erc20PriceInvalid(address,int256)
```*/
        #[inline]
        pub fn erc_20_price_invalid(
            oracle: alloy::sol_types::private::Address,
            answer: alloy::sol_types::private::primitives::aliases::I256,
        ) -> Self {
            Self::Erc20PriceInvalid(Erc20PriceInvalid {
                oracle: oracle,
                answer: answer,
            })
        }
        /**Creates a [`Erc20PriceStale`] error.

```solidity
error Erc20PriceStale(address,uint256,uint256)
```*/
        #[inline]
        pub fn erc_20_price_stale(
            oracle: alloy::sol_types::private::Address,
            updated_at: alloy::sol_types::private::primitives::aliases::U256,
            max_staleness: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::Erc20PriceStale(Erc20PriceStale {
                oracle: oracle,
                updatedAt: updated_at,
                maxStaleness: max_staleness,
            })
        }
        /**Creates a [`Erc20TokenNotEnabled`] error.

```solidity
error Erc20TokenNotEnabled(address)
```*/
        #[inline]
        pub fn erc_20_token_not_enabled(
            token: alloy::sol_types::private::Address,
        ) -> Self {
            Self::Erc20TokenNotEnabled(Erc20TokenNotEnabled {
                token: token,
            })
        }
        /**Creates a [`ExpectedPause`] error.

```solidity
error ExpectedPause()
```*/
        #[inline]
        pub fn expected_pause() -> Self {
            Self::ExpectedPause(ExpectedPause)
        }
        /**Creates a [`InvalidParams`] error.

```solidity
error InvalidParams()
```*/
        #[inline]
        pub fn invalid_params() -> Self {
            Self::InvalidParams(InvalidParams)
        }
        /**Creates a [`InvalidPoolId`] error.

```solidity
error InvalidPoolId(uint256)
```*/
        #[inline]
        pub fn invalid_pool_id(
            pool_id: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::InvalidPoolId(InvalidPoolId { poolId: pool_id })
        }
        /**Creates a [`MevPaymaster__SenderNotSponsored`] error.

```solidity
error MevPaymaster__SenderNotSponsored(address)
```*/
        #[inline]
        pub fn mev_paymaster_sender_not_sponsored(
            sender: alloy::sol_types::private::Address,
        ) -> Self {
            Self::MevPaymaster__SenderNotSponsored(MevPaymaster__SenderNotSponsored {
                sender: sender,
            })
        }
        /**Creates a [`MevPaymaster__UnexpectedEntryPoint`] error.

```solidity
error MevPaymaster__UnexpectedEntryPoint(address,address)
```*/
        #[inline]
        pub fn mev_paymaster_unexpected_entry_point(
            actual: alloy::sol_types::private::Address,
            expected: alloy::sol_types::private::Address,
        ) -> Self {
            Self::MevPaymaster__UnexpectedEntryPoint(MevPaymaster__UnexpectedEntryPoint {
                actual: actual,
                expected: expected,
            })
        }
        /**Creates a [`MevPaymaster__UntrustedDelegate`] error.

```solidity
error MevPaymaster__UntrustedDelegate(address,address)
```*/
        #[inline]
        pub fn mev_paymaster_untrusted_delegate(
            sender: alloy::sol_types::private::Address,
            delegate: alloy::sol_types::private::Address,
        ) -> Self {
            Self::MevPaymaster__UntrustedDelegate(MevPaymaster__UntrustedDelegate {
                sender: sender,
                delegate: delegate,
            })
        }
        /**Creates a [`MustOverride`] error.

```solidity
error MustOverride()
```*/
        #[inline]
        pub fn must_override() -> Self {
            Self::MustOverride(MustOverride)
        }
        /**Creates a [`NativeBalanceWithdrawFailed`] error.

```solidity
error NativeBalanceWithdrawFailed()
```*/
        #[inline]
        pub fn native_balance_withdraw_failed() -> Self {
            Self::NativeBalanceWithdrawFailed(NativeBalanceWithdrawFailed)
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
        /**Creates a [`PaymasterPaused`] error.

```solidity
error PaymasterPaused()
```*/
        #[inline]
        pub fn paymaster_paused() -> Self {
            Self::PaymasterPaused(PaymasterPaused)
        }
        /**Creates a [`PoolEthBalanceInsufficient`] error.

```solidity
error PoolEthBalanceInsufficient(uint256,uint256,uint256)
```*/
        #[inline]
        pub fn pool_eth_balance_insufficient(
            pool_id: alloy::sol_types::private::primitives::aliases::U256,
            requested: alloy::sol_types::private::primitives::aliases::U256,
            available: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::PoolEthBalanceInsufficient(PoolEthBalanceInsufficient {
                poolId: pool_id,
                requested: requested,
                available: available,
            })
        }
        /**Creates a [`PoolTokenBalanceInsufficient`] error.

```solidity
error PoolTokenBalanceInsufficient(uint256,address,uint256,uint256)
```*/
        #[inline]
        pub fn pool_token_balance_insufficient(
            pool_id: alloy::sol_types::private::primitives::aliases::U256,
            token: alloy::sol_types::private::Address,
            requested: alloy::sol_types::private::primitives::aliases::U256,
            available: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::PoolTokenBalanceInsufficient(PoolTokenBalanceInsufficient {
                poolId: pool_id,
                token: token,
                requested: requested,
                available: available,
            })
        }
        /**Creates a [`UnsupportedOperation`] error.

```solidity
error UnsupportedOperation()
```*/
        #[inline]
        pub fn unsupported_operation() -> Self {
            Self::UnsupportedOperation(UnsupportedOperation)
        }
    }
    ///Container for all the [`MevPaymasterV9`](self) events.
    #[derive(Clone)]
    #[derive(serde::Serialize, serde::Deserialize)]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub enum MevPaymasterV9Events {
        #[allow(missing_docs)]
        Erc20ConfigChanged(Erc20ConfigChanged),
        #[allow(missing_docs)]
        Erc20Settled(Erc20Settled),
        #[allow(missing_docs)]
        Erc20Sponsored(Erc20Sponsored),
        #[allow(missing_docs)]
        EthOracleChanged(EthOracleChanged),
        #[allow(missing_docs)]
        NativeBalanceWithdrawn(NativeBalanceWithdrawn),
        #[allow(missing_docs)]
        OwnershipTransferStarted(OwnershipTransferStarted),
        #[allow(missing_docs)]
        OwnershipTransferred(OwnershipTransferred),
        #[allow(missing_docs)]
        Paused(Paused),
        #[allow(missing_docs)]
        PoolErc20Settled(PoolErc20Settled),
        #[allow(missing_docs)]
        PoolErc20Sponsored(PoolErc20Sponsored),
        #[allow(missing_docs)]
        PoolEthSettled(PoolEthSettled),
        #[allow(missing_docs)]
        PoolEthSponsored(PoolEthSponsored),
        #[allow(missing_docs)]
        SenderPoolChanged(SenderPoolChanged),
        #[allow(missing_docs)]
        Settled(Settled),
        #[allow(missing_docs)]
        Sponsored(Sponsored),
        #[allow(missing_docs)]
        SponsoredAccountChanged(SponsoredAccountChanged),
        #[allow(missing_docs)]
        Transfer(Transfer),
        #[allow(missing_docs)]
        TrustedDelegateChanged(TrustedDelegateChanged),
        #[allow(missing_docs)]
        TuningUpdated(TuningUpdated),
        #[allow(missing_docs)]
        Unpaused(Unpaused),
    }
    impl MevPaymasterV9Events {
        /// All the selectors of this enum.
        ///
        /// Note that the selectors might not be in the same order as the variants.
        /// No guarantees are made about the order of the selectors.
        ///
        /// Prefer using `SolInterface` methods instead.
        pub const SELECTORS: &'static [[u8; 32usize]] = &[
            [
                27u8, 61u8, 126u8, 219u8, 46u8, 156u8, 11u8, 14u8, 124u8, 82u8, 91u8,
                32u8, 170u8, 174u8, 240u8, 245u8, 148u8, 13u8, 46u8, 215u8, 22u8, 99u8,
                199u8, 211u8, 146u8, 102u8, 236u8, 175u8, 172u8, 114u8, 136u8, 89u8,
            ],
            [
                37u8, 55u8, 116u8, 247u8, 175u8, 30u8, 163u8, 188u8, 140u8, 248u8, 228u8,
                173u8, 180u8, 55u8, 249u8, 8u8, 237u8, 217u8, 87u8, 202u8, 211u8, 77u8,
                7u8, 5u8, 243u8, 35u8, 58u8, 89u8, 234u8, 11u8, 241u8, 231u8,
            ],
            [
                41u8, 108u8, 174u8, 245u8, 244u8, 87u8, 78u8, 160u8, 26u8, 74u8, 35u8,
                230u8, 221u8, 49u8, 92u8, 121u8, 140u8, 76u8, 66u8, 106u8, 3u8, 74u8,
                7u8, 212u8, 27u8, 171u8, 249u8, 39u8, 24u8, 139u8, 206u8, 174u8,
            ],
            [
                56u8, 209u8, 107u8, 140u8, 172u8, 34u8, 217u8, 159u8, 199u8, 193u8, 36u8,
                185u8, 205u8, 13u8, 226u8, 211u8, 250u8, 31u8, 174u8, 244u8, 32u8, 191u8,
                231u8, 145u8, 216u8, 195u8, 98u8, 215u8, 101u8, 226u8, 39u8, 0u8,
            ],
            [
                70u8, 193u8, 14u8, 103u8, 214u8, 214u8, 239u8, 45u8, 255u8, 127u8, 58u8,
                154u8, 124u8, 74u8, 40u8, 244u8, 113u8, 97u8, 113u8, 216u8, 128u8, 138u8,
                80u8, 99u8, 227u8, 200u8, 156u8, 18u8, 168u8, 30u8, 71u8, 95u8,
            ],
            [
                92u8, 234u8, 201u8, 247u8, 3u8, 106u8, 5u8, 226u8, 49u8, 250u8, 38u8,
                61u8, 107u8, 103u8, 49u8, 222u8, 67u8, 4u8, 96u8, 212u8, 175u8, 72u8,
                48u8, 22u8, 11u8, 182u8, 240u8, 13u8, 27u8, 149u8, 127u8, 80u8,
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
                113u8, 180u8, 177u8, 8u8, 40u8, 213u8, 254u8, 83u8, 105u8, 64u8, 254u8,
                118u8, 126u8, 222u8, 140u8, 22u8, 186u8, 24u8, 66u8, 97u8, 8u8, 212u8,
                98u8, 140u8, 175u8, 245u8, 118u8, 173u8, 69u8, 172u8, 243u8, 149u8,
            ],
            [
                122u8, 120u8, 127u8, 92u8, 189u8, 169u8, 215u8, 112u8, 81u8, 69u8, 65u8,
                26u8, 250u8, 115u8, 119u8, 235u8, 201u8, 222u8, 179u8, 109u8, 100u8,
                11u8, 71u8, 245u8, 243u8, 148u8, 52u8, 230u8, 36u8, 168u8, 136u8, 70u8,
            ],
            [
                139u8, 224u8, 7u8, 156u8, 83u8, 22u8, 89u8, 20u8, 19u8, 68u8, 205u8,
                31u8, 208u8, 164u8, 242u8, 132u8, 25u8, 73u8, 127u8, 151u8, 34u8, 163u8,
                218u8, 175u8, 227u8, 180u8, 24u8, 111u8, 107u8, 100u8, 87u8, 224u8,
            ],
            [
                141u8, 6u8, 53u8, 235u8, 53u8, 50u8, 75u8, 35u8, 114u8, 154u8, 11u8,
                138u8, 131u8, 172u8, 88u8, 204u8, 225u8, 13u8, 254u8, 69u8, 242u8, 176u8,
                132u8, 73u8, 7u8, 112u8, 202u8, 224u8, 181u8, 91u8, 3u8, 134u8,
            ],
            [
                142u8, 244u8, 173u8, 149u8, 2u8, 63u8, 251u8, 213u8, 65u8, 139u8, 217u8,
                216u8, 219u8, 44u8, 103u8, 243u8, 226u8, 70u8, 154u8, 198u8, 24u8, 154u8,
                80u8, 217u8, 192u8, 227u8, 203u8, 155u8, 55u8, 218u8, 91u8, 105u8,
            ],
            [
                145u8, 186u8, 156u8, 37u8, 239u8, 195u8, 193u8, 175u8, 121u8, 5u8, 205u8,
                185u8, 4u8, 143u8, 232u8, 142u8, 43u8, 125u8, 170u8, 183u8, 194u8, 208u8,
                208u8, 195u8, 246u8, 39u8, 119u8, 43u8, 80u8, 6u8, 224u8, 4u8,
            ],
            [
                154u8, 100u8, 158u8, 138u8, 194u8, 229u8, 208u8, 106u8, 41u8, 122u8,
                213u8, 195u8, 213u8, 99u8, 108u8, 46u8, 200u8, 0u8, 104u8, 107u8, 162u8,
                23u8, 236u8, 143u8, 23u8, 203u8, 17u8, 254u8, 169u8, 104u8, 123u8, 40u8,
            ],
            [
                174u8, 251u8, 124u8, 239u8, 96u8, 139u8, 83u8, 129u8, 178u8, 193u8,
                224u8, 99u8, 119u8, 236u8, 106u8, 194u8, 238u8, 93u8, 25u8, 73u8, 73u8,
                149u8, 217u8, 250u8, 15u8, 77u8, 159u8, 163u8, 117u8, 181u8, 88u8, 237u8,
            ],
            [
                180u8, 115u8, 76u8, 31u8, 242u8, 46u8, 243u8, 48u8, 172u8, 197u8, 5u8,
                203u8, 39u8, 249u8, 60u8, 58u8, 218u8, 20u8, 61u8, 143u8, 47u8, 218u8,
                141u8, 130u8, 237u8, 174u8, 131u8, 228u8, 12u8, 43u8, 238u8, 22u8,
            ],
            [
                221u8, 120u8, 110u8, 123u8, 219u8, 235u8, 152u8, 159u8, 175u8, 27u8,
                92u8, 139u8, 68u8, 138u8, 121u8, 39u8, 9u8, 161u8, 16u8, 77u8, 34u8,
                34u8, 217u8, 218u8, 210u8, 170u8, 83u8, 27u8, 55u8, 181u8, 110u8, 148u8,
            ],
            [
                235u8, 67u8, 41u8, 14u8, 252u8, 28u8, 148u8, 66u8, 113u8, 51u8, 71u8,
                225u8, 213u8, 65u8, 201u8, 226u8, 77u8, 53u8, 90u8, 48u8, 241u8, 25u8,
                81u8, 8u8, 48u8, 33u8, 69u8, 89u8, 204u8, 215u8, 46u8, 143u8,
            ],
            [
                238u8, 140u8, 244u8, 29u8, 240u8, 54u8, 135u8, 86u8, 96u8, 222u8, 21u8,
                219u8, 210u8, 127u8, 90u8, 112u8, 196u8, 93u8, 136u8, 159u8, 46u8, 24u8,
                35u8, 109u8, 160u8, 10u8, 233u8, 215u8, 36u8, 134u8, 158u8, 185u8,
            ],
        ];
        /// The names of the variants in the same order as `SELECTORS`.
        pub const VARIANT_NAMES: &'static [&'static str] = &[
            ::core::stringify!(Transfer),
            ::core::stringify!(SenderPoolChanged),
            ::core::stringify!(Erc20Sponsored),
            ::core::stringify!(OwnershipTransferStarted),
            ::core::stringify!(Sponsored),
            ::core::stringify!(NativeBalanceWithdrawn),
            ::core::stringify!(Unpaused),
            ::core::stringify!(Paused),
            ::core::stringify!(SponsoredAccountChanged),
            ::core::stringify!(Erc20ConfigChanged),
            ::core::stringify!(OwnershipTransferred),
            ::core::stringify!(PoolErc20Sponsored),
            ::core::stringify!(Erc20Settled),
            ::core::stringify!(PoolEthSponsored),
            ::core::stringify!(TuningUpdated),
            ::core::stringify!(PoolEthSettled),
            ::core::stringify!(TrustedDelegateChanged),
            ::core::stringify!(EthOracleChanged),
            ::core::stringify!(Settled),
            ::core::stringify!(PoolErc20Settled),
        ];
        /// The signatures in the same order as `SELECTORS`.
        pub const SIGNATURES: &'static [&'static str] = &[
            <Transfer as alloy_sol_types::SolEvent>::SIGNATURE,
            <SenderPoolChanged as alloy_sol_types::SolEvent>::SIGNATURE,
            <Erc20Sponsored as alloy_sol_types::SolEvent>::SIGNATURE,
            <OwnershipTransferStarted as alloy_sol_types::SolEvent>::SIGNATURE,
            <Sponsored as alloy_sol_types::SolEvent>::SIGNATURE,
            <NativeBalanceWithdrawn as alloy_sol_types::SolEvent>::SIGNATURE,
            <Unpaused as alloy_sol_types::SolEvent>::SIGNATURE,
            <Paused as alloy_sol_types::SolEvent>::SIGNATURE,
            <SponsoredAccountChanged as alloy_sol_types::SolEvent>::SIGNATURE,
            <Erc20ConfigChanged as alloy_sol_types::SolEvent>::SIGNATURE,
            <OwnershipTransferred as alloy_sol_types::SolEvent>::SIGNATURE,
            <PoolErc20Sponsored as alloy_sol_types::SolEvent>::SIGNATURE,
            <Erc20Settled as alloy_sol_types::SolEvent>::SIGNATURE,
            <PoolEthSponsored as alloy_sol_types::SolEvent>::SIGNATURE,
            <TuningUpdated as alloy_sol_types::SolEvent>::SIGNATURE,
            <PoolEthSettled as alloy_sol_types::SolEvent>::SIGNATURE,
            <TrustedDelegateChanged as alloy_sol_types::SolEvent>::SIGNATURE,
            <EthOracleChanged as alloy_sol_types::SolEvent>::SIGNATURE,
            <Settled as alloy_sol_types::SolEvent>::SIGNATURE,
            <PoolErc20Settled as alloy_sol_types::SolEvent>::SIGNATURE,
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
    impl alloy_sol_types::SolEventInterface for MevPaymasterV9Events {
        const NAME: &'static str = "MevPaymasterV9Events";
        const COUNT: usize = 20usize;
        fn decode_raw_log(
            topics: &[alloy_sol_types::Word],
            data: &[u8],
        ) -> alloy_sol_types::Result<Self> {
            match topics.first().copied() {
                Some(
                    <Erc20ConfigChanged as alloy_sol_types::SolEvent>::SIGNATURE_HASH,
                ) => {
                    <Erc20ConfigChanged as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::Erc20ConfigChanged)
                }
                Some(<Erc20Settled as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <Erc20Settled as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::Erc20Settled)
                }
                Some(<Erc20Sponsored as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <Erc20Sponsored as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::Erc20Sponsored)
                }
                Some(<EthOracleChanged as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <EthOracleChanged as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::EthOracleChanged)
                }
                Some(
                    <NativeBalanceWithdrawn as alloy_sol_types::SolEvent>::SIGNATURE_HASH,
                ) => {
                    <NativeBalanceWithdrawn as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::NativeBalanceWithdrawn)
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
                Some(<PoolErc20Settled as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <PoolErc20Settled as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::PoolErc20Settled)
                }
                Some(
                    <PoolErc20Sponsored as alloy_sol_types::SolEvent>::SIGNATURE_HASH,
                ) => {
                    <PoolErc20Sponsored as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::PoolErc20Sponsored)
                }
                Some(<PoolEthSettled as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <PoolEthSettled as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::PoolEthSettled)
                }
                Some(<PoolEthSponsored as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <PoolEthSponsored as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::PoolEthSponsored)
                }
                Some(
                    <SenderPoolChanged as alloy_sol_types::SolEvent>::SIGNATURE_HASH,
                ) => {
                    <SenderPoolChanged as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::SenderPoolChanged)
                }
                Some(<Settled as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <Settled as alloy_sol_types::SolEvent>::decode_raw_log(topics, data)
                        .map(Self::Settled)
                }
                Some(<Sponsored as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <Sponsored as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::Sponsored)
                }
                Some(
                    <SponsoredAccountChanged as alloy_sol_types::SolEvent>::SIGNATURE_HASH,
                ) => {
                    <SponsoredAccountChanged as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::SponsoredAccountChanged)
                }
                Some(<Transfer as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <Transfer as alloy_sol_types::SolEvent>::decode_raw_log(topics, data)
                        .map(Self::Transfer)
                }
                Some(
                    <TrustedDelegateChanged as alloy_sol_types::SolEvent>::SIGNATURE_HASH,
                ) => {
                    <TrustedDelegateChanged as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::TrustedDelegateChanged)
                }
                Some(<TuningUpdated as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <TuningUpdated as alloy_sol_types::SolEvent>::decode_raw_log(
                            topics,
                            data,
                        )
                        .map(Self::TuningUpdated)
                }
                Some(<Unpaused as alloy_sol_types::SolEvent>::SIGNATURE_HASH) => {
                    <Unpaused as alloy_sol_types::SolEvent>::decode_raw_log(topics, data)
                        .map(Self::Unpaused)
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
    impl alloy_sol_types::private::IntoLogData for MevPaymasterV9Events {
        fn to_log_data(&self) -> alloy_sol_types::private::LogData {
            match self {
                Self::Erc20ConfigChanged(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Erc20Settled(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Erc20Sponsored(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::EthOracleChanged(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::NativeBalanceWithdrawn(inner) => {
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
                Self::PoolErc20Settled(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::PoolErc20Sponsored(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::PoolEthSettled(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::PoolEthSponsored(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::SenderPoolChanged(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Settled(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Sponsored(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::SponsoredAccountChanged(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Transfer(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::TrustedDelegateChanged(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::TuningUpdated(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
                Self::Unpaused(inner) => {
                    alloy_sol_types::private::IntoLogData::to_log_data(inner)
                }
            }
        }
        fn into_log_data(self) -> alloy_sol_types::private::LogData {
            match self {
                Self::Erc20ConfigChanged(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Erc20Settled(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Erc20Sponsored(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::EthOracleChanged(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::NativeBalanceWithdrawn(inner) => {
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
                Self::PoolErc20Settled(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::PoolErc20Sponsored(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::PoolEthSettled(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::PoolEthSponsored(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::SenderPoolChanged(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Settled(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Sponsored(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::SponsoredAccountChanged(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Transfer(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::TrustedDelegateChanged(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::TuningUpdated(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
                Self::Unpaused(inner) => {
                    alloy_sol_types::private::IntoLogData::into_log_data(inner)
                }
            }
        }
    }
    #[automatically_derived]
    impl MevPaymasterV9Events {
        /**Creates a [`Erc20ConfigChanged`] event.

```solidity
event Erc20ConfigChanged(address,bool,address,uint32,uint16,address)
```*/
        #[inline]
        pub fn erc_20_config_changed(
            token: alloy::sol_types::private::Address,
            enabled: bool,
            token_oracle: alloy::sol_types::private::Address,
            max_staleness: u32,
            markup_bps: u16,
            treasury: alloy::sol_types::private::Address,
        ) -> Self {
            Self::Erc20ConfigChanged(Erc20ConfigChanged {
                token: token,
                enabled: enabled,
                tokenOracle: token_oracle,
                maxStaleness: max_staleness,
                markupBps: markup_bps,
                treasury: treasury,
            })
        }
        /**Creates a [`Erc20Settled`] event.

```solidity
event Erc20Settled(address,address,uint256,uint256)
```*/
        #[inline]
        pub fn erc_20_settled(
            sender: alloy::sol_types::private::Address,
            token: alloy::sol_types::private::Address,
            actual_gas_cost: alloy::sol_types::private::primitives::aliases::U256,
            actual_token_charge: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::Erc20Settled(Erc20Settled {
                sender: sender,
                token: token,
                actualGasCost: actual_gas_cost,
                actualTokenCharge: actual_token_charge,
            })
        }
        /**Creates a [`Erc20Sponsored`] event.

```solidity
event Erc20Sponsored(address,address,uint256,uint256)
```*/
        #[inline]
        pub fn erc_20_sponsored(
            sender: alloy::sol_types::private::Address,
            token: alloy::sol_types::private::Address,
            max_token_amount: alloy::sol_types::private::primitives::aliases::U256,
            price_with_markup: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::Erc20Sponsored(Erc20Sponsored {
                sender: sender,
                token: token,
                maxTokenAmount: max_token_amount,
                priceWithMarkup: price_with_markup,
            })
        }
        /**Creates a [`EthOracleChanged`] event.

```solidity
event EthOracleChanged(address,address)
```*/
        #[inline]
        pub fn eth_oracle_changed(
            old_oracle: alloy::sol_types::private::Address,
            new_oracle: alloy::sol_types::private::Address,
        ) -> Self {
            Self::EthOracleChanged(EthOracleChanged {
                oldOracle: old_oracle,
                newOracle: new_oracle,
            })
        }
        /**Creates a [`NativeBalanceWithdrawn`] event.

```solidity
event NativeBalanceWithdrawn(address,uint256)
```*/
        #[inline]
        pub fn native_balance_withdrawn(
            to: alloy::sol_types::private::Address,
            amount: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::NativeBalanceWithdrawn(NativeBalanceWithdrawn {
                to: to,
                amount: amount,
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
        /**Creates a [`PoolErc20Settled`] event.

```solidity
event PoolErc20Settled(address,uint256,address,uint256,uint256)
```*/
        #[inline]
        pub fn pool_erc_20_settled(
            sender: alloy::sol_types::private::Address,
            pool_id: alloy::sol_types::private::primitives::aliases::U256,
            token: alloy::sol_types::private::Address,
            actual_gas_cost: alloy::sol_types::private::primitives::aliases::U256,
            actual_charge: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::PoolErc20Settled(PoolErc20Settled {
                sender: sender,
                poolId: pool_id,
                token: token,
                actualGasCost: actual_gas_cost,
                actualCharge: actual_charge,
            })
        }
        /**Creates a [`PoolErc20Sponsored`] event.

```solidity
event PoolErc20Sponsored(address,uint256,address,uint256,uint256)
```*/
        #[inline]
        pub fn pool_erc_20_sponsored(
            sender: alloy::sol_types::private::Address,
            pool_id: alloy::sol_types::private::primitives::aliases::U256,
            token: alloy::sol_types::private::Address,
            reserved_amount: alloy::sol_types::private::primitives::aliases::U256,
            price_with_markup: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::PoolErc20Sponsored(PoolErc20Sponsored {
                sender: sender,
                poolId: pool_id,
                token: token,
                reservedAmount: reserved_amount,
                priceWithMarkup: price_with_markup,
            })
        }
        /**Creates a [`PoolEthSettled`] event.

```solidity
event PoolEthSettled(address,uint256,uint256,uint256)
```*/
        #[inline]
        pub fn pool_eth_settled(
            sender: alloy::sol_types::private::Address,
            pool_id: alloy::sol_types::private::primitives::aliases::U256,
            actual_gas_cost: alloy::sol_types::private::primitives::aliases::U256,
            charged_from_pool: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::PoolEthSettled(PoolEthSettled {
                sender: sender,
                poolId: pool_id,
                actualGasCost: actual_gas_cost,
                chargedFromPool: charged_from_pool,
            })
        }
        /**Creates a [`PoolEthSponsored`] event.

```solidity
event PoolEthSponsored(address,uint256,uint256)
```*/
        #[inline]
        pub fn pool_eth_sponsored(
            sender: alloy::sol_types::private::Address,
            pool_id: alloy::sol_types::private::primitives::aliases::U256,
            reserved: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::PoolEthSponsored(PoolEthSponsored {
                sender: sender,
                poolId: pool_id,
                reserved: reserved,
            })
        }
        /**Creates a [`SenderPoolChanged`] event.

```solidity
event SenderPoolChanged(address,uint256,uint256)
```*/
        #[inline]
        pub fn sender_pool_changed(
            sender: alloy::sol_types::private::Address,
            old_pool_id: alloy::sol_types::private::primitives::aliases::U256,
            new_pool_id: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::SenderPoolChanged(SenderPoolChanged {
                sender: sender,
                oldPoolId: old_pool_id,
                newPoolId: new_pool_id,
            })
        }
        /**Creates a [`Settled`] event.

```solidity
event Settled(address,uint256,uint64,uint128)
```*/
        #[inline]
        pub fn settled(
            sender: alloy::sol_types::private::Address,
            actual_gas_cost: alloy::sol_types::private::primitives::aliases::U256,
            epoch: u64,
            spent_in_epoch: u128,
        ) -> Self {
            Self::Settled(Settled {
                sender: sender,
                actualGasCost: actual_gas_cost,
                epoch: epoch,
                spentInEpoch: spent_in_epoch,
            })
        }
        /**Creates a [`Sponsored`] event.

```solidity
event Sponsored(address,uint256,uint64)
```*/
        #[inline]
        pub fn sponsored(
            sender: alloy::sol_types::private::Address,
            max_cost_estimate: alloy::sol_types::private::primitives::aliases::U256,
            epoch: u64,
        ) -> Self {
            Self::Sponsored(Sponsored {
                sender: sender,
                maxCostEstimate: max_cost_estimate,
                epoch: epoch,
            })
        }
        /**Creates a [`SponsoredAccountChanged`] event.

```solidity
event SponsoredAccountChanged(address,bool)
```*/
        #[inline]
        pub fn sponsored_account_changed(
            account: alloy::sol_types::private::Address,
            allowed: bool,
        ) -> Self {
            Self::SponsoredAccountChanged(SponsoredAccountChanged {
                account: account,
                allowed: allowed,
            })
        }
        /**Creates a [`Transfer`] event.

```solidity
event Transfer(address,address,address,uint256,uint256)
```*/
        #[inline]
        pub fn transfer(
            caller: alloy::sol_types::private::Address,
            from: alloy::sol_types::private::Address,
            to: alloy::sol_types::private::Address,
            id: alloy::sol_types::private::primitives::aliases::U256,
            amount: alloy::sol_types::private::primitives::aliases::U256,
        ) -> Self {
            Self::Transfer(Transfer {
                caller: caller,
                from: from,
                to: to,
                id: id,
                amount: amount,
            })
        }
        /**Creates a [`TrustedDelegateChanged`] event.

```solidity
event TrustedDelegateChanged(address,bool)
```*/
        #[inline]
        pub fn trusted_delegate_changed(
            delegate: alloy::sol_types::private::Address,
            allowed: bool,
        ) -> Self {
            Self::TrustedDelegateChanged(TrustedDelegateChanged {
                delegate: delegate,
                allowed: allowed,
            })
        }
        /**Creates a [`TuningUpdated`] event.

```solidity
event TuningUpdated(uint128,uint64)
```*/
        #[inline]
        pub fn tuning_updated(max_wei_per_epoch: u128, epoch_length: u64) -> Self {
            Self::TuningUpdated(TuningUpdated {
                maxWeiPerEpoch: max_wei_per_epoch,
                epochLength: epoch_length,
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
    }
    use alloy::contract as alloy_contract;
    /**Creates a new wrapper around an on-chain [`MevPaymasterV9`](self) contract instance.

See the [wrapper's documentation](`MevPaymasterV9Instance`) for more details.*/
    #[inline]
    pub const fn new<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    >(
        address: alloy_sol_types::private::Address,
        __provider: P,
    ) -> MevPaymasterV9Instance<P, N> {
        MevPaymasterV9Instance::<P, N>::new(address, __provider)
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
        ep: alloy::sol_types::private::Address,
        initialOwner: alloy::sol_types::private::Address,
    ) -> impl ::core::future::Future<
        Output = alloy_contract::Result<MevPaymasterV9Instance<P, N>>,
    > {
        MevPaymasterV9Instance::<P, N>::deploy(__provider, ep, initialOwner)
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
        ep: alloy::sol_types::private::Address,
        initialOwner: alloy::sol_types::private::Address,
    ) -> alloy_contract::RawCallBuilder<P, N> {
        MevPaymasterV9Instance::<P, N>::deploy_builder(__provider, ep, initialOwner)
    }
    /**A [`MevPaymasterV9`](self) instance.

Contains type-safe methods for interacting with an on-chain instance of the
[`MevPaymasterV9`](self) contract located at a given `address`, using a given
provider `P`.

If the contract bytecode is available (see the [`sol!`](alloy_sol_types::sol!)
documentation on how to provide it), the `deploy` and `deploy_builder` methods can
be used to deploy a new instance of the contract.

See the [module-level documentation](self) for all the available methods.*/
    #[derive(Clone)]
    pub struct MevPaymasterV9Instance<P, N = alloy_contract::private::Ethereum> {
        address: alloy_sol_types::private::Address,
        provider: P,
        _network: ::core::marker::PhantomData<N>,
    }
    #[automatically_derived]
    impl<P, N> ::core::fmt::Debug for MevPaymasterV9Instance<P, N> {
        #[inline]
        fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
            f.debug_tuple("MevPaymasterV9Instance").field(&self.address).finish()
        }
    }
    /// Instantiation and getters/setters.
    impl<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    > MevPaymasterV9Instance<P, N> {
        /**Creates a new wrapper around an on-chain [`MevPaymasterV9`](self) contract instance.

See the [wrapper's documentation](`MevPaymasterV9Instance`) for more details.*/
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
            ep: alloy::sol_types::private::Address,
            initialOwner: alloy::sol_types::private::Address,
        ) -> alloy_contract::Result<MevPaymasterV9Instance<P, N>> {
            let call_builder = Self::deploy_builder(__provider, ep, initialOwner);
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
            ep: alloy::sol_types::private::Address,
            initialOwner: alloy::sol_types::private::Address,
        ) -> alloy_contract::RawCallBuilder<P, N> {
            alloy_contract::RawCallBuilder::new_raw_deploy(
                __provider,
                [
                    &BYTECODE[..],
                    &alloy_sol_types::SolConstructor::abi_encode(
                        &constructorCall {
                            ep,
                            initialOwner,
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
    impl<P: ::core::clone::Clone, N> MevPaymasterV9Instance<&P, N> {
        /// Clones the provider and returns a new instance with the cloned provider.
        #[inline]
        pub fn with_cloned_provider(self) -> MevPaymasterV9Instance<P, N> {
            MevPaymasterV9Instance {
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
    > MevPaymasterV9Instance<P, N> {
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
        ///Creates a new call builder for the [`acceptOwnership`] function.
        pub fn acceptOwnership(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, acceptOwnershipCall, N> {
            self.call_builder(&acceptOwnershipCall)
        }
        ///Creates a new call builder for the [`addStake`] function.
        pub fn addStake(
            &self,
            unstakeDelaySec: u32,
        ) -> alloy_contract::SolCallBuilder<&P, addStakeCall, N> {
            self.call_builder(&addStakeCall { unstakeDelaySec })
        }
        ///Creates a new call builder for the [`allowance`] function.
        pub fn allowance(
            &self,
            _0: alloy::sol_types::private::Address,
            _1: alloy::sol_types::private::Address,
            _2: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, allowanceCall, N> {
            self.call_builder(&allowanceCall { _0, _1, _2 })
        }
        ///Creates a new call builder for the [`approve`] function.
        pub fn approve(
            &self,
            _0: alloy::sol_types::private::Address,
            _1: alloy::sol_types::private::primitives::aliases::U256,
            _2: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, approveCall, N> {
            self.call_builder(&approveCall { _0, _1, _2 })
        }
        ///Creates a new call builder for the [`balanceOf`] function.
        pub fn balanceOf(
            &self,
            owner: alloy::sol_types::private::Address,
            id: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, balanceOfCall, N> {
            self.call_builder(&balanceOfCall { owner, id })
        }
        ///Creates a new call builder for the [`budgets`] function.
        pub fn budgets(
            &self,
            _0: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, budgetsCall, N> {
            self.call_builder(&budgetsCall(_0))
        }
        ///Creates a new call builder for the [`burnPoolEth`] function.
        pub fn burnPoolEth(
            &self,
            poolId: alloy::sol_types::private::primitives::aliases::U256,
            amount: alloy::sol_types::private::primitives::aliases::U256,
            to: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, burnPoolEthCall, N> {
            self.call_builder(
                &burnPoolEthCall {
                    poolId,
                    amount,
                    to,
                },
            )
        }
        ///Creates a new call builder for the [`burnPoolToken`] function.
        pub fn burnPoolToken(
            &self,
            poolId: alloy::sol_types::private::primitives::aliases::U256,
            token: alloy::sol_types::private::Address,
            amount: alloy::sol_types::private::primitives::aliases::U256,
            to: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, burnPoolTokenCall, N> {
            self.call_builder(
                &burnPoolTokenCall {
                    poolId,
                    token,
                    amount,
                    to,
                },
            )
        }
        ///Creates a new call builder for the [`creditPoolEth`] function.
        pub fn creditPoolEth(
            &self,
            poolId: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, creditPoolEthCall, N> {
            self.call_builder(&creditPoolEthCall { poolId })
        }
        ///Creates a new call builder for the [`creditPoolToken`] function.
        pub fn creditPoolToken(
            &self,
            poolId: alloy::sol_types::private::primitives::aliases::U256,
            token: alloy::sol_types::private::Address,
            amount: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, creditPoolTokenCall, N> {
            self.call_builder(
                &creditPoolTokenCall {
                    poolId,
                    token,
                    amount,
                },
            )
        }
        ///Creates a new call builder for the [`currentEpoch`] function.
        pub fn currentEpoch(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, currentEpochCall, N> {
            self.call_builder(&currentEpochCall)
        }
        ///Creates a new call builder for the [`deposit`] function.
        pub fn deposit(&self) -> alloy_contract::SolCallBuilder<&P, depositCall, N> {
            self.call_builder(&depositCall)
        }
        ///Creates a new call builder for the [`entryPoint`] function.
        pub fn entryPoint(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, entryPointCall, N> {
            self.call_builder(&entryPointCall)
        }
        ///Creates a new call builder for the [`epochLength`] function.
        pub fn epochLength(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, epochLengthCall, N> {
            self.call_builder(&epochLengthCall)
        }
        ///Creates a new call builder for the [`erc20Config`] function.
        pub fn erc20Config(
            &self,
            token: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, erc20ConfigCall, N> {
            self.call_builder(&erc20ConfigCall { token })
        }
        ///Creates a new call builder for the [`ethOracle`] function.
        pub fn ethOracle(&self) -> alloy_contract::SolCallBuilder<&P, ethOracleCall, N> {
            self.call_builder(&ethOracleCall)
        }
        ///Creates a new call builder for the [`getDeposit`] function.
        pub fn getDeposit(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, getDepositCall, N> {
            self.call_builder(&getDepositCall)
        }
        ///Creates a new call builder for the [`isOperator`] function.
        pub fn isOperator(
            &self,
            _0: alloy::sol_types::private::Address,
            _1: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, isOperatorCall, N> {
            self.call_builder(&isOperatorCall { _0, _1 })
        }
        ///Creates a new call builder for the [`maxWeiPerEpoch`] function.
        pub fn maxWeiPerEpoch(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, maxWeiPerEpochCall, N> {
            self.call_builder(&maxWeiPerEpochCall)
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
        ///Creates a new call builder for the [`poolEthBalance`] function.
        pub fn poolEthBalance(
            &self,
            poolId: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, poolEthBalanceCall, N> {
            self.call_builder(&poolEthBalanceCall { poolId })
        }
        ///Creates a new call builder for the [`poolTokenBalance`] function.
        pub fn poolTokenBalance(
            &self,
            poolId: alloy::sol_types::private::primitives::aliases::U256,
            token: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, poolTokenBalanceCall, N> {
            self.call_builder(
                &poolTokenBalanceCall {
                    poolId,
                    token,
                },
            )
        }
        ///Creates a new call builder for the [`postOp`] function.
        pub fn postOp(
            &self,
            mode: <IPaymaster::PostOpMode as alloy::sol_types::SolType>::RustType,
            context: alloy::sol_types::private::Bytes,
            actualGasCost: alloy::sol_types::private::primitives::aliases::U256,
            actualUserOpFeePerGas: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, postOpCall, N> {
            self.call_builder(
                &postOpCall {
                    mode,
                    context,
                    actualGasCost,
                    actualUserOpFeePerGas,
                },
            )
        }
        ///Creates a new call builder for the [`remainingBudget`] function.
        pub fn remainingBudget(
            &self,
            sender: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, remainingBudgetCall, N> {
            self.call_builder(&remainingBudgetCall { sender })
        }
        ///Creates a new call builder for the [`removeErc20Token`] function.
        pub fn removeErc20Token(
            &self,
            token: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, removeErc20TokenCall, N> {
            self.call_builder(&removeErc20TokenCall { token })
        }
        ///Creates a new call builder for the [`renounceOwnership`] function.
        pub fn renounceOwnership(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, renounceOwnershipCall, N> {
            self.call_builder(&renounceOwnershipCall)
        }
        ///Creates a new call builder for the [`senderPool`] function.
        pub fn senderPool(
            &self,
            sender: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, senderPoolCall, N> {
            self.call_builder(&senderPoolCall { sender })
        }
        ///Creates a new call builder for the [`setErc20Config`] function.
        pub fn setErc20Config(
            &self,
            token: alloy::sol_types::private::Address,
            tokenOracle: alloy::sol_types::private::Address,
            maxStaleness: u32,
            markupBps: u16,
            treasury: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, setErc20ConfigCall, N> {
            self.call_builder(
                &setErc20ConfigCall {
                    token,
                    tokenOracle,
                    maxStaleness,
                    markupBps,
                    treasury,
                },
            )
        }
        ///Creates a new call builder for the [`setEthOracle`] function.
        pub fn setEthOracle(
            &self,
            newOracle: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, setEthOracleCall, N> {
            self.call_builder(&setEthOracleCall { newOracle })
        }
        ///Creates a new call builder for the [`setOperator`] function.
        pub fn setOperator(
            &self,
            _0: alloy::sol_types::private::Address,
            _1: bool,
        ) -> alloy_contract::SolCallBuilder<&P, setOperatorCall, N> {
            self.call_builder(&setOperatorCall { _0, _1 })
        }
        ///Creates a new call builder for the [`setSenderPool`] function.
        pub fn setSenderPool(
            &self,
            sender: alloy::sol_types::private::Address,
            poolId: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, setSenderPoolCall, N> {
            self.call_builder(
                &setSenderPoolCall {
                    sender,
                    poolId,
                },
            )
        }
        ///Creates a new call builder for the [`setSponsored`] function.
        pub fn setSponsored(
            &self,
            account: alloy::sol_types::private::Address,
            allowed: bool,
        ) -> alloy_contract::SolCallBuilder<&P, setSponsoredCall, N> {
            self.call_builder(
                &setSponsoredCall {
                    account,
                    allowed,
                },
            )
        }
        ///Creates a new call builder for the [`setTrustedDelegate`] function.
        pub fn setTrustedDelegate(
            &self,
            delegate: alloy::sol_types::private::Address,
            allowed: bool,
        ) -> alloy_contract::SolCallBuilder<&P, setTrustedDelegateCall, N> {
            self.call_builder(
                &setTrustedDelegateCall {
                    delegate,
                    allowed,
                },
            )
        }
        ///Creates a new call builder for the [`setTuning`] function.
        pub fn setTuning(
            &self,
            newMaxWeiPerEpoch: u128,
            newEpochLength: u64,
        ) -> alloy_contract::SolCallBuilder<&P, setTuningCall, N> {
            self.call_builder(
                &setTuningCall {
                    newMaxWeiPerEpoch,
                    newEpochLength,
                },
            )
        }
        ///Creates a new call builder for the [`sponsoredAccount`] function.
        pub fn sponsoredAccount(
            &self,
            _0: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, sponsoredAccountCall, N> {
            self.call_builder(&sponsoredAccountCall(_0))
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
        ///Creates a new call builder for the [`transfer`] function.
        pub fn transfer(
            &self,
            _0: alloy::sol_types::private::Address,
            _1: alloy::sol_types::private::primitives::aliases::U256,
            _2: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, transferCall, N> {
            self.call_builder(&transferCall { _0, _1, _2 })
        }
        ///Creates a new call builder for the [`transferFrom`] function.
        pub fn transferFrom(
            &self,
            _0: alloy::sol_types::private::Address,
            _1: alloy::sol_types::private::Address,
            _2: alloy::sol_types::private::primitives::aliases::U256,
            _3: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, transferFromCall, N> {
            self.call_builder(&transferFromCall { _0, _1, _2, _3 })
        }
        ///Creates a new call builder for the [`transferOwnership`] function.
        pub fn transferOwnership(
            &self,
            newOwner: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, transferOwnershipCall, N> {
            self.call_builder(&transferOwnershipCall { newOwner })
        }
        ///Creates a new call builder for the [`trustedDelegate`] function.
        pub fn trustedDelegate(
            &self,
            _0: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, trustedDelegateCall, N> {
            self.call_builder(&trustedDelegateCall(_0))
        }
        ///Creates a new call builder for the [`unlockStake`] function.
        pub fn unlockStake(
            &self,
        ) -> alloy_contract::SolCallBuilder<&P, unlockStakeCall, N> {
            self.call_builder(&unlockStakeCall)
        }
        ///Creates a new call builder for the [`unpause`] function.
        pub fn unpause(&self) -> alloy_contract::SolCallBuilder<&P, unpauseCall, N> {
            self.call_builder(&unpauseCall)
        }
        ///Creates a new call builder for the [`validatePaymasterUserOp`] function.
        pub fn validatePaymasterUserOp(
            &self,
            userOp: <PackedUserOperation as alloy::sol_types::SolType>::RustType,
            userOpHash: alloy::sol_types::private::FixedBytes<32>,
            maxCost: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, validatePaymasterUserOpCall, N> {
            self.call_builder(
                &validatePaymasterUserOpCall {
                    userOp,
                    userOpHash,
                    maxCost,
                },
            )
        }
        ///Creates a new call builder for the [`withdrawNativeBalance`] function.
        pub fn withdrawNativeBalance(
            &self,
            to: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, withdrawNativeBalanceCall, N> {
            self.call_builder(&withdrawNativeBalanceCall { to })
        }
        ///Creates a new call builder for the [`withdrawStake`] function.
        pub fn withdrawStake(
            &self,
            withdrawAddress: alloy::sol_types::private::Address,
        ) -> alloy_contract::SolCallBuilder<&P, withdrawStakeCall, N> {
            self.call_builder(
                &withdrawStakeCall {
                    withdrawAddress,
                },
            )
        }
        ///Creates a new call builder for the [`withdrawTo`] function.
        pub fn withdrawTo(
            &self,
            withdrawAddress: alloy::sol_types::private::Address,
            amount: alloy::sol_types::private::primitives::aliases::U256,
        ) -> alloy_contract::SolCallBuilder<&P, withdrawToCall, N> {
            self.call_builder(
                &withdrawToCall {
                    withdrawAddress,
                    amount,
                },
            )
        }
    }
    /// Event filters.
    impl<
        P: alloy_contract::private::Provider<N>,
        N: alloy_contract::private::Network,
    > MevPaymasterV9Instance<P, N> {
        /// Creates a new event filter using this contract instance's provider and address.
        ///
        /// Note that the type can be any event, not just those defined in this contract.
        /// Prefer using the other methods for building type-safe event filters.
        pub fn event_filter<E: alloy_sol_types::SolEvent>(
            &self,
        ) -> alloy_contract::Event<&P, E, N> {
            alloy_contract::Event::new_sol(&self.provider, &self.address)
        }
        ///Creates a new event filter for the [`Erc20ConfigChanged`] event.
        pub fn Erc20ConfigChanged_filter(
            &self,
        ) -> alloy_contract::Event<&P, Erc20ConfigChanged, N> {
            self.event_filter::<Erc20ConfigChanged>()
        }
        ///Creates a new event filter for the [`Erc20Settled`] event.
        pub fn Erc20Settled_filter(&self) -> alloy_contract::Event<&P, Erc20Settled, N> {
            self.event_filter::<Erc20Settled>()
        }
        ///Creates a new event filter for the [`Erc20Sponsored`] event.
        pub fn Erc20Sponsored_filter(
            &self,
        ) -> alloy_contract::Event<&P, Erc20Sponsored, N> {
            self.event_filter::<Erc20Sponsored>()
        }
        ///Creates a new event filter for the [`EthOracleChanged`] event.
        pub fn EthOracleChanged_filter(
            &self,
        ) -> alloy_contract::Event<&P, EthOracleChanged, N> {
            self.event_filter::<EthOracleChanged>()
        }
        ///Creates a new event filter for the [`NativeBalanceWithdrawn`] event.
        pub fn NativeBalanceWithdrawn_filter(
            &self,
        ) -> alloy_contract::Event<&P, NativeBalanceWithdrawn, N> {
            self.event_filter::<NativeBalanceWithdrawn>()
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
        ///Creates a new event filter for the [`PoolErc20Settled`] event.
        pub fn PoolErc20Settled_filter(
            &self,
        ) -> alloy_contract::Event<&P, PoolErc20Settled, N> {
            self.event_filter::<PoolErc20Settled>()
        }
        ///Creates a new event filter for the [`PoolErc20Sponsored`] event.
        pub fn PoolErc20Sponsored_filter(
            &self,
        ) -> alloy_contract::Event<&P, PoolErc20Sponsored, N> {
            self.event_filter::<PoolErc20Sponsored>()
        }
        ///Creates a new event filter for the [`PoolEthSettled`] event.
        pub fn PoolEthSettled_filter(
            &self,
        ) -> alloy_contract::Event<&P, PoolEthSettled, N> {
            self.event_filter::<PoolEthSettled>()
        }
        ///Creates a new event filter for the [`PoolEthSponsored`] event.
        pub fn PoolEthSponsored_filter(
            &self,
        ) -> alloy_contract::Event<&P, PoolEthSponsored, N> {
            self.event_filter::<PoolEthSponsored>()
        }
        ///Creates a new event filter for the [`SenderPoolChanged`] event.
        pub fn SenderPoolChanged_filter(
            &self,
        ) -> alloy_contract::Event<&P, SenderPoolChanged, N> {
            self.event_filter::<SenderPoolChanged>()
        }
        ///Creates a new event filter for the [`Settled`] event.
        pub fn Settled_filter(&self) -> alloy_contract::Event<&P, Settled, N> {
            self.event_filter::<Settled>()
        }
        ///Creates a new event filter for the [`Sponsored`] event.
        pub fn Sponsored_filter(&self) -> alloy_contract::Event<&P, Sponsored, N> {
            self.event_filter::<Sponsored>()
        }
        ///Creates a new event filter for the [`SponsoredAccountChanged`] event.
        pub fn SponsoredAccountChanged_filter(
            &self,
        ) -> alloy_contract::Event<&P, SponsoredAccountChanged, N> {
            self.event_filter::<SponsoredAccountChanged>()
        }
        ///Creates a new event filter for the [`Transfer`] event.
        pub fn Transfer_filter(&self) -> alloy_contract::Event<&P, Transfer, N> {
            self.event_filter::<Transfer>()
        }
        ///Creates a new event filter for the [`TrustedDelegateChanged`] event.
        pub fn TrustedDelegateChanged_filter(
            &self,
        ) -> alloy_contract::Event<&P, TrustedDelegateChanged, N> {
            self.event_filter::<TrustedDelegateChanged>()
        }
        ///Creates a new event filter for the [`TuningUpdated`] event.
        pub fn TuningUpdated_filter(
            &self,
        ) -> alloy_contract::Event<&P, TuningUpdated, N> {
            self.event_filter::<TuningUpdated>()
        }
        ///Creates a new event filter for the [`Unpaused`] event.
        pub fn Unpaused_filter(&self) -> alloy_contract::Event<&P, Unpaused, N> {
            self.event_filter::<Unpaused>()
        }
    }
}
