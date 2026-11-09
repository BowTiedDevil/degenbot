//! The declared contract interface (ADR-100) — the neutral declaration of one
//! contract's callable surface, its transaction policy, the per-encode checks,
//! the optional [`ValidatedAdapter`] wrapper, and tier 2's explicit validation
//! cases.
//!
//! Three tiers, one direction of dependency (ADR-100 D1). This module is tier 1
//! and tier 2; tier 0 is the absence of a declaration (today's path, unchanged)
//! and tier 3 is execution-backed certification, which lives in the fork lane.
//!
//! - **Tier 1 — contract interface.** An operator declares the contract's ABI
//!   and, optionally, the target it must be called at, the selectors it may be
//!   called with, and the native value it accepts. Every `encode` through a
//!   [`ValidatedAdapter`] checks the produced transaction against that
//!   declaration. A declaration is derived from standard ABI JSON
//!   ([`ContractInterface::from_abi_json`]) or from the same `sol!` signatures
//!   that produce the calldata ([`ContractInterface::from_signatures`]), so a
//!   Rust adapter needs no second ABI source.
//! - **Tier 2 — validation cases.** Explicit `path` + `amounts` + expected
//!   transaction literals, run by [`run_cases`] when a caller asks. A case hands
//!   the adapter a real [`PathInfo`] and real [`ComposerInputs`], so it exercises
//!   the entry point production uses — no chain I/O, no engine, no registry.
//!
//! A violation is a **hard** encode failure ([`crate::ComposeError::Validation`]),
//! never the routine capability decline: an adapter that produces calldata its
//! own declaration rejects is broken, and offering the same path to another
//! target would hide the defect. A pass is a *screen*, not a proof — only
//! execution shows what the contract does with the bytes (ADR-100 D8).
//!
//! The declaration is compiled once, at construction: a selector-keyed table of
//! declared functions, their signatures, and their resolved parameter types. Per
//! encode the cost is one selector lookup and one argument decode over the bytes
//! after the selector — no keccak, no provider, no ABI re-parse. An adapter that
//! declares nothing pays none of it: no wrapper exists, so no decode runs.

use std::collections::{BTreeMap, BTreeSet};

use alloy::dyn_abi::{DynSolType, DynSolValue, JsonAbiExt, Specifier};
use alloy::json_abi::{Function, JsonAbi};
use alloy::primitives::hex;
use alloy::primitives::{Address, Bytes, U256};
use thiserror::Error;

use crate::adapter::ExecutionAdapter;
use crate::gate::{AssessError, AssessOptions, ExecutionResult, FeePolicy, ProbeSpec, ProbeSpecs};
use crate::observation::ExecutionObservation;
use crate::path::{HopInfo, PathInfo};
use crate::payload::{ComposeError, ComposeOptions, ComposerInputs};
use crate::solve_result::{HopFamily, SolveResult};
use crate::transaction::TransactionRequest;

/// The declaration is unusable (ADR-100 D5).
///
/// Produced where a declaration is read — construction, or a case document being
/// turned into typed values. Reading a declaration invokes no operator callback
/// and performs no I/O (ADR-100 D9), so every arm is pure data.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DeclarationError {
    /// The text is not a standard ABI JSON array.
    #[error("the ABI JSON is not a standard ABI array: {detail}")]
    AbiJson {
        /// What the parser refused.
        detail: String,
    },

    /// The ABI declares no function, so the interface could never validate
    /// anything.
    #[error("the declaration contains no function")]
    EmptyInterface,

    /// Two declared functions share a 4-byte selector.
    ///
    /// Resolved by refusal rather than by ordering: the declaration would
    /// otherwise be ambiguous about which argument types a given calldata must
    /// decode against.
    #[error(
        "declared functions `{first}` and `{second}` share selector 0x{}",
        hex::encode(selector)
    )]
    DuplicateSelector {
        /// The shared 4-byte selector.
        selector: [u8; 4],
        /// The first signature that claimed it.
        first: String,
        /// The second signature that claimed it.
        second: String,
    },

    /// An allow-listed signature is not declared by the ABI.
    #[error("allow-listed signature `{signature}` is not declared by the ABI")]
    UnknownSelector {
        /// The signature the allow-list named.
        signature: String,
    },

    /// A `sol!`-route signature string does not parse.
    #[error("signature `{signature}` is not a function signature")]
    UnparsableSignature {
        /// The signature string as given.
        signature: String,
    },

    /// A declared target is the zero address — a typo, not a deployment.
    #[error("a declared target is the zero address")]
    ZeroTarget,

    /// The native-value policy cannot mean anything.
    #[error("invalid value policy: {detail}")]
    InvalidValuePolicy {
        /// What the policy document said.
        detail: String,
    },

    /// A declared probe that targets the interface's own contract names an
    /// undeclared selector, or carries argument bytes that do not decode against
    /// its declared types. A probe to a *different* contract is not this
    /// interface's business and is left alone.
    #[error("declared probe `{label}`: {detail}")]
    Probe {
        /// The probe's diagnostic label.
        label: String,
        /// What the interface refused.
        detail: String,
    },

    /// A case document is malformed.
    #[error("case `{case}`: {detail}")]
    CaseDocument {
        /// The case's name (a `?` when the document did not declare one).
        case: String,
        /// What the document said that cannot be typed.
        detail: String,
    },

    /// A case's hop amounts do not match its hop count.
    #[error("case `{case}`: {detail}")]
    CaseArity {
        /// The case's name.
        case: String,
        /// Which list disagrees and by how much.
        detail: String,
    },

    /// A case amount is outside the seam's `int128` bound.
    ///
    /// [`CaseAmounts`] carries unsigned amounts, so a negative is unrepresentable
    /// rather than refused; a value above `i128::MAX` is this arm.
    #[error("case `{case}`: amount `{field}` is above the seam's int128 bound")]
    CaseAmount {
        /// The case's name.
        case: String,
        /// The offending amount, named by its path within the case.
        field: String,
    },
}

