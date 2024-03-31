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
use syn::Pat;
use syn::PathArguments;
use syn::Type;
use syn::{parse_macro_input, ItemImpl};

#[proc_macro_attribute]
pub fn php_async_impl(
    _: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    match parser(parse_macro_input!(input as ItemImpl)) {
        Ok(parsed) => parsed,
        Err(e) => syn::Error::new(Span::call_site(), e).to_compile_error(),
    }
    .into()
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
                receiver_mutable = r.mutability.is_some();
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
                        if t.path.segments[0].ident.to_string() == "Option" {
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
