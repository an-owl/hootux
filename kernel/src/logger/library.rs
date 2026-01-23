use alloc::collections::BTreeMap;
use alloc::string::ToString;
use alloc::vec::Vec;
use byteyarn::{Yarn, YarnBox};
use spin::RwLock;

pub struct Library {
    pub(super) lib: BTreeMap<u64, RwLock<TaskLogs>>,
    pub(super) kernel_log: RwLock<TaskLogs>,
}

impl Library {
    pub(super) const fn new() -> Self {
        Self {
            lib: BTreeMap::new(),
            kernel_log: TaskLogs::new_const(0, "Hootux"),
        }
    }

    pub(super) fn get_task(&self, task: u64) -> Option<&RwLock<TaskLogs>> {
        self.lib.get(&task)
    }

    /// Creates a new log for the requested task. This should only be used when [Self::lo]
    ///
    /// # Panics
    ///
    /// This fn will panic when `task` is already present.
    pub(super) fn insert_task(&mut self, task: u64, name: &str) {
        self.lib
            .insert(task, TaskLogs::new(task, name))
            .ok_or(())
            .map(|_| ())
            .expect_err("task already exists");
    }

    /// Removes a tasks logs.
    /// The caller should ensure that all the logs have been copied, as the logs will no longer be available.
    pub fn remove_task(&mut self, task: u64) {
        self.lib.remove(&task);
    }

    pub(super) fn poke(&self, task: u64) -> bool {
        self.lib.contains_key(&task)
    }
}

pub(super) struct TaskLogs {
    id: u64,
    name: Yarn,
    logs: Vec<LogEntry>,
}

impl TaskLogs {
    const fn new_const(pid: u64, name: &'static str) -> RwLock<Self> {
        RwLock::new(Self {
            id: pid,
            name: Yarn::new(name),
            logs: Vec::new(),
        })
    }

    fn new(pid: u64, name: &str) -> RwLock<Self> {
        RwLock::new(Self {
            id: pid,
            name: Yarn::copy(name),
            logs: Vec::new(),
        })
    }

    pub(super) fn insert(&mut self, name: &str, record: &log::Record) -> AssociatedLogEntry<'_> {
        if self.name != name {
            self.name = Yarn::copy(name);
        }

        self.logs.push(LogEntry {
            time: crate::time::AbsoluteTime::now(),
            level: record.level(),
            // todo: Create some mechanism to handle deduplicating strings.
            module: Yarn::copy(record.module_path().unwrap_or_default()),
            file: Yarn::copy(record.file().unwrap_or_default()),
            line: record.line().unwrap_or(0),
            payload: Yarn::from_string(record.args().to_string()),
        });

        let Some(rc) = self.get(self.logs.len() - 1) else {
            unreachable!()
        };
        rc
    }

    pub(super) fn get(&self, entry: usize) -> Option<AssociatedLogEntry<'_>> {
        let e = self.logs.get(entry)?;
        Some(AssociatedLogEntry {
            log: self,
            entry: e,
        })
    }
}

pub(super) struct LogEntry {
    time: crate::time::AbsoluteTime,
    level: log::Level,

    module: Yarn,
    file: Yarn,
    line: u32,

    payload: Yarn,
}

pub struct AssociatedLogEntry<'a> {
    log: &'a TaskLogs,
    entry: &'a LogEntry,
}

impl AssociatedLogEntry<'_> {
    pub fn level(&self) -> log::Level {
        self.entry.level
    }
    pub fn module(&self) -> YarnBox<'_, str> {
        self.entry.module.aliased()
    }
    pub fn file(&self) -> YarnBox<'_, str> {
        self.entry.file.aliased()
    }
    pub fn line(&self) -> u32 {
        self.entry.line
    }
    pub fn payload(&self) -> YarnBox<'_, str> {
        self.entry.payload.aliased()
    }
    pub fn time(&self) -> crate::time::AbsoluteTime {
        self.entry.time
    }
    pub fn tid(&self) -> u64 {
        self.log.id
    }
    pub fn name(&self) -> YarnBox<'_, str> {
        self.log.name.aliased()
    }
}

impl core::fmt::Display for AssociatedLogEntry<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::write!(f, "[{:^7}] [{}] ", self.entry.level, self.entry.time,)?;
        if self.log.name == "Hootux" {
            core::write!(f, "{}: ", self.log.name)?;
        } else {
            core::write!(f, "({}){}: ", self.log.id, self.log.name)?;
        }

        core::write!(f, "{}", self.entry.payload)
    }
}

impl core::fmt::Debug for AssociatedLogEntry<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if !f.alternate() {
            write!(
                f,
                "[{level}]<{time}>({pid},{t_name}){{ {file}:{line}@{module} }} {payload}",
                level = self.entry.level,
                time = self.entry.time,
                file = self.entry.file,
                line = self.entry.line,
                pid = self.log.id,
                module = self.entry.module,
                payload = self.entry.payload,
                t_name = self.log.name,
            )
        } else {
            writeln!(f, "[{}]", self.entry.level)?;
            writeln!(f, "PID: ({}) Name: \"{}\"", self.log.id, self.entry.module)?;
            writeln!(
                f,
                "Module {} @ {}:{} ",
                self.entry.module, self.entry.file, self.entry.line
            )?;
            writeln!(f, "Time: {}s", self.entry.time)?;
            writeln!(f, "{}", self.entry.payload)
        }
    }
}