/// The encoder's output disagrees with the declaration (ADR-100 D5).
///
/// Produced per encode (tier 1) and per case comparison (tier 2), so both tiers
/// speak one vocabulary. Every variant names the offending field.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContractViolation {
    /// The payload is shorter than a 4-byte selector.
    #[error("calldata {detail}")]
    Calldata {
        /// The measured payload.
        detail: String,
    },

    /// The selector is not declared (or not in the allow-list).
    #[error(
        "selector 0x{} is not declared by the interface",
        hex::encode(selector)
    )]
    Selector {
        /// The refused 4-byte selector.
        selector: [u8; 4],
    },

    /// The bytes after the selector do not decode against the declared parameter
    /// types, or are not the canonical encoding of those values.
    #[error("`{signature}` arguments do not decode: {detail}")]
    Arguments {
        /// The declared signature the calldata was checked against.
        signature: String,
        /// What the decoder or the exactness check refused.
        detail: String,
    },

    /// A declared target disagrees with the request's `to`.
    #[error("declared target {declared} does not match the encoded target {encoded}")]
    Target {
        /// The declaration's target.
        declared: Address,
        /// The transaction's own `to`.
        encoded: Address,
    },

    /// The request's native value violates the declared policy.
    #[error(
        "native value {encoded} violates the `{}` policy", policy.name()
    )]
    NativeValue {
        /// The declared policy.
        policy: ValuePolicy,
        /// The transaction's own value (wei).
        encoded: U256,
    },

    /// A case's declared evidence disagrees with what the encoder produced
    /// (tier 2 only).
    #[error("case `{case}`: `{field}` expected {expected}, the adapter produced {actual}")]
    CaseExpectation {
        /// The case's name.
        case: String,
        /// The field that disagrees (`selector`, `target`, `value`,
        /// `calldata`, `arguments`, `arguments[i]`).
        field: String,
        /// The declared evidence, rendered.
        expected: String,
        /// The produced evidence, rendered.
        actual: String,
    },
}

/// The native-value policy a declaration promises to obey (ADR-100 D2).
///
/// Omitted means [`ValuePolicy::Any`]: native value is a funding decision the
/// deployment owns (ADR-098 D2), not a fact the ABI states. The other three
/// forms make it a declared fact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ValuePolicy {
    /// No check.
    #[default]
    Any,
    /// `value == 0` — a flash-funded contract.
    Zero,
    /// `value <= max`.
    Max(U256),
    /// `value == exact`.
    Exact(U256),
}

impl ValuePolicy {
    /// The policy's declaration-document `kind`.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Zero => "zero",
            Self::Max(_) => "max",
            Self::Exact(_) => "exact",
        }
    }

    /// Read a declaration document's `value` policy: its `kind` plus the optional
    /// decimal amount (`max_wei` / `exact_wei`).
    ///
    /// The amount is a decimal string, never a JSON number: a u256 bound does not
    /// survive a float. `any` and `zero` ignore an amount.
    ///
    /// # Errors
    ///
    /// [`DeclarationError::InvalidValuePolicy`] for an unknown `kind`, a missing
    /// amount on `max`/`exact`, a non-decimal amount, or an amount above u256.
    pub fn parse(kind: &str, amount: Option<&str>) -> Result<Self, DeclarationError> {
        let bound = |field: &str| -> Result<U256, DeclarationError> {
            let amount = amount.ok_or_else(|| DeclarationError::InvalidValuePolicy {
                detail: format!("`{kind}` requires `{field}`"),
            })?;
            U256::from_str_radix(amount, 10).map_err(|_| DeclarationError::InvalidValuePolicy {
                detail: format!("`{field}` is not a decimal u256: `{amount}`"),
            })
        };

        match kind {
            "any" => Ok(Self::Any),
            "zero" => Ok(Self::Zero),
            "max" => Ok(Self::Max(bound("max_wei")?)),
            "exact" => Ok(Self::Exact(bound("exact_wei")?)),
            other => Err(DeclarationError::InvalidValuePolicy {
                detail: format!("`{other}` is not a value-policy kind"),
            }),
        }
    }

    /// Whether `value` satisfies this policy.
    #[must_use]
    pub fn accepts(&self, value: U256) -> bool {
        match self {
            Self::Any => true,
            Self::Zero => value.is_zero(),
            Self::Max(maximum) => value <= *maximum,
            Self::Exact(exact) => value == *exact,
        }
    }
}

