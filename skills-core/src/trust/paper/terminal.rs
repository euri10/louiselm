//! Operator-only local recovery boundary; never an Agent/robot secret channel.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::MetadataExt,
};

use rustix::termios::{self, OptionalActions, Termios};
use zeroize::Zeroizing;

use super::PaperError;
use crate::store::Store;

pub(crate) struct Terminal {
    file: File,
    original: Termios,
    restored: bool,
}

impl Terminal {
    pub(crate) fn open() -> Result<Self, PaperError> {
        let file = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
        if termios::tcgetpgrp(&file).map_err(std::io::Error::from)? != rustix::process::getpgrp() {
            return Err(PaperError::UntrustedAuthority);
        }
        Self::from_file(file)
    }

    fn from_file(file: File) -> Result<Self, PaperError> {
        let original = termios::tcgetattr(&file).map_err(std::io::Error::from)?;
        let mut terminal = Self {
            file,
            original,
            restored: false,
        };
        let mut hidden = terminal.original.clone();
        // Handle keyboard cancellation ourselves so Ctrl-C cannot bypass Drop.
        // A killed process cannot publish before the separate confirmed apply.
        hidden.make_raw();
        termios::tcsetattr(&terminal.file, OptionalActions::Flush, &hidden)
            .map_err(std::io::Error::from)?;
        terminal.write("\x1b[?1049h\x1b[2J\x1b[H")?;
        Ok(terminal)
    }

    pub(crate) fn write(&mut self, text: &str) -> Result<(), PaperError> {
        self.file.write_all(text.as_bytes())?;
        self.file.flush()?;
        Ok(())
    }

    pub(crate) fn clear(&mut self) -> Result<(), PaperError> {
        self.write("\x1b[2J\x1b[H")
    }

    pub(crate) fn read_hidden(&mut self) -> Result<Zeroizing<String>, PaperError> {
        let mut input = Zeroizing::new(String::with_capacity(512));
        let mut byte = Zeroizing::new([0_u8; 1]);
        loop {
            match self.file.read_exact(&mut *byte) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
            match byte[0] {
                b'\r' | b'\n' => return Ok(input),
                3 | 4 | 27 => return Err(PaperError::Cancelled),
                8 | 127 => {
                    input.pop();
                }
                b' '..=b'~' if input.len() < 512 => input.push(char::from(byte[0])),
                _ => return Err(PaperError::InvalidPhrase),
            }
        }
    }

    pub(crate) fn restore(&mut self) -> Result<(), PaperError> {
        // Attempt both even if clearing fails. Do not mutate authority if either
        // terminal cleanup step cannot be acknowledged.
        let clear = self.write("\x1b[2J\x1b[H\x1b[?1049l");
        let restore = termios::tcsetattr(&self.file, OptionalActions::Flush, &self.original)
            .map_err(std::io::Error::from);
        self.restored = clear.is_ok() && restore.is_ok();
        clear?;
        restore?;
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if !self.restored {
            // Best effort after an earlier refusal; apply has not run. The
            // success path explicitly checks restore before changing authority.
            let _ = self.restore();
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "PTY fixture setup and assertions abort only the test."
    )]
    use super::*;
    use rustix::pty::{self, OpenptFlags};

    #[test]
    fn real_pty_hides_input_and_restores_flags_on_success_and_cancel() {
        for input in [b"abx\x7fc\r".as_slice(), b"private\x03".as_slice()] {
            let master = pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
            pty::grantpt(&master).unwrap();
            pty::unlockpt(&master).unwrap();
            let slave =
                pty::ioctl_tiocgptpeer(&master, OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
            let slave = File::from(slave);
            let original = termios::tcgetattr(&slave).unwrap();
            let observer = slave.try_clone().unwrap();
            let mut terminal = Terminal::from_file(slave).unwrap();
            assert!(
                !termios::tcgetattr(&observer)
                    .unwrap()
                    .local_modes
                    .contains(termios::LocalModes::ECHO)
            );
            let mut master = File::from(master);
            master.write_all(input).unwrap();
            let answer = terminal.read_hidden();
            if input.ends_with(b"\r") {
                assert_eq!(answer.unwrap().as_str(), "abc");
                terminal.restore().unwrap();
            } else {
                assert!(matches!(answer, Err(PaperError::Cancelled)));
            }
            drop(terminal);
            let restored = termios::tcgetattr(&observer).unwrap();
            assert_eq!(restored.local_modes, original.local_modes);
            assert_eq!(restored.input_modes, original.input_modes);
            assert_eq!(restored.output_modes, original.output_modes);
            assert_eq!(restored.control_modes, original.control_modes);
            for index in [
                termios::SpecialCodeIndex::VMIN,
                termios::SpecialCodeIndex::VTIME,
                termios::SpecialCodeIndex::VINTR,
            ] {
                assert_eq!(restored.special_codes[index], original.special_codes[index]);
            }
            rustix::io::ioctl_fionbio(&master, true).unwrap();
            let mut output = [0_u8; 256];
            let count = master.read(&mut output).unwrap();
            assert_eq!(
                &output[..count],
                b"\x1b[?1049h\x1b[2J\x1b[H\x1b[2J\x1b[H\x1b[?1049l"
            );
        }
    }
}

pub(crate) fn require_production(store: &Store) -> Result<(), PaperError> {
    if !crate::release::running_identity().verified
        || !store.provenance()?.trusted
        || rustix::process::geteuid().as_raw() != 0
    {
        return Err(PaperError::UntrustedAuthority);
    }
    let root = std::path::absolute(store.root())?;
    for path in root.ancestors().map(std::path::Path::to_path_buf).chain([
        root.join("trust"),
        root.join("trust/roles.json"),
        root.join("provenance.json"),
    ]) {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0
        {
            return Err(PaperError::UntrustedAuthority);
        }
    }
    Ok(())
}
