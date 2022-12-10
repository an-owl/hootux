extern crate core;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const OVMF_DIR: &str = "/usr/share/edk2-ovmf/x64/";
const OVMF_BIN: &str = "OVMF_CODE.fd";

const RUN_ARGS: &[&str] = &[
    "--no-reboot",
    "-serial",
    "stdio",
    "-d",
    "int",
    "--no-reboot",
];

const TEST_ARGS: &[&str] = &[
    "-device",
    "isa-debug-exit,iobase=0xf4,iosize=0x04",
    "-serial",
    "stdio",
    "-display",
    "none",
    "--no-reboot",
];
const TEST_TIMEOUT: u64 = 10;

fn main() {
    let mut args = std::env::args().skip(1); // skip executable name

    //gather args
    let kernel_binary_path = {
        let path = PathBuf::from(args.next().unwrap());
        path.canonicalize().unwrap()
    };

    //todo add proper option parsing to enable more capabilities
    // maybe use env vars instead
    let no_boot = if let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-run" => true,
            other => panic!("unexpected argument `{}`", other),
        }
    } else {
        false
    };

    //build image
    let efi = create_images(&kernel_binary_path);
    if no_boot {
        println!("efi image created at {}", efi.display());
        return;
    }

    let mut qemu_cmd = Command::new("qemu-system-x86_64");
    qemu_cmd
        .arg("-bios")
        .arg(format!("{}{}", OVMF_DIR, OVMF_BIN));
    qemu_cmd.arg("-kernel").arg(format!("{}", efi.display()));

    let binary_kind = runner_utils::binary_kind(&kernel_binary_path);

    //run
    if binary_kind.is_test() {
        qemu_cmd.args(TEST_ARGS);
        qemu_cmd.stdout(Stdio::inherit());
        qemu_cmd.stderr(Stdio::inherit());

        let exit_status =
            runner_utils::run_with_timeout(&mut qemu_cmd, Duration::from_secs(TEST_TIMEOUT))
                .unwrap();

        match exit_status.code() {
            Some(33) => {}
            e => panic!("Failed: exit code: {:?}", e),
        }
    } else {
        qemu_cmd.args(RUN_ARGS);
        let exit_status = qemu_cmd.status().unwrap();
        if !exit_status.success() {
            std::process::exit(exit_status.code().unwrap_or(1));
        }
    }
}

fn create_images(kernel_binary_path: &Path) -> PathBuf {
    //gather paths
    let bootloader_manifest_path = bootloader_locator::locate_bootloader("bootloader").unwrap();
    let kernel_manifest_path = locate_cargo_manifest::locate_manifest().unwrap();

    //build images
    let mut img_cmd = Command::new("cargo");
    img_cmd.current_dir(bootloader_manifest_path.parent().unwrap());
    img_cmd.arg("builder");
    img_cmd.arg("--kernel-manifest").arg(&kernel_manifest_path);
    img_cmd.arg("--kernel-binary").arg(&kernel_binary_path);
    img_cmd
        .arg("--out-dir")
        .arg(kernel_binary_path.parent().unwrap().parent().unwrap()); //this dir should be target/$TARGET/

    if !img_cmd.status().unwrap().success() {
        panic!("build failed");
    }

    //return .efi
    let kernel_binary_name = kernel_binary_path.file_name().unwrap().to_str().unwrap();
    let efi = kernel_binary_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("boot-uefi-{}.efi", kernel_binary_name));
    println!("{},", efi.display());

    if !efi.exists() {
        panic!("Disk image not found")
    }

    efi
}