/// One declared function: its selector, its canonical signature, and its
/// resolved parameter types — the compiled table's row (ADR-100 D6).
#[derive(Clone, Debug)]
struct DeclaredFunction {
    /// The 4-byte selector `keccak256(signature)[..4]`, as Alloy computes it.
    selector: [u8; 4],
    /// The canonical signature (`name(type,type,…)`), the diagnostic name.
    signature: String,
    /// The declared parameter types, resolved to Alloy's dynamic types.
    parameters: Vec<DynSolType>,
    /// The ABI item itself — the decoder's own type source, so no repository
    /// code re-derives the ABI's rules.
    function: Function,
}

impl DeclaredFunction {
    /// Compile one ABI item: resolve its parameter types and keep its selector.
    fn from_function(function: &Function) -> Result<Self, String> {
        let parameters = function
            .inputs
            .iter()
            .map(Specifier::resolve)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            selector: function.selector().0,
            signature: function.signature(),
            parameters,
            function: function.clone(),
        })
    }

    /// Compile one `sol!`-route signature string.
    fn from_signature(signature: &str) -> Result<Self, DeclarationError> {
        let function =
            Function::parse(signature).map_err(|_| DeclarationError::UnparsableSignature {
                signature: signature.to_string(),
            })?;
        Self::from_function(&function).map_err(|_| DeclarationError::UnparsableSignature {
            signature: signature.to_string(),
        })
    }

    /// Decode `arguments` (the payload after the selector) against this
    /// function's own parameter types, requiring the canonical encoding.
    ///
    /// Alloy's dynamic decoder does not expose its strict mode through
    /// [`DynSolType`], so exactness — no trailing bytes, no non-canonical
    /// dynamic offsets or padding — is checked by re-encoding the decoded values
    /// and comparing. That comparison is what makes a padded payload
    /// distinguishable from the declared call; it runs only on a declared
    /// interface, never on the default path.
    fn decode(&self, arguments: &[u8]) -> Result<Vec<DynSolValue>, ContractViolation> {
        let refuse = |detail: String| ContractViolation::Arguments {
            signature: self.signature.clone(),
            detail,
        };
        let values = self
            .function
            .abi_decode_input(arguments)
            .map_err(|error| refuse(error.to_string()))?;
        let canonical = self
            .function
            .abi_encode_input_raw(&values)
            .map_err(|error| refuse(error.to_string()))?;
        if canonical != arguments {
            return Err(refuse(format!(
                "the payload is not the canonical encoding of the declared arguments \
                 ({} decoded bytes, {} encoded bytes)",
                arguments.len(),
                canonical.len()
            )));
        }
        Ok(values)
    }
}

/// The compiled declaration of one contract's callable surface plus its
/// transaction policy (ADR-100 D2).
///
/// Built from standard ABI JSON ([`Self::from_abi_json`]) or from `sol!`
/// signatures ([`Self::from_signatures`]), then optionally narrowed by
/// [`Self::with_target`], [`Self::with_selectors`], and
/// [`Self::with_value_policy`]. The declaration holds no version, no release tag,
/// and no capability list (ADR-100 D2 rule 6): it describes one contract's
/// surface and nothing about its build or its future.
///
/// [`Self::validate`] checks one [`TransactionRequest`]; connect it to the seam
/// by wrapping an adapter in [`ValidatedAdapter`].
#[derive(Clone, Debug)]
pub struct ContractInterface {
    /// Selector-keyed declared functions — the compiled table (ADR-100 D6).
    functions: BTreeMap<[u8; 4], DeclaredFunction>,
    /// When present, the transaction's `to` must equal it.
    target: Option<Address>,
    /// When present, the encoded selector must be one of these.
    selectors: Option<BTreeSet<[u8; 4]>>,
    /// The native-value policy; [`ValuePolicy::Any`] when omitted.
    value: ValuePolicy,
}

