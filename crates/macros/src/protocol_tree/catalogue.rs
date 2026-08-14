//! Parsing and materialization for one complete protocol catalogue.
//!
//! The catalogue is deliberately one declaration. Payloads remain ordinary
//! Rust modules; this parser only binds their local type names to endpoint
//! descriptors and builds the generated endpoint/topic trees.

use std::collections::BTreeMap;

use proc_macro2::{Span, TokenStream};
use syn::parse::{Parse, ParseStream};
use syn::{Ident, Token};

use super::model::{MaterializedTree, Node, TopicDef};

mod kw {
    syn::custom_keyword!(path);
}

const FAMILIES: [&str; 3] = ["robot", "runtime", "supervisor"];

#[derive(Clone)]
struct PathSegment {
    name: Ident,
    var: Option<Ident>,
}

struct PathSection {
    path: Vec<PathSegment>,
    endpoints: Vec<TopicDef>,
}

impl Parse for PathSection {
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
        while !input.is_empty() && !input.peek(kw::path) {
            let lookahead = input.fork();
            let attrs = lookahead.call(syn::Attribute::parse_outer)?;
            if endpoint_ahead(&lookahead) {
                if let Some(attr) = attrs.first() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "attributes belong on payload items, not endpoint declarations",
                    ));
                }
                endpoints.push(input.parse()?);
                continue;
            }
            return Err(input.error(
                "the protocol catalogue contains only `path` sections and descriptor declarations",
            ));
        }
        Ok(Self { path, endpoints })
    }
}

fn endpoint_ahead(input: ParseStream<'_>) -> bool {
    let ahead = input.fork();
    if ahead.peek(Token![self]) {
        if ahead.parse::<Token![self]>().is_err() {
            return false;
        }
    } else if ahead.parse::<Ident>().is_err() {
        return false;
    }
    ahead.peek(Token![:])
}

fn validate_family(path: &[PathSegment]) -> syn::Result<()> {
    let Some(root) = path.first() else {
        return Err(syn::Error::new(
            Span::call_site(),
            "a protocol path cannot be empty",
        ));
    };
    if root.var.is_some() {
        return Err(syn::Error::new_spanned(
            &root.name,
            "a contract family root is static",
        ));
    }
    if !FAMILIES.contains(&root.name.to_string().as_str()) {
        return Err(syn::Error::new_spanned(
            &root.name,
            format!("a protocol path starts with `{}`", FAMILIES.join("`, `")),
        ));
    }
    if path.len() < 2 {
        return Err(syn::Error::new_spanned(
            &root.name,
            "a protocol path includes a module below its family",
        ));
    }
    Ok(())
}

struct Catalogue {
    sections: Vec<PathSection>,
}

impl Parse for Catalogue {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut sections = Vec::new();
        while !input.is_empty() {
            sections.push(input.parse()?);
        }
        if sections.is_empty() {
            return Err(syn::Error::new(
                Span::call_site(),
                "declare at least one protocol path",
            ));
        }
        Ok(Self { sections })
    }
}

struct PathContract {
    path: Vec<PathSegment>,
    endpoints: BTreeMap<String, TopicDef>,
}

pub(crate) fn expand_tree(input: TokenStream) -> syn::Result<TokenStream> {
    let input: Catalogue = syn::parse2(input)?;
    let mut owners = BTreeMap::<String, (Vec<PathSegment>, BTreeMap<String, TopicDef>)>::new();
    for section in input.sections {
        let path_name = path_key(&section.path);
        if let Some((first_path, _)) = owners.get(&path_name) {
            let mut error = syn::Error::new(
                section.path[0].name.span(),
                format!("duplicate path `{path_name}`"),
            );
            error.combine(syn::Error::new(
                first_path[0].name.span(),
                "first path is here",
            ));
            return Err(error);
        }

        let mut endpoints = BTreeMap::new();
        for mut endpoint in section.endpoints {
            qualify_endpoint_payloads(&mut endpoint, &section.path)?;
            let ident = endpoint.leaf.method_ident();
            let name = ident.to_string();
            if let Some(first) = endpoints.insert(name.clone(), endpoint) {
                let mut error = syn::Error::new_spanned(
                    ident,
                    format!("endpoint `{name}` is declared more than once in this path"),
                );
                error.combine(syn::Error::new_spanned(
                    first.leaf.method_ident(),
                    "first declaration is here",
                ));
                return Err(error);
            }
        }
        owners.insert(path_name, (section.path, endpoints));
    }

    let mut families = BTreeMap::<String, (Ident, Vec<PathContract>)>::new();
    for (path, endpoints) in owners.into_values() {
        let Some((family, below)) = path.split_first() else {
            return Err(syn::Error::new(
                Span::call_site(),
                "protocol path cannot be empty",
            ));
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
    for (_, (module, contracts)) in families {
        trees.push(MaterializedTree {
            module,
            nodes: build_nodes(contracts)?,
        });
    }
    Ok(super::expand_trees(&trees))
}

fn qualify_endpoint_payloads(
    endpoint: &mut TopicDef,
    logical_path: &[PathSegment],
) -> syn::Result<()> {
    for body in endpoint.descriptor.body_paths_mut() {
        let path = &body.path;
        if path.leading_colon.is_some()
            || path.segments.len() != 1
            || !matches!(path.segments[0].arguments, syn::PathArguments::None)
        {
            return Err(syn::Error::new_spanned(
                path,
                "endpoint payloads name a type defined in their path module",
            ));
        }
        let leaf = path.segments[0].ident.clone();
        let mut qualified: syn::Path = syn::parse_quote!(crate);
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
            "protocol path cannot be empty",
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
