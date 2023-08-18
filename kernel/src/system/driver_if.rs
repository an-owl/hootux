use alloc::string::ToString;
use alloc::{boxed::Box, string::String};

pub trait DriverProfile: Send {
    // todo fis link
    /// The [driver if] calls this fn to attempt to start a drier instance for this resource.
    /// If the driver did not start the resource must be returned
    ///
    /// Implementations should check the [core::any::TypeId] of `resource` to check if it is known
    /// by the driver.
    /// If the resource is known it should be downcast and the driver should be started using the resource.
    fn try_start(&self, resource: Box<dyn ResourceId>)
        -> (MatchState, Option<Box<dyn ResourceId>>);

    /// Returns a string used to identify the bus this driver is looking for.
    ///
    /// The returned string should be formatted in the following ways.
    ///
    /// - If the string uses an initialism "PCI","ACPI" etc. the initialism should be used.
    /// - The string should be in camelCase not PascalCase.
    /// - The string should not include version numbers. Incompatible versions should be checked inside [Self::try_start].
    fn bus_name(&self) -> &str;
}

/// This trait is for structs that store and locate system resources that should be made available to drivers.
///
/// When the resource is discovered the driver that discovered it should wrap it however necessary
/// and call pass the resource into the discovery driver. This trait is designed to prevent any
/// limitations it can, to allow for bus/driver specific behaviour wherever possible.
/// A driver which "owns" a resource may share access to the resource.
/// A resource owned by the system may become unavailable before a driver is found.
/// A driver must check that the resource is still available to the system and the resource must
/// provide a mechanism to detect this.
/// Resource specific behaviour should be described by the implementation.
///
/// When a driver exits it should free the resource to back to the system where this is applicable.
/// If a driver encounters an issue it may decide to poison the resource and not free it back to the
/// system to prevent further issues.
///
/// The display implementation should output the type and a simple way to identify the resource.
pub trait ResourceId: core::fmt::Display + Send {
    fn as_any(&self) -> &dyn core::any::Any;

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any;

    /// See [DriverProfile::bus_name]
    fn bus_name(&self) -> &str;
}

#[derive(Eq, PartialEq, Copy, Clone, Debug)]
pub enum MatchState {
    /// The resource bus did not match the requested bus one or was not recognized.
    WrongBus,
    /// The resource did not contain the metadata the driver is requesting.
    NoMatch,
    /// The driver matched the metadata but was rejected fo an unspecified reason. The driver must
    /// free the resource to return this value
    MatchRejected,
    /// The driver matched the device and has taken ownership of the resource
    Success,
}

pub struct DiscoveryDriver {
    inner: spin::Mutex<DiscoveryDriverInner>,
}

struct DiscoveryDriverInner {
    driver_registry: alloc::collections::BTreeMap<String, alloc::vec::Vec<Box<dyn DriverProfile>>>,
    free_resources: alloc::collections::BTreeMap<String, alloc::vec::Vec<Box<dyn ResourceId>>>,
}

impl DiscoveryDriver {
    pub(super) const fn new() -> Self {
        Self {
            inner: spin::Mutex::new(DiscoveryDriverInner {
                driver_registry: alloc::collections::BTreeMap::new(),
                free_resources: alloc::collections::BTreeMap::new(),
            }),
        }
    }

    /// Attempts to bind a driver to `resource` if no driver can be started the resource is registers within `self`.
    pub fn register_resource(&self, resource: Box<dyn ResourceId>) {
        let mut l = self.inner.lock();
        l.register_res(resource);
    }

    /// Registers the driver into self checking if any instances can be started.
    pub fn register_driver(&self, driver: Box<dyn DriverProfile>) {
        let mut l = self.inner.lock();
        l.register_drv(driver);
    }

    /// Returns the buses currently allocated into self.
    pub fn get_busses(&self) -> alloc::vec::Vec<String> {
        let mut arr = alloc::vec::Vec::new();
        let l = self.inner.lock();
        let mut rk = l.driver_registry.keys();
        for i in rk {
            arr.push(i.clone());
        }
        let mut fk = l.free_resources.keys();
        for i in fk {
            arr.push(i.clone());
        }
        arr.dedup();
        arr
    }

    pub fn resources(&self) -> String {
        use core::fmt::Write;
        let l = self.inner.lock();
        let mut s = String::new();
        for (_, b) in &l.free_resources {
            for i in b {
                write!(s, "{i}").unwrap();
            }
        }
        s
    }
}

impl DiscoveryDriverInner {
    /// Registers the driver into self checking if any instances can be started.
    fn register_drv(&mut self, driver: Box<dyn DriverProfile>) {
        self.locate_res(&*driver);
        if let Some(list) = self.driver_registry.get_mut(driver.bus_name().into()) {
            list.push(driver);
        } else {
            let mut n = alloc::vec::Vec::new();
            let b = driver.bus_name().into();
            n.push(driver);
            self.driver_registry.insert(b, n);
        }
    }

    /// Attempts to bind a driver to `resource` if no driver can be started the resource is registers within `self`.
    fn register_res(&mut self, resource: Box<dyn ResourceId>) {
        log::trace!("Registered resource {}", resource);

        if let Some(r) = self.locate_drv(resource) {
            if let Some(l) = self.free_resources.get_mut(r.bus_name().into()) {
                l.push(r);
            } else {
                let mut n = alloc::vec::Vec::new();
                let k = r.bus_name().into();
                n.push(r);
                self.free_resources.insert(k, n);
            }
        }
    }

    /// Attempts to locate a driver for the resource. The resource is consumed if a driver is
    /// started otherwise it is returned.
    fn locate_drv(&self, resource: Box<dyn ResourceId>) -> Option<Box<dyn ResourceId>> {
        let bus: String = resource.bus_name().into();
        let mut res = Some(resource);
        if let Some(list) = self.driver_registry.get(&*bus) {
            for i in list {
                let (s, r) = i.try_start(res.take().unwrap());
                if s == MatchState::Success {
                    assert!(!r.is_none(), "Driver unexpectedly did not consume resource");
                } else {
                    assert!(r.is_some(), "Driver unexpectedly dropped resource");
                    res = r;
                }
            }
        }
        res
    }

    /// Attempts to locate resources for the given driver, removing any used by the driver from the free list.
    fn locate_res(&mut self, drv: &dyn DriverProfile) {
        let bus = drv.bus_name();
        if let Some(list) = self.free_resources.get_mut(bus.into()) {
            // this can probably be optimized better
            let mut sink = alloc::vec::Vec::with_capacity(list.len());

            // This needs ownership of the resource, There is probably a better way of doing this.
            core::mem::swap(&mut sink, list);

            for i in sink {
                let id = format_args!("{i}").to_string();
                let (s, r) = drv.try_start(i);
                if s == MatchState::Success {
                    assert!(!r.is_none(), "Driver unexpectedly did not consume resource");
                    log::trace!("Driver started for {id}");
                } else {
                    list.push(r.expect("Driver unexpectedly dropped resource"));
                }
            }
        }
    }
}
