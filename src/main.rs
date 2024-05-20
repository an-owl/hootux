use getopts::{Fail, HasArg, Occur};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use toml::value::Value;

const QEMU: &str = "qemu-system-x86_64";

static BRIEF: &str = r#"\
Usage `cargo run -- [OPTIONS]`
"#;

const GRUB_DEFAULT_CONFIG: &str =
r#"set timeout=0
set default=0

menuentry hootux {
    multiboot2 /boot/hootux
    boot
}
"#;

/*
Return codes:
0. Ok
1. Unable to select run mode
2. Argument error
4. Unable to locate firmware
5. Unable to locate qemu
6. fs error during build
0x1? toml misconfigured
0x6x
0x2x failed to run something
*/

fn main() {
    let opts = Options::get_args();
    let toml = opts.fetch_toml();

    let kernel = if let Some(k) = build_kernel() {
        k
    } else {
        eprintln!("Failed to get path to kernel");
        std::process::exit(0x26)
    };
    let img = opts.build_grub_img(&kernel);

    let mut qemu = if let Some(q) = opts.build_exec(&img, &toml) {
        eprintln!("{q:?}");
        q
    } else {
        std::process::exit(0)
    };

    let mut children = Vec::new();

    let mut run = || {
        let qemu_child = qemu.spawn().unwrap();
        if let Some(mut c) = opts.run_debug(&toml, &kernel) {
            children.push(c.spawn().unwrap());
        }
        qemu_child
    };

    if opts.daemonize {
        let d = daemonize::Daemonize::new()
            .user(&*std::env::var("USER").expect("Who are you people!?: No user"))
            .working_directory(&*std::env::current_dir().unwrap());
        if d.execute().is_child() {
            // idc what it returns
            let _ = run().wait();
        }
    } else {
        let _ = run().wait();
    };
}

#[non_exhaustive]
#[derive(Eq, PartialEq, Debug)]
enum Subcommand {
    Bios,
    Uefi,
    NoRun,
}

impl Subcommand {
    fn build_qemu(&self, drive: impl AsRef<std::path::Path>) -> Option<Command> {
        let mut command = Command::new(QEMU);

        match self {
            Subcommand::Uefi | Subcommand::Bios => {
                command
                    .arg("-drive")
                    .arg(format!("format=raw,file={}",drive.as_ref().to_str().unwrap()));
            }
            Subcommand::NoRun => return None,
        }

        command.into()
    }
}

#[derive(Debug, Copy, Clone)]
enum Debugger {
    Lldb,
    Gdb,
}

#[derive(Debug)]
struct Options {
    subcommand: Subcommand,
    debug: bool,
    d_int: bool,
    serial: Option<String>,
    launch_debug: Option<Debugger>,
    // these will probably be re-used at some point
    #[allow(dead_code)]
    export: bool,
    #[allow(dead_code)]
    export_path: Option<String>,
    confg_path: String,
    native_dbg_shell: bool,
    daemonize: bool,
    grub_cfg: String,
}

