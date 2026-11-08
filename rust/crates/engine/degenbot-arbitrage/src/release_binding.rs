//! Validating built-in target binding (ADR-098 D5).
//!
//! A `cmd_executor` deployment is an immutable contract permanently bound to
//! the implementation it was compiled from. This module is what turns the
//! deployment's own declaration — the four metadata getters — into that
//! binding, and what refuses the deployment when the declaration does not name
//! a release this build dispatches.
//!
//! # The binding path
//!
//! 1. **Read** the four getters ([`read_metadata`]). A node failure is an error,
//!    never an absent getter ([`degenbot_executor::discovery`]).
//! 2. **Classify** the answers
//!    ([`degenbot_executor::release::classify_deployment`]): a full declaration
//!    names itself, a getterless deployment is named only by the operator's
//!    [`LegacyReleaseMapping`], and partial answers are an error.
//! 3. **Resolve** the declared tag in the bounded
//!    [`ReleaseRegistry`]: an unregistered tag is a refusal, never "the newest
//!    release this build has".
//! 4. **Validate** the declaration against that release's manifest
//!    ([`degenbot_executor::release::ReleaseManifest::check_metadata_within`]),
//!    bounded by the majors the registry actually dispatches. A deployment that
//!    disagrees with its own manifest — a different major, encoding version, or
//!    capability set — is refused here, before anything is encoded.
//!
//! Nothing defaults. There is no path from "no answer", "an unknown tag", or "a
//! disagreeing answer" to a working encoder.
//!
//! # What the binding carries
//!
//! [`BoundCmdExecutor`] holds the deployment's identities
//! ([`CmdExecutorBinding`]), the resolved [`Release`], and the source that named
//! the release ([`ReleaseSource`]). Its adapter is the release's own complete
//! encoding, and its deployments inject the release's own runtime artifact — the
//! committed file the manifest names, or the artifact the release definition
//! ships in memory for an undeployed build. An explicit runtime that is not the
//! release's artifact is refused ([`BoundRuntimeError::Mismatch`]).
//!
//! [`CmdExecutorBinding`]: crate::cmd_executor::CmdExecutorBinding

use std::path::PathBuf;
use std::sync::Arc;

use alloy::primitives::{Address, Bytes};

use degenbot_core::errors::ProviderError;
use degenbot_execution::DeploymentId;
use degenbot_executor::discovery::{classify_getter, GetterAnswers, GetterCall, METADATA_GETTERS};
use degenbot_executor::release::{
    classify_deployment, DeploymentRelease, ExecutorMetadata, LegacyReleaseMapping,
    MetadataDecodeError, ReleaseId, ReleaseMismatch,
};
use degenbot_executor::release_registry::{Release, ReleaseRegistry};
use degenbot_rpc::provider::AlloyProvider;

use crate::cmd_executor::{CmdExecutorBinding, CmdExecutorSimAdapter};
use crate::deployment::SimulationDeployment;
use crate::executor_runtime::{load_executor_runtime, repository_root, ExecutorRuntimeError};

/// How a bound deployment's release became known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseSource {
    /// The deployment declared its own release identity through the four
    /// metadata getters, and the declaration matched the release manifest.
    Declared(ExecutorMetadata),
    /// The deployment answered nothing and the operator's
    /// [`LegacyReleaseMapping`] named this release.
    Legacy {
        /// The operator-mapped release tag.
        release: ReleaseId,
    },
}

/// Why a deployment could not be bound to a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingError {
    /// A metadata getter could not be read. A transport or node failure proves
    /// nothing about the deployment, so it is never classified as an absent
    /// getter.
    Node {
        /// The getter whose read failed.
        getter: &'static str,
        /// The transport's own account of the failure.
        detail: String,
    },
    /// The deployment answered no getter and no legacy mapping names it. Its
    /// release is unknown — deliberately not "the latest".
    Unknown {
        /// The deployment address.
        address: Address,
    },
    /// The named release is not in this build's bounded registry.
    UnknownRelease {
        /// The declared (or operator-mapped) release tag.
        tag: String,
        /// The tags this build dispatches.
        registered: Vec<String>,
    },
    /// The metadata answered, but does not describe the release it names.
    Mismatch {
        /// The deployment address.
        address: Address,
        /// The release tag it named.
        release: String,
        /// The first disagreement.
        mismatch: ReleaseMismatch,
    },
    /// The answers are not a usable identity declaration (partial, short,
    /// malformed, or self-contradictory).
    Decode(MetadataDecodeError),
}

