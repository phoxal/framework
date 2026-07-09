//! Detects whether an impl's `#[setup]` method belongs to the OLD
//! (`Result<Self>`) or NEW (`Result<(Self, Self::Api)>`) participant
//! authoring model, so `#[phoxal::behavior]` (`runtime_impl::expand`) can
//! dispatch to the matching codegen - old participants keep compiling
//! unchanged (`runtime_impl.rs`), new ones get the `Self::Api`-threaded
//! lifecycle (`behavior_v2.rs`). This is a pure peek: it does not validate
//! the rest of the signature (each codegen path does its own full
//! validation with its own error messages) and never mutates the impl.

use syn::{GenericArgument, ImplItem, ItemImpl, PathArguments, ReturnType, Type};

/// `true` iff the impl's `#[setup]` method returns `Result<(A, B)>`-shaped
/// (a 2-tuple inside a single-generic `Result`-named return) - the NEW
/// model's `Result<(Self, Self::Api)>`. `false` for the OLD `Result<Self>`
/// shape, any other shape, or when no `#[setup]` method is present (the OLD
/// path's existing "a participant impl must declare exactly one #[setup]"
/// error already covers that case, so this function only needs to peek).
pub fn is_new_model(item_impl: &ItemImpl) -> bool {
    for item in &item_impl.items {
        let ImplItem::Fn(method) = item else { continue };
        let is_setup = method
            .attrs
            .iter()
            .any(|attr| attr.path().is_ident("setup"));
        if !is_setup {
            continue;
        }
        return returns_tuple(&method.sig.output);
    }
    false
}

fn returns_tuple(output: &ReturnType) -> bool {
    let ReturnType::Type(_, ty) = output else {
        return false;
    };
    let Type::Path(path) = ty.as_ref() else {
        return false;
    };
    let Some(seg) = path.path.segments.last() else {
        return false;
    };
    if seg.ident != "Result" {
        return false;
    }
    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return false;
    };
    matches!(
        args.args.first(),
        Some(GenericArgument::Type(Type::Tuple(_)))
    )
}