impl Options {
    fn get_args() -> Self {
        let mut opts = getopts::Options::new();
        opts.optflag(
            "d",
            "debug",
            "pauses the the VM on startup with debug enabled on 'localhost:1234'. This option is redundant if `--lldb` or `--gdb` is used",
        );
        opts.optflag("", "display-interrupts", "Display interrupts on stdout");
        opts.optflagopt("s", "serial", "Enables serial output, argument is directly given to qemu via `-serial [FILE]` defaults to stdio ", "FILE");
        opts.optflag("h", "help", "Displays a help message");
        opts.optopt(
            "l",
            "launch-debugger",
            "launches the specified debugger with the kernel binary as first argument",
            "DEBUG",
        );
        opts.optflag("", "gdb", "Runs gdb alongside qemu");
        opts.optflag("", "lldb", "Runs lldb alongside qemu");
        opts.opt(
            "e",
            "export",
            "Deprecated - does nothing. I may re-enable this in the future",
            "OUT_FILE",
            HasArg::Maybe,
            Occur::Optional,
        );
        opts.optflag("", "bios", "Runs kernel using BIOS image");
        opts.opt(
            "",
            "uefi",
            "Boots system using QEMU using UEFI image. Attempts to automatically locate firmware. Cannot be used with --bios",
            "PATH",
            HasArg::Maybe,
            Occur::Optional
        );
        opts.opt(
            "",
            "config",
            "overrides the config used by the runner",
            "PATH",
            HasArg::Yes,
            Occur::Optional,
        );
        opts.opt(
            "n",
            "native-shell",
            "Uses this shell for the debug window.",
            "",
            HasArg::No,
            Occur::Optional,
        );
        opts.opt(
            "",
            "daemonize",
            "Daemoizes all children, exiting the runner",
            "",
            HasArg::No,
            Occur::Optional,
        );
        opts.opt(
            "g",
            "grub-cfg",
            "Overrides the default grub config with the one given in the path",
            "-g PATH",
            HasArg::Yes,
            Occur::Optional
        );

        let matches = match opts.parse(std::env::args_os()) {
            Ok(matches) => {
                if matches.opt_present("help") {
                    println!("{}", opts.usage(BRIEF));
                    std::process::exit(0);
                }

                matches
            }
            Err(Fail::OptionDuplicated(o)) => {
                eprintln!("Expected {o} once");
                std::process::exit(2);
            }

            Err(Fail::UnrecognizedOption(o)) => {
                eprintln!("Argument {o} not recognised");
                eprintln!("{}", opts.usage(BRIEF));
                std::process::exit(2);
            }

            Err(Fail::ArgumentMissing(o)) => {
                eprintln!("Required argument {o} missing");
                std::process::exit(2);
            }
            Err(Fail::OptionMissing(o)) => {
                eprintln!("Required argument {o} missing");
                eprintln!("{}", opts.usage(BRIEF));
                std::process::exit(2);
            }
            Err(Fail::UnexpectedArgument(o)) => {
                eprintln!("Unexpected argument {o} missing");
                std::process::exit(2);
            }
        };

        let subcommand = {
            match (matches.opt_present("uefi"), matches.opt_present("bios")) {
                (true, true) => {
                    eprintln!("Error: Attempted to run multiple run modes");
                    std::process::exit(1);
                }
                (true, false) => Subcommand::Uefi,
                (false, true) => Subcommand::Bios,
                (false, false) => Subcommand::NoRun, // bios may have a use in the future with NoRun
            }
        };

        let serial = {
            if matches.opt_present("s") {
                Some(matches.opt_str("s").unwrap_or(String::new()))
            } else {
                None
            }
        };

        let debug = {
            match (matches.opt_present("gdb"), matches.opt_present("lldb")) {
                (true, true) => {
                    eprintln!("Error: Tried to run gdb and lldb, aborting");
                    std::process::exit(2);
                }
                (true, false) => Some(Debugger::Gdb),
                (false, true) => Some(Debugger::Lldb),
                (false, false) => None,
            }
        };

        let grub_cfg = {
            if matches.opt_present("g") {
                let path = matches.opt_str("g").expect("Opt `g` require argument but `None` was returned");
                match std::fs::read_to_string(path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("Failed to read supplied grub config {e}");
                        std::process::exit(0x25);
                    }
                }
            } else {
                GRUB_DEFAULT_CONFIG.to_string()
            }
        };

