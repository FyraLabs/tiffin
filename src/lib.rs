use std::{
    collections::BTreeMap,
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
    pub fn mount(
        &self,
        source: &PathBuf,
        root: &Path,
    ) -> Result<UnmountDrop<Mount>, std::io::Error> {
        // sanitize target path
        let target = self.target.strip_prefix("/").unwrap_or(&self.target);
        tracing::info!(?root, "Mounting {:?} to {:?}", source, target);
        let target = {
            let t = root.join(target);
            if !target.exists() {
                // create the target directory
                std::fs::create_dir_all(&target)?;
            }
            t
        };

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

    pub fn umount(&self, root: &Path) -> Result<(), std::io::Error> {
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

impl Default for MountTable {
    fn default() -> Self {
        Self::new()
    }
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
    pub fn add_mount(&mut self, mount: MountTarget, source: &Path) {
        self.inner.insert(source.to_path_buf(), mount);
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
    pub fn mount_chroot(&mut self, root: &Path) -> Result<(), std::io::Error> {
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

    pub fn umount_chroot(&mut self) -> Result<(), std::io::Error> {
        // let ordered = self.sort_mounts();
        let flags = UnmountFlags::DETACH;
        // why is it not unmounting properly
        self.mounts.drain(..).rev().for_each(|mount| {
            tracing::trace!("Unmounting {:?}", mount.target_path());
            // this causes ENOENT when not chrooting properly
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
    sysroot: File,
    pwd: File,
}

impl Container {
    /// Enter chroot jail
    ///
    /// This makes use of the `chroot` syscall to enter the chroot jail.
    ///
    #[inline(always)]
    pub fn chroot(&mut self) -> Result<(), std::io::Error> {
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
    pub fn exit_chroot(&mut self) -> Result<(), std::io::Error> {
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
    pub fn mount(&mut self) -> Result<(), std::io::Error> {
        self.mount_table.mount_chroot(&self.root)?;
        self._initialized = true;
        Ok(())
    }

    /// Unmounts all mountpoints inside the container
    pub fn umount(&mut self) -> Result<(), std::io::Error> {
        self.mount_table.umount_chroot()?;
        self._initialized = false;
        Ok(())
    }

    /// Adds a bind mount for the system's root filesystem to
    /// the container's root filesystem at `/run/host`
    pub fn host_bind_mount(&mut self) -> &mut Self {
        self.bind_mount(&PathBuf::from("/"), &PathBuf::from("/run/host"));
        self
    }

    /// Adds a bind mount to a file or directory inside the container
    pub fn bind_mount(&mut self, source: &Path, target: &Path) {
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
    pub fn add_mount(&mut self, mount: MountTarget, source: &Path) {
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
            Path::new("/proc"),
        );

        self.mount_table.add_mount(
            MountTarget {
                target: "sys".into(),
                fstype: Some("sysfs".to_string()),
                flags: MountFlags::empty(),
                data: None,
            },
            Path::new("/sys"),
        );

        self.mount_table.add_mount(
            MountTarget {
                target: "dev".into(),
                fstype: None,
                flags: MountFlags::BIND,
                data: None,
            },
            Path::new("/dev"),
        );

        self.mount_table.add_mount(
            MountTarget {
                target: "dev/pts".into(),
                fstype: None,
                flags: MountFlags::BIND,
                data: None,
            },
            Path::new("/dev/pts"),
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
