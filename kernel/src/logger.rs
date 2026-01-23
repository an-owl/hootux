//! The Hootux logger logs messages using the [log] crate.
//! The logger can set local and global states for tasks which may be set using environment variables "log_level".
//! Supported levels are "trace", "debug", "info", "warn", "error".
//! "logger_serial" and "logger_display" can be set to "true" or "false" to specify which outputs to use.
//!
//! [RecallLog] can be used recall logs

use crate::mem::dma::DmaBuff;
use crate::task::TaskResult;
use crate::task::kernel_scheduler::ContextContainer;
use crate::{cast_file, println};
use alloc::boxed::Box;
use alloc::string::ToString;
use byteyarn::Yarn;
use hootux::serial_println;
use log::{Log, Metadata, Record};
use spin::RwLock;

mod library;

pub(crate) static LOGGER: Logger = Logger::new();

pub(crate) struct Logger {
    inner: RwLock<LoggerInner>,
}

struct LoggerInner {
    serial: bool,
    graphical: bool,
    library: RwLock<library::Library>,
    async_tx: Option<crate::task::util::MessageQueue<DmaBuff, crate::task::util::Sender>>,
}

impl Logger {
    pub(crate) const fn new() -> Self {
        Self {
            inner: RwLock::new(LoggerInner::new()),
        }
    }
}

impl LoggerInner {
    const fn new() -> Self {
        Self {
            serial: true,
            graphical: true,
            library: RwLock::new(library::Library::new()),
            async_tx: None,
        }
    }
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        let logger = self.inner.read();
        if log::max_level() >= metadata.level() && (logger.serial || logger.graphical) {
            true
        } else {
            false
        }
    }

    fn log(&self, record: &Record) {
        let t = ContextContainer::get();
        let state: LocalLoggerState = if let Some(ctx) = t {
            (&ctx).into()
        } else {
            Default::default()
        };

        let inner = self.inner.read();

        let lib = if let Some(id) = state.id {
            let lib = inner.library.upgradeable_read();
            if lib.poke(id) {
                lib.downgrade()
            } else {
                let mut ug = lib.upgrade();
                ug.insert_task(id, state.name);
                ug.downgrade()
            }
        } else {
            inner.library.read()
        };
        let mut tgt_log = if let Some(id) = state.id {
            let Some(logs) = lib.get_task(id) else {
                unreachable!()
            };
            logs.write()
        } else {
            lib.kernel_log.write()
        };

        if state.min_level >= record.level() {
            let log_entry = tgt_log.insert(state.name, record);
            if inner.graphical.min(state.graphical) {
                println!("{}", log_entry);
            }
            if inner.serial.min(state.serial) {
                if crate::runlevel::runlevel() >= crate::runlevel::Runlevel::Kernel {
                    let log_buff = log_entry.to_string() + "\n";
                    let buffer = DmaBuff::from(log_buff);
                    drop(log_entry);
                    drop(tgt_log);
                    drop(lib);
                    if let Some(tx) = inner.async_tx.as_ref() {
                        tx.send(buffer).unwrap();
                    } else {
                        let rx = crate::task::util::MessageQueue::new(1024);
                        drop(inner);
                        let mut wl = self.inner.write();
                        wl.async_tx = Some(rx.sender());
                        let tx = wl.async_tx.as_mut().unwrap();
                        start_logger_worker("/sys/bus/uart/uart0", rx).unwrap();
                        tx.send(buffer).unwrap()
                    };
                } else {
                    serial_println!("{}", log_entry);
                }
            }
        }
    }

    fn flush(&self) {
        //lmao
    }
}

struct LocalLoggerState<'task> {
    id: Option<u64>,
    name: &'task str,
    min_level: log::Level,
    serial: bool,
    graphical: bool,
}

impl From<&ContextContainer> for LocalLoggerState<'_> {
    fn from(container: &ContextContainer) -> Self {
        // SAFETY: We can guarantee that this will not mutate while we maintain a reference to it.
        let ctx = unsafe { &*container.context };

        let min_level = ctx.envs.get("log_level").map_or_else(
            || log::Level::Trace,
            |st| match &**st {
                "trace" => log::Level::Trace,
                "debug" => log::Level::Debug,
                "info" => log::Level::Info,
                "warn" => log::Level::Warn,
                "error" => log::Level::Error,
                _ => log::Level::Trace,
            },
        );

        Self {
            id: Some(ctx.id()),
            name: &ctx.name,

            min_level,
            serial: ctx
                .envs
                .get("logger_serial")
                .map_or_else(|| true, |s| s.parse().unwrap_or(true)),
            graphical: ctx
                .envs
                .get("logger_display")
                .map_or_else(|| true, |s| s.parse().unwrap_or(true)),
        }
    }
}

