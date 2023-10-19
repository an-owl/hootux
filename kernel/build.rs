use std::fs::File;
use std::path::PathBuf;

// file names here
const FONT_MAP: &str = "font_map.rs";

fn main() {
    let ws_root = workspace_root();
    let resources = ws_root.join("res");
    build_fonts(&resources);
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
