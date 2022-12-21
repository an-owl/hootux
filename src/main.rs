use getopts::Fail;

const QEMU: &str = "qemu-system-x86_64";
const EDK: &str = "/usr/share/edk2/x64/OVMF_CODE.fd";

static BRIEF: &str =
r#"\
Usage `cargo run -- [SUBCOMMAND] [OPTIONS]`
Supported subcommands are:
uefi: boots a uefi image using qemu
bios: boots a bios image using qemu\
"#;

fn main() {

    let opts= get_opts();
    let mut qemu = std::process::Command::new(QEMU);
    opts.qemu_args(&mut qemu);

    let mut c = qemu.spawn().unwrap();
    c.wait().unwrap();
}

#[non_exhaustive]
enum Subcommand{
    Bios,
    Uefi,
}

impl Subcommand {
    fn fetch() -> Self {
        let args: Vec<String> = std::env::args().collect();
        let subcommand = args[1].clone();

        match &*subcommand {
            "bios" => Subcommand::Bios,
            "uefi" => Subcommand::Uefi,

            e => {
                eprintln!(r#"Err command not found "{e} use --help for more info"#);
                std::process::exit(1);
            },
        }
    }
    fn append_args(&self, command: &mut std::process::Command) {
        let uefi_path = env!("UEFI_PATH");
        let bios_path = env!("BIOS_PATH");
        match self {
            Subcommand::Bios => {
                command.arg("-drive").arg(format!("format=raw,file={bios_path}"));
            }
            Subcommand::Uefi => {
                command.arg("-bios").arg(EDK);
                command.arg("-drive").arg(format!("format=raw,file={uefi_path}"));
            }
        }
    }

    fn is_vm(&self) -> bool {
        match self {
            Subcommand::Bios => true,
            Subcommand::Uefi => true,
        }
    }
}

struct Options {
    subcommand: Subcommand,
    debug: bool,
    d_int: bool,
    serial: Option<String>,
}

impl Options {
    fn qemu_args(&self, command: &mut std::process::Command) {

        if self.subcommand.is_vm() {
            self.subcommand.append_args(command);
        } else {
            panic!("Tried to start qemu with non vm subcommand");
        }

        if self.debug {
            command.arg("-S").arg("-s");
        }

        if self.d_int {
            command.arg("-d").arg("int");
        }

        if let Some(s) = &self.serial {
            command.arg("-serial");
            if s.is_empty() {
                command.arg("stdio");
            } else {
                command.arg(s);
            }
        }
    }
}

fn get_opts() -> Options {
    let mut opts = getopts::Options::new();
    opts.optflag("d","debug","pauses the the VM on startup with debug enabled on 'localhost:1234'");
    opts.optflag("","display-interrupts", "Display interrupts on stdout");
    opts.optflagopt("s", "serial", "Enables serial output, argument is directly given to qemu via `-serial [FILE]` defaults to stdio ", "FILE");
    opts.optflag("h","help", "Displays a help message");



    let matches = match opts.parse(std::env::args_os()) {
        Ok(matches) => {
            if matches.opt_present("help") {
                println!("{}",opts.usage(BRIEF));
                std::process::exit(0);
            }

            matches
        }
        Err(Fail::OptionDuplicated(o)) => {
            eprintln!("Expected {o} once");
            std::process::exit(2);
        }

        Err(Fail::UnrecognizedOption(o)) =>  {
            eprintln!("Argument {o} not recognised");
            eprintln!("{}",opts.usage(BRIEF));
            std::process::exit(2);
        }

        Err(Fail::ArgumentMissing(o)) => {
            eprintln!("Required argument {o} missing");
            std::process::exit(2);
        }
        Err(Fail::OptionMissing(o)) => {
            eprintln!("Required argument {o} missing");
            eprintln!("{}",opts.usage(BRIEF));
            std::process::exit(2);

        }
        Err(Fail::UnexpectedArgument(o)) =>  {
            eprintln!("Unexpected argument {o} missing");
            std::process::exit(2);
        }
    };

    let s = Subcommand::fetch();

    Options{
        subcommand: s,
        debug: matches.opt_present("debug"),
        d_int: matches.opt_present("display-interrupts"),
        serial: matches.opt_str("serial"),
    }
}