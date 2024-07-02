use std::{collections::BTreeMap, error::Error, path::PathBuf};
use sys_mount::{FilesystemType, Mount, MountFlags, Unmount, UnmountDrop, UnmountFlags};
const INIT_CWD: &str = "/proc/1/cwd/";
/// Mount object struct
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub struct MountTarget {
    pub target: PathBuf,
    pub fstype: Option<String>,
    pub flags: MountFlags,
    pub data: Option<String>,
}

impl MountTarget {
    /// Create a new mount object
    pub fn new(
        target: PathBuf,
        fstype: Option<String>,
        flags: MountFlags,
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
        source: &PathBuf,
        root: &PathBuf,
    ) -> Result<UnmountDrop<Mount>, Box<dyn Error>> {
        // sanitize target path
        let target = self.target.strip_prefix("/").unwrap_or(&self.target);
        let target = root.join(target).canonicalize()?;

        std::fs::create_dir_all(&target)?;

        // nix::mount::mount(
        //     source,
        //     &target,
        //     self.fstype.as_deref(),
        //     self.flags,
        //     self.data.as_deref(),
        // )?;
        let mut mount = Mount::builder().flags(self.flags);
        if let Some(fstype) = &self.fstype {
            let fstype = fstype.as_str();
            mount = mount.fstype(FilesystemType::Manual(fstype));
        }

        if let Some(data) = &self.data {
            mount = mount.data(data);
        }

        let mount = mount.mount_autodrop(source, &target, UnmountFlags::empty())?;
        Ok(mount)
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
// #[derive(Debug)]
pub struct MountTable {
    /// The table of mounts
    /// The key is the device name, and value is the mount object
    inner: BTreeMap<PathBuf, MountTarget>,
    mounts: Vec<UnmountDrop<Mount>>,
}

impl MountTable {
    pub fn new() -> Self {
        Self {
            inner: BTreeMap::new(),
            mounts: Vec::new(),
        }
    }
    /// Sets the mount table
    pub fn set_table(&mut self, table: BTreeMap<PathBuf, MountTarget>) {
        self.inner = table;
    }

    /// Adds a mount to the table
    pub fn add_mount(&mut self, mount: MountTarget, source: &PathBuf) {
        self.inner.insert(source.clone(), mount);
    }

    pub fn add_sysmount(&mut self, mount: UnmountDrop<Mount>) {
        self.mounts.push(mount);
    }

    /// Sort mounts by mountpoint and depth
    /// Closer to root, and root is first
    /// everything else is either sorted by depth, or alphabetically
    fn sort_mounts(&self) -> Vec<(&PathBuf, &MountTarget)> {
        let mut mounts: Vec<(&PathBuf, &MountTarget)> = self.inner.iter().collect();
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
    pub fn mount_chroot(&mut self, root: &PathBuf) -> Result<(), Box<dyn Error>> {
        // let ordered = self.sort_mounts();
        // for (source, mount) in ordered {
        //     let m = mount.mount(source, root)?;
        //     self.mounts.push(m);
        // }
        //
        self.mounts = self
            .sort_mounts()
            .iter()
            .map(|(source, mount)| {
                tracing::trace!(?mount, ?source, "Mounting");
                // make source if not exists
                if !root.join(source).exists() {
                    std::fs::create_dir_all(root.join(source)).unwrap();
                }
                mount.mount(source, root).unwrap()
            })
            .collect();
        Ok(())
    }

    pub fn umount_chroot(&mut self) -> Result<(), Box<dyn Error>> {
        // let ordered = self.sort_mounts();
        let flags = UnmountFlags::DETACH;
        // why is it not unmounting properly
        self.mounts.drain(..).rev().for_each(|mount| {
            println!("Unmounting {:?}", mount.target_path());
            mount.unmount(flags).unwrap();
            drop(mount);
        });
        Ok(())
    }
}

/// Container Struct
/// A tiffin container is a simple chroot jail that can be used to run code inside.
///
/// May require root permissions to use.
// #[derive(Debug)]
pub struct Container {
    pub root: PathBuf,
    pub mount_table: MountTable,
    _initialized: bool,
    chroot: bool,
}

impl Container {
    // todo: pivot root with this
    // #[must_use]
    // fn mount_rootfs(&mut self) -> Result<(), Box<dyn Error>> {
    //     nix::sched::unshare(nix::sched::CloneFlags::CLONE_NEWNS)?;
    //     // mount --bind $rootfs $rootfs

    //     let rootfs = self.root.clone();

    //     let m = sys_mount::Mount::builder()
    //         .flags(MountFlags::BIND | MountFlags::REC)
    //         .mount_autodrop(&rootfs, &rootfs, UnmountFlags::empty())?;

    //     self.mount_table.add_sysmount(m);

    //     // let rootfs = self.inner.get(&PathBuf::from("/")).unwrap();
    //     // let source = PathBuf::from(INIT_CWD);
    //     // let root = PathBuf::from("/");
    //     // let m = rootfs.mount(&source, &root)?;
    //     // self.mounts.push(m);
    //     Ok(())
    // }

    /// Enter chroot jail
    ///
    /// This makes use of the `chroot` syscall to enter the chroot jail.
    ///
    pub fn chroot(&mut self) -> Result<(), Box<dyn Error>> {
        // self.mount_rootfs()?;

        // std::fs::create_dir_all(self.root.join("host"))?;
        // nix::unistd::pivot_root(
        //     &self.root.canonicalize()?,
        //     &self.root.join("host").canonicalize()?,
        // )?;

        nix::unistd::chroot(&self.root)?;
        self.chroot = true;
        nix::unistd::chdir("/")?;
        Ok(())
    }

    /// Exits the chroot
    ///
    /// This works by making use of our /proc passthrough,
    /// and chrooting to the init process (pid 1)'s cwd.
    ///
    /// This should always work as long as we're not
    /// overriding init's cwd.
    ///
    /// A side effect of this is that if we're running inside a nested
    /// container, we may end up escaping that container and go straight
    /// to the host, not just the parent container.
    pub fn exit_chroot(&mut self) -> Result<(), Box<dyn Error>> {
        nix::unistd::chroot(INIT_CWD)?;
        // nix::unistd::chroot("/host")?;
        // nix::unistd::(std::path::Path::new("/"))?;
        self.chroot = false;
        Ok(())
    }

    /// Create a new tiffin container
    ///
    /// To use it, you need to create a new container with `root`
    /// set to the location of the chroot you'd like to use.
    pub fn new(chrootpath: PathBuf) -> Self {
        let mut container = Self {
            root: chrootpath,
            mount_table: MountTable::new(),
            // sysroot: root.as_raw_fd(),
            _initialized: false,
            chroot: false,
        };

        container.setup_minimal_mounts();

        container
    }

    /// Run a function inside the container chroot
    pub fn run<F>(&mut self, f: F) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<(), Box<dyn Error>>,
    {
        // Only mount and chroot if we're not already initialized
        if !self._initialized {
            self.mount()?;
        }
        if !self.chroot {
            self.chroot()?;
        }
        tracing::trace!("Running function inside container");
        f()?;
        if self.chroot {
            self.exit_chroot()?;
        }
        if self._initialized {
            self.umount()?;
        }

        Ok(())
    }

    /// Start mounting files inside the container
    pub fn mount(&mut self) -> Result<(), Box<dyn Error>> {
        self.mount_table.mount_chroot(&self.root)?;
        self._initialized = true;
        Ok(())
    }

    /// Unmounts all mountpoints inside the container
    pub fn umount(&mut self) -> Result<(), Box<dyn Error>> {
        self.mount_table.umount_chroot()?;
        self._initialized = false;
        Ok(())
    }

    /// Adds a bind mount to a file or directory inside the container
    pub fn bind_mount(&mut self, source: &PathBuf, target: &PathBuf) {
        self.mount_table.add_mount(
            MountTarget {
                target: target.to_owned(),
                fstype: None,
                flags: MountFlags::BIND,
                data: None,
            },
            source,
        );
    }

    /// Adds an additional mount target to the container mount table
    ///
    /// Useful for mounting disks or other filesystems
    pub fn add_mount(&mut self, mount: MountTarget, source: &PathBuf) {
        self.mount_table.add_mount(mount, source);
    }

    fn setup_minimal_mounts(&mut self) {
        self.mount_table.add_mount(
            MountTarget {
                target: "proc".into(),
                fstype: Some("proc".to_string()),
                flags: MountFlags::empty(),
                data: None,
            },
            &"/proc".into(),
        );

        self.mount_table.add_mount(
            MountTarget {
                target: "sys".into(),
                fstype: Some("sysfs".to_string()),
                flags: MountFlags::empty(),
                data: None,
            },
            &"/sys".into(),
        );

        self.mount_table.add_mount(
            MountTarget {
                target: "dev".into(),
                fstype: None,
                flags: MountFlags::BIND,
                data: None,
            },
            &"/dev".into(),
        );

        self.mount_table.add_mount(
            MountTarget {
                target: "dev/pts".into(),
                fstype: None,
                flags: MountFlags::BIND,
                data: None,
            },
            &"/dev/pts".into(),
        );

        // add /host mount
        self.mount_table.add_mount(
            MountTarget {
                target: "host".into(),
                fstype: None,
                flags: MountFlags::BIND,
                data: None,
            },
            &"/".into(),
        );
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        tracing::trace!("Dropping container, images will be unmounted");
        if self.chroot {
            self.exit_chroot().unwrap();
        }
        if self._initialized {
            self.umount().unwrap();
        }
    }
}
