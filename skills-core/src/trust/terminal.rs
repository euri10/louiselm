//! Operator-only local recovery boundary; never an Agent/robot secret channel.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    time::Instant,
};

use rustix::termios::{self, OptionalActions, Termios};
use zeroize::Zeroizing;

use super::recovery::RecoveryError;
use crate::store::Store;

pub(crate) struct Terminal {
    file: File,
    original: Termios,
    restored: bool,
}

impl Terminal {
    pub(crate) fn open() -> Result<Self, RecoveryError> {
        let file = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
        if termios::tcgetpgrp(&file).map_err(std::io::Error::from)? != rustix::process::getpgrp() {
            return Err(RecoveryError::UntrustedAuthority);
        }
        Self::from_file(file)
    }

    fn from_file(file: File) -> Result<Self, RecoveryError> {
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

    pub(crate) fn write(&mut self, text: &str) -> Result<(), RecoveryError> {
        self.file.write_all(text.as_bytes())?;
        self.file.flush()?;
        Ok(())
    }

    pub(crate) fn clear(&mut self) -> Result<(), RecoveryError> {
        self.write("\x1b[2J\x1b[H")
    }

    pub(crate) fn read_hidden_until(
        &mut self,
        deadline: Instant,
    ) -> Result<Zeroizing<String>, RecoveryError> {
        let mut input = Zeroizing::new(String::with_capacity(512));
        let mut byte = Zeroizing::new([0_u8; 1]);
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(RecoveryError::Passkey)?;
            let timeout =
                rustix::event::Timespec::try_from(remaining).map_err(|_| RecoveryError::Passkey)?;
            let mut descriptors = [rustix::event::PollFd::new(
                &self.file,
                rustix::event::PollFlags::IN,
            )];
            match rustix::event::poll(&mut descriptors, Some(&timeout)) {
                Ok(0) => return Err(RecoveryError::Passkey),
                Ok(_) => (),
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => return Err(RecoveryError::Io(error.into())),
            }
            match self.file.read_exact(&mut *byte) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
            match byte[0] {
                b'\r' | b'\n' => return Ok(input),
                3 | 4 | 27 => return Err(RecoveryError::Cancelled),
                8 | 127 => {
                    input.pop();
                }
                b' '..=b'~' if input.len() < 512 => input.push(char::from(byte[0])),
                _ => return Err(RecoveryError::InvalidPhrase),
            }
        }
    }

    pub(crate) fn restore(&mut self) -> Result<(), RecoveryError> {
        if self.restored {
            return Ok(());
        }
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
            let answer =
                terminal.read_hidden_until(Instant::now() + std::time::Duration::from_secs(5));
            if input.ends_with(b"\r") {
                assert_eq!(answer.unwrap().as_str(), "abc");
                terminal.restore().unwrap();
            } else {
                assert!(matches!(answer, Err(RecoveryError::Cancelled)));
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
            // Closing the final slave makes EIO the Linux PTY's end-of-stream.
            // Force small reads: screen writes need not arrive as one packet.
            drop(observer);
            rustix::io::ioctl_fionbio(&master, true).unwrap();
            let deadline = Instant::now() + std::time::Duration::from_secs(1);
            let mut output = Vec::new();
            loop {
                let remaining = deadline.checked_duration_since(Instant::now()).unwrap();
                let timeout = rustix::event::Timespec::try_from(remaining).unwrap();
                let mut descriptors = [rustix::event::PollFd::new(
                    &master,
                    rustix::event::PollFlags::IN,
                )];
                assert_ne!(
                    rustix::event::poll(&mut descriptors, Some(&timeout)).unwrap(),
                    0
                );
                let mut chunk = [0_u8; 7];
                match master.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => output.extend_from_slice(&chunk[..count]),
                    Err(error)
                        if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) =>
                    {
                        break;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue;
                    }
                    Err(error) => panic!("PTY output failed: {error}"),
                }
                assert!(output.len() <= 256, "unexpected terminal output");
            }
            assert_eq!(output, b"\x1b[?1049h\x1b[2J\x1b[H\x1b[2J\x1b[H\x1b[?1049l");
        }
    }

    #[test]
    fn pending_registration_terminal_input_expires_and_restores_echo() {
        let master = pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        pty::grantpt(&master).unwrap();
        pty::unlockpt(&master).unwrap();
        let slave = File::from(
            pty::ioctl_tiocgptpeer(&master, OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap(),
        );
        let original = termios::tcgetattr(&slave).unwrap();
        let observer = slave.try_clone().unwrap();
        let mut terminal = Terminal::from_file(slave).unwrap();
        assert!(matches!(
            terminal.read_hidden_until(Instant::now() + std::time::Duration::from_millis(5)),
            Err(RecoveryError::Passkey)
        ));
        drop(terminal);
        assert_eq!(
            termios::tcgetattr(&observer).unwrap().local_modes,
            original.local_modes
        );
    }
}

pub(crate) fn require_production(store: &Store) -> Result<(), RecoveryError> {
    require_production_root(store)?;
    for path in [
        store.root().join("trust"),
        store.root().join("trust/roles.json"),
    ] {
        require_protected(&path)?;
    }
    Ok(())
}

pub(crate) fn require_production_root(store: &Store) -> Result<(), RecoveryError> {
    if !crate::release::running_identity().verified
        || !store.provenance()?.trusted
        || rustix::process::geteuid().as_raw() != 0
    {
        return Err(RecoveryError::UntrustedAuthority);
    }
    require_store_path(store.root())?;
    require_protected(&store.root().join("provenance.json"))?;
    // Initial enrollment may not exist yet; existing paths must still be guarded.
    for name in ["trust", "trust/roles.json", "trust/roles.lock"] {
        match require_protected(&store.root().join(name)) {
            Err(RecoveryError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => (),
            result => result?,
        }
    }
    Ok(())
}

/// Check before `Store::open` creates anything, including its provenance record.
pub(crate) fn require_store_path(path: &std::path::Path) -> Result<(), RecoveryError> {
    if !crate::release::running_identity().verified || rustix::process::geteuid().as_raw() != 0 {
        return Err(RecoveryError::UntrustedAuthority);
    }
    let absolute = std::path::absolute(path)?;
    if absolute
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(RecoveryError::UntrustedAuthority);
    }
    for ancestor in absolute.ancestors() {
        match require_protected(ancestor) {
            Err(RecoveryError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => (),
            result => result?,
        }
    }
    for name in [
        "provenance.json",
        "trust",
        "trust/roles.json",
        "trust/roles.lock",
    ] {
        match require_protected(&absolute.join(name)) {
            Err(RecoveryError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => (),
            result => result?,
        }
    }
    Ok(())
}

fn require_protected(path: &std::path::Path) -> Result<(), RecoveryError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || (!metadata.is_dir() && (!metadata.is_file() || metadata.nlink() != 1))
    {
        return Err(RecoveryError::UntrustedAuthority);
    }
    Ok(())
}
