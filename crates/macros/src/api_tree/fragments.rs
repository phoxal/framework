//! Fragment collection and endpoint-only Robot API materialization.

use std::collections::{BTreeMap, BTreeSet};

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, Token};

use super::grammar::VERSION_HAS_NO_PARENT;
use super::model::{ConcreteVersion, Node, TopicDef, TopicKind};

mod kw {
    syn::custom_keyword!(endpoint);
    syn::custom_keyword!(extends);
    syn::custom_keyword!(fragments);
    syn::custom_keyword!(latest);
    syn::custom_keyword!(output);
    syn::custom_keyword!(path);
    syn::custom_keyword!(remove);
    syn::custom_keyword!(replace);
    syn::custom_keyword!(source);
    syn::custom_keyword!(version);
    syn::custom_keyword!(versions);
}

#[derive(Clone)]
struct PathSegment {
    name: Ident,
    var: Option<Ident>,
}

#[derive(Clone)]
struct FragmentContribution {
    name: Ident,
    endpoints: Vec<TopicDef>,
    removals: Vec<Ident>,
}

#[derive(Clone)]
struct Fragment {
    path: Vec<PathSegment>,
    contribution: FragmentContribution,
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

        input.parse::<kw::version>()?;
        let name: Ident = input.parse()?;
        validate_revision(&name)?;
        input.parse::<Token![;]>()?;

        let mut endpoints = Vec::new();
        let mut removals = Vec::new();
        while !input.is_empty() {
            let lookahead = input.fork();
            let attrs = lookahead.call(syn::Attribute::parse_outer)?;
            if lookahead.peek(kw::replace) {
                if let Some(attr) = attrs.first() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "attributes belong on payload items, not endpoint declarations",
                    ));
                }
                input.parse::<kw::replace>()?;
                if input.peek(Token![struct])
                    || input.peek(Token![enum])
                    || input.peek(Token![impl])
                    || input.peek(Token![type])
                {
                    return Err(input.error(
                        "payload types and implementations are ordinary sibling Rust items; only endpoints can be replaced",
                    ));
                }
                reject_node_topic(input)?;
                let mut endpoint: TopicDef = input.parse()?;
                endpoint.replace = true;
                endpoints.push(endpoint);
                continue;
            }
            if lookahead.peek(kw::remove) {
                if let Some(attr) = attrs.first() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "attributes are not allowed on `remove endpoint`",
                    ));
                }
                input.parse::<kw::remove>()?;
                if !input.peek(kw::endpoint) {
                    return Err(
                        input.error("only endpoints can be removed; use `remove endpoint <name>;`")
                    );
                }
                input.parse::<kw::endpoint>()?;
                removals.push(input.parse()?);
                input.parse::<Token![;]>()?;
                continue;
            }
            if endpoint_ahead(&lookahead) {
                if let Some(attr) = attrs.first() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "attributes belong on payload items, not endpoint declarations",
                    ));
                }
                reject_node_topic(input)?;
                let mut endpoint: TopicDef = input.parse()?;
                endpoint.replace = false;
                endpoints.push(endpoint);
                continue;
            }

            return Err(input.error(
                "fragments declare endpoints only; define payload types, implementations, and tests as ordinary sibling Rust items",
            ));
        }
        Ok(Self {
            path,
            contribution: FragmentContribution {
                name,
                endpoints,
                removals,
            },
        })
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
            "Robot API endpoints require a named leaf; `self` is available only in protocol trees",
        ));
    }
    Ok(())
}