impl std::fmt::Display for BindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Node { getter, detail } => write!(
                f,
                "{getter}() could not be read ({detail}): a node failure is not an \
                 absent getter"
            ),
            Self::Unknown { address } => write!(
                f,
                "deployment {address} answered no metadata getter and no legacy release \
                 mapping names it: an unknown release is never read as the latest one"
            ),
            Self::UnknownRelease { tag, registered } => write!(
                f,
                "deployment names release {tag:?}, which this build does not dispatch \
                 (registered: {registered:?}): an unknown release is a refusal, never the \
                 newest encoder"
            ),
            Self::Mismatch {
                address,
                release,
                mismatch,
            } => write!(
                f,
                "deployment {address} does not match release {release}: {mismatch}"
            ),
            Self::Decode(error) => write!(
                f,
                "deployment metadata is not a usable identity declaration: {error}"
            ),
        }
    }
}

impl std::error::Error for BindingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Decode(error) => Some(error),
            Self::Mismatch { mismatch, .. } => Some(mismatch),
            Self::Node { .. } | Self::Unknown { .. } | Self::UnknownRelease { .. } => None,
        }
    }
}

impl From<MetadataDecodeError> for BindingError {
    fn from(error: MetadataDecodeError) -> Self {
        Self::Decode(error)
    }
}

/// Why a bound deployment's runtime artifact was refused.
#[derive(Debug)]
pub enum BoundRuntimeError {
    /// The release's artifact could not be located, read, or decoded.
    Artifact(ExecutorRuntimeError),
    /// The caller supplied runtime bytes that are not the release's artifact:
    /// injecting another release's code under this deployment would simulate an
    /// implementation the deployment does not run.
    Mismatch {
        /// The release whose artifact was expected.
        release: String,
        /// The repository-relative artifact the release names.
        artifact: String,
        /// The byte length the caller supplied.
        provided_len: usize,
        /// The byte length the release's artifact has.
        expected_len: usize,
    },
}

impl std::fmt::Display for BoundRuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Artifact(error) => write!(f, "{error}"),
            Self::Mismatch {
                release,
                artifact,
                provided_len,
                expected_len,
            } => write!(
                f,
                "the supplied runtime is {provided_len} bytes, but release {release} ships \
                 {artifact} ({expected_len} bytes): an injected build must be the release's \
                 own artifact"
            ),
        }
    }
}

impl std::error::Error for BoundRuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Artifact(error) => Some(error),
            Self::Mismatch { .. } => None,
        }
    }
}

impl From<ExecutorRuntimeError> for BoundRuntimeError {
    fn from(error: ExecutorRuntimeError) -> Self {
        Self::Artifact(error)
    }
}

/// Read a deployment's four metadata getters through `provider`.
///
/// Each getter is one `eth_call`. The classification of a failure is
/// deliberate, because it decides whether a deployment enters the legacy path:
///
/// - a successful call with data is an answer;
/// - a successful call with NO data (`0x`) is an absent getter — the address
///   holds no code, or its fallback returns nothing;
/// - an EVM revert (`ProviderError::ExecutionReverted`) is an absent getter —
///   an unknown selector on a contract without a fallback reverts;
/// - anything else is [`BindingError::Node`]: a timeout, a dropped connection,
///   a rate limit, or a node that does not serve `eth_call` proves nothing
///   about the deployment, and treating it as absence would let a broken
///   endpoint push a declaring deployment into the legacy path.
///
/// # Errors
///
/// [`BindingError::Node`] when any getter's read failed.
pub async fn read_metadata(
    provider: &AlloyProvider,
    executor: Address,
) -> Result<GetterAnswers, BindingError> {
    let mut answers = GetterAnswers::all_absent();
    for getter in &METADATA_GETTERS {
        let result = provider
            .eth_call(&executor, Bytes::from(getter.selector.to_vec()), None)
            .await;

        // The classification decision, made once per getter: a revert or an
        // empty answer is absence, and every other failure is the node's.
        let failure = result.as_ref().err().map(ToString::to_string);
        let call = match &result {
            Ok(data) => GetterCall::Returned(data.as_ref()),
            Err(ProviderError::ExecutionReverted { .. }) => GetterCall::Reverted,
            Err(_) => GetterCall::Failed {
                detail: failure.as_deref().unwrap_or_default(),
            },
        };
        match classify_getter(getter.name, call) {
            Ok(outcome) => answers.record(getter, outcome),
            Err(degenbot_executor::discovery::DiscoveryError::Node { getter, detail }) => {
                return Err(BindingError::Node { getter, detail });
            }
        }
    }
    Ok(answers)
}

