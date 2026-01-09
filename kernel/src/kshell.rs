//! This module implements a simple shell within the kernel to assist with debugging.
//!
//! If you're writing a driver feel free to extend this in any way you feel necessary
//! it's not intended to be present in a proper environment.

use crate::fs::{IoError, get_vfs};
use crate::mem::dma::DmaBuff;
use crate::println;
use crate::task::TaskResult;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use hootux::fs::{device::*, file::*};

pub struct KernelShell {
    in_fo: Box<dyn Fifo<u8>>,
    command_sets: Vec<Box<dyn Command>>,
    buffered: Vec<u8>,
}

impl KernelShell {
    const BUFER_SIZE: usize = 256;
    /// Constructs a new instance of Self using the stream from `input` as the input.
    ///
    /// On failure this will return the error returned when opening `input`
    pub fn new(input_file: &dyn Fifo<u8>) -> Result<KernelShell, IoError> {
        // clone_file must return the same file type
        let mut t = cast_file!(Fifo<u8>: input_file.clone_file()).unwrap();
        t.open(OpenMode::Read)?;

        Ok(Self {
            in_fo: t,
            command_sets: vec![Box::new(BuiltinCommands)],
            buffered: vec![],
        })
    }

    pub fn install_cmd_set(&mut self, cmd: Box<dyn Command>) {
        self.command_sets.push(cmd);
    }

    pub async fn run(mut self) -> hootux::task::TaskResult {
        let mut buffer = DmaBuff::from(vec![0u8; Self::BUFER_SIZE]);
        loop {
            if self.buffered.is_empty() {
                crate::print!("\nKSHELL# ")
            }
            match self.in_fo.read(0, buffer).await {
                Ok((returned, len)) => {
                    // echo
                    // Note: this allocates a string if `t` is not utf8
                    crate::print!("{}", String::from_utf8_lossy(&returned[..len]));
                    buffer = returned;

                    match len {
                        0 => {} // ???
                        1 => {
                            // unbuffered handling.
                            self.buffered.extend_from_slice(&buffer[..len]);
                            if buffer[0] == b'\n' {
                                // if newline char run command
                                if self
                                    .parse_input(&self.buffered[..self.buffered.len() - 1])
                                    .await
                                {
                                    return TaskResult::ExitedNormally;
                                }
                            };
                        }
                        Self::BUFER_SIZE if buffer.last() != Some(&b'\n') => {
                            self.buffered.extend_from_slice(&buffer[..len]);
                        }
                        _ => {
                            let use_buffer = if buffer.last() == Some(&b'\n') {
                                &buffer[..len - 1] // strip trailing newline
                            } else {
                                &buffer[..len]
                            };
                            // buffered run command
                            if self.buffered.len() != 0 {
                                self.buffered.extend_from_slice(&use_buffer);
                                if self
                                    .parse_input(&self.buffered[..self.buffered.len()])
                                    .await
                                {
                                    return TaskResult::ExitedNormally;
                                }
                                self.buffered.clear();
                            } else {
                                if self.parse_input(&use_buffer).await {
                                    return TaskResult::ExitedNormally;
                                }
                            }
                        }
                    }
                }
                Err((IoError::EndOfFile, out, ..)) => {
                    buffer = out;
                    continue;
                }
                Err((err, _, _)) => {
                    log::error!("Kshell: Exiting with error {:?}", err);
                    return hootux::task::TaskResult::Error;
                }
            }
        }
    }

    /// Passes the input to the command-sets.
    ///
    /// If the input string starts with "exit" then this will indicate the shell should stop.
    #[must_use]
    async fn parse_input(&self, buffer: &[u8]) -> bool {
        let args = match str::from_utf8(buffer) {
            // intellij inputs may contain leading whitespace chars this will remove them
            Ok(args) => {
                args.split_at(args.find(|c| !char::is_whitespace(c)).unwrap_or(0))
                    .1
            }
            Err(e) => {
                log::error!("KShell: Failed to parse args into &str {e:?}");
                return false;
            }
        };

        if args.starts_with("exit") {
            return true;
        }

        for i in &self.command_sets {
            match i.execute(args).await {
                CommandResult::Ok | CommandResult::Err => return false,
                CommandResult::BadMatch => continue,
            }
        }
        false
    }
}

pub trait Command: Send + Sync {
    /// This is called by the shell when a command is entered.
    ///
    /// The implementation should attempt to match until the first whitespace character to determine
    /// whether it matches one of its commands. If the requested command does not match one of the
    /// expected commands it must return [CommandResult::BadMatch].
    ///
    /// The shell will iterate over each registered command until one doesnt return [CommandResult::BadMatch].
    ///
    /// Implementations should beware of matching the command string over-eagerly, there is no
    /// safeguard preventing command implementations from matching the same command string.
    fn execute<'a>(&self, args: &'a str) -> BoxFuture<'a, CommandResult>;
}

#[derive(Debug, Clone, Copy, Ord, PartialOrd, Eq, PartialEq)]
pub enum CommandResult {
    /// Command completed without error
    Ok,
    /// Command string did not match this fn
    BadMatch,
    /// Command failed with error
    Err,
}

/// Implements the ls command which prints the files within the specified directory.
struct BuiltinCommands;