fn validate_revision(name: &Ident) -> syn::Result<(u16, u16)> {
    let text = name.to_string();
    let Some((major, minor)) = text.strip_prefix('v').and_then(|rest| rest.split_once('_')) else {
        return Err(syn::Error::new_spanned(
            name,
            "API revisions use Rust identifiers such as `v0_1`",
        ));
    };
    let canonical = |part: &str| {
        !part.is_empty()
            && (part == "0" || !part.starts_with('0'))
            && part.bytes().all(|byte| byte.is_ascii_digit())
    };
    if !canonical(major) || !canonical(minor) {
        return Err(syn::Error::new_spanned(
            name,
            "API revision components must be canonical decimal numbers",
        ));
    }
    let major = major.parse().map_err(|_| {
        syn::Error::new_spanned(name, "API revision major component must fit in a `u16`")
    })?;
    let minor = minor.parse().map_err(|_| {
        syn::Error::new_spanned(name, "API revision minor component must fit in a `u16`")
    })?;
    Ok((major, minor))
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

#[derive(Clone)]
struct VersionDecl {
    name: Ident,
    major: u16,
    minor: u16,
    parent: Option<Ident>,
    latest: bool,
}

struct TreeInput {
    output: Ident,
    source: syn::Path,
    versions: Vec<VersionDecl>,
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

        input.parse::<kw::versions>()?;
        let body;
        syn::braced!(body in input);
        let mut versions = Vec::new();
        while !body.is_empty() {
            let latest = if body.peek(kw::latest) {
                body.parse::<kw::latest>()?;
                true
            } else {
                false
            };
            body.parse::<kw::version>()?;
            let name: Ident = body.parse()?;
            let (major, minor) = validate_revision(&name)?;
            let parent = if body.peek(kw::extends) {
                body.parse::<kw::extends>()?;
                Some(body.parse()?)
            } else {
                None
            };
            body.parse::<Token![;]>()?;
            versions.push(VersionDecl {
                name,
                major,
                minor,
                parent,
                latest,
            });
        }
        let fragments: FragmentList = input.parse()?;
        if !input.is_empty() {
            return Err(input.error("unexpected tokens after the root fragment list"));
        }
        validate_version_graph(&versions)?;
        Ok(Self {
            output,
            source,
            versions,
            fragments: fragments.paths,
        })
    }
}

