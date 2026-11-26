//! The committed replay fixtures, as typed readers.
//!
//! A committed capture document is evidence: its bytes are the authority, and a
//! test reads them through ONE reader so no consumer grows a second
//! interpretation of the same file. Each document states its schema version, and
//! a version this crate does not implement is refused rather than read under
//! today's rules.
//!
//! Test-only: production paths never read a fixture.

#[cfg(test)]
pub(crate) mod captured_reports;