impl ContractInterface {
    /// Read a standard Solidity **ABI JSON array**.
    ///
    /// Only `type: "function"` entries are part of the callable surface;
    /// `constructor`, `event`, `error`, `fallback`, and `receive` entries are
    /// accepted and ignored. Each function's selector is Alloy's
    /// `keccak256(canonical signature)[..4]`, so a declaration and a toolchain
    /// cannot disagree about what a signature means.
    ///
    /// # Errors
    ///
    /// [`DeclarationError::AbiJson`] when the text is not an ABI JSON array or a
    /// declared parameter type does not resolve, [`DeclarationError::EmptyInterface`]
    /// when it declares no function, [`DeclarationError::DuplicateSelector`] when
    /// two declared functions share a selector.
    pub fn from_abi_json(abi: &str) -> Result<Self, DeclarationError> {
        let parsed = JsonAbi::from_json_str(abi).map_err(|error| DeclarationError::AbiJson {
            detail: error.to_string(),
        })?;
        let mut functions = BTreeMap::new();
        for function in parsed.functions() {
            let declared = DeclaredFunction::from_function(function).map_err(|detail| {
                DeclarationError::AbiJson {
                    detail: format!("`{}`: {detail}", function.signature()),
                }
            })?;
            insert_declared(&mut functions, declared)?;
        }
        Self::from_table(functions)
    }

    /// Read the same declaration from `sol!` signature strings — the Rust-facing
    /// route (ADR-100 D3).
    ///
    /// This is the constructor that keeps an encoder and its declaration from
    /// drifting: derive both from one `sol!` type, and no hand-written ABI JSON
    /// can disagree with the calldata the adapter emits.
    ///
    /// # Errors
    ///
    /// [`DeclarationError::UnparsableSignature`] when a string is not a function
    /// signature or a parameter type does not resolve,
    /// [`DeclarationError::EmptyInterface`] when no signature is given,
    /// [`DeclarationError::DuplicateSelector`] when two signatures share a
    /// selector.
    pub fn from_signatures<I, S>(signatures: I) -> Result<Self, DeclarationError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut functions = BTreeMap::new();
        for signature in signatures {
            insert_declared(
                &mut functions,
                DeclaredFunction::from_signature(signature.as_ref())?,
            )?;
        }
        Self::from_table(functions)
    }

    fn from_table(
        functions: BTreeMap<[u8; 4], DeclaredFunction>,
    ) -> Result<Self, DeclarationError> {
        if functions.is_empty() {
            return Err(DeclarationError::EmptyInterface);
        }
        Ok(Self {
            functions,
            target: None,
            selectors: None,
            value: ValuePolicy::Any,
        })
    }

    /// Declare the one address this interface's transactions must target.
    ///
    /// # Errors
    ///
    /// [`DeclarationError::ZeroTarget`] for the zero address — a typo, not a
    /// deployment.
    pub fn with_target(self, target: Address) -> Result<Self, DeclarationError> {
        if target == Address::ZERO {
            return Err(DeclarationError::ZeroTarget);
        }
        Ok(Self {
            target: Some(target),
            ..self
        })
    }

    /// Narrow the callable surface to an allow-list of signatures drawn from the
    /// ABI. Absent means every function the ABI declares is allowed.
    ///
    /// # Errors
    ///
    /// [`DeclarationError::UnknownSelector`] when a signature is not declared by
    /// the ABI — a typo to refuse, not an implicit second declaration.
    /// [`DeclarationError::UnparsableSignature`] when a signature string does not
    /// parse.
    pub fn with_selectors<I, S>(mut self, signatures: I) -> Result<Self, DeclarationError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut allowed = BTreeSet::new();
        for signature in signatures {
            let signature = signature.as_ref();
            let function =
                Function::parse(signature).map_err(|_| DeclarationError::UnparsableSignature {
                    signature: signature.to_string(),
                })?;
            if !self.functions.contains_key(&function.selector().0) {
                return Err(DeclarationError::UnknownSelector {
                    signature: signature.to_string(),
                });
            }
            allowed.insert(function.selector().0);
        }
        self.selectors = Some(allowed);
        Ok(self)
    }

    /// Declare the native-value policy the adapter promises to obey.
    #[must_use]
    pub fn with_value_policy(mut self, policy: ValuePolicy) -> Self {
        self.value = policy;
        self
    }

    /// The declared target, or `None` when the interface is address-agnostic.
    #[must_use]
    pub const fn target(&self) -> Option<Address> {
        self.target
    }

    /// The declared native-value policy.
    #[must_use]
    pub const fn value_policy(&self) -> ValuePolicy {
        self.value
    }

    /// The effective allow-list: the declared signatures this interface accepts,
    /// in selector order. Every declared signature when no allow-list is set.
    #[must_use]
    pub fn selectors(&self) -> Vec<&str> {
        self.functions
            .iter()
            .filter(|(selector, _)| {
                self.selectors
                    .as_ref()
                    .is_none_or(|allowed| allowed.contains(*selector))
            })
            .map(|(_, declared)| declared.signature.as_str())
            .collect()
    }

    /// The declared parameter types of one signature, or `None` when the
    /// interface does not declare it.
    ///
    /// The second declaration route's drift gate reads this: a `sol!`-derived
    /// interface and its ABI JSON form must resolve the same parameter types.
    #[must_use]
    pub fn parameter_types(&self, signature: &str) -> Option<&[DynSolType]> {
        self.functions
            .values()
            .find(|declared| declared.signature == signature)
            .map(|declared| declared.parameters.as_slice())
    }

    /// Check one complete transaction against this declaration: declared
    /// selector, arguments that decode against the declared types, allowed
    /// target, allowed native value.
    ///
    /// The transaction is inspected, never modified: a refusal leaves the
    /// request exactly as the adapter produced it (ADR-100 D4).
    ///
    /// # Errors
    ///
    /// [`ContractViolation`] naming the field that disagrees with the
    /// declaration.
    pub fn validate(&self, transaction: &TransactionRequest) -> Result<(), ContractViolation> {
        self.resolve_call(&transaction.data)?;
        if let Some(declared) = self.target {
            if transaction.to != declared {
                return Err(ContractViolation::Target {
                    declared,
                    encoded: transaction.to,
                });
            }
        }
        if !self.value.accepts(transaction.value) {
            return Err(ContractViolation::NativeValue {
                policy: self.value,
                encoded: transaction.value,
            });
        }
        Ok(())
    }

    /// Check an adapter's declared probe list against this declaration (ADR-100
    /// D9).
    ///
    /// A probe to the interface's own contract is a read of that contract, so it
    /// must name a declared selector and its argument bytes must decode. A probe
    /// to another contract (WETH's `balanceOf`, a credit ledger) is not this
    /// interface's business and is left alone. Probes are *data*: checking them
    /// invokes no callback and reads no chain.
    ///
    /// # Errors
    ///
    /// [`DeclarationError::Probe`] naming the probe's label.
    pub fn validate_probes(&self, probes: &[ProbeSpec]) -> Result<(), DeclarationError> {
        let Some(target) = self.target else {
            return Ok(());
        };
        for probe in probes {
            let ProbeSpec::Call { label, to, data } = probe else {
                continue;
            };
            if *to != target {
                continue;
            }
            let refuse = |detail: String| DeclarationError::Probe {
                label: label.clone(),
                detail,
            };
            let Some(selector) = data.get(..4) else {
                return Err(refuse(format!(
                    "calldata is {} bytes, shorter than a 4-byte selector",
                    data.len()
                )));
            };
            let selector: [u8; 4] = selector
                .try_into()
                .map_err(|_| refuse("calldata is shorter than a 4-byte selector".to_string()))?;
            let Some(declared) = self.functions.get(&selector) else {
                return Err(refuse(format!(
                    "selector 0x{} is not declared by the interface",
                    hex::encode(selector)
                )));
            };
            declared
                .decode(data.get(4..).unwrap_or_default())
                .map_err(|violation| refuse(violation.to_string()))?;
        }
        Ok(())
    }

    /// Resolve the payload's declared function and decode its arguments.
    fn resolve_call(
        &self,
        data: &[u8],
    ) -> Result<(&DeclaredFunction, Vec<DynSolValue>), ContractViolation> {
        let Some(selector) = data.get(..4) else {
            return Err(ContractViolation::Calldata {
                detail: format!("is {} bytes, shorter than a 4-byte selector", data.len()),
            });
        };
        let mut key = [0u8; 4];
        key.copy_from_slice(selector);
        if let Some(allowed) = &self.selectors {
            if !allowed.contains(&key) {
                return Err(ContractViolation::Selector { selector: key });
            }
        }
        let Some(declared) = self.functions.get(&key) else {
            return Err(ContractViolation::Selector { selector: key });
        };
        let values = declared.decode(data.get(4..).unwrap_or_default())?;
        Ok((declared, values))
    }
}

