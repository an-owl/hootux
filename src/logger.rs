use log::{Log, Metadata, Record};
use spin::RwLock;
use crate::{println, serial_println};

pub(crate) static LOGGER: Logger = Logger::new();

pub(crate) struct Logger{
    inner: RwLock<LoggerInner>
}

struct LoggerInner {
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
            serial: true,
            graphical: true,
        }
    }
}

impl Log for Logger{

    fn enabled(&self, metadata: &Metadata) -> bool {
        let logger = self.inner.read();
        return if log::max_level() >= metadata.level() && (logger.serial || logger.graphical) {
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

#[macro_export]
macro_rules! set_logger_level {
    ($lvl:expr) => {
        unsafe {
            log::set_max_level($lvl)
        }
    }
}
