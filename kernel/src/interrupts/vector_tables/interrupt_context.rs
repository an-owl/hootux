/// Contains interrupt context information. Variants are used to store and pass arguments to IRQ handlers.
#[non_exhaustive]
#[derive(Copy, Clone, Debug)]
enum InterruptContext {
    Unused,
    Reserved,
    Basic(fn()),
    PciMsi(fn(PciMsiContext), PciMsiContext),
    PciMsiX(fn(PciMsiXContext), PciMsiXContext),
    PciLegacy(fn()),
}

impl InterruptContext {
    fn is_present(&self) -> Option<&self> {
        match self {
            Self::Unused => None,
            Self::Reserved => None,
            s => Some(self),
        }
    }

    fn call(&self) {
        match self {
            InterruptContext::Unused => {}
            InterruptContext::Reserved => {}
            InterruptContext::Basic(f) => f(),
            InterruptContext::PciMsi(f, a) => f(*a),
            InterruptContext::PciMsiX(f, a) => f(*a),
        }
    }
}