/// A `cmd_executor` deployment whose release was resolved and validated.
#[derive(Debug)]
pub struct BoundCmdExecutor {
    binding: CmdExecutorBinding,
    release: Arc<Release>,
    source: ReleaseSource,
    artifact_root: Option<PathBuf>,
}

impl BoundCmdExecutor {
    /// Bind a deployment from its already-read metadata answers.
    ///
    /// This is the synchronous half of [`read_metadata`] + [`Self::discover`],
    /// so an offline consumer (a fixture, a replay, a recorded capture) can bind
    /// without a provider.
    ///
    /// # Errors
    ///
    /// [`BindingError::Unknown`] for a getterless deployment nobody named;
    /// [`BindingError::Decode`] for partial or malformed answers;
    /// [`BindingError::UnknownRelease`] when the named tag is not registered;
    /// [`BindingError::Mismatch`] when the declaration disagrees with the
    /// release manifest.
    pub fn bind(
        binding: CmdExecutorBinding,
        answers: &GetterAnswers,
        legacy: Option<&LegacyReleaseMapping>,
        registry: &ReleaseRegistry,
    ) -> Result<Self, BindingError> {
        let address = binding.executor();

        // What the deployment (or the operator's mapping) claims. A partial or
        // malformed declaration errors here; a getterless deployment nobody
        // named is `Unknown`, and that is a refusal.
        let (source, tag) = match classify_deployment(address, answers.observations(), legacy)? {
            DeploymentRelease::Declared(metadata) => {
                let tag = metadata.release.ascii_tag().to_owned();
                (ReleaseSource::Declared(metadata), tag)
            }
            DeploymentRelease::Legacy { release } => (
                ReleaseSource::Legacy { release },
                release.ascii_tag().to_owned(),
            ),
            DeploymentRelease::Unknown => return Err(BindingError::Unknown { address }),
        };

        // The tag names a release only when this build dispatches it: an
        // unregistered tag is a refusal, never the newest encoder.
        let release = registry
            .get(&tag)
            .ok_or_else(|| BindingError::UnknownRelease {
                tag: tag.clone(),
                registered: registry.tags().map(str::to_owned).collect(),
            })?;

        // A declaring deployment must agree with its own manifest, bounded by
        // the majors this registry dispatches. An operator-mapped deployment
        // declared nothing to check: the mapping IS its evidence.
        if let ReleaseSource::Declared(metadata) = &source {
            release
                .manifest()
                .check_metadata_within(metadata, &registry.majors())
                .map_err(|mismatch| BindingError::Mismatch {
                    address,
                    release: tag,
                    mismatch,
                })?;
        }

        Ok(Self {
            binding,
            release,
            source,
            artifact_root: None,
        })
    }

    /// Discover, validate, and bind a deployment over RPC.
    ///
    /// # Errors
    ///
    /// [`BindingError::Node`] when a getter could not be read, and every
    /// [`Self::bind`] refusal.
    pub async fn discover(
        binding: CmdExecutorBinding,
        provider: &AlloyProvider,
        legacy: Option<&LegacyReleaseMapping>,
        registry: &ReleaseRegistry,
    ) -> Result<Self, BindingError> {
        let answers = read_metadata(provider, binding.executor()).await?;
        Self::bind(binding, &answers, legacy, registry)
    }

    /// Set the directory the release manifest's repository-relative artifact
    /// paths resolve against (a checkout root by default).
    #[must_use]
    pub fn with_artifact_root(mut self, root: Option<&std::path::Path>) -> Self {
        self.artifact_root = root.map(std::path::Path::to_path_buf);
        self
    }

    /// The deployment's identities and encode policy.
    #[must_use]
    pub const fn binding(&self) -> &CmdExecutorBinding {
        &self.binding
    }

