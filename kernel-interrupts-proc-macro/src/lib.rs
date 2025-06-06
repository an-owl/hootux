use proc_macro::TokenStream;
use quote::ToTokens;

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

mod slablike;

/// Uses the syntax
/// `$st:struct_definition  $alloc_ty:path $size:literal $($backing_alloc_path:path , $backing_alloc_constructor:expr)?`
///
/// This defines the struct `$st` verbatim, implementing [core::alloc::Allocator] using a
/// static SlabLike allocator. This is intended to minimize the size of smart pointers where their
/// size is important.
///
/// When `$backing_alloc_path` and `$backing_alloc_constructor` are not given this will fall back to [alloc::alloc::Global]
///
/// `$alloc_ty` and `$size` are passed to the SlabLike allocator as the configured type and slab size.
/// `$size` will always be [usize] regardless of the suffix given.
///
/// A `clean` method will be implemented for the struct which is a call wrapper to `slablike::SlabLike::clean`
#[proc_macro]
pub fn local_slab_allocator(input: TokenStream) -> TokenStream {
    let p: slablike::SlabLikeParser = syn::parse_macro_input!(input);
    let ts2: proc_macro2::TokenStream = p.into();
    ts2.into()
}

mod lazy_prop;

#[proc_macro]
pub fn ok_or_lazy(input: TokenStream) -> TokenStream {
    let ok: lazy_prop::OkOrLazy = syn::parse_macro_input!(input);
    let ts: proc_macro2::TokenStream = ok.into();
    ts.into()
}

mod sysfs_impl;

/// Implements filesystem traits for teh sysfs filesystem root.
#[proc_macro_attribute]
pub fn impl_sysfs_root_traits(_: TokenStream, input: TokenStream) -> TokenStream {
    let t: sysfs_impl::ImplSysFsRoot = syn::parse_macro_input!(input);
    let ts: proc_macro2::TokenStream = t.into();
    ts.into()
}

/// Implements hootux::fs::sysfs::SysfsDir for item-structs.
///
/// This macro defines two helpers, `#[index(_)]` and `#[file(_)]`.
/// These attributes take structured arguments, as `key=value` the valid keys are
///
/// * alias: For `file` only. Sets a filename override, default filename is the field identifier. This field expects an identifier
/// * getter: Optional for `file`, default is `field.clone_file()` Required by `index`. The argument identifier for getters is `name`.
/// This overrides the getter for the field, so types that don't implement file may be used.
/// * keys: Required by `index`, not available to `file`. Used to fetch stored filenames from index. This must return an iterator over [String]
///
/// `file` may be allowed any number of times, `index` may be used zero or one times.
///
/// An index will have the methods `len() -> u64`,`remove(&str) -> Result<(),IoError>`,`store(&str,Box<dyn SysfsFile>) -> Result<(),IoError>` called on them.
/// Using an extension trait is recommended where these are not implemented already is recommended.
#[proc_macro_derive(SysfsDir, attributes(file, index))]
pub fn derive_sysfs_dir(input: TokenStream) -> TokenStream {
    let s: sysfs_impl::SysfsDirDerive = syn::parse_macro_input!(input);
    let ts = quote::quote! {#s};
    ts.into()
}

/// Returns [dyn_cast](https://docs.rs/cast_trait_object/0.1.4/cast_trait_object/) implementations required by `File` trait.
///
/// Requires `use crate::fs::file::*`
#[proc_macro_attribute]
pub fn file(_: TokenStream, input: TokenStream) -> TokenStream {
    let f: file::FileCastImplParser = syn::parse_macro_input!(input);
    quote::quote! {#f}.into()
}

mod file {
    use quote::TokenStreamExt;
    use syn::ItemStruct;
    use syn::parse::ParseStream;

    pub struct FileCastImplParser {
        ty: ItemStruct,
    }

    impl syn::parse::Parse for FileCastImplParser {
        fn parse(input: ParseStream) -> syn::Result<Self> {
            Ok(Self { ty: input.parse()? })
        }
    }

    impl quote::ToTokens for FileCastImplParser {
        fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
            let ident = &self.ty.ident;
            // fixme: potential to optimize later to reduce compile times
            let cast_cfg = [
                quote::quote! {::hootux::fs::file::CastFileToFile},
                quote::quote! {::hootux::fs::file::CastFileToNormalFile},
                quote::quote! {::hootux::fs::file::CastFileToDirectory},
                quote::quote! {::hootux::fs::file::CastFileToFilesystem},
                quote::quote! {::hootux::fs::file::CastFileToFifo},
                quote::quote! {::hootux::fs::file::CastFileToDevice},
            ];

            let verbatim = &self.ty;

            let ts = quote::quote! {

                ::cast_trait_object::impl_dyn_cast!(#ident => #(#cast_cfg),*);

                #verbatim
            };

            tokens.append_all(ts);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn field_struct() {
            let ts = quote::quote! {
                struct Foo {
                    field_a: u8,
                    field_b: u8,
                }
            };

            let f: FileCastImplParser = syn::parse2(ts).unwrap();
        }

        #[test]
        fn tuple_struct() {
            let ts = quote::quote! {
                struct Foo(u8, u8);
            };

            let f: FileCastImplParser = syn::parse2(ts).unwrap();
        }

        #[test]
        fn returns_correct() {
            let ts = quote::quote! {
                struct Foo {
                    field_a: u8,
                    field_b: u8,
                }
            };
            let f: FileCastImplParser = syn::parse2(ts).unwrap();
            println!("{}", quote::quote!(#f).to_string());
        }
    }
}

mod method_call;

/// See `hootux::fs::file::File::method_call` for information.
#[proc_macro]
pub fn impl_method_call(input: TokenStream) -> TokenStream {
    let parsed: method_call::MethodCallParser = syn::parse_macro_input!(input);
    let ts = parsed.to_token_stream();
    ts.into()
}