        Options {
            subcommand,
            debug: debug.map_or_else(|| matches.opt_present("debug"), |_| true),
            d_int: matches.opt_present("display-interrupts"),
            serial,
            launch_debug: debug,
            export: matches.opt_present("e"),
            export_path: matches.opt_str("e"),
            confg_path: matches
                .opt_str("config")
                .unwrap_or("runcfg.toml".to_string()),
            native_dbg_shell: matches.opt_present("n"),
            daemonize: matches.opt_present("daemonize"),
            grub_cfg
        }
    }

    fn fetch_toml(&self) -> Value {
        if std::path::Path::new(&self.confg_path).exists() {
            let mut f = std::fs::File::open(std::path::Path::new(&self.confg_path))
                .expect(&format!("Failed to open config: {}", self.confg_path));
            let mut s = String::new();
            f.read_to_string(&mut s)
                .expect(&format!("Failed to read config: {}", self.confg_path));

            s.parse::<Value>()
                .expect(&format!("Failed to parse {}", self.confg_path))
        } else {
            eprintln!("Failed to locate config file: {}", self.confg_path);
            std::process::exit(17);
        }
    }

    /// This fn may return a Command for QEMU.  
    fn build_exec(&self, drive: impl AsRef<std::path::Path>,toml: &Value) -> Option<Command> {
        let mut qemu = self.subcommand.build_qemu(drive)?;
        let mut args = Vec::new();

        let mut parse_args = |table| {
            if let Some(Value::Array(arr)) = toml_fast(&toml, table + ".args") {
                for i in arr {
                    if let Value::String(s) = i {
                        args.push(s);
                    }
                }
            }
        };

        parse_args("common".to_string());

        match self.subcommand {
            Subcommand::Bios => {
                parse_args("bios".to_string());
            }

            Subcommand::Uefi => {
                if Subcommand::Uefi == self.subcommand {
                    let bios = if let Some(Value::String(bios)) =
                        toml_fast(toml, "uefi.bios".to_string())
                    {
                        if std::path::Path::new(bios).exists() {
                            bios
                        } else {
                            eprintln!("Failed to locate bios at {}", bios);
                            std::process::exit(4);
                        }
                    } else {
                        eprintln!("Error: Key 'uefi.bios' not found or not string");
                        std::process::exit(0x10);
                    };

                    let vars = if let Some(Value::String(vars)) =
                        toml_fast(toml, "uefi.vars".to_string())
                    {
                        if std::path::Path::new(vars).exists() {
                            Some(vars)
                        } else {
                            eprintln!("Failed to locate vars at {}", vars);
                            std::process::exit(4);
                        }
                    } else {
                        None
                    };

                    parse_args("uefi".to_string());

                    qemu.arg("-drive")
                        .arg(format!("if=pflash,format=raw,file={}", bios));
                    if let Some(vars) = vars {
                        qemu.arg("-drive")
                            .arg(format!("if=pflash,format=raw,file={}", vars));
                    }
                }
            }
            Subcommand::NoRun => return None,
        }

        for i in args {
            qemu.arg(i);
        }

        if self.debug {
            qemu.arg("-S").arg("-s");
        }

        if self.d_int {
            qemu.arg("-d").arg("int");
        }

        if let Some(s) = &self.serial {
            qemu.arg("-serial");
            if s.is_empty() {
                qemu.arg("stdio");
                qemu.stdout(Stdio::inherit());
            } else {
                qemu.arg(s);
            }
        }

        Some(qemu)
    }

    fn run_debug(&self, toml: &Value, kernel: impl AsRef<std::path::Path>) -> Option<Command> {
        let dbg = self.launch_debug?;
        let mut term_args =
            if let Some(Value::Array(args)) = toml_fast(toml, "debug.terminal".to_string()) {
                Some(args.clone())
            } else {
                None
            };

        let dbg_path;
        let mut dbg_args = None;
        match dbg {
            Debugger::Lldb => {
                if let Some(Value::Array(term)) = toml_fast(toml, "debug.lldb.terminal".to_string())
                {
                    term_args = Some(term.clone())
                };

                if let Some(Value::String(path)) = toml_fast(toml, "debug.lldb.path".to_string()) {
                    dbg_path = path.clone()
                } else {
                    dbg_path = "lldb".to_string();
                }

                if let Some(Value::Array(dbg)) = toml_fast(toml, "debug.lldb.args".to_string()) {
                    dbg_args = Some(dbg);
                }
            }
            Debugger::Gdb => {
                if let Some(Value::Array(term)) = toml_fast(toml, "debug.gdb.terminal".to_string())
                {
                    term_args = Some(term.clone())
                };

                if let Some(Value::String(path)) = toml_fast(toml, "debug.gdb.path".to_string()) {
                    dbg_path = path.clone()
                } else {
                    dbg_path = "gdb".to_string();
                }

                if let Some(Value::Array(dbg)) = toml_fast(toml, "debug.gdb.args".to_string()) {
                    dbg_args = Some(dbg);
                } else {
                }
            }
        }

        // tricks next statement into thinking no shell is specified
        let term_args = if self.native_dbg_shell {
            None
        } else {
            term_args
        };

        return if let Some(term) = term_args {
            // if a terminal has been specified
            let mut arg_arr = Vec::new();
            for i in term {
                if let Value::String(s) = i {
                    arg_arr.push(s);
                }
            }

            let mut command = Command::new(&arg_arr[0]);
            for i in &arg_arr[1..] {
                command.arg(i);
            }

            let mut dbg = dbg_path;
            dbg += " ";
            dbg += kernel.as_ref().to_str().unwrap();
            if let Some(dbg_args) = dbg_args {
                for i in dbg_args {
                    if let Value::String(s) = i {
                        (dbg += " ");
                        dbg += s;
                    }
                }
            }

            command.arg(dbg);
            Some(command)
        } else {
            // if a terminal has not been specified
            let mut command = Command::new(dbg_path);
            command.arg(kernel.as_ref().as_os_str());
            if let Some(dbg_args) = dbg_args {
                for i in dbg_args {
                    if let Value::String(s) = i {
                        command.arg(s);
                    }
                }
            }

            Some(command)
        };
    }

    fn build_grub_img(&self, kernel: impl AsRef<std::path::Path>) -> std::path::PathBuf {
        macro_rules! mkdir {
            ($path:expr) => {
                match std::fs::create_dir($path) {
                    Ok(_) => {},
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {},
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(0x60)
                    },
                }
            };
        }

        let tgt: std::path::PathBuf = "target".into();
        mkdir!(tgt.join("img"));
        mkdir!(tgt.join("img/boot"));
        mkdir!(tgt.join("img/boot/grub"));
        let mut cfg = match std::fs::File::create(tgt.join("img/boot/grub/grub.cfg")) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(0x60)
            }
        };
        if let Err(e) = write!(cfg, "{}", self.grub_cfg) {
            eprintln!("Failed to write grub.cfg {e}");
            std::process::exit(0x61)
        };

        match std::fs::copy(kernel,tgt.join("img/boot/hootux")) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("Failed to copy kernel {e}");
                std::process::exit(0x61);
            }
        };

        const IMG_NAME: &str = "grub.img";
        let img = tgt.join(IMG_NAME);
        let mut mkrescue = Command::new("grub-mkrescue");
        mkrescue.args(["-o",IMG_NAME,"img"]);
        mkrescue.current_dir(tgt);
        eprintln!("Running {:?}",mkrescue);
        let mut child = match mkrescue.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Failed to start, returned {e}");
                std::process::exit(0x20)
            }
        };

        match child.wait() {
            Ok(_) => {}
            Err(e) => {
                eprintln!("{mkrescue:?} returned {e}");
                std::process::exit(0x21)
            }
        }
        img
    }
}

