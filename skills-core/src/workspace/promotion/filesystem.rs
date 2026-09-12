//! Descriptor-relative regular-file effects; never Git or candidate execution.

use crate::workspace::{SourceFile, WorkspaceError};
use rustix::fs::{Mode, OFlags};
use std::{fs::File, io::Write as _};

fn parent(root: &File, path: &str, create: bool) -> Result<(File, String), WorkspaceError> {
    let mut parts: Vec<_> = path.split('/').collect();
    let name = parts
        .pop()
        .ok_or(WorkspaceError::Invalid("missing file name"))?
        .to_owned();
    let mut directory = root.try_clone()?;
    for part in parts {
        if create {
            match rustix::fs::mkdirat(&directory, part, Mode::RWXU) {
                Ok(()) => directory.sync_all()?,
                Err(rustix::io::Errno::EXIST) => (),
                Err(error) => return Err(std::io::Error::from(error).into()),
            }
        }
        directory = File::from(
            rustix::fs::openat(
                &directory,
                part,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
    }
    Ok((directory, name))
}

pub(super) fn change(
    root: &File,
    path: &str,
    file: Option<&SourceFile>,
) -> Result<(), WorkspaceError> {
    let (directory, name) = parent(root, path, file.is_some())?;
    if let Some(file) = file {
        // A random O_EXCL staging file is opened relative to the pinned parent.
        let mut random = [0_u8; 32];
        if rustix::rand::getrandom(&mut random, rustix::rand::GetRandomFlags::empty())
            .map_err(std::io::Error::from)?
            != random.len()
        {
            return Err(WorkspaceError::Invalid(
                "temporary file identity unavailable",
            ));
        }
        let temporary = format!(".louiselm-{}", crate::Digest::of(&random).hex());
        let fd = rustix::fs::openat(
            &directory,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(std::io::Error::from)?;
        let mut output = File::from(fd);
        let result = (|| {
            output.write_all(&file.bytes)?;
            rustix::fs::fchmod(
                &output,
                if file.executable {
                    Mode::RWXU
                } else {
                    Mode::RUSR | Mode::WUSR
                },
            )
            .map_err(std::io::Error::from)?;
            output.sync_all()?;
            rustix::fs::renameat(&directory, temporary.as_str(), &directory, name.as_str())
                .map_err(std::io::Error::from)?;
            directory.sync_all()
        })();
        if result.is_err() {
            match rustix::fs::unlinkat(&directory, temporary.as_str(), rustix::fs::AtFlags::empty())
            {
                Ok(()) | Err(rustix::io::Errno::NOENT) => (),
                Err(error) => return Err(std::io::Error::from(error).into()),
            }
        }
        result?;
    } else {
        rustix::fs::unlinkat(&directory, name.as_str(), rustix::fs::AtFlags::empty())
            .map_err(std::io::Error::from)?;
        directory.sync_all()?;
        remove_empty_parents(root, path)?;
    }
    Ok(())
}

fn remove_empty_parents(root: &File, path: &str) -> Result<(), WorkspaceError> {
    let mut path = path;
    while let Some((ancestor, _)) = path.rsplit_once('/') {
        let (directory, name) = parent(root, ancestor, false)?;
        match rustix::fs::unlinkat(&directory, name.as_str(), rustix::fs::AtFlags::REMOVEDIR) {
            Ok(()) => directory.sync_all()?,
            Err(rustix::io::Errno::NOTEMPTY) => break,
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
        path = ancestor;
    }
    Ok(())
}
