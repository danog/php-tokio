// Copyright 2023-2024 Daniil Gentili
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use anyhow::{bail, Result};
use proc_macro2::Span;
use proc_macro2::TokenStream;
use quote::TokenStreamExt;
use quote::{quote, ToTokens};
use syn::parse_quote;
use syn::FnArg;
use syn::GenericArgument;
use syn::ImplItemFn;
use syn::ItemImpl;
use syn::Pat;
use syn::PathArguments;
use syn::ReceiverKind;
use syn::Type;

#[proc_macro_attribute]
pub fn php_async_impl(
    _: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    expand(input.into()).into()
}

fn expand(input: TokenStream) -> TokenStream {
    match syn::parse2::<ItemImpl>(input) {
        Ok(input) => parser(input)
            .unwrap_or_else(|e| syn::Error::new(Span::call_site(), e).to_compile_error()),
        Err(e) => e.to_compile_error(),
    }
}

fn parser(input: ItemImpl) -> Result<TokenStream> {
    let ItemImpl { self_ty, items, .. } = input;

    if input.trait_.is_some() {
        bail!("This macro cannot be used on trait implementations.");
    }

    let tokens = items
        .into_iter()
        .map(|item| {
            Ok(match item {
                syn::ImplItem::Fn(method) => handle_method(method)?,
                item => item.to_token_stream(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let output = quote! {
        #[::ext_php_rs::php_impl]
        impl #self_ty {
            #(#tokens)*
        }
    };

    Ok(output)
}

fn handle_method(input: ImplItemFn) -> Result<TokenStream> {
    let mut receiver = false;
    let mut receiver_mutable = false;
    let mut hack_tokens = quote! {};
    for arg in input.sig.inputs.iter() {
        match arg {
            FnArg::Receiver(r) => {
                receiver = true;
                // The receiver is borrowed for the suspension: `self` itself can't be.
                match &r.kind {
                    ReceiverKind::Reference(_, _, mutability) => {
                        receiver_mutable = mutability.is_some();
                    }
                    _ if input.sig.asyncness.is_some() => {
                        bail!("Async methods must take &self or &mut self.")
                    }
                    _ => {}
                }
            }
            FnArg::Typed(ty) => {
                let mut this = false;
                for attr in ty.attrs.iter() {
                    if attr.path().to_token_stream().to_string() == "this" {
                        this = true;
                    }
                }

                if !this {
                    let param = match &*ty.pat {
                        Pat::Ident(pat) => &pat.ident,
                        _ => bail!("Invalid parameter type."),
                    };

                    let mut ty_inner = &*ty.ty;
                    let mut is_option = false;

                    if let Type::Path(t) = ty_inner {
                        if t.path.segments[0].ident == "Option" {
                            if let PathArguments::AngleBracketed(t) = &t.path.segments[0].arguments
                            {
                                if let GenericArgument::Type(t) = &t.args[0] {
                                    ty_inner = t;
                                    is_option = true;
                                }
                            }
                        }
                    }
                    let mut is_str = false;
                    if let Type::Reference(t) = ty_inner {
                        if t.mutability.is_none() {
                            if let Type::Path(t) = &*t.elem {
                                is_str = t.path.is_ident("str");
                            }
                        }
                        hack_tokens.append_all(if is_str {
                            if is_option {
                                quote! { let #param = #param.and_then(|__temp| Some(unsafe { ::core::mem::transmute::<&str, &'static str>(__temp) })); }
                            } else {
                                quote! { let #param = unsafe { ::core::mem::transmute::<&str, &'static str>(#param) }; }
                            }
                        } else {
                            if is_option {
                                quote! { let #param = #param.and_then(|__temp| Some(unsafe { ::php_tokio::borrow_unchecked::borrow_unchecked(__temp) })); }
                            } else {
                                quote! { let #param = unsafe { ::php_tokio::borrow_unchecked::borrow_unchecked(#param) }; }
                            }
                        });
                    }
                }
            }
        }
    }

    let mut input = input.clone();
    if input.sig.asyncness.is_some() {
        input.sig.asyncness = None;
        let stmts = input.block;
        let this = if receiver {
            if receiver_mutable {
                quote! { let this = unsafe { std::mem::transmute::<&mut Self, &'static mut Self>(self) }; }
            } else {
                quote! { let this = unsafe { std::mem::transmute::<&Self, &'static Self>(self) }; }
            }
        } else {
            quote! {}
        };
        input.block = parse_quote! {{
            #this
            #hack_tokens

            ::php_tokio::EventLoop::suspend_on(async move #stmts)
        }};
    }

    let result = quote! {
        #input
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::expand;
    use proc_macro2::TokenStream;
    use quote::quote;

    #[track_caller]
    fn assert_expands(input: TokenStream, expected: TokenStream) {
        assert_eq!(expand(input).to_string(), expected.to_string());
    }

    #[test]
    fn async_methods_suspend_on_their_body() {
        assert_expands(
            quote! {
                impl Native {
                    const C: u8 = 1;
                    pub fn sync(a: &str) -> u8 { 1 }
                    pub async fn plain(ms: u64, n: Option<u64>, v: Vec<u8>) -> u64 { ms }
                }
            },
            quote! {
                #[::ext_php_rs::php_impl]
                impl Native {
                    const C: u8 = 1;
                    pub fn sync(a: &str) -> u8 { 1 }
                    pub fn plain(ms: u64, n: Option<u64>, v: Vec<u8>) -> u64 {
                        ::php_tokio::EventLoop::suspend_on(async move { ms })
                    }
                }
            },
        );
    }

    #[test]
    fn receivers_are_borrowed_for_the_suspension() {
        assert_expands(
            quote! {
                impl Native {
                    async fn shared(&self) {}
                    async fn exclusive(&mut self) {}
                }
            },
            quote! {
                #[::ext_php_rs::php_impl]
                impl Native {
                    fn shared(&self) {
                        let this = unsafe { std::mem::transmute::<&Self, &'static Self>(self) };
                        ::php_tokio::EventLoop::suspend_on(async move {})
                    }
                    fn exclusive(&mut self) {
                        let this = unsafe { std::mem::transmute::<&mut Self, &'static mut Self>(self) };
                        ::php_tokio::EventLoop::suspend_on(async move {})
                    }
                }
            },
        );
    }

    #[test]
    fn references_are_borrowed_for_the_suspension() {
        assert_expands(
            quote! {
                impl Native {
                    async fn args(
                        s: &str,
                        os: Option<&str>,
                        b: &[u8],
                        ob: Option<&[u8]>,
                        m: &mut str,
                        r: &&str,
                        #[this] t: &Native,
                        #[allow(unused)] u: &str,
                        o: Option,
                        l: Option<'a>,
                    ) {}
                }
            },
            quote! {
                #[::ext_php_rs::php_impl]
                impl Native {
                    fn args(
                        s: &str,
                        os: Option<&str>,
                        b: &[u8],
                        ob: Option<&[u8]>,
                        m: &mut str,
                        r: & &str,
                        #[this] t: &Native,
                        #[allow(unused)] u: &str,
                        o: Option,
                        l: Option<'a>,
                    ) {
                        let s = unsafe { ::core::mem::transmute::<&str, &'static str>(s) };
                        let os = os.and_then(|__temp| Some(unsafe { ::core::mem::transmute::<&str, &'static str>(__temp) }));
                        let b = unsafe { ::php_tokio::borrow_unchecked::borrow_unchecked(b) };
                        let ob = ob.and_then(|__temp| Some(unsafe { ::php_tokio::borrow_unchecked::borrow_unchecked(__temp) }));
                        let m = unsafe { ::php_tokio::borrow_unchecked::borrow_unchecked(m) };
                        let r = unsafe { ::php_tokio::borrow_unchecked::borrow_unchecked(r) };
                        let u = unsafe { ::core::mem::transmute::<&str, &'static str>(u) };
                        ::php_tokio::EventLoop::suspend_on(async move {})
                    }
                }
            },
        );
    }

    #[test]
    fn errors_become_compile_errors() {
        let error = |message: &str| quote! { ::core::compile_error! { #message } }.to_string();
        assert_eq!(
            expand(quote! { impl Trait for Native {} }).to_string(),
            error("This macro cannot be used on trait implementations.")
        );
        assert_eq!(
            expand(quote! { impl Native { async fn f((a, b): (u8, u8)) {} } }).to_string(),
            error("Invalid parameter type.")
        );
        for receiver in [quote! { self }, quote! { self: Box<Self> }] {
            assert_eq!(
                expand(quote! { impl Native { async fn f(#receiver) {} } }).to_string(),
                error("Async methods must take &self or &mut self.")
            );
            // Sync methods are left alone.
            assert_eq!(
                expand(quote! { impl Native { fn f(#receiver) {} } }).to_string(),
                quote! { #[::ext_php_rs::php_impl] impl Native { fn f(#receiver) {} } }.to_string()
            );
        }
        assert_eq!(
            expand(quote! { fn f() {} }).to_string(),
            error("expected `impl`")
        );
    }
}