fn toml_fast(toml: &Value, path: String) -> Option<&Value> {
    let split = path.split(".");
    let mut path = Vec::new();
    for i in split {
        path.push(i.to_string());
    }

    let mut value = toml;
    for (count, t_name) in path.iter().enumerate() {
        if count == path.len() {
            return Some(value);
        }

        if let Value::Table(table) = value {
            let new_value = table.get(&*t_name)?;
            value = new_value
        } else {
            return None;
        }
    }
    Some(value)
}

fn build_kernel() -> Option<std::path::PathBuf> {
    let mut cargo = Command::new("cargo");
    cargo.current_dir("kernel-bin");
    cargo.arg("build").arg("--message-format=json").arg("--target=x86_64-unknown-none");
    cargo.stdout(Stdio::piped());

    let mut child = cargo.spawn().ok()?;
    let out = child.stdout.take()?;

    for i in cargo_metadata::Message::parse_stream(std::io::BufReader::new(out)) {
        match i {
            Ok(cargo_metadata::Message::CompilerArtifact(m)) => {
                if m.target.name == "hootux-bin" {
                    return m.executable.map(|p| p.into())
                }
            }
            Ok(_) => continue,
            Err(e) => panic!("Failed to read stdout for cargo: {e}"),
        }
    };
    eprintln!("Error: Artifact for `kernel-bin` not found");
    std::process::exit(0x24);
}