/// Insert one compiled declaration, refusing a shared selector.
fn insert_declared(
    functions: &mut BTreeMap<[u8; 4], DeclaredFunction>,
    declared: DeclaredFunction,
) -> Result<(), DeclarationError> {
    if let Some(existing) = functions.get(&declared.selector) {
        return Err(DeclarationError::DuplicateSelector {
            selector: declared.selector,
            first: existing.signature.clone(),
            second: declared.signature,
        });
    }
    functions.insert(declared.selector, declared);
    Ok(())
}

/// Attach a [`ContractInterface`] to any [`ExecutionAdapter`] (ADR-100 D4).
///
/// The wrapper **forwards and never repairs**: an encode either returns the
/// inner's complete request unchanged or returns an error. There is no coercion
/// of `to`, no zeroing of `value`, and no re-encode — repair would invent a
/// transaction the adapter did not produce, which is what the seam exists to
/// prevent.
///
/// The seam's trait gain no method for this, so an adapter that declares nothing
/// is untouched: tier 0 has no wrapper and pays nothing.
pub struct ValidatedAdapter<A> {
    inner: A,
    interface: ContractInterface,
}

impl<A> ValidatedAdapter<A> {
    /// Attach the declaration.
    ///
    /// Fallible because the inner adapter's declared probes are checked against
    /// the same interface once, here: a probe that reads the interface's own
    /// contract must name a declared selector with decodable arguments. The
    /// adapter's encode callback is never invoked at this point (ADR-100 D9).
    ///
    /// # Errors
    ///
    /// [`DeclarationError::Probe`] for a probe to the interface's own contract
    /// that the declaration does not describe.
    pub fn new(inner: A, interface: ContractInterface) -> Result<Self, DeclarationError>
    where
        A: ExecutionAdapter,
    {
        interface.validate_probes(inner.probe_spec())?;
        Ok(Self { inner, interface })
    }

