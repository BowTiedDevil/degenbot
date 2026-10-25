//! `tracing_subscriber::fmt::MakeWriter` bridge (feature `tracing`).
//!
//! The writer opens `stdout.log` in append mode per event. Open failure falls
//! back to stderr so a full disk or a removed directory never silences the
//! process; the `tee` variant additionally mirrors every write to stderr for
//! interactive runs.

use std::io::{self, Write};
use std::path::PathBuf;

use tracing_subscriber::fmt::MakeWriter;

/// A `MakeWriter` appending to one run-artifact file (optionally teeing to
/// stderr).
#[derive(Debug, Clone)]
pub struct LogWriter {
    path: PathBuf,
    mirror_stderr: bool,
}

impl LogWriter {
    /// File-only writer.
    pub(crate) fn file(path: PathBuf) -> Self {
        Self {
            path,
            mirror_stderr: false,
        }
    }

    /// File + stderr writer.
    pub(crate) fn tee(path: PathBuf) -> Self {
        Self {
            path,
            mirror_stderr: true,
        }
    }
}

impl<'a> MakeWriter<'a> for LogWriter {
    type Writer = LogSink;

    fn make_writer(&'a self) -> Self::Writer {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok();
        // A failed open degrades to stderr rather than dropping the record.
        let mirror_stderr = self.mirror_stderr || file.is_none();
        LogSink {
            file,
            mirror_stderr,
        }
    }
}

/// The per-event sink returned by [`LogWriter`].
pub struct LogSink {
    file: Option<std::fs::File>,
    mirror_stderr: bool,
}

impl Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(file) = &mut self.file {
            file.write_all(buf)?;
        }
        if self.mirror_stderr {
            io::stderr().write_all(buf)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = &mut self.file {
            file.flush()?;
        }
        if self.mirror_stderr {
            io::stderr().flush()?;
        }
        Ok(())
    }
}