impl Command for BuiltinCommands {
    fn execute<'a>(&self, args: &'a str) -> BoxFuture<'a, CommandResult> {
        async move {
            let ls_pat = regex::Regex::new(r#"^\s*(?<cmd>ls) (?<arg>/\S*)"#).unwrap();
            if let Some(captures) = ls_pat.captures(args) {
                if !captures["cmd"].is_empty() {
                    let path = &captures["arg"];
                    let f = match get_vfs().open(path).await {
                        Ok(f) => f,
                        Err(e) => {
                            log::error!("ls: got {e:?} when opening {path}");
                            return CommandResult::Err;
                        }
                    };
                    let Ok(dir) = cast_dir!(f) else {
                        log::error!("ls: file was not a directory");
                        return CommandResult::Err;
                    };

                    let Ok(len) = dir.len().await else {
                        log::error!("ls: Failed to open {path}");
                        return CommandResult::Err;
                    };
                    println!("{path}: {len} files",);
                    let Ok(iter) = dir.file_list().await else {
                        log::error!("ls: Failed to enumerate files");
                        return CommandResult::Err;
                    };
                    for i in iter {
                        println!("{i}");
                    }
                    return CommandResult::Ok;
                }
            }

            let cat_pat = regex::Regex::new(r#"^\s*(?<cmd>cat) (?<arg>/\S*)"#).unwrap();
            if let Some(captures) = cat_pat.captures(args) {
                let path = &captures["arg"];
                let f = match get_vfs().open(path).await {
                    Ok(f) => f,
                    Err(e) => {
                        log::error!("cat: failed to open {path}: {e:?}");
                        return CommandResult::Err;
                    }
                };
                let f_ty = f.file_type();
                let Ok(f) = cast_file!(NormalFile: f) else {
                    log::error!("cat: {path} is {:?}", f_ty);
                    return CommandResult::Err;
                };

                let mut full_buff = Vec::new();
                let mut count = 0;
                loop {
                    let mut partial = Vec::new();
                    const PARTIAL_SIZE: usize = 4096;
                    partial.resize(PARTIAL_SIZE, 0u8);
                    let buffer = DmaBuff::from(partial);
                    match f.read(count, buffer).await {
                        Ok((buff, len)) => {
                            full_buff.extend_from_slice(&buff[..len]);
                            count += len as u64;
                            if len != PARTIAL_SIZE {
                                break;
                            }
                        }
                        Err((IoError::EndOfFile, ..)) => break,
                        Err((e, ..)) => {
                            log::error!("cat: Got {e:?} when reading {path}")
                        }
                    }
                }

                match core::str::from_utf8(&full_buff) {
                    Ok(s) => println!("{}\nformat: utf8\n{}", path, s),
                    Err(_) => println!("{}\nformat: byte hex\n{:?}", path, &*full_buff),
                }
                return CommandResult::Ok;
            }

            let read_pat =
                regex::Regex::new(r#"(?<cmd>read) @(?<pos>[0-9]+) >>(?<len>[0-9]+) (?<path>/\S+)"#)
                    .unwrap();
            if let Some(captures) = read_pat.captures(args) {
                let file = match get_vfs().open(&captures["path"]).await {
                    Ok(f) => f,
                    Err(err) => {
                        log::info!("Read: got {err:?} when opening {}", &captures["path"]);
                        return CommandResult::Err;
                    }
                };
                let file = match file.file_type() {
                    FileType::NormalFile => cast_file!(NormalFile: file).unwrap(),
                    FileType::CharDev => {
                        let mut chardev = cast_file!(Fifo<u8>: file).unwrap();
                        let Ok(_) = chardev
                            .open(OpenMode::Read)
                            .inspect_err(|e| log::error!("failed to open chardev {e:?}"))
                        else {
                            return CommandResult::Err;
                        };
                        let mut buff = Vec::new();

                        let Ok(len) = captures["len"].parse().inspect_err(|e| {
                            log::error!("Read: got invalid len {e:?}");
                        }) else {
                            return CommandResult::BadMatch;
                        };
                        let Ok(pos) = captures["pos"]
                            .parse()
                            .inspect_err(|e| log::error!("Read: got invalid position {e:?}"))
                        else {
                            return CommandResult::BadMatch;
                        };
                        buff.resize(len, 0);
                        match chardev.read(pos, buff.into()).await {
                            Ok((buff, len)) => match str::from_utf8(&buff[..len]) {
                                Ok(s) => println!("{}", s),
                                Err(_) => println!("{:?}", &buff[..len]),
                            },
                            Err((err, ..)) => {
                                log::error!("Failed to read file: {err:?}");
                                return CommandResult::Err;
                            }
                        }
                        return CommandResult::Ok;
                    }
                    f => {
                        log::error!("Read: unsupported file type: {f:?}");
                        return CommandResult::Err;
                    }
                };

                let Ok(len) = captures["len"]
                    .parse()
                    .inspect_err(|_| log::error!("Read: Invalid len {}", &captures["len"]))
                else {
                    return CommandResult::Err;
                };
                let buffer = alloc::vec![0u8;len];
                let Ok(pos) = captures["pos"]
                    .parse()
                    .inspect_err(|_| log::error!("Read: Invalid position {}", &captures["len"]))
                else {
                    return CommandResult::Err;
                };
                let dma = DmaBuff::from(buffer);
                match file.read(pos, dma).await {
                    Ok((dma, read_len)) => {
                        if read_len != len {
                            log::warn!("Read {read_len} bytes expected {len}");
                        }
                        let buff = dma;
                        match str::from_utf8(&buff[..read_len]) {
                            Ok(s) => {
                                println!("{}", s)
                            }
                            Err(_) => {
                                use core::fmt::Write as _;
                                let mut s = String::new();
                                for i in &*buff {
                                    core::write!(s, "{:#x} ", i).unwrap();
                                }
                                println!("{s}");
                            }
                        }
                    }
                    Err((err, ..)) => {
                        log::error!("Got {err:?} when reading {}", &captures["path"])
                    }
                }
            }
            CommandResult::BadMatch
        }
        .boxed()
    }
}
