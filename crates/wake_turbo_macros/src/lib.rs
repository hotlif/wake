//! `#[wake::task]` 过程宏（PLAN §2.5.1）。
//!
//! 把一个纯函数登记为 wake_turbo 增量任务：调用时不直接执行函数体，而是计算 `TaskId`、
//! 交由引擎记忆化/按需重算（DESIGN §10.3）。
//!
//! ## 展开约定
//!
//! - 作者写 `fn f(a: Vc<A>, b: Vc<B>) -> R { <体> }`，宏产出对外签名 `fn f(...) -> Vc<R>`：
//!   函数体挪进私有 `__wake_inner_f`（返回 `R`），外层负责算 `TaskId` + 调 `wake_turbo::query`。
//! - **参数必须是简单标识符**（`ident: Type`）；第一版参数应为 `Vc<_>`（参与 `TaskId` 的是其
//!   稳定句柄，而非当前值）。不支持 `self` 接收者与模式解构参数。
//! - 返回类型缺省视为 `()`（`()` 满足 `TaskOutput`）。

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ItemFn, Pat, ReturnType, parse_macro_input};

/// 把一个纯函数登记为 wake_turbo 任务。见模块文档的展开约定。
#[proc_macro_attribute]
pub fn task(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);

    let vis = &func.vis;
    let sig = &func.sig;
    let block = &func.block;
    let fn_name = &sig.ident;
    let generics = &sig.generics;
    let (impl_generics, _ty_generics, where_clause) = generics.split_for_impl();
    let inputs = &sig.inputs;

    // 原返回类型 R（缺省为 `()`）。
    let output_ty: syn::Type = match &sig.output {
        ReturnType::Default => syn::parse_quote!(()),
        ReturnType::Type(_, ty) => (**ty).clone(),
    };

    // 收集参数标识符，并校验形态（不支持 self / 模式解构）。
    let mut arg_idents = Vec::new();
    for input in inputs.iter() {
        match input {
            FnArg::Receiver(recv) => {
                return syn::Error::new_spanned(recv, "#[wake::task] 不支持 self 接收者")
                    .to_compile_error()
                    .into();
            }
            FnArg::Typed(pt) => match &*pt.pat {
                Pat::Ident(pi) => arg_idents.push(pi.ident.clone()),
                other => {
                    return syn::Error::new_spanned(
                        other,
                        "#[wake::task] 参数必须是简单标识符（ident: Type）",
                    )
                    .to_compile_error()
                    .into();
                }
            },
        }
    }

    let inner_ident = format_ident!("__wake_inner_{}", fn_name);

    let expanded = quote! {
        #vis fn #fn_name #impl_generics (#inputs) -> ::wake_turbo::Vc<#output_ty>
        #where_clause
        {
            // 原函数体：真正的计算，返回未包装的 `R`。
            fn #inner_ident #impl_generics (#inputs) -> #output_ty #where_clause #block

            // TaskId = fx_hash(模块路径, 函数名, 参数句柄...)——同参调用全局唯一执行。
            let __wake_id = ::wake_turbo::TaskId::of(
                module_path!(),
                stringify!(#fn_name),
                &[ #( ::wake_turbo::TaskArg::arg_ref(&#arg_idents) ),* ],
            );
            // 交给引擎：登记依赖、幂等注册重算器、确保 green，返回输出句柄。
            ::wake_turbo::query(__wake_id, move || #inner_ident(#( #arg_idents ),*))
        }
    };
    expanded.into()
}
