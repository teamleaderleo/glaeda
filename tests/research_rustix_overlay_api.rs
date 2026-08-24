#![cfg(target_os = "linux")]
#![allow(dead_code)]

use std::os::fd::BorrowedFd;

use rustix::mount::{
    FsMountFlags, FsOpenFlags, MountAttrFlags, MoveMountFlags, fsconfig_create, fsconfig_set_fd,
    fsmount, fsopen, move_mount,
};

fn compile_overlay_transaction(
    lower: BorrowedFd<'_>,
    upper: BorrowedFd<'_>,
    work: BorrowedFd<'_>,
    merged: BorrowedFd<'_>,
) -> rustix::io::Result<()> {
    let fs = fsopen("overlay", FsOpenFlags::CLOEXEC)?;
    fsconfig_set_fd(&fs, "lowerdir+", lower)?;
    fsconfig_set_fd(&fs, "upperdir", upper)?;
    fsconfig_set_fd(&fs, "workdir", work)?;
    fsconfig_create(&fs)?;
    let mount = fsmount(
        &fs,
        FsMountFlags::CLOEXEC,
        MountAttrFlags::NODEV | MountAttrFlags::NOSUID,
    )?;
    move_mount(
        &mount,
        "",
        merged,
        "",
        MoveMountFlags::F_EMPTY_PATH | MoveMountFlags::T_EMPTY_PATH,
    )?;
    Ok(())
}
