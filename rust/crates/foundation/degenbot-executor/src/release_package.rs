//! Typed production release packages.
//!
//! Each package supplies one complete encoding/receipt pair. The production
//! registry enumerates [`ProductionRelease`] exhaustively, so adding a release
//! without its package implementation is a compile error rather than a runtime
//! discovery.

use std::sync::Arc;

use crate::release::{
    ReleaseManifest, RELEASE_ID_TAG, RELEASE_MANIFEST_V2, SUPPORTED_PROTOCOL_MAJORS,
};
use crate::release_registry::{Release, ReleaseEncoding, ReleaseError};

#[path = "../releases/v2/encoding.rs"]
pub mod v2_encoding;

mod sealed {
    pub trait Sealed {}
}

/// One sealed, complete release package.
trait ReleaseDefinition: sealed::Sealed + Send + Sync + 'static {
    /// The release tag the encoding and manifest both declare.
    const RELEASE_ID: &'static str;
    /// The embedded manifest for this package.
    const MANIFEST: &'static str;
    /// The complete stream/config/receipt encoding.
    type Encoding: ReleaseEncoding + Default;
}

/// Build one package after checking its two release-tag declarations agree.
fn build<R: ReleaseDefinition>() -> Result<Release, ReleaseError> {
    let manifest = ReleaseManifest::parse(R::MANIFEST)?;
    if R::RELEASE_ID != manifest.release_id {
        return Err(ReleaseError::EncodingReleaseId {
            declared: manifest.release_id.clone(),
            encoding: R::RELEASE_ID.to_owned(),
        });
    }
    Release::new(
        manifest,
        SUPPORTED_PROTOCOL_MAJORS,
        Arc::new(R::Encoding::default()),
        None,
    )
}

/// Every production package compiled into this build, in declaration order.
///
/// Adding a variant without its [`Self::build`] arm is a compile error.
pub(crate) enum ProductionRelease {
    V2,
}

impl ProductionRelease {
    pub(crate) const ALL: &[Self] = &[Self::V2];

    pub(crate) fn build(&self) -> Result<Release, ReleaseError> {
        match self {
            Self::V2 => build::<CmdExecutorV2>(),
        }
    }
}

struct CmdExecutorV2;

impl sealed::Sealed for CmdExecutorV2 {}

impl ReleaseDefinition for CmdExecutorV2 {
    const RELEASE_ID: &'static str = RELEASE_ID_TAG;
    const MANIFEST: &'static str = RELEASE_MANIFEST_V2;
    type Encoding = v2_encoding::CmdStreamV1Encoding;
}
