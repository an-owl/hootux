use getopts::{Fail, HasArg, Occur};
use std::process::{Command, Stdio};

const QEMU: &str = "qemu-system-x86_64";
const EDK: &str = "/usr/share/edk2/x64/OVMF_CODE.fd";

static BRIEF: &str = r#"\
Usage `cargo run -- [SUBCOMMAND] [OPTIONS]`
Supported subcommands are:
uefi: boots a uefi image using qemu
bios: boots a bios image using qemu\
"#;

/*
Return codes:
0. Ok
1. Unable to select run mode
2. Argument error
4. Unable to locate firmware
5. Unable to locate qemu
*/

fn main() {
    let opts = Options::get_args();
    opts.export();

    let mut qemu = if let Some(q) = opts.build_exec() {
        q
    } else {
        std::process::exit(0)
    };

    let mut children = Vec::new();
    let mut qemu_child = qemu.spawn().unwrap();
    if let Some(mut c) = opts.run_debug() {
        children.push(c.spawn().unwrap());
    }

    qemu_child.wait().unwrap();
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

#[derive(Debug)]
struct Options {
    subcommand: Subcommand,
    debug: bool,
    d_int: bool,
    serial: Option<String>,
    launch_debug: Option<String>,
    debug_args: Option<String>,
    term: Option<String>,
    export: bool,
    export_path: Option<String>,
    bios_path: Option<String>,
}

impl Options {
    fn get_args() -> Self {
        let mut opts = getopts::Options::new();
        opts.optflag(
            "d",
            "debug",
            "pauses the the VM on startup with debug enabled on 'localhost:1234'",
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
        opts.optopt(
            "a",
            "debug-args",
            "specifies debug arguments hnded to the debugger",
            "ARGS",
        );
        opts.optopt("t","terminal", "Sets which terminal window to open when the debugger is enabled (only konsole is supported at the moment)", "TERM");
        opts.opt(
            "e",
            "export",
            "Exports binary. By default located in `./target`. Cannot be used with --uefi",
            "--export [OUT_FILE]",
            HasArg::Maybe,
            Occur::Optional,
        );
        opts.optflag("", "bios", "Runs kernel using BIOS image");
        opts.opt(
            "",
            "uefi",
            "Boots system using QEMU using UEFI image. Attempts to automatically locate firmware. Cannot be used with --bios",
            "--uefi [PATH]",
            HasArg::Maybe,
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

        let (subcommand, bios_path) = {
            match (
                matches.opt_present("uefi"),
                matches.opt_present("bios"),
                matches.opt_str("uefi"),
            ) {
                (true, true, _) => {
                    eprintln!("Error: Attempted to run multiple run modes");
                    std::process::exit(1);
                }
                (true, false, Some(bios)) => (Subcommand::Uefi, Some(bios)),
                (true, false, None) => {
                    if std::path::Path::new(EDK).exists() {
                        (Subcommand::Uefi, Some(EDK.to_string()))
                    } else {
                        eprintln!("Error unable to locate EDK firmware");
                        std::process::exit(4);
                    }
                }
                (false, true, _) => (Subcommand::Bios, None),
                (false, false, bios) => (Subcommand::NoRun, bios), // bios may have a use in the future with NoRun
            }
        };

        let serial = {
            if matches.opt_present("s") {
                Some(matches.opt_str("s").unwrap_or(String::new()))
            } else {
                None
            }
        };

        Options {
            subcommand,
            debug: matches.opt_present("debug"),
            d_int: matches.opt_present("display-interrupts"),
            serial,
            launch_debug: matches.opt_str("launch-debugger"),
            debug_args: matches.opt_str("debug-args"),
            term: matches.opt_str("terminal"),
            export: matches.opt_present("e"),
            export_path: matches.opt_str("e"),
            bios_path,
        }
    }

    /// This fn may return a Command for QEMU.  
    fn build_exec(&self) -> Option<Command> {
        let mut qemu = self.subcommand.build_qemu()?;

        if let Some(path) = &self.bios_path {
            qemu.arg("-bios").arg(path);
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
                        target = Some(p.clone())
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

    fn run_debug(&self) -> Option<Command> {
        let name = self.launch_debug.as_ref()?;
        let term_name = self.term.as_ref().unwrap_or_else(|| {
            eprintln!("debugger enabled without -t specified");
            std::process::exit(3)
        });

        let dbg_args = self.debug_args.as_ref().unwrap_or(&String::new()).clone();

        let term = match &**term_name {
            "konsole" => {
                let mut term = Command::new("konsole");
                term.arg("-e")
                    .arg(format!("{name} {0:} {dbg_args}", env!("KERNEL_BIN")));
                term
            }
            t => {
                eprintln!("Terminal {t} unsupported");
                std::process::exit(4);
            }
        };
        Some(term)
    }
}