    /// The attached declaration.
    #[must_use]
    pub const fn interface(&self) -> &ContractInterface {
        &self.interface
    }

    /// Unwrap the inner adapter, dropping the declaration.
    #[must_use]
    pub fn into_inner(self) -> A {
        self.inner
    }
}

impl<A: ExecutionAdapter> ExecutionAdapter for ValidatedAdapter<A> {
    /// Runs the inner `encode`, then validates the returned transaction against
    /// the interface before returning it.
    ///
    /// # Errors
    ///
    /// The inner's own [`ComposeError`], or [`ComposeError::Validation`] when the
    /// produced transaction violates the declaration.
    fn encode(
        &self,
        path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        let transaction = self.inner.encode(path, inputs)?;
        self.interface.validate(&transaction)?;
        Ok(transaction)
    }

    /// The inner's declared probes, unchanged.
    fn probe_spec(&self) -> &ProbeSpecs {
        self.inner.probe_spec()
    }

    /// The inner's assessment defaults, unchanged.
    fn assess_options(&self) -> AssessOptions {
        self.inner.assess_options()
    }

    /// The inner's pricing policy, unchanged.
    fn fee_policy(&self) -> FeePolicy {
        self.inner.fee_policy()
    }

    /// The inner's assessment, unchanged — custom overrides included, with
    /// [`ExecutionResult::price`]'s terminal-status invariant intact.
    ///
    /// # Errors
    ///
    /// Whatever the inner assessment returns.
    fn assess(&self, observation: &ExecutionObservation) -> Result<ExecutionResult, AssessError> {
        self.inner.assess(observation)
    }

    /// Runs the **inner's** `compose_view` — so an inner override of that
    /// projection is never bypassed — then validates.
    ///
    /// Both encode entry points are validated because both reach production:
    /// `compose_transaction` calls `encode`, and a consumer may call
    /// `compose_view`.
    ///
    /// # Errors
    ///
    /// The inner's own [`ComposeError`], or [`ComposeError::Validation`] when the
    /// projected transaction violates the declaration.
    fn compose_view(
        &self,
        path: &PathInfo,
        result: &SolveResult,
    ) -> Result<TransactionRequest, ComposeError> {
        let transaction = self.inner.compose_view(path, result)?;
        self.interface.validate(&transaction)?;
        Ok(transaction)
    }
}

/// A validation case's amounts — the solver-driven bundle
/// [`ExecutionAdapter::encode`] takes, in the seam's own integer units.
///
/// Amounts are `u128` (the seam's `int128` convention, never floats), so a
/// negative is unrepresentable; a value above `i128::MAX` is
/// [`DeclarationError::CaseAmount`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseAmounts {
    /// The flash/optimal input amount.
    pub optimal_input: u128,
    /// Per-hop output amounts; exactly one per hop.
    pub hop_outputs: Vec<u128>,
    /// Per-hop consumed input amounts; exactly one per hop.
    pub consumed_inputs: Vec<u128>,
}

/// What a case expects from the adapter (ADR-100 D7).
#[derive(Clone, Debug, PartialEq)]
pub enum Expected {
    /// The adapter must produce this transaction.
    Transaction(ExpectedTransaction),
    /// The adapter must refuse with [`ComposeError::Unsupported`] — a capability
    /// decline, not a violation.
    Decline,
}

/// The transaction a case expects.
#[derive(Clone, Debug, PartialEq)]
pub struct ExpectedTransaction {
    /// The canonical function signature the call must be (`execute(...)`).
    pub selector: String,
    /// The target the call must name; `None` defers to the interface's own
    /// declared policy.
    pub target: Option<Address>,
    /// The native value the call must carry; `None` defers to the interface's
    /// own declared policy.
    pub value: Option<U256>,
    /// The expected call's arguments or its exact calldata golden — `None`
    /// checks the declaration alone. The two forms are mutually exclusive by
    /// construction.
    pub call: Option<ExpectedCall>,
}

/// The expected call, as decoded arguments or as exact calldata.
#[derive(Clone, Debug, PartialEq)]
pub enum ExpectedCall {
    /// Decoded argument values in declaration order. The produced calldata is
    /// decoded against the interface's declared types and compared, so the
    /// expectation speaks the call's own vocabulary rather than its bytes.
    Arguments(Vec<DynSolValue>),
    /// An exact calldata golden.
    Calldata(Bytes),
}

