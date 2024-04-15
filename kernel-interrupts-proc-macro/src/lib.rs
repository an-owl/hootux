use proc_macro::TokenStream;

mod interrupts;

mod multiboot2;

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


/// Constructs a Multiboot2 header using the given arguments.
/// It does this by defining a header struct containing any tags which are given to the macro.
/// A static is constructed using the constructors given as arguments.
/// Any attributes given will be applied to the static.
///
/// The magic and tail tags will be added implicitly. Adding them manually may cause UB upon startup.
///
/// In order the arguments are
/// 1. The architecture to use
/// 2. Any attributes to be applied to the header static.
/// 3. Multiboot Tag constructors
///
/// This will construct a header with an I386 architecture with a EfiBootServiceHeaderTag, a
/// ConsoleHeaderTag in a section called ".multiboot2" without mangling the name of the static.
/// ``` ignore
/// fn example() {
///     kernel_proc_macro::multiboot2_header!(
///         multiboot2_header::HeaderTagISA::I386,
///         #[link_section = ".multiboot2"]
///         #[no_mangle],
///         multiboot2_header::EfiBootServiceHeaderTag::new(multiboot2_header::HeaderTagFlag::Required),
///         multiboot2_header::ConsoleHeaderTag::new(multiboot2_header::HeaderTagFlag::Optional,multiboot2_header::ConsoleHeaderTagFlags::EgaTextSupported)
///     )
/// }
/// ```
/// note that attributes are not comma seperated but tags are
#[proc_macro]
pub fn multiboot2_header(input: TokenStream) -> TokenStream {
    let p: multiboot2::MultiBootHeaderParser = syn::parse_macro_input!(input);
    p.into()
}