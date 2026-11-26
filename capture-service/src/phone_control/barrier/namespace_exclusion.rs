//! Conditional exclusion among cooperating processes in one network namespace.
//!
//! These names do not authenticate the namespace, root, receiver, boot, or
//! participants. They are not a first-start, recovery, or Send permit.

use std::{io, os::unix::net::UnixListener};

const STARTUP: &[u8] = b"louiselm.phone.startup.v1";
const REGISTRAR: &[u8] = b"louiselm.phone.registrar.v1";

/// A short, nonblocking transaction. Never hold it across an async await.
pub(crate) struct Startup {
    _listener: UnixListener,
    registrar: Box<[u8]>,
}

/// Retain until the receiver and every delegated registrar have lost authority.
pub(crate) struct Registrar {
    _listener: UnixListener,
}

impl Startup {
    /// Excludes only cooperating processes in the caller's network namespace.
    pub(crate) fn try_acquire() -> io::Result<Self> {
        Self::acquire(STARTUP, REGISTRAR)
    }

    fn acquire(startup: &[u8], registrar: &[u8]) -> io::Result<Self> {
        Ok(Self {
            _listener: bind(startup)?,
            registrar: registrar.into(),
        })
    }

    /// Must be obtained while startup exclusion is held.
    pub(crate) fn registrar(&self) -> io::Result<Registrar> {
        Ok(Registrar {
            _listener: bind(&self.registrar)?,
        })
    }

    #[cfg(test)]
    pub(super) fn for_fixture(root: &std::path::Path) -> io::Result<Self> {
        use std::os::unix::ffi::OsStrExt;

        use sha2::{Digest, Sha256};

        let digest = crate::hex::lower_hex(&Sha256::digest(root.as_os_str().as_bytes()));
        let startup = format!("louiselm.test.startup.{digest}");
        let registrar = format!("louiselm.test.registrar.{digest}");
        Self::acquire(startup.as_bytes(), registrar.as_bytes())
    }

    #[cfg(test)]
    pub(super) fn wait_for_fixture(root: &std::path::Path) -> io::Result<Self> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match Self::for_fixture(root) {
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                result => return result,
            }
        }
    }

    #[cfg(test)]
    pub(super) fn wait_for_fixture_registrar(&self) -> io::Result<Registrar> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match self.registrar() {
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                result => return result,
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn bind(name: &[u8]) -> io::Result<UnixListener> {
    use std::os::{linux::net::SocketAddrExt, unix::net::SocketAddr};

    let address = SocketAddr::from_abstract_name(name)?;
    UnixListener::bind_addr(&address).map_err(|error| {
        if error.kind() == io::ErrorKind::AddrInUse {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "phone namespace exclusion is held",
            )
        } else {
            error
        }
    })
}

#[cfg(not(target_os = "linux"))]
fn bind(_name: &[u8]) -> io::Result<UnixListener> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "phone namespace exclusion requires Linux",
    ))
}

#[cfg(test)]
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]
mod tests {
    use std::{io, path::Path, process::Command};

    use super::Startup;

    #[test]
    fn concurrent_process() {
        let Ok(expected) = std::env::var("LOUISELM_TEST_NAMESPACE_EXCLUSION") else {
            return;
        };
        let root = std::env::var_os("LOUISELM_TEST_NAMESPACE_ROOT").unwrap();
        let root = Path::new(&root);
        match expected.as_str() {
            "startup_held" => assert!(matches!(
                Startup::for_fixture(root),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock
            )),
            "registrar_held" => {
                let startup = Startup::wait_for_fixture(root).unwrap();
                assert!(matches!(
                    startup.registrar(),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock
                ));
            }
            "available" => {
                let startup = Startup::wait_for_fixture(root).unwrap();
                let _registrar = startup.wait_for_fixture_registrar().unwrap();
            }
            _ => panic!("unknown namespace fixture phase"),
        }
    }

    fn concurrent_start(root: &Path, expected: &str) {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "phone_control::barrier::namespace_exclusion::tests::concurrent_process",
            ])
            .env("LOUISELM_TEST_NAMESPACE_EXCLUSION", expected)
            .env("LOUISELM_TEST_NAMESPACE_ROOT", root)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn names_serialize_startup_and_retain_registrar_until_last_descriptor() {
        let root = tempfile::tempdir().unwrap();
        let startup = Startup::for_fixture(root.path()).unwrap();
        assert!(Startup::for_fixture(root.path()).is_err());
        concurrent_start(root.path(), "startup_held");
        let registrar = startup.registrar().unwrap();
        #[expect(
            clippy::used_underscore_binding,
            reason = "the fixture verifies a duplicated retained descriptor"
        )]
        let duplicate = registrar._listener.try_clone().unwrap();
        drop(startup);
        concurrent_start(root.path(), "registrar_held");
        let next = Startup::wait_for_fixture(root.path()).unwrap();
        assert!(next.registrar().is_err());
        drop(registrar);
        assert!(next.registrar().is_err());
        drop(duplicate);
        let registrar = next.wait_for_fixture_registrar().unwrap();
        drop(registrar);
        drop(next);
        concurrent_start(root.path(), "available");
    }

    #[test]
    #[ignore = "requires a disposable network namespace"]
    fn production_names_are_global_and_exclusive_in_the_namespace() {
        let startup = Startup::try_acquire().unwrap();
        assert!(Startup::try_acquire().is_err());
        let registrar = startup.registrar().unwrap();
        drop(startup);
        let next = Startup::try_acquire().unwrap();
        assert!(next.registrar().is_err());
        drop(registrar);
        assert!(next.registrar().is_ok());
    }
}
