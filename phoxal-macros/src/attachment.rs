//! Reads internal attachment fragments from the single generated API product.

use proc_macro2::Span;

pub(crate) fn fragment(name: &str) -> syn::Result<Option<String>> {
    let Some(out) = std::env::var_os("OUT_DIR") else {
        return Ok(None);
    };
    let path = std::path::Path::new(&out).join("phoxal_api.rs");
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|error| {
        syn::Error::new(
            Span::call_site(),
            format!("cannot read generated API {}: {error}", path.display()),
        )
    })?;
    let file = syn::parse_file(&text)?;
    for item in file.items {
        if let syn::Item::Const(item) = item
            && item.ident == name
        {
            if let syn::Expr::Lit(literal) = *item.expr
                && let syn::Lit::Str(literal) = literal.lit
            {
                return Ok(Some(literal.value()));
            }
            return Err(syn::Error::new(
                Span::call_site(),
                "generated API attachment must be a string literal",
            ));
        }
    }
    Ok(None)
}
