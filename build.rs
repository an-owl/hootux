fn main() {
    let out_dir = "./target/".to_string();
    let kernel = if let Some(k) = std::env::var_os("CARGO_BIN_FILE_HOOTUX_BIN") {
        k.into_string().unwrap()
    } else {
        eprintln!("CARGO_BIN_FILE_HOOTUX_hootux not found\n");
        for (k, v) in std::env::vars() {
            eprintln!("{k} = {v}");
        }
        std::process::exit(1);
    };

    let builder = bootloader::DiskImageBuilder::new(kernel.clone().into());

    {
        let k_out = out_dir.clone() + "hootux";
        let mut out = std::io::BufWriter::new(std::fs::File::create(&k_out).unwrap());
        let mut in_f = std::fs::File::open(&kernel).unwrap();
        std::io::copy(&mut in_f, &mut out).unwrap();
    }

    let uefi_build_out = out_dir.clone() + "uefi.img";
    let bios = out_dir.clone() + "bios.img";

    builder.create_uefi_image(uefi_build_out.as_ref()).unwrap();
    builder.create_uefi_image(bios.as_ref()).unwrap();

    println!("cargo:rustc-env=UEFI_PATH={uefi_build_out}");
    println!("cargo:rustc-env=BIOS_PATH={bios}");
    println!("cargo:rustc-env=KERNEL_BIN={kernel}");
}
