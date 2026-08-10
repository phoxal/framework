//! Fragment collection and endpoint-only contract-family materialization.

use std::collections::BTreeMap;

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, Token};

use super::model::{MaterializedTree, Node, TopicDef, TopicKind};

mod kw {
    syn::custom_keyword!(fragments);
    syn::custom_keyword!(output);
    syn::custom_keyword!(path);
    syn::custom_keyword!(source);
}

/// The closed set of semantic contract families. A family is a namespace of
/// meaning, not a version: compatibility is the framework train version, so a
/// family root never carries a revision segment.
const FAMILIES: [&str; 3] = ["robot", "runtime", "supervisor"];

#[derive(Clone)]
struct PathSegment {
    name: Ident,
    var: Option<Ident>,
}

#[derive(Clone)]
struct Fragment {
    /// The authored logical path, family root first.
    path: Vec<PathSegment>,
    endpoints: Vec<TopicDef>,
}

impl Parse for Fragment {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        input.parse::<kw::path>()?;
        let mut path = Vec::new();
        loop {
            let name: Ident = input.parse()?;
            let var = if input.peek(syn::token::Paren) {
                let content;
                syn::parenthesized!(content in input);
                let var = content.parse()?;
                if !content.is_empty() {
                    return Err(content.error("a dynamic path segment binds exactly one variable"));
                }
                Some(var)
            } else {
                None
            };
            path.push(PathSegment { name, var });
            if !input.peek(Token![/]) {
                break;
            }
            input.parse::<Token![/]>()?;
        }
        input.parse::<Token![;]>()?;
        validate_family(&path)?;

        let mut endpoints = Vec::new();
        while !input.is_empty() {
            let lookahead = input.fork();
            let attrs = lookahead.call(syn::Attribute::parse_outer)?;
            if endpoint_ahead(&lookahead) {
                if let Some(attr) = attrs.first() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "attributes belong on payload items, not endpoint declarations",
                    ));
                }
                reject_node_topic(input)?;
                endpoints.push(input.parse()?);
                continue;
            }

            return Err(input.error(
                "fragments declare endpoints only; define payload types, implementations, and tests as ordinary sibling Rust items",
            ));
        }
        Ok(Self { path, endpoints })
    }
}

fn endpoint_ahead(input: ParseStream<'_>) -> bool {
    let Ok(prefix) = input.fork().parse::<Ident>() else {
        return false;
    };
    matches!(prefix.to_string().as_str(), "topic" | "command" | "query")
}

fn reject_node_topic(input: ParseStream<'_>) -> syn::Result<()> {
    let lookahead = input.fork();
    let _: Ident = lookahead.parse()?;
    if lookahead.peek(Token![self]) {
        let node: Token![self] = lookahead.parse()?;
        return Err(syn::Error::new_spanned(
            node,
            "fragment endpoints require a named leaf; `self` is available only in protocol trees",
        ));
    }
    Ok(())
}

/// A fragment path roots at one semantic family and owns at least one segment
/// below it. The family is the leading wire-key segment, so it is static: a
/// dynamic segment names an instance, which cannot be a namespace of meaning.
fn validate_family(path: &[PathSegment]) -> syn::Result<()> {
    let root = &path[0];
    if root.var.is_some() {
        return Err(syn::Error::new_spanned(
            &root.name,
            "a contract family root is a static path segment; dynamic segments live below it",
        ));
    }
    if !FAMILIES.contains(&root.name.to_string().as_str()) {
        return Err(syn::Error::new_spanned(
            &root.name,
            format!(
                "a fragment path starts at one semantic contract family: `{}`",
                FAMILIES.join("`, `"),
            ),
        ));
    }
    if path.len() < 2 {
        return Err(syn::Error::new_spanned(
            &root.name,
            "a fragment declares at least one path segment below its contract family",
        ));
    }
    Ok(())
}

