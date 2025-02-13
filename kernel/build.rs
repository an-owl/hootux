use std::fs::File;
use std::path::PathBuf;

// file names here
const FONT_MAP: &str = "font_map.rs";

fn main() {
    let ws_root = workspace_root();
    let resources = ws_root.join("res");
    build_fonts(&resources);
    assemble();
}

fn workspace_root() -> PathBuf {
    let output = std::process::Command::new(env!("CARGO"))
        .arg("locate-project")
        .arg("--workspace")
        .arg("--message-format=plain")
        .output()
        .unwrap()
        .stdout;

    let cargo_path = PathBuf::from(std::str::from_utf8(&output).unwrap().trim());
    cargo_path.parent().unwrap().to_path_buf()
}

fn build_fonts(resources: &PathBuf) {
    let font_dir = resources.join("font");

    let t = std::fs::read_dir(&font_dir)
        .expect(&format!("Failed to open {}", font_dir.display()))
        .map(|f| {
            f.expect(&format!("Encountered error trying to read from dir"))
                .path()
        })
        .collect::<Vec<PathBuf>>();

    let path_out = std::env::var("OUT_DIR").unwrap().to_string() + FONT_MAP;
    let mut of = File::create(&path_out).expect(&format!("Failed to create {path_out}"));
    bitmap_fontgen::codegen::gen_font(t, &mut of);

    println!("cargo:rustc-env=FONT_MAP_FILE={}", path_out);
}

/// Assembles all files contained within `assemble` and directs rustc to link them
fn assemble() {
    let out_dir = std::env::var("OUT_DIR").expect("$OUT_DIR not specified");
    // Need something assembled? Wang it in the assemble list.
    let assemble = vec!["mp/x86_trampoline.asm"];
    println!("cargo:rustc-link-search=native={}", &out_dir);

    // todo parallelize this?
    for i in assemble {
        let mut nasm = std::process::Command::new("nasm");
        nasm.args(&["-f", "elf64", "-O0"]);

        let path = PathBuf::from("src")
            .join(i)
            .canonicalize()
            .expect(&format!("src/{} does not exist", i));
        let mut obj_path = PathBuf::from(&out_dir).join(path.file_name().unwrap());
        obj_path.set_extension("o");
        nasm.args(&["-felf64", "-g"]);
        nasm.arg("-o")
            .arg(obj_path.as_os_str())
            .arg(path.as_os_str());
        eprintln!("Running {nasm:?}");
        let mut child = nasm
            .spawn()
            .expect("Failed to start `nasm`. Maybe it's not installed?");

        match child.wait() {
            Ok(s) => {
                match s.code() {
                    Some(0) => {
                        let mut ar = std::process::Command::new("ar");
                        let mut obj_no_exten = obj_path.clone();
                        obj_no_exten.set_extension("");
                        let lib_name: String = "lib".to_string()
                            + obj_no_exten.file_name().unwrap().to_str().unwrap()
                            + ".a";
                        let lib_path = PathBuf::from(&out_dir).join(lib_name);
                        ar.arg("-crDs").arg(lib_path.as_os_str()).arg(&obj_path);
                        let mut ar_child = ar.spawn().expect("Failed to start `ar`");
                        match ar_child.wait() {
                            Ok(s) => {
                                match s.code() {
                                    Some(0) => {
                                        println!(
                                            "cargo:rustc-link-lib=static={}",
                                            obj_no_exten.file_name().unwrap().to_str().unwrap()
                                        );
                                    } //good
                                    Some(n) => panic!("{ar:?} returned {n}"),
                                    None => panic!("{ar:?}\nExited unexpectedly"),
                                }
                            }
                            Err(e) => panic!("{ar:?}\n{e:?}"),
                        }
                    }
                    None => panic!("{nasm:?}\nExited unexpectedly"),
                    Some(e) => panic!("{nasm:?} returned {e}"),
                }
            }
            Err(e) => {
                panic!("{nasm:?}\n{e:?}")
            }
        }
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
