use itertools::Itertools;
use std::{
    collections::HashMap,
    fs::File,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
};
use sys_mount::{FilesystemType, Mount, MountFlags, Unmount, UnmountDrop, UnmountFlags};
/// Mount object struct
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub struct MountTarget {
    pub target: PathBuf,
    pub fstype: Option<String>,
    pub flags: MountFlags,
    pub data: Option<String>,
}

impl Default for MountTarget {
    fn default() -> Self {
        Self {
            target: Default::default(),
            fstype: Default::default(),
            flags: MountFlags::empty(),
            data: Default::default(),
        }
    }
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

    #[tracing::instrument]
    pub fn mount(&self, source: &PathBuf, root: &Path) -> std::io::Result<UnmountDrop<Mount>> {
        // sanitize target path
        let target = self.target.strip_prefix("/").unwrap_or(&self.target);
        tracing::info!(?root, "Mounting {source:?} to {target:?}");
        let target = root.join(target);
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
            mount = mount.fstype(FilesystemType::Manual(fstype));
        }

        if let Some(data) = &self.data {
            mount = mount.data(data);
        }

        let mount = mount.mount_autodrop(source, &target, UnmountFlags::empty())?;
        Ok(mount)
    }

    pub fn umount(&self, root: &Path) -> std::io::Result<()> {
        // sanitize target path
        let target = self.target.strip_prefix("/").unwrap_or(&self.target);
        let target = root.join(target);

        nix::mount::umount(&target)?;
        Ok(())
    }
}

/// Mount Table Struct
/// This is used to mount filesystems inside the container. It is essentially an fstab, for the container.
#[derive(Default)]
pub struct MountTable {
    /// The table of mounts
    /// The key is the device name, and value is the mount object
    inner: HashMap<PathBuf, MountTarget>,
    mounts: Vec<UnmountDrop<Mount>>,
}

impl MountTable {
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
            mounts: Vec::new(),
        }
    }
    /// Sets the mount table
    pub fn set_table(&mut self, table: HashMap<PathBuf, MountTarget>) {
        self.inner = table;
    }

    /// Adds a mount to the table
    pub fn add_mount(&mut self, mount: MountTarget, source: PathBuf) {
        self.inner.insert(source, mount);
    }

    pub fn add_sysmount(&mut self, mount: UnmountDrop<Mount>) {
        self.mounts.push(mount);
    }

    /// Sort mounts by mountpoint and depth
    /// Closer to root, and root is first
    /// everything else is either sorted by depth, or alphabetically
    fn sort_mounts(&self) -> impl Iterator<Item = (&PathBuf, &MountTarget)> {
        self.inner.iter().sorted_unstable_by(|(_, a), (_, b)| {
            match (a.target.components().count(), b.target.components().count()) {
                (1, _) => std::cmp::Ordering::Less,    // root dir
                (_, 1) => std::cmp::Ordering::Greater, // root dir
                (x, y) if x == y => a.target.cmp(&b.target),
                (x, y) => x.cmp(&y),
            }
        })
    }

    /// Mounts everything to the root
    pub fn mount_chroot(&mut self, root: &Path) -> std::io::Result<()> {
        // let ordered = self.sort_mounts();
        // for (source, mount) in ordered {
        //     let m = mount.mount(source, root)?;
        //     self.mounts.push(m);
        // }
        //
        self.mounts = self
            .sort_mounts()
            .map(|(source, mount)| {
                tracing::trace!(?mount, ?source, "Mounting");
                std::fs::create_dir_all(root.join(source))?;
                mount.mount(source, root)
            })
            .collect::<std::io::Result<_>>()?;
        Ok(())
    }

    pub fn umount_chroot(&mut self) -> std::io::Result<()> {
        self.mounts.drain(..).rev().try_for_each(|mount| {
            tracing::trace!("Unmounting {:?}", mount.target_path());
            // this causes ENOENT when not chrooting properly
            mount.unmount(UnmountFlags::DETACH)
        })
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
    sysroot: File,
    pwd: File,
}

impl Container {
    /// Enter chroot jail
    ///
    /// This makes use of the `chroot` syscall to enter the chroot jail.
    ///
    #[inline(always)]
    pub fn chroot(&mut self) -> std::io::Result<()> {
        if !self._initialized {
            // mount the tmpfs first, idiot proofing in case the
            // programmer forgets to mount it before chrooting
            //
            // This should be fine as it's going to be dismounted after dropping regardless
            self.mount()?;
        }

        nix::unistd::chroot(&self.root)?;
        self.chroot = true;
        nix::unistd::chdir("/")?;
        Ok(())
    }

