use std::process::Command;

const TARGET: &str = "x86_64-owl-os-build";

fn main(){
    build_kernel_bin().expect("failed to build kernel");

    build_bootloader().expect("failed to create bootable images");
}

fn build_kernel_bin() -> Result<(),()> {
    let mut build_cmd = Command::new("/usr/bin/cargo");
    build_cmd.arg("+nightly");
    build_cmd.arg("build");
    build_cmd.arg("--release");
    build_cmd.arg("-Zbuild-std=core,alloc");
    build_cmd.arg("--target").arg(format!("{}.json",TARGET));

    if build_cmd.status().is_err(){
        println!("failed to build");
        return Err(());
    }
    Ok(())
}

fn build_bootloader() -> Result<(),()> {
    let bootloader_manifest = bootloader_locator::locate_bootloader("bootloader").expect("failed to locate bootloader manifest");
    let kernel_manifest = locate_cargo_manifest::locate_manifest().expect("failed to locate kernel manifest");

    //Kernel target
    let target_path = kernel_manifest.parent().unwrap()
        .join("target").join(TARGET).join("release").join("hootux");

    let mut build_cmd = Command::new("/usr/bin/cargo");
    build_cmd.current_dir(&bootloader_manifest.parent().unwrap());
    build_cmd.arg("builder");
    build_cmd.arg("--kernel-manifest").arg(&kernel_manifest);
    build_cmd.arg("--kernel-binary").arg(&target_path);
    build_cmd.arg("--out-dir").arg("/home/an_owl/source/rust/owl_os/target/x86_64-owl-os-build/");

    if build_cmd.status().is_err(){
        println!("failed to create bootable images");
        return Err(())
    }

    Ok(())
}

