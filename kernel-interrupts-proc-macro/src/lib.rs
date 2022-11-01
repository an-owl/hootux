//! This crate contain procedural macros for hootux

use proc_macro::TokenStream;
use syn::__private::quote::format_ident;
use quote::quote;
use syn::__private::TokenStream2;
use syn::Lit;

const MAX_INT_VECTOR: u32 = 255;


/// This macro is for generating interrupt stub functions starting at the number given and ending at [MAX_VECTOR].
/// The code generated for each fn is as follows
///
/// ``` ignore
/// extern "x86_interrupt" fn __proc_interrupt_stub_fn_vector_#interrupt_vec_num() {
///     kernel_statics::fetch_local().interrupt_log.log(#interrupt_vec_num);
///     if let Some(f) = *vector_tables::IHR.get(#interrupt_vec_num).read() {
///         f();
///     } else {
///         warn!("Unhandled interrupt at vector {}", #interrupt_vec_num)
///     }
/// }
/// ```
///
/// The stub fn will log the interrupt
#[proc_macro]
pub fn gen_interrupt_stubs(start: TokenStream) -> TokenStream {
    let p: Lit = syn::parse(start).unwrap();

    if let Lit::Int(int) = p {
        let num: u32 = int.base10_digits().parse().unwrap();
        gen_interrupt_stubs_inner(num).into()
    } else { panic!() }
}

fn gen_interrupt_stubs_inner(num: u32) -> TokenStream2 {
    if num > MAX_INT_VECTOR {
        return quote!();
    }

    let byte = num as u8;

    let next = gen_interrupt_stubs_inner(num+1);

    let name = format_ident!("__proc_interrupt_stub_fn_vector_{num:}");

    let this = quote!(
        extern "x86-interrupt" fn #name(_sf: InterruptStackFrame) {

            unsafe {
            crate::interrupts::vector_tables::INT_LOG.log(#byte);
            }

            if let Some(f) = *vector_tables::IHR.get(#byte).read() {
                f();
            } else {
                warn!("Unhandled interrupt at vector {}", #byte)
            }
        }
    );

    quote!(
        #this
        #next
    )
}

#[proc_macro]
pub fn set_idt_entries(start: TokenStream) -> TokenStream {
    let p: Lit = syn::parse(start).unwrap();

    if let Lit::Int(int) = p {
        let num: u32 = int.base10_digits().parse().unwrap();
        gen_idt_load(num).into()
    } else { panic!() }
}

fn gen_idt_load(num: u32) -> TokenStream2 {
    if num >= MAX_INT_VECTOR {
        return quote!();
    }

    let t = num as usize;

    let name = format_ident!("__proc_interrupt_stub_fn_vector_{num:}");

    let next = gen_idt_load(num+1);
    let this = quote!(
        idt[#t].set_handler_fn(#name);
    );

    quote!(
        #this
        #next
    )
}