use log::{Level, LevelFilter, Log, log_enabled, Metadata, Record, set_logger};
use spin::RwLock;
use crate::{println, serial_println};
use crate::kernel_statics::fetch_local;

pub(crate) struct Logger{
    inner: RwLock<LoggerInner>
}

struct LoggerInner {
    level: Level,
    serial: bool,
    graphical: bool,
}

impl Logger {
    pub(crate) const fn new() -> Self {
        Self{inner: RwLock::new(LoggerInner::new())}
    }
}

impl LoggerInner {
    const fn new() -> Self {
        Self{
            level: Level::Info,
            serial: true,
            graphical: true,
        }
    }
}

impl Log for Logger{

    fn enabled(&self, metadata: &Metadata) -> bool {
        let logger = self.inner.read();
        return if logger.level >= metadata.level() && (logger.serial || logger.graphical) {
            true
        } else { false }
    }

    fn log(&self, record: &Record) {
        let logger = self.inner.read();
        if self.enabled(record.metadata()){
            if logger.graphical{
                println!("[{}] {}",record.level(),record.args());
            }
            if logger.serial {
                serial_println!("[{}] {}",record.level(),record.args());
            }
        }
    }

    fn flush(&self) {
        //lmao
    }
}

fn set_level(level: LevelFilter){
    log::set_max_level(level)
}

macro_rules! set_logger_level {
    ($lvl:expr) => {
        unsafe {
            set_level(lvl)
        }
    }
}