    /// The validated release this deployment runs.
    #[must_use]
    pub fn release(&self) -> &Release {
        &self.release
    }

    /// How the release became known.
    #[must_use]
    pub const fn source(&self) -> &ReleaseSource {
        &self.source
    }

    /// The deployment's adapter: the release's complete encoding, planning
    /// against the release's declared capabilities.
    #[must_use]
    pub fn adapter(&self) -> CmdExecutorSimAdapter {
        CmdExecutorSimAdapter::new(
            &self.binding,
            self.release.encoding(),
            Some(Arc::clone(&self.release)),
        )
    }

    /// The deployment of the ACTUAL deployed contract: production sims the code
    /// the chain holds at the executor address.
    #[must_use]
    pub fn deployment(&self, deployment_id: DeploymentId) -> SimulationDeployment {
        self.binding
            .deployment_with_adapter(deployment_id, None, None, self.adapter())
    }

    /// The release's own runtime artifact bytes.
    ///
    /// A release definition that ships its artifact in memory (an undeployed
    /// build, an injected test release) answers with those bytes. Otherwise the
    /// release's committed DEPLOYMENT artifact is read through the one
    /// executor-runtime precedence chain rooted at [`Self::with_artifact_root`]:
    /// the deployed runtime the repository ships, with its immutables baked in.
    /// (The manifest separately names the compiler artifact — `artifact.runtime_code`
    /// — which the tier3 verification gate compiles and byte-compares; it is
    /// referenced there, never injected here.)
    ///
    /// # Errors
    ///
    /// [`BoundRuntimeError::Artifact`] when the artifact is missing,
    /// unreadable, or not `0x`-hex bytecode.
    pub fn release_runtime(&self) -> Result<Bytes, BoundRuntimeError> {
        if let Some(runtime) = self.release.development_runtime() {
            return Ok(runtime.clone());
        }
        let root = self
            .artifact_root
            .clone()
            .unwrap_or_else(default_artifact_root);
        let contracts = root.join("contracts");
        let runtime = load_executor_runtime(None, Some(contracts.as_path()))?;
        Ok(runtime.bytecode)
    }

    /// The deployment of an UNDEPLOYED build of this release: the release's own
    /// artifact installed at the executor address.
    ///
    /// The address is the deployment's executor by construction — the adapter
    /// encodes its transaction to it, so injecting anywhere else would simulate
    /// code the transaction never runs.
    ///
    /// # Errors
    ///
    /// [`BoundRuntimeError::Artifact`] when the release's artifact cannot be
    /// loaded.
    pub fn injected(
        &self,
        deployment_id: DeploymentId,
    ) -> Result<SimulationDeployment, BoundRuntimeError> {
        let runtime = self.release_runtime()?;
        Ok(self.install(deployment_id, runtime))
    }

    /// The deployment of an UNDEPLOYED build whose runtime bytes must BE the
    /// release's artifact.
    ///
    /// A caller that supplies its own bytes (a test stub, a hand-built build) is
    /// refused unless they equal the release's artifact: a declared-code
    /// override that belongs to another release would simulate an implementation
    /// this deployment does not run.
    ///
    /// # Errors
    ///
    /// [`BoundRuntimeError::Mismatch`] when `runtime` is not the release's
    /// artifact; [`BoundRuntimeError::Artifact`] when the release's artifact
    /// cannot be loaded to compare against.
    pub fn injected_with_runtime(
        &self,
        deployment_id: DeploymentId,
        runtime: Bytes,
    ) -> Result<SimulationDeployment, BoundRuntimeError> {
        let expected = self.release_runtime()?;
        if runtime != expected {
            return Err(BoundRuntimeError::Mismatch {
                release: self.release.release_id().to_owned(),
                artifact: self.release.runtime_artifact_path().to_owned(),
                provided_len: runtime.len(),
                expected_len: expected.len(),
            });
        }
        Ok(self.install(deployment_id, runtime))
    }

    fn install(&self, deployment_id: DeploymentId, runtime: Bytes) -> SimulationDeployment {
        self.binding.deployment_with_adapter(
            deployment_id,
            Some(self.binding.executor()),
            Some(runtime),
            self.adapter(),
        )
    }
}

/// The checkout root a committed artifact path resolves against.
fn default_artifact_root() -> PathBuf {
    repository_root().unwrap_or_else(|| PathBuf::from("."))
}
