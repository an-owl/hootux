use proc_macro::TokenStream;

mod interrupts;

/// sets the kernel interrupt configuration by defining a constant containing the
/// first public vector which may be used. A function name must also be given, which will be used
/// to bind interrupt stub handles to their interrupt vectors.
///
/// ```ignore
///
/// kernel_interrupts_proc_macro::interrupt_config!(const COUNT: u32 = 0x20; fn config);
///
/// fn example(idt: x86_64::structures::idt::InterruptDescriptorTable) {
///     config(idt) // config is provided to `interrupt_config!` macro
///                 // interrupts vectors [0x20..] are now bound to interrupt stubs
/// }
/// ```
#[proc_macro]
pub fn interrupt_config(input: TokenStream) -> TokenStream {
    let i: interrupts::InterruptConfig = syn::parse(input).unwrap();
    i.into()

}