fn validate_version_graph(versions: &[VersionDecl]) -> syn::Result<()> {
    if versions.is_empty() {
        return Err(syn::Error::new(
            Span::call_site(),
            "declare at least one Robot API version",
        ));
    }
    if versions.iter().filter(|version| version.latest).count() != 1 {
        return Err(syn::Error::new(
            Span::call_site(),
            "exactly one declaration must be marked `latest version`",
        ));
    }
    let mut declared = BTreeSet::new();
    for version in versions {
        if !declared.insert(version.name.to_string()) {
            return Err(syn::Error::new_spanned(
                &version.name,
                "duplicate Robot API revision",
            ));
        }
        if let Some(parent) = &version.parent
            && !declared.contains(&parent.to_string())
        {
            return Err(syn::Error::new_spanned(
                parent,
                "an `extends` parent must be declared earlier",
            ));
        }
    }
    Ok(())
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
            "list at least one Robot API fragment",
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

#[derive(Clone)]
struct PathContract {
    path: Vec<PathSegment>,
    endpoints: BTreeMap<String, TopicDef>,
}

type Materialized = BTreeMap<String, PathContract>;

pub(crate) fn expand_materialized(input: TokenStream) -> syn::Result<TokenStream> {
    let input: MaterializeInput = syn::parse2(input)?;
    let known: BTreeSet<String> = input
        .tree
        .versions
        .iter()
        .map(|version| version.name.to_string())
        .collect();
    let mut owners = BTreeMap::<(String, String), (Vec<PathSegment>, FragmentContribution)>::new();
    for fragment in input.fragments {
        let path_name = path_key(&fragment.path);
        let contribution = fragment.contribution;
        let version_name = contribution.name.to_string();
        if !known.contains(&version_name) {
            return Err(syn::Error::new_spanned(
                &contribution.name,
                "fragment contributes to a version absent from the root catalogue",
            ));
        }
        let key = (version_name, path_name.clone());
        if let Some((first_path, _)) = owners.get(&key) {
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
        owners.insert(key, (fragment.path, contribution));
    }

    let mut materialized = BTreeMap::<String, Materialized>::new();
    let mut complete = Vec::new();
    for declaration in &input.tree.versions {
        let mut revision = match &declaration.parent {
            Some(parent) => materialized
                .get(&parent.to_string())
                .cloned()
                .ok_or_else(|| {
                    syn::Error::new_spanned(parent, "an `extends` parent must be declared earlier")
                })?,
            None => Materialized::new(),
        };
        let revision_name = declaration.name.to_string();
        let contributions: Vec<_> = owners
            .iter()
            .filter(|((version, _), _)| version == &revision_name)
            .map(|((_, path), value)| (path.clone(), value.clone()))
            .collect();
        for (path_key, (path, contribution)) in contributions {
            apply_contribution(
                &mut revision,
                path_key,
                path,
                contribution,
                declaration.parent.is_some(),
                &input.tree.source,
                &declaration.name,
            )?;
        }
        let nodes = build_nodes(revision.values().cloned())?;
        complete.push(ConcreteVersion {
            name: declaration.name.clone(),
            wire_id: wire_id(&declaration.name),
            major: declaration.major,
            minor: declaration.minor,
            nodes,
        });
        materialized.insert(revision_name, revision);
    }

    let latest = input
        .tree
        .versions
        .iter()
        .find(|version| version.latest)
        .ok_or_else(|| {
            syn::Error::new(
                Span::call_site(),
                "exactly one declaration must be marked `latest version`",
            )
        })?
        .name
        .clone();
    let generated = super::expand_api(&complete, &latest, &input.tree.source)?;
    let output = input.tree.output;
    Ok(quote! { pub mod #output { #generated } })
}

fn apply_contribution(
    revision: &mut Materialized,
    key: String,
    path: Vec<PathSegment>,
    mut contribution: FragmentContribution,
    has_parent: bool,
    source: &syn::Path,
    revision_name: &Ident,
) -> syn::Result<()> {
    for endpoint in &mut contribution.endpoints {
        qualify_endpoint_payloads(endpoint, source, revision_name, &path)?;
    }
    let inherited_names = revision
        .get(&key)
        .map(|contract| contract.endpoints.keys().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    let mut declared_endpoints = BTreeMap::<String, Ident>::new();
    for endpoint in &contribution.endpoints {
        let ident = endpoint.leaf.method_ident();
        let name = ident.to_string();
        if let Some(first) = declared_endpoints.insert(name.clone(), ident.clone()) {
            let mut error = syn::Error::new_spanned(
                ident,
                format!("endpoint `{name}` is declared more than once in this fragment"),
            );
            error.combine(syn::Error::new_spanned(first, "first declaration is here"));
            return Err(error);
        }
    }
    let mut removed_endpoints = BTreeMap::<String, Ident>::new();
    for removal in &contribution.removals {
        let name = removal.to_string();
        if let Some(first) = removed_endpoints.insert(name.clone(), removal.clone()) {
            let mut error = syn::Error::new_spanned(
                removal,
                format!("endpoint `{name}` is removed more than once in this fragment"),
            );
            error.combine(syn::Error::new_spanned(first, "first removal is here"));
            return Err(error);
        }
        if let Some(declaration) = declared_endpoints.get(&name) {
            let mut error = syn::Error::new_spanned(
                declaration,
                "one revision cannot both remove and declare the same endpoint; use `replace`",
            );
            error.combine(syn::Error::new_spanned(removal, "removal is here"));
            return Err(error);
        }
    }
    let contract = revision.entry(key.clone()).or_insert_with(|| PathContract {
        path: path.clone(),
        endpoints: BTreeMap::new(),
    });
    if path_key(&contract.path) != path_key(&path) {
        return Err(syn::Error::new_spanned(
            &path[0].name,
            "logical path binding mismatch",
        ));
    }
    if !has_parent {
        if let Some(endpoint) = contribution
            .endpoints
            .iter()
            .find(|endpoint| endpoint.replace)
        {
            return Err(syn::Error::new_spanned(
                endpoint.leaf.method_ident(),
                format!("`replace` {VERSION_HAS_NO_PARENT}"),
            ));
        }
        if let Some(removal) = contribution.removals.first() {
            return Err(syn::Error::new_spanned(
                removal,
                format!("`remove endpoint` {VERSION_HAS_NO_PARENT}"),
            ));
        }
    }

    for removal in contribution.removals {
        if contract.endpoints.remove(&removal.to_string()).is_none() {
            return Err(syn::Error::new_spanned(
                removal,
                "`remove endpoint` target does not exist in the parent revision",
            ));
        }
    }
    for mut endpoint in contribution.endpoints {
        let name = endpoint.leaf.method_ident().to_string();
        match (inherited_names.contains(&name), endpoint.replace) {
            (true, false) => {
                return Err(syn::Error::new_spanned(
                    endpoint.leaf.method_ident(),
                    "inherited endpoint already exists; prefix it with `replace`",
                ));
            }
            (false, true) => {
                return Err(syn::Error::new_spanned(
                    endpoint.leaf.method_ident(),
                    "`replace` endpoint target does not exist in the parent revision",
                ));
            }
            _ => {}
        }
        endpoint.replace = false;
        contract.endpoints.insert(name, endpoint);
    }
    if contract.endpoints.is_empty() {
        revision.remove(&key);
    }
    Ok(())
}

fn qualify_endpoint_payloads(
    endpoint: &mut TopicDef,
    source: &syn::Path,
    revision: &Ident,
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
        qualified
            .segments
            .push(syn::PathSegment::from(revision.clone()));
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

fn wire_id(name: &Ident) -> String {
    name.to_string().replacen('_', ".", 1)
}