    /// Exits the chroot
    ///
    /// This works by changing the current working directory
    /// to a raw file descriptor of the sysroot we saved earlier
    /// in `[Container::new]`, and then chrooting to the directory
    /// we just moved to.
    ///
    /// We then also take the pwd stored earlier and move back to it,
    /// for good measure.
    #[inline(always)]
    pub fn exit_chroot(&mut self) -> std::io::Result<()> {
        nix::unistd::fchdir(self.sysroot.as_raw_fd())?;
        nix::unistd::chroot(".")?;
        self.chroot = false;

        // Let's return back to pwd
        nix::unistd::fchdir(self.pwd.as_raw_fd())?;
        Ok(())
    }

    /// Create a new tiffin container
    ///
    /// To use it, you need to create a new container with `root`
    /// set to the location of the chroot you'd like to use.
    pub fn new(chrootpath: PathBuf) -> Self {
        let pwd = std::fs::File::open("/proc/self/cwd").unwrap();
        let sysroot = std::fs::File::open("/").unwrap();

        let mut container = Self {
            pwd,
            root: chrootpath,
            mount_table: MountTable::new(),
            sysroot,
            _initialized: false,
            chroot: false,
        };

        container.setup_minimal_mounts();

        container
    }

    /// Run a function inside the container chroot
    #[inline(always)]
    pub fn run<F, T>(&mut self, f: F) -> std::io::Result<T>
    where
        F: FnOnce() -> T,
    {
        // Only mount and chroot if we're not already initialized
        if !self._initialized {
            self.mount()?;
        }
        if !self.chroot {
            self.chroot()?;
        }
        tracing::trace!("Running function inside container");
        let ret = f();
        if self.chroot {
            self.exit_chroot()?;
        }
        if self._initialized {
            self.umount()?;
        }
        Ok(ret)
    }

    /// Start mounting files inside the container
    pub fn mount(&mut self) -> std::io::Result<()> {
        self.mount_table.mount_chroot(&self.root)?;
        self._initialized = true;
        Ok(())
    }

    /// Unmounts all mountpoints inside the container
    pub fn umount(&mut self) -> std::io::Result<()> {
        self.mount_table.umount_chroot()?;
        self._initialized = false;
        Ok(())
    }

    /// Adds a bind mount for the system's root filesystem to
    /// the container's root filesystem at `/run/host`
    pub fn host_bind_mount(&mut self) -> &mut Self {
        self.bind_mount(PathBuf::from("/"), PathBuf::from("/run/host"));
        self
    }

    /// Adds a bind mount to a file or directory inside the container
    pub fn bind_mount(&mut self, source: PathBuf, target: PathBuf) {
        self.mount_table.add_mount(
            MountTarget {
                target,
                flags: MountFlags::BIND,
                ..MountTarget::default()
            },
            source,
        );
    }

    /// Adds an additional mount target to the container mount table
    ///
    /// Useful for mounting disks or other filesystems
    pub fn add_mount(&mut self, mount: MountTarget, source: PathBuf) {
        self.mount_table.add_mount(mount, source);
    }

    fn setup_minimal_mounts(&mut self) {
        self.mount_table.add_mount(
            MountTarget {
                target: "proc".into(),
                fstype: Some("proc".to_string()),
                ..MountTarget::default()
            },
            PathBuf::from("/proc"),
        );

        self.mount_table.add_mount(
            MountTarget {
                target: "sys".into(),
                fstype: Some("sysfs".to_string()),
                ..MountTarget::default()
            },
            PathBuf::from("/sys"),
        );

        self.bind_mount("/dev".into(), "dev".into());
        self.bind_mount("/dev/pts".into(), "dev/pts".into());
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

// We can't really reproduce this test in a CI environment, so let's just ignore it
#[cfg(test)]
// Test only if we're running as root
mod tests {
    use super::*;
    use std::path::PathBuf;
    #[ignore = "This test requires root"]
    #[test]
    fn test_container() {
        std::fs::create_dir_all("/tmp/tiffin").unwrap();
        let mut container = Container::new(PathBuf::from("/tmp/tiffin"));
        container
            .run(|| std::fs::create_dir_all("/tmp/tiffin/test").unwrap())
            .unwrap();
    }
}
