//! Fixed offline fixture probe, executed by the confined Agent itself.

use louiselm_skills::{
    discovery::{INVENTORY_PATH, Inventory, MAX_DISCOVERY_BYTES},
    discovery_source::{SourceKind, SourceRoot},
};
use std::{
    fs,
    io::{self, Read, Write},
    path::Path,
};

pub(super) fn inspect(output: &mut impl Write) -> io::Result<()> {
    let refused = || io::Error::other("skills-off fixture probe refused");
    let executable = std::env::current_exe()?;
    let runtime = executable.parent().ok_or_else(refused)?;
    let mut bytes = Vec::new();
    fs::File::open(runtime.join(INVENTORY_PATH))?
        .take((MAX_DISCOVERY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let inventory = Inventory::parse(&bytes).map_err(|_| refused())?;
    let workspace = std::env::current_dir()?;
    let home = workspace.parent().ok_or_else(refused)?.join("home");
    for source in inventory.sources {
        let base = match source.root {
            SourceRoot::Home => &home,
            SourceRoot::Workspace => &workspace,
            SourceRoot::Runtime => runtime,
            SourceRoot::System => Path::new("/"),
        };
        let path = base.join(&source.path);
        if matches!(
            source.kind,
            SourceKind::ManagedSkills
                | SourceKind::RepositorySkills
                | SourceKind::NativeSkills
                | SourceKind::SystemSkills
        ) {
            if fs::read_dir(&path)?.next().is_some()
                || fs::write(path.join("forbidden"), b"not a Skill").is_ok()
            {
                return Err(refused());
            }
            if matches!(source.root, SourceRoot::Home | SourceRoot::Workspace)
                && let Some((first, _)) = source.path.split_once('/')
            {
                let parent = base.join(first);
                if fs::rename(&parent, parent.with_extension("moved")).is_ok() {
                    return Err(refused());
                }
            }
        } else if source.kind == SourceKind::ProjectInstructions
            && fs::read(path)? != b"project instructions survive skills-off\n"
        {
            return Err(refused());
        }
    }
    fs::write(workspace.join("ordinary-edit"), b"still writable")?;
    output.write_all(b"\x1aSKILL_MASKS_OK\n")?;
    output.flush()
}
