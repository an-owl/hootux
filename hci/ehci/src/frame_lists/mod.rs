pub mod periodic_list;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TransactionDirection {
    Out,
    In,
}

impl From<bool> for TransactionDirection {
    fn from(value: bool) -> Self {
        match value {
            true => Self::In,
            false => Self::Out,
        }
    }
}

impl From<TransactionDirection> for bool {
    fn from(value: TransactionDirection) -> Self {
        match value {
            TransactionDirection::In => true,
            TransactionDirection::Out => false,
        }
    }
}

pub struct Target {
    pub address: Address,
    pub endpoint: Endpoint,
}

/// Each USB function contains 16 possible endpoints.
/// Endpoint 0 is reserved as the control endpoint, the remaining 15 can be configured depending 
/// on the function of the device.
pub struct Endpoint(u8);

impl Endpoint {
    pub const CONTROL: Self = Self(0);
    
    /// Constructs a new `self`.
    /// 
    /// # Panics
    /// 
    /// This fn will panic if `endpoint >= 16`
    const fn new(endpoint: u8) -> Self {
        assert!(endpoint < 16);
        Self(endpoint)
    }
}

impl From<Endpoint> for u8 {
    fn from(endpoint: Endpoint) -> Self {
        endpoint.0
    }
}

/// Each USB function must be configured by the host with an address ranging from 1 to 63. 
/// This struct represents possible addresses.
/// Address `0` is reserved for broadcasts where all functions are required to respond and cannot
/// be assigned to a function. 
pub struct Address(u8);

impl Address {
    pub const BROADCAST: Self = Self(0);
    
    /// Constructs a new Address.
    /// 
    /// # Panics
    /// 
    /// This fn will panic if `address >= 64`
    const fn new(address: u8) -> Self {
        assert!(address < 64);
        Self(address)
    }
}

impl From<Address> for u8 {
    fn from(address: Address) -> Self {
        address.0
    }
}