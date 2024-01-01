use getopts::{Fail, HasArg, Occur};
use std::io::Read;
use std::process::{Command, Stdio};
use toml::value::Value;

const QEMU: &str = "qemu-system-x86_64";

static BRIEF: &str = r#"\
Usage `cargo run -- [OPTIONS]`
"#;

/*
Return codes:
0. Ok
1. Unable to select run mode
2. Argument error
4. Unable to locate firmware
5. Unable to locate qemu
0x1? toml misconfigured
*/

fn main() {
    let opts = Options::get_args();
    opts.export();
    let toml = opts.fetch_toml();

    let mut qemu = if let Some(q) = opts.build_exec(&toml) {
        q
    } else {
        std::process::exit(0)
    };

    let mut children = Vec::new();

    let mut run = || {
        let mut qemu_child = qemu.spawn().unwrap().wait();
        if let Some(mut c) = opts.run_debug(&toml) {
            children.push(c.spawn().unwrap());
        }
    };

    if opts.daemonize {
        let d = daemonize::Daemonize::new()
            .user(&*std::env::var("USER").expect("Who are you people!?: No user"))
            .working_directory(&*std::env::current_dir().unwrap());

        if d.execute().is_child() {
            run()
        }
    } else {
        run()
    }
}

#[non_exhaustive]
#[derive(Eq, PartialEq, Debug)]
enum Subcommand {
    Bios,
    Uefi,
    NoRun,
}

impl Subcommand {
    fn build_qemu(&self) -> Option<Command> {
        let mut command = Command::new(QEMU);

        let uefi_path = env!("UEFI_PATH");
        let bios_path = env!("BIOS_PATH");
        match self {
            Subcommand::Bios => {
                command
                    .arg("-drive")
                    .arg(format!("format=raw,file={bios_path}"));
            }
            Subcommand::Uefi => {
                command
                    .arg("-drive")
                    .arg(format!("format=raw,file={uefi_path}"));
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
    export: bool,
    export_path: Option<String>,
    confg_path: String,
    native_dbg_shell: bool,
    daemonize: bool,
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
            "Exports binary. By default located in `./target`. Cannot be used with --uefi",
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
    fn build_exec(&self, toml: &Value) -> Option<Command> {
        let mut qemu = self.subcommand.build_qemu()?;
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

    /// Runs export operation if export is enabled. Exports to `./` target if no path si specified
    ///
    /// # Panics
    ///
    /// This fn will panic if `std::fs::copy()` fails
    fn export(&self) {
        if self.export {
            if let Some(path) = self.export_path.as_ref() {
                let destination = std::path::PathBuf::from(path);
                let bin = std::path::Path::new(env!("KERNEL_BIN"));

                std::fs::copy(bin, destination)
                    .expect(&*format!("Failed to export file to {}", path));
            } else {
                let path = env!("KERNEL_BIN").to_string();
                let bin = std::path::PathBuf::from(path.clone());

                let mut target = None;
                for p in bin.ancestors() {
                    if p.ends_with("release") || p.ends_with("debug") {
                        target = Some(p)
                    }
                }

                if let Some(p) = target {
                    std::fs::copy(&bin, p).expect(&*format!("Failed to export file to {}", path));
                } else {
                    eprintln!(
                        "Unable to figure out where to export. Please provide path to --export"
                    );
                    eprintln!("Error: failed to export continuing anyway");
                }
            }
        }
    }

    fn run_debug(&self, toml: &Value) -> Option<Command> {
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
            dbg += env!("KERNEL_BIN");
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
            command.arg(env!("KERNEL_BIN"));
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
