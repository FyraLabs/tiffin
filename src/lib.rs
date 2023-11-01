use std::{collections::BTreeMap, error::Error, path::PathBuf};
/// Mount object struct
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub struct Mount {
    pub target: PathBuf,
    pub fstype: Option<String>,
    pub flags: nix::mount::MsFlags,
    pub data: Option<String>,
}

impl Mount {
    /// Create a new mount object
    pub fn new(
        target: PathBuf,
        fstype: Option<String>,
        flags: nix::mount::MsFlags,
        data: Option<String>,
    ) -> Self {
        Self {
            target,
            fstype,
            flags,
            data,
        }
    }

    pub fn mount(
        &self,
        source: Option<&PathBuf>,
        root: &PathBuf,
    ) -> Result<(), Box<dyn Error>> {
        // sanitize target path
        let target = self.target.strip_prefix("/").unwrap_or(&self.target);
        let target = root.join(target);

        std::fs::create_dir_all(&target)?;

        nix::mount::mount(
            source,
            &target,
            self.fstype.as_deref(),
            self.flags,
            self.data.as_deref(),
        )?;
        Ok(())
    }

    pub fn umount(&self, root: &PathBuf) -> Result<(), Box<dyn Error>> {
        // sanitize target path
        let target = self.target.strip_prefix("/").unwrap_or(&self.target);
        let target = root.join(target);

        nix::mount::umount(&target)?;
        Ok(())
    }
}

/// Mount Table Struct
/// This is used to mount filesystems inside the container. It is essentially an fstab, for the container.
#[derive(Debug)]
pub struct MountTable {
    /// The table of mounts
    /// The key is the device name, and value is the mount object
    inner: BTreeMap<PathBuf, Mount>,
}

impl MountTable {
    pub fn new() -> Self {
        Self {
            inner: BTreeMap::new(),
        }
    }
    /// Sets the mount table
    pub fn set_table(&mut self, table: BTreeMap<PathBuf, Mount>) {
        self.inner = table;
    }

    /// Adds a mount to the table
    pub fn add_mount(&mut self, mount: Mount, source: &PathBuf) {
        self.inner.insert(source.clone(), mount);
    }

    /// Sort mounts by mountpoint and depth
    /// Closer to root, and root is first
    /// everything else is either sorted by depth, or alphabetically
    fn sort_mounts(&self) -> Vec<(&PathBuf, &Mount)> {
        let mut mounts: Vec<(&PathBuf, &Mount)> = self.inner.iter().collect();
        mounts.sort_unstable_by(|(_, a), (_, b)| {
			let am = a.target.display().to_string().matches('/').count();
			let bm = b.target.display().to_string().matches('/').count();
            if a.target.display().to_string() == "/" {
                std::cmp::Ordering::Less
            } else if b.target.display().to_string() == "/" {
                std::cmp::Ordering::Greater
            } else if am == bm {
                a.target.cmp(&b.target)
            } else {
                am.cmp(&bm)
            }
            
        });
        mounts
    }
    
    /// Mounts everything to the root
    pub fn mount_chroot(&self, root: &PathBuf) -> Result<(), Box<dyn Error>> {
        let ordered = self.sort_mounts();
        for (source, mount) in ordered {
            mount.mount(Some(source), root)?;
        }
        Ok(())
    }

    pub fn umount_chroot(&self, root: &PathBuf) -> Result<(), Box<dyn Error>> {
        let ordered = self.sort_mounts();
        let ordered = ordered.iter().rev();
        for (_, mount) in ordered {
            mount.umount(root)?;
        }
        Ok(())
    }
}

/// A chroot container.
/// This is the main entry point for the library.
/// It is used to create a new minimal container, so that
/// the user can run a process inside it.
///
/// Might require root privileges.
#[derive(Debug)]
pub struct Container {
    pub root: PathBuf,
    pub mount_table: MountTable,
    _initialized: bool,
}

impl Container {
    /// Create a new container.
    pub fn new(root: PathBuf) -> Self {
        let mut container = Self {
            root,
            mount_table: MountTable::new(),
            _initialized: false,
        };

        container.setup_minimal_mounts();

        container
    }

    pub fn mount(&self) -> Result<(), Box<dyn Error>> {
        self.mount_table.mount_chroot(&self.root)?;
        Ok(())
    }

    fn setup_minimal_mounts(&mut self) {
        self.mount_table.add_mount(Mount {
            target: "proc".into(),
            fstype: Some("proc".to_string()),
            flags: nix::mount::MsFlags::empty(),
            data: None,
        }, &"/proc".into());

        self.mount_table.add_mount(Mount {
            target: "sys".into(),
            fstype: Some("sysfs".to_string()),
            flags: nix::mount::MsFlags::empty(),
            data: None,
        }, &"/sys".into());

        self.mount_table.add_mount(Mount {
            target: "dev".into(),
            fstype: None,
            flags: nix::mount::MsFlags::MS_BIND,
            data: None,
        }, &"/dev".into());

        self.mount_table.add_mount(Mount {
            target: "dev/pts".into(),
            fstype: None,
            flags: nix::mount::MsFlags::MS_BIND,
            data: None,
        }, &"/dev/pts".into());
    } 
}

impl Drop for Container {
    fn drop(&mut self) {
        tracing::trace!("Dropping container, images will be unmounted");

        if self._initialized {
            self.mount_table.umount_chroot(&self.root).unwrap();
        }
    }
}
