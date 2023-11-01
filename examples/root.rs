use std::process::Command;

use tiffin::{Container, Mount, MountTable};

fn main() {
    let container = Container::new("tiffin".into());

    container.mount().unwrap();

    Command::new("/bin/findmnt")
        .arg("-l")
        .arg("-o")
        .arg("source,target,fstype")
        .spawn()
        .unwrap()
        .wait()
        .unwrap();

    drop(container);
}