use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, parse_macro_input};

/// Registers a bare-metal test function to be automatically collected and run
/// by `kernel::testing::run_all_tests()`.
///
/// Usage:
/// ```ignore
/// #[test_case]
/// fn my_test() {
///     assert_eq!(1, 1);
/// }
/// ```
#[proc_macro_attribute]
pub fn test_case(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    let fn_name = &func.sig.ident;
    let static_name = format_ident!("__KTEST_{}", fn_name.to_string().to_uppercase());

    quote! {
        #func

        #[::linkme::distributed_slice(::kernel::testing::KERNEL_TESTS)]
        #[allow(non_upper_case_globals)]
        static #static_name: ::kernel::testing::KernelTest = ::kernel::testing::KernelTest {
            name: stringify!(#fn_name),
            run: #fn_name,
        };
    }
    .into()
}
