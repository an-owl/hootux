#[cfg(feature = "alloc-debug-serial")]
use crate::{serial_print, serial_println};

/// A linked list for reserving memory locations
pub struct RawLinkedList {
    len: usize,
    head: Option<&'static mut RawLinkedListNode>,
}

impl RawLinkedList {
    pub const fn new() -> Self {
        Self { len: 0, head: None }
    }

    pub unsafe fn add(&mut self, ptr: *mut u64) {
        unsafe {
            #[cfg(feature = "alloc-debug-serial")]
            serial_println!(r#"Adding:   {:#x}"#, ptr as usize);

            let old = self.head.take();
            let new = RawLinkedListNode::new(ptr);
            new.next = old;

            self.head = Some(new);

            self.len += 1;
        }
    }

    pub fn is_empty(&self) -> bool {
        return if let Some(_) = self.head { false } else { true };
    }

    /// Fetches the location of the first node of the list
    pub fn fetch(&mut self) -> Option<*mut u8> {
        let ret = self.head.take();

        return if let Some(inner) = ret {
            self.len -= 1;
            self.head = inner.next.take();
            let ret = inner.get_addr() as *mut u8;
            #[cfg(feature = "alloc-debug-serial")]
            serial_println!("Removing: {:#x}", ret as usize);

            Some(ret)
        } else {
            None
        };
    }

    #[cfg(feature = "alloc-debug-serial")]
    pub fn dump(&self) {
        let mut next = self.head.as_ref();
        let mut count = 0;
        if let Some(_) = next {
            serial_println!("Dumping {} nodes", self.len);
        } else {
            serial_println!("List Empty ");
        }
        while let Some(n) = next {
            let addr = n.get_addr() as usize;

            serial_print!("Node {:#x} ", addr);
            if addr == usize::MAX {
                break;
            }
            next = n.next.as_ref();

            if count == self.len {
                serial_print!("Continues");
                break;
            } else {
                count += 1;
            }
        }
        serial_println!("")
    }
}

struct RawLinkedListNode {
    next: Option<&'static mut Self>,
}

#[doc(hidden)]
const _ASSERT_LLN_SIZE: () = assert!(RawLinkedListNode::is_ok());

impl RawLinkedListNode {
    #[doc(hidden)]
    /// This exists to check that `Self` is the size of `u64` and is called by _ASSERT_LLN_SIZE.
    // this is not dead its called at compile time to ensure that self fits within 8 bytes
    #[allow(dead_code)]
    const fn is_ok() -> bool {
        core::mem::size_of::<u64>() == core::mem::size_of::<Self>()
    }

    /// Creates a new node at the given pointer.
    ///
    /// #Safety
    ///
    /// This fn is unsafe because the caller must guarantee that that the address is unused.
    unsafe fn new(ptr: *mut u64) -> &'static mut Self {
        unsafe {
            let p = ptr as *mut Self;
            p.write(Self { next: None });
            &mut *p
        }
    }

    pub fn get_addr(&self) -> usize {
        self as *const Self as usize
    }
}
