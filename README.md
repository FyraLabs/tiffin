# Tiffin

Tiffin is a simple and lightweight Rust library for creating and entering chroot jails on Linux.

It spawned from Katsu's chroot code, which was originally designed for setting up Linux environment from scratch.

This library does not contain methods for setting up the chroot environment, but will use an existing rootfs to create a jail out of.

## References

<https://github.com/util-linux/util-linux/blob/master/sys-utils/unshare.c>

<https://gitee.com/kt10/nspawn-lite/blob/master/src/main.rs>
