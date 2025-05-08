//! This module implements a simple shell within the kernel to assist with debugging.
//!
//! If you're writing a driver feel free to extend this in any way you feel necessary
//! it's not intended to be present in a proper environment.

use crate::fs::{IoError, get_vfs};
use crate::mem::dma::{DmaClaimable, DmaGuard};
use crate::println;
use alloc::boxed::Box;
use alloc::string::ToString;
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
        let mut buffer = vec![0u8; 256];
        loop {
            if self.buffered.is_empty() {
                crate::print!("KSHELL# ")
            }
            // indicates if we should run the command or wait for more data
            let mut exec = false;
            let dma = DmaGuard::new(buffer);
            let (claimed, buff) = dma.claim().unwrap();
            match self.in_fo.read(0, buff).await {
                Ok((returned, len)) => {
                    drop(returned); // drop borrowed we can now reclaim the buffer
                    let t = claimed.unwrap().ok().unwrap().unwrap();
                    // echo
                    // Note: this allocates a string if `t` is not utf8
                    crate::print!("{}", alloc::string::String::from_utf8_lossy(&t[..len]));
                    buffer = t;

                    // We may receive data before the command string has been fully received
                    if buffer.contains(&b'\n') {
                        exec = true;
                    }
                    // If we filled the buffer or the internal buffer contains data
                    if len == buffer.len() || self.buffered.len() != 0 {
                        self.buffered.extend_from_slice(&buffer);
                    }

                    if exec {
                        let b = if self.buffered.len() == 0 {
                            &buffer[..len - 1]
                        } else {
                            &self.buffered[..self.buffered.len() - 1]
                        };
                        if self.parse_input(b).await {
                            return hootux::task::TaskResult::ExitedNormally;
                        }
                        self.buffered.clear();
                    }
                }
                Err((IoError::EndOfFile, out, ..)) => {
                    drop(out);
                    buffer = claimed.unwrap().ok().unwrap().unwrap();
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
    async fn parse_input(&self, buffer: &[u8]) -> bool {
        let args = match str::from_utf8(buffer) {
            Ok(args) => args,
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

trait Command: Send + Sync {
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

enum CommandResult {
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
            if args.starts_with("ls") {
                let Some(p) = args.find("/") else {
                    log::error!("invalid arguments");
                    return CommandResult::Err;
                };
                let path = args.split_at(p).1;
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
                let Ok(iter) = dir.file_list().await else {
                    log::error!("ls: Failed to enumerate files");
                    return CommandResult::Err;
                };
                for i in iter {
                    println!("{i}");
                }
            }
            CommandResult::BadMatch
        }
        .boxed()
    }
}