/// A literal that says: for *this* path, with *these* amounts, the adapter must
/// produce *that* transaction (or decline) (ADR-100 D7).
///
/// The path is data — real [`HopInfo`] descriptors the case owns, never a pool
/// lookup, an `eth_call`, or a path registration. That is the deliberate answer
/// to "where does a path come from": from the case, as data.
///
/// [`Self::new`] validates the document before anything is encoded, so a
/// malformed case is refused rather than encoded against. The view the encode
/// seam projects carries `path_id = 0`, so a case cannot assert a path id and
/// must not try.
#[derive(Clone, Debug)]
pub struct ValidationCase {
    /// Unique within a case list: the report key and the message prefix.
    pub name: String,
    /// The full `HopInfo` shape per family, in path order.
    pub hops: Vec<HopInfo>,
    /// The amounts the encoder is handed.
    pub amounts: CaseAmounts,
    /// What the adapter must produce.
    pub expect: Expected,
}

impl ValidationCase {
    /// Turn a case document into a checked case.
    ///
    /// # Errors
    ///
    /// [`DeclarationError::CaseArity`] when an amount list does not have exactly
    /// one entry per hop, [`DeclarationError::CaseAmount`] when an amount is
    /// outside the seam's `int128` bound, [`DeclarationError::CaseDocument`] when
    /// the expected selector is not a function signature.
    pub fn new(
        name: impl Into<String>,
        hops: Vec<HopInfo>,
        amounts: CaseAmounts,
        expect: Expected,
    ) -> Result<Self, DeclarationError> {
        let name = name.into();

        if amounts.hop_outputs.len() != hops.len() {
            return Err(DeclarationError::CaseArity {
                case: name,
                detail: format!(
                    "`hop_outputs` has {} entries for {} hops",
                    amounts.hop_outputs.len(),
                    hops.len()
                ),
            });
        }
        if amounts.consumed_inputs.len() != hops.len() {
            return Err(DeclarationError::CaseArity {
                case: name,
                detail: format!(
                    "`consumed_inputs` has {} entries for {} hops",
                    amounts.consumed_inputs.len(),
                    hops.len()
                ),
            });
        }

        check_amount(&name, "optimal_input", amounts.optimal_input)?;
        for (index, amount) in amounts.hop_outputs.iter().enumerate() {
            check_amount(&name, &format!("hop_outputs[{index}]"), *amount)?;
        }
        for (index, amount) in amounts.consumed_inputs.iter().enumerate() {
            check_amount(&name, &format!("consumed_inputs[{index}]"), *amount)?;
        }

        let expect = match expect {
            Expected::Decline => Expected::Decline,
            Expected::Transaction(expected) => {
                let function = Function::parse(&expected.selector).map_err(|error| {
                    DeclarationError::CaseDocument {
                        case: name.clone(),
                        detail: format!(
                            "`expect.selector` `{}` is not a function signature: {error}",
                            expected.selector
                        ),
                    }
                })?;
                Expected::Transaction(ExpectedTransaction {
                    selector: function.signature(),
                    ..expected
                })
            }
        };

        Ok(Self {
            name,
            hops,
            amounts,
            expect,
        })
    }
}

fn check_amount(case: &str, field: &str, amount: u128) -> Result<(), DeclarationError> {
    if amount > i128::MAX as u128 {
        return Err(DeclarationError::CaseAmount {
            case: case.to_string(),
            field: field.to_string(),
        });
    }
    Ok(())
}

/// Resolve a case document's hop `family` tag (`"v2"` / `"v3"` / `"v4"`).
///
/// The tag is the only part of a hop a document cannot type directly; the
/// per-family fields then fill the [`HopInfo`] variant the tag names.
///
/// # Errors
///
/// [`DeclarationError::CaseDocument`] when the tag names no family.
pub fn case_hop_family(case: &str, tag: &str) -> Result<HopFamily, DeclarationError> {
    match tag {
        "v2" => Ok(HopFamily::V2),
        "v3" => Ok(HopFamily::V3),
        "v4" => Ok(HopFamily::V4),
        other => Err(DeclarationError::CaseDocument {
            case: case.to_string(),
            detail: format!("`hops[].family` `{other}` names no pool family"),
        }),
    }
}

/// One case's outcome: its name plus `Ok(())` or the failure that named it.
#[derive(Debug, PartialEq, Eq)]
pub struct CaseReport {
    /// The case's name — the report key.
    pub name: String,
    /// The case's outcome.
    pub result: Result<(), CaseFailure>,
}

impl CaseReport {
    /// Whether the case's expectation was met.
    #[must_use]
    pub const fn passed(&self) -> bool {
        self.result.is_ok()
    }
}

