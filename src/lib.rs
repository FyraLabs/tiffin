use std::{collections::BTreeMap, error::Error, path::PathBuf};

const MNTS: &[(
    Option<&str>,
    &str,
    Option<&str>,
    nix::mount::MsFlags,
    Option<&str>,
); 4] = &[
    (
        Some("/proc"),
        "proc",
        Some("proc"),
        nix::mount::MsFlags::empty(),
        None,
    ),
    (
        Some("/sys"),
        "sys",
        Some("sysfs"),
        nix::mount::MsFlags::empty(),
        None,
    ),
    (
        Some("/dev"),
        "dev",
        None,
        nix::mount::MsFlags::MS_BIND,
        None,
    ),
    (
        Some("/dev/pts"),
        "dev/pts",
        None,
        nix::mount::MsFlags::MS_BIND,
        None,
    ),
];

/// Mount object struct
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
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

    pub fn mount_chroot(
        &self,
        source: Option<&PathBuf>,
        root: &PathBuf,
    ) -> Result<(), Box<dyn Error>> {
        // sanitize target path
        let target = self.target.strip_prefix("/").unwrap_or(&self.target);
        let target = root.join(target);
        nix::mount::mount(
            source,
            &target,
            self.fstype.as_deref(),
            self.flags,
            self.data.as_deref(),
        )?;
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
            mount.mount_chroot(Some(source), root)?;
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
    _initialized: bool,
}

impl Container {
    /// Create a new container.
    pub fn new(root: PathBuf) -> Self {
        let mut s = Self {
            root,
            _initialized: false,
        };
        s.mount_tmpfs().unwrap();
        s
    }
    /// Mounts temporary filesystems inside the container.
    fn mount_tmpfs(&mut self) -> Result<(), Box<dyn Error>> {
        for (source, target, fstype, flags, data) in MNTS {
            // let source = source.map(|s| self.root.join(s));
            // let target = self.root.join(target);
            // nix::mount::mount(source.as_deref(), &target, *fstype, *flags, data.as_deref())?;

            std::fs::create_dir_all(self.root.join(target))?;
            let mut tries = 0;
            loop {
                if nix::mount::mount(
                    source.as_deref(),
                    &self.root.join(target),
                    *fstype,
                    *flags,
                    data.as_deref(),
                )
                .is_ok()
                {
                    break;
                }
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
                if tries > 10 {
                    return Err("Failed to mount tmpfs".into());
                }
            }
            self._initialized = true;
        }
        Ok(())
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        tracing::trace!("Dropping container, images will be unmounted");

        if self._initialized {
            let mounts = vec![
                self.root.join("dev/pts"),
                self.root.join("dev"),
                self.root.join("sys"),
                self.root.join("proc"),
            ];

            for mount in mounts {
                tracing::trace!("Unmounting {}", mount.display());
                nix::mount::umount(&mount).unwrap();
            }
        }
    }
}