impl Default for LocalLoggerState<'_> {
    fn default() -> Self {
        LocalLoggerState {
            id: None,
            name: "Hootux",
            min_level: log::Level::Trace,
            serial: true,
            graphical: true,
        }
    }
}

#[macro_export]
macro_rules! set_logger_level {
    ($lvl:expr_2021) => {
        unsafe { log::set_max_level($lvl) }
    };
}

async fn logger_worker(
    file: Box<dyn crate::fs::file::Write<u8> + Sync + Send>,
    pipe: crate::task::util::MessageQueue<DmaBuff, crate::task::util::Receiver>,
) -> TaskResult {
    let t = unsafe { &mut *ContextContainer::get().unwrap().context };
    t.name = alloc::string::String::from("Async Logger");

    loop {
        let t = pipe.next().await;

        match file.write(0, t).await {
            Ok(_) => {} // Do nothing, drop buffer.
            Err(_) => return TaskResult::Error,
        }
    }
}

fn start_logger_worker(
    file: &str,
    pipe: crate::task::util::MessageQueue<DmaBuff, crate::task::util::Receiver>,
) -> Result<(), ()> {
    let file =
        crate::block_on!(core::pin::pin!(crate::fs::get_vfs().open(file))).map_err(|_| ())?;
    let file: Box<dyn crate::fs::file::Write<u8> + Send + Sync> = match cast_file!(crate::fs::file::NormalFile: file)
    {
        Ok(file) => file,
        Err(file) => {
            let Ok(mut fifo) = cast_file!(crate::fs::device::Fifo<u8>: file) else {
                return Err(());
            };
            let _ = fifo.open(crate::fs::device::OpenMode::Write);
            fifo
        }
    };

    crate::task::kernel_scheduler::Task::new(Box::pin(logger_worker(file, pipe))).run();
    Ok(())
}

/// A single log entry recalled from the logger.
pub struct RecallLog {
    pub tid: Option<u64>,
    pub level: log::Level,
    pub time: crate::time::AbsoluteTime,

    pub name: Yarn,
    pub module: Yarn,
    pub filename: Yarn,
    pub line: u32,
    pub message: Yarn,
}

impl RecallLog {
    /// Recall a log from the logger.
    /// This requires a task ID and an entry number.
    pub fn recall(tid: u64, entry: usize) -> Option<Self> {
        let logger = LOGGER.inner.read();
        let lib = logger.library.read();
        let log = lib.get_task(tid)?;
        let read_log = log.read();
        let entry = read_log.get(entry)?;

        Some(Self {
            tid: Some(entry.tid()),
            level: entry.level(),
            time: entry.time(),

            name: entry.name().immortalize(),
            module: entry.module().immortalize(),
            filename: entry.file().immortalize(),
            line: entry.line(),
            message: entry.payload().immortalize(),
        })
    }

    /// Fetches a kernel log entry.
    pub fn get_kernel(entry: usize) -> Option<Self> {
        let logger = LOGGER.inner.read();
        let lib = logger.library.read();
        let log = lib.kernel_log.read();
        let entry = log.get(entry)?;

        Some(Self {
            tid: Some(entry.tid()),
            level: entry.level(),
            time: entry.time(),

            name: entry.name().immortalize(),
            module: entry.module().immortalize(),
            filename: entry.file().immortalize(),
            line: entry.line(),
            message: entry.payload().immortalize(),
        })
    }
}

impl core::fmt::Display for RecallLog {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if !f.alternate() {
            core::write!(f, "[{:^7}] [{}] ", self.level, self.time,)?;
            if self.name == "Hootux" {
                core::write!(f, "{}: ", self.name)?;
            } else {
                core::write!(f, "({}){}: ", self.tid.unwrap(), self.name)?;
            }

            core::write!(f, "{}", self.message)
        } else {
            core::writeln!(f, "----[{:^11}]----", self.level)?;
            if self.name == "Hootux" {
                core::writeln!(f, "{}", self.name)?;
            } else {
                core::writeln!(f, "{1}(tid: {0}): ", self.tid.unwrap(), self.name)?;
            };
            core::writeln!(
                f,
                "module: {} @ {}:{}",
                self.module,
                self.filename,
                self.line
            )?;
            core::writeln!(f, "vv Message vv")?;
            core::writeln!(f, "{}", self.message)?;
            core::writeln!(f, "----End Log Entry----")
        }
    }
}