/// Why a case failed — the adapter's refusal, or a declared-evidence mismatch.
///
/// Both are data, so a caller can report every failing case in one run.
#[derive(Debug, PartialEq, Eq)]
pub enum CaseFailure {
    /// The adapter declined (or failed) when the case expected a transaction, or
    /// produced one when it expected a decline.
    Refused(ComposeError),
    /// The produced transaction disagrees with the case's declared evidence, or
    /// with the interface itself.
    Mismatch(ContractViolation),
}

/// Run `cases` against one adapter, one report per case (ADR-100 D7).
///
/// Each case calls [`ExecutionAdapter::encode`] — the entry point production
/// uses (`compose_transaction`) — with the case's own [`PathInfo`] and
/// [`ComposerInputs`]. Case comparison reuses tier 1's checks plus
/// [`ContractViolation::CaseExpectation`], so a case can never pass while the
/// same transaction would fail the interface check.
///
/// The run is explicit, synchronous, and offline: no provider, no engine, no
/// registry, and no adapter callback beyond the encodes the caller asked for.
#[must_use]
pub fn run_cases<A>(
    adapter: &A,
    interface: &ContractInterface,
    cases: &[ValidationCase],
) -> Vec<CaseReport>
where
    A: ExecutionAdapter + ?Sized,
{
    cases
        .iter()
        .map(|case| CaseReport {
            name: case.name.clone(),
            result: run_case(adapter, interface, case),
        })
        .collect()
}

fn run_case<A>(
    adapter: &A,
    interface: &ContractInterface,
    case: &ValidationCase,
) -> Result<(), CaseFailure>
where
    A: ExecutionAdapter + ?Sized,
{
    let path = PathInfo::new(case.hops.clone());
    let inputs = ComposerInputs {
        optimal_input: case.amounts.optimal_input,
        hop_outputs: &case.amounts.hop_outputs,
        consumed_inputs: &case.amounts.consumed_inputs,
        opts: ComposeOptions,
    };

    match (adapter.encode(&path, &inputs), &case.expect) {
        // The declared behavior: a capability decline where the case expects one.
        (Err(ComposeError::Unsupported(_)), Expected::Decline) => Ok(()),
        // Any other refusal is the adapter's own failure, whatever the case
        // expected — a broken encoder is reported, never excused.
        (Err(error), _) => Err(CaseFailure::Refused(error)),
        (Ok(_), Expected::Decline) => Err(CaseFailure::Refused(ComposeError::encode(format!(
            "case `{}` expected a decline; the adapter produced a transaction",
            case.name
        )))),
        (Ok(transaction), Expected::Transaction(expected)) => {
            check_expectation(interface, &case.name, expected, &transaction)
                .map_err(CaseFailure::Mismatch)
        }
    }
}

/// Compare one produced transaction with a case's declared evidence, on top of
/// the interface's own checks.
fn check_expectation(
    interface: &ContractInterface,
    case: &str,
    expected: &ExpectedTransaction,
    transaction: &TransactionRequest,
) -> Result<(), ContractViolation> {
    interface.validate(transaction)?;
    let (declared, values) = interface.resolve_call(&transaction.data)?;

    if declared.signature != expected.selector {
        return Err(ContractViolation::CaseExpectation {
            case: case.to_string(),
            field: "selector".to_string(),
            expected: expected.selector.clone(),
            actual: declared.signature.clone(),
        });
    }
    if let Some(target) = expected.target {
        if transaction.to != target {
            return Err(ContractViolation::CaseExpectation {
                case: case.to_string(),
                field: "target".to_string(),
                expected: target.to_string(),
                actual: transaction.to.to_string(),
            });
        }
    }
    if let Some(value) = expected.value {
        if transaction.value != value {
            return Err(ContractViolation::CaseExpectation {
                case: case.to_string(),
                field: "value".to_string(),
                expected: value.to_string(),
                actual: transaction.value.to_string(),
            });
        }
    }

    match &expected.call {
        None => Ok(()),
        Some(ExpectedCall::Calldata(golden)) => {
            if transaction.data == *golden {
                return Ok(());
            }
            Err(ContractViolation::CaseExpectation {
                case: case.to_string(),
                field: "calldata".to_string(),
                expected: format!("0x{}", hex::encode(golden)),
                actual: format!("0x{}", hex::encode(&transaction.data)),
            })
        }
        Some(ExpectedCall::Arguments(wanted)) => {
            if wanted.len() != values.len() {
                return Err(ContractViolation::CaseExpectation {
                    case: case.to_string(),
                    field: "arguments".to_string(),
                    expected: format!("{} values", wanted.len()),
                    actual: format!("{} values", values.len()),
                });
            }
            for (index, (wanted, actual)) in wanted.iter().zip(&values).enumerate() {
                if wanted != actual {
                    return Err(ContractViolation::CaseExpectation {
                        case: case.to_string(),
                        field: format!("arguments[{index}]"),
                        expected: format!("{wanted:?}"),
                        actual: format!("{actual:?}"),
                    });
                }
            }
            Ok(())
        }
    }
}
