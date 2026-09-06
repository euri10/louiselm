//! Owned, nonblocking controller descriptors; no arbitrary blocking Rust I/O.

use std::{
    fs::File,
    io::{self, BufReader, Cursor, Read, Write},
};

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};

/// Controller ACP descriptors, including bytes prefetched with the launch frame.
///
/// The launcher temporarily enables `O_NONBLOCK`. Callers must give it exclusive
/// use of these open file descriptions (including duplicates) until relay cleanup.
/// Original flags are restored before successful cleanup. Use pipes, sockets or
/// local files; this cannot make an uninterruptible kernel/filesystem fault safe.
pub struct RelayStdio {
    input: NonblockingFile,
    buffered: Cursor<Vec<u8>>,
    output: NonblockingFile,
}

impl RelayStdio {
    /// Takes the controller's descriptors without losing buffered ACP bytes.
    ///
    /// # Errors
    /// Returns descriptor flag-query/configuration errors.
    pub fn new(input: BufReader<File>, output: File) -> io::Result<Self> {
        let buffered = Cursor::new(input.buffer().to_vec());
        // stdin/stdout may alias one duplex open file description. Capture both
        // originals before either guard enables the shared nonblocking flag.
        let input_flags = fcntl_getfl(input.get_ref())?;
        let output_flags = fcntl_getfl(&output)?;
        Ok(Self {
            input: NonblockingFile::with_flags(input.into_inner(), input_flags)?,
            buffered,
            output: NonblockingFile::with_flags(output, output_flags)?,
        })
    }

    /// Restores descriptor flags and closes both owned handles.
    ///
    /// # Errors
    /// Returns a flag-restoration failure after attempting both descriptors.
    pub fn close(mut self) -> io::Result<()> {
        let input = self.input.restore();
        let output = self.output.restore();
        input.and(output)
    }
}

impl Read for RelayStdio {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.buffered.read(buffer)?;
        if read > 0 {
            Ok(read)
        } else {
            self.input.read(buffer)
        }
    }
}

impl Write for RelayStdio {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.output.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

pub(super) struct NonblockingFile {
    file: File,
    flags: OFlags,
    restored: bool,
}

impl NonblockingFile {
    pub(super) fn new(file: File) -> io::Result<Self> {
        let flags = fcntl_getfl(&file)?;
        Self::with_flags(file, flags)
    }

    fn with_flags(file: File, flags: OFlags) -> io::Result<Self> {
        fcntl_setfl(&file, flags | OFlags::NONBLOCK)?;
        Ok(Self {
            file,
            flags,
            restored: false,
        })
    }

    pub(super) fn restore(&mut self) -> io::Result<()> {
        if !self.restored {
            fcntl_setfl(&self.file, self.flags)?;
            self.restored = true;
        }
        Ok(())
    }
}

impl Read for NonblockingFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.file.read(buffer)
    }
}

impl Write for NonblockingFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.file.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Drop for NonblockingFile {
    fn drop(&mut self) {
        // Explicit relay cleanup reports restoration failure. This fallback is
        // only for setup failure/unwinding, which cannot report successful cleanup;
        // ownership still closes the descriptor even when restoration fails.
        let _ = self.restore();
    }
}
