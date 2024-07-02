use std::process::Command;


use tiffin::{Container, MountTarget, MountTable};

fn main() {
    let mut container = Container::new("chroot".into());

    container.mount().unwrap();

    Command::new("/bin/findmnt")
        .arg("-l")
        .arg("-o")
        .arg("source,target,fstype")
        .spawn()
        .unwrap()
        .wait()
        .unwrap();

    // drop(container);
    // 
    // match container.chroot() {
    //     Ok(_) => println!("Chrooted successfully"),
    //     Err(e) => {
    //         println!("Error: {:?}", e);
    //         std::process::exit(1);
    //     }
    // }
    // 
    container.chroot().unwrap();
    
    // get the current working directory
    let cwd = std::env::current_dir().unwrap();
    println!("Current working directory: {:?}", cwd);
    // Command::new("ls")
    //     .arg("-la")
    //     .arg("/host")
    //     .spawn()
    //     .unwrap()
    //     .wait()
    //     .unwrap();
    // container.exit_chroot().unwrap();
    
    
    
    // container.umount().unwrap();
    
    // you don't even need to call umount(), it will be called when the container object is dropped
    // but if you'd like to just be sure or you don't want to drop it yet, you can call it manually
    drop(container);
    
    
    Command::new("/bin/findmnt")
        .arg("-l")
        .arg("-o")
        .arg("source,target,fstype")
        .spawn()
        .unwrap()
        .wait()
        .unwrap();
}