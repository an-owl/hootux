
fn main() {
    let out_dir = std::env::var_os("OUT_DIR").unwrap().into_string().unwrap();
    let kernel = if let Some(k) = std::env::var_os("CARGO_BIN_FILE_HOOTUX_hootux") {
        k.into_string().unwrap()
    } else {
        eprintln!("CARGO_BIN_FILE_HOOTUX_hootux not found\n");
        for (k,v) in std::env::vars() {
            eprintln!("{k} = {v}");
        };
        std::process::exit(1);
    };

    let uefi_build_out = out_dir.clone() + "uefi.img";
    bootloader::UefiBoot::new(std::path::Path::new(&kernel)).create_disk_image(uefi_build_out.as_ref()).unwrap();

    let bios = out_dir.clone() + "bios.img";
    bootloader::BiosBoot::new(std::path::Path::new(&kernel)).create_disk_image(bios.as_ref()).unwrap();

    println!("cargo:rustc-env=UEFI_PATH={uefi_build_out}");
    println!("cargo:rustc-env=BIOS_PATH={bios}");
    println!("cargo:rustc-env=KERNEL_BIN={kernel}");

}