/// Parse locally, then stash the original spanned tokens in a path-scoped
/// collector. Cross-fragment semantics deliberately wait for the tree pass.
pub(crate) fn expand_fragment(input: TokenStream) -> syn::Result<TokenStream> {
    let _: Fragment = syn::parse2(input.clone())?;
    Ok(quote! {
        #[doc(hidden)]
        pub(crate) struct __PhoxalApiFragmentMarker;
        #[doc(hidden)]
        pub(crate) trait __PhoxalApiFragmentRegistered { const REGISTERED: (); }
        const _: () = <__PhoxalApiFragmentMarker as __PhoxalApiFragmentRegistered>::REGISTERED;

        macro_rules! __phoxal_api_fragment_collect {
            ($chain:path, $finish:path, ($($tail:path),* $(,)?), ($($collected:tt)*)) => {
                $chain!(
                    $chain,
                    $finish,
                    ($($tail),*),
                    ($($collected)* fragment { #input })
                );
            };
        }
        pub(crate) use __phoxal_api_fragment_collect;
    })
}

struct FragmentList {
    paths: Vec<syn::Path>,
}

impl Parse for FragmentList {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        if input.peek(kw::path) {
            return Err(
                input.error("fragment groups relay registration only and cannot declare a `path`")
            );
        }
        input.parse::<kw::fragments>()?;
        let body;
        syn::braced!(body in input);
        let paths = body
            .parse_terminated(syn::Path::parse_mod_style, Token![;])?
            .into_iter()
            .collect();
        if !input.is_empty() {
            return Err(input.error("a fragment group contains only `fragments { ... }`"));
        }
        Ok(Self { paths })
    }
}

pub(crate) fn expand_fragment_group(input: TokenStream) -> syn::Result<TokenStream> {
    let group: FragmentList = syn::parse2(input)?;
    let collectors: Vec<TokenStream> = group
        .paths
        .iter()
        .map(|path| quote! { #path::__phoxal_api_fragment_collect })
        .collect();
    let registrations = group.paths.iter().map(|path| {
        quote! {
            impl #path::__PhoxalApiFragmentRegistered for #path::__PhoxalApiFragmentMarker {
                const REGISTERED: () = ();
            }
        }
    });
    let Some((first, tail)) = collectors.split_first() else {
        return Err(syn::Error::new(
            Span::call_site(),
            "a fragment group must register at least one child",
        ));
    };
    Ok(quote! {
        #(#registrations)*

        macro_rules! __phoxal_api_group_chain {
            ($chain:path, $finish:path, (), ($($collected:tt)*)) => {
                $finish! { $($collected)* }
            };
            ($chain:path, $finish:path, ($head:path $(, $tail:path)* $(,)?), ($($collected:tt)*)) => {
                $head!($chain, $finish, ($($tail),*), ($($collected)*));
            };
        }

        #first!(
            __phoxal_api_group_chain,
            ::phoxal_macros::__phoxal_api_define_group,
            (#(#tail),*),
            ()
        );
    })
}

/// Freeze the already-collected child tokens into one path-scoped collector.
/// Child paths were resolved in the group module, so the root never has to
/// reconstruct a filesystem/module prefix.
pub(crate) fn expand_group_collector(input: TokenStream) -> TokenStream {
    quote! {
        #[doc(hidden)]
        pub(crate) struct __PhoxalApiFragmentMarker;
        #[doc(hidden)]
        pub(crate) trait __PhoxalApiFragmentRegistered { const REGISTERED: (); }
        const _: () = <__PhoxalApiFragmentMarker as __PhoxalApiFragmentRegistered>::REGISTERED;

        macro_rules! __phoxal_api_fragment_collect {
            ($chain:path, $finish:path, ($($tail:path),* $(,)?), ($($collected:tt)*)) => {
                $chain!(
                    $chain,
                    $finish,
                    ($($tail),*),
                    ($($collected)* #input)
                );
            };
        }
        pub(crate) use __phoxal_api_fragment_collect;
    }
}

struct TreeInput {
    output: Ident,
    source: syn::Path,
    fragments: Vec<syn::Path>,
}

impl Parse for TreeInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        input.parse::<kw::output>()?;
        let output = input.parse()?;
        input.parse::<Token![;]>()?;

        input.parse::<kw::source>()?;
        let source = input.parse()?;
        input.parse::<Token![;]>()?;

        let fragments: FragmentList = input.parse()?;
        if !input.is_empty() {
            return Err(input.error("unexpected tokens after the root fragment list"));
        }
        Ok(Self {
            output,
            source,
            fragments: fragments.paths,
        })
    }
}

pub(crate) fn expand_tree(input: TokenStream) -> syn::Result<TokenStream> {
    let tree: TreeInput = syn::parse2(input.clone())?;
    let collectors: Vec<TokenStream> = tree
        .fragments
        .iter()
        .map(|path| quote! { #path::__phoxal_api_fragment_collect })
        .collect();
    let registrations = tree.fragments.iter().map(|path| {
        quote! {
            impl #path::__PhoxalApiFragmentRegistered for #path::__PhoxalApiFragmentMarker {
                const REGISTERED: () = ();
            }
        }
    });
    let Some((first, tail)) = collectors.split_first() else {
        return Err(syn::Error::new(
            Span::call_site(),
            "list at least one contract fragment",
        ));
    };

    Ok(quote! {
        #(#registrations)*

        macro_rules! __phoxal_api_collect_chain {
            ($chain:path, $finish:path, (), ($($collected:tt)*)) => {
                $finish! { tree { #input } collected { $($collected)* } }
            };
            ($chain:path, $finish:path, ($head:path $(, $tail:path)* $(,)?), ($($collected:tt)*)) => {
                $head!($chain, $finish, ($($tail),*), ($($collected)*));
            };
        }

        #first!(
            __phoxal_api_collect_chain,
            ::phoxal_macros::__phoxal_api_materialize,
            (#(#tail),*),
            ()
        );
    })
}

struct MaterializeInput {
    tree: TreeInput,
    fragments: Vec<Fragment>,
}

impl Parse for MaterializeInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let tree: Ident = input.parse()?;
        if tree != "tree" {
            return Err(syn::Error::new_spanned(
                tree,
                "expected collected tree declaration",
            ));
        }
        let tree_body;
        syn::braced!(tree_body in input);
        let tree = tree_body.parse()?;

        let collected: Ident = input.parse()?;
        if collected != "collected" {
            return Err(syn::Error::new_spanned(
                collected,
                "expected collected fragments",
            ));
        }
        let body;
        syn::braced!(body in input);
        let mut fragments = Vec::new();
        while !body.is_empty() {
            let fragment: Ident = body.parse()?;
            if fragment != "fragment" {
                return Err(syn::Error::new_spanned(
                    fragment,
                    "expected a collected fragment",
                ));
            }
            let fragment_body;
            syn::braced!(fragment_body in body);
            fragments.push(fragment_body.parse()?);
        }
        Ok(Self { tree, fragments })
    }
}

/// One logical path below a family root and the endpoints declared at it.
/// Endpoints are keyed by name so emission order is authoring-order
/// independent.
struct PathContract {
    path: Vec<PathSegment>,
    endpoints: BTreeMap<String, TopicDef>,
}

pub(crate) fn expand_materialized(input: TokenStream) -> syn::Result<TokenStream> {
    let input: MaterializeInput = syn::parse2(input)?;
    let source = &input.tree.source;
    let mut owners = BTreeMap::<String, (Vec<PathSegment>, BTreeMap<String, TopicDef>)>::new();
    for fragment in input.fragments {
        let path_name = path_key(&fragment.path);
        if let Some((first_path, _)) = owners.get(&path_name) {
            let mut error = syn::Error::new(
                fragment.path[0].name.span(),
                format!("duplicate fragment ownership for `{path_name}`"),
            );
            error.combine(syn::Error::new(
                first_path[0].name.span(),
                "first owner is here",
            ));
            return Err(error);
        }
        let mut endpoints = BTreeMap::<String, TopicDef>::new();
        for mut endpoint in fragment.endpoints {
            qualify_endpoint_payloads(&mut endpoint, source, &fragment.path)?;
            let ident = endpoint.leaf.method_ident();
            let name = ident.to_string();
            if let Some(first) = endpoints.insert(name.clone(), endpoint) {
                let mut error = syn::Error::new_spanned(
                    ident,
                    format!("endpoint `{name}` is declared more than once in this fragment"),
                );
                error.combine(syn::Error::new_spanned(
                    first.leaf.method_ident(),
                    "first declaration is here",
                ));
                return Err(error);
            }
        }
        owners.insert(path_name, (fragment.path, endpoints));
    }

    let mut families = BTreeMap::<String, (Ident, Vec<PathContract>)>::new();
    for (path, endpoints) in owners.into_values() {
        // `validate_family` already rejected a path that is empty or is nothing
        // but its family root, so a family and a non-empty remainder both exist.
        let Some((family, below)) = path.split_first() else {
            continue;
        };
        families
            .entry(family.name.to_string())
            .or_insert_with(|| (family.name.clone(), Vec::new()))
            .1
            .push(PathContract {
                path: below.to_vec(),
                endpoints,
            });
    }

    let mut trees = Vec::new();
    for (name, (module, contracts)) in families {
        let nodes = build_nodes(contracts)?;
        trees.push(MaterializedTree {
            module,
            doc: format!("The `{name}` contract family."),
            id: name,
            source: Some(source.clone()),
            nodes,
        });
    }
    let generated = super::expand_trees(&trees);
    let output = input.tree.output;
    Ok(quote! { pub mod #output { #generated } })
}

/// Resolve an endpoint's local payload names against the fragment's own module
/// path: the tree `source` prefix plus every authored path segment, family root
/// included.
fn qualify_endpoint_payloads(
    endpoint: &mut TopicDef,
    source: &syn::Path,
    logical_path: &[PathSegment],
) -> syn::Result<()> {
    for body in endpoint_body_paths_mut(endpoint) {
        let path = &body.path;
        if path.leading_colon.is_some()
            || path.segments.len() != 1
            || !matches!(path.segments[0].arguments, syn::PathArguments::None)
        {
            return Err(syn::Error::new_spanned(
                path,
                "endpoint payloads use a local type name; define that type beside this fragment",
            ));
        }
        let leaf = path.segments[0].ident.clone();
        let mut qualified = source.clone();
        for segment in logical_path {
            qualified
                .segments
                .push(syn::PathSegment::from(segment.name.clone()));
        }
        qualified.segments.push(syn::PathSegment::from(leaf));
        body.path = qualified;
    }
    Ok(())
}

fn endpoint_body_paths_mut(endpoint: &mut TopicDef) -> Vec<&mut super::model::BodyPath> {
    match &mut endpoint.kind {
        TopicKind::PubSub(body) => vec![body],
        TopicKind::Query { request, response } => vec![request, response],
    }
}

fn build_nodes(contracts: impl IntoIterator<Item = PathContract>) -> syn::Result<Vec<Node>> {
    let mut roots = Vec::new();
    for contract in contracts {
        insert_path(
            &mut roots,
            &contract.path,
            contract.endpoints.into_values().collect(),
        )?;
    }
    Ok(roots)
}

fn insert_path(
    nodes: &mut Vec<Node>,
    path: &[PathSegment],
    endpoints: Vec<TopicDef>,
) -> syn::Result<()> {
    let Some((head, tail)) = path.split_first() else {
        return Err(syn::Error::new(
            Span::call_site(),
            "fragment path cannot be empty",
        ));
    };
    let index = if let Some(index) = nodes.iter().position(|node| node.name == head.name) {
        index
    } else {
        nodes.push(Node {
            name: head.name.clone(),
            var: head.var.clone(),
            topics: Vec::new(),
            children: Vec::new(),
        });
        nodes.len() - 1
    };
    if nodes[index].var.as_ref().map(ToString::to_string)
        != head.var.as_ref().map(ToString::to_string)
    {
        return Err(syn::Error::new_spanned(
            &head.name,
            "dynamic path binding is inconsistent",
        ));
    }
    if tail.is_empty() {
        nodes[index].topics = endpoints;
    } else {
        insert_path(&mut nodes[index].children, tail, endpoints)?;
    }
    Ok(())
}

fn path_key(path: &[PathSegment]) -> String {
    path.iter()
        .map(|segment| match &segment.var {
            Some(var) => format!("{}({var})", segment.name),
            None => segment.name.to_string(),
        })
        .collect::<Vec<_>>()
        .join("/")
}
