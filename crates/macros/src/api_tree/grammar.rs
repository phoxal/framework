//! The semantic family/protocol grammar: the keyword table and every `Parse` impl
//! that turns an invocation's tokens into the [`super::model`] tree, plus the
//! diagnostics that name why a form is not accepted where it was written.

use syn::parse::{Parse, ParseStream};
use syn::{Ident, Token};

use super::model::{BodyPath, Node, Protocol, SemanticKind, TopicDef, TopicKind, TopicLeaf};

mod kw {
    syn::custom_keyword!(protocol);
    syn::custom_keyword!(topic);
    syn::custom_keyword!(command);
    syn::custom_keyword!(stream);
    syn::custom_keyword!(state);
    syn::custom_keyword!(delivery);
    syn::custom_keyword!(sample);
    syn::custom_keyword!(setpoint);
    syn::custom_keyword!(event);
    syn::custom_keyword!(query);
}

pub(super) struct ProtocolInput(pub(super) Vec<Protocol>);

impl Parse for ProtocolInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if !input.peek(kw::protocol) {
            return Err(input.error(
                "`phoxal_protocol!` accepts only `protocol <name> { ... }` trees; use \
                 `phoxal_api_tree!` plus fragments for semantic contract families",
            ));
        }
        let mut protocols = Vec::new();
        while input.peek(kw::protocol) {
            protocols.push(input.parse()?);
        }
        if !input.is_empty() {
            return Err(
                input.error("a `protocol` invocation declares only `protocol <name> { … }` trees")
            );
        }
        Ok(Self(protocols))
    }
}

impl Parse for Protocol {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<kw::protocol>()?;
        let name: Ident = input.parse()?;
        let text = name.to_string();
        let valid = text
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase())
            && text
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_');
        if !valid {
            return Err(syn::Error::new(
                name.span(),
                "a protocol name is a lowercase Rust identifier such as `supervisor`; it is both \
                 the generated module and the leading wire-key segment",
            ));
        }
        let body;
        syn::braced!(body in input);
        let mut nodes = Vec::new();
        while !body.is_empty() {
            nodes.push(body.parse()?);
        }
        Ok(Protocol { name, nodes })
    }
}

impl Parse for Node {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: Ident = input.parse()?;
        // Optional `(var)` makes the node dynamic.
        let var = if input.peek(syn::token::Paren) {
            let content;
            syn::parenthesized!(content in input);
            let var: Ident = content.parse()?;
            if !content.is_empty() {
                return Err(content.error(
                    "a dynamic node binds exactly one variable, e.g. `motor(capability) { … }`",
                ));
            }
            Some(var)
        } else {
            None
        };

        let body;
        syn::braced!(body in input);
        let mut topics = Vec::new();
        let mut children = Vec::new();
        while !body.is_empty() {
            // Leading doc-comments / attributes apply to the next item; `topic`
            // declarations take none.
            let attrs = body.call(syn::Attribute::parse_outer)?;
            if body.peek(kw::topic) || body.peek(kw::command) || body.peek(kw::query) {
                if let Some(attr) = attrs.first() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "attributes are not allowed on a `topic` declaration",
                    ));
                }
                topics.push(body.parse()?);
            } else if body.peek(Token![struct]) || body.peek(Token![enum]) {
                return Err(body.error(
                    "semantic family/protocol trees declare endpoints over ordinary Rust domain types; move this struct/enum into its domain module",
                ));
            } else if body.peek(Ident)
                && (body.peek2(syn::token::Paren) || body.peek2(syn::token::Brace))
            {
                // `name(var) { … }` or `name { … }` - a child node.
                if let Some(attr) = attrs.first() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "attributes are not allowed on a child node declaration",
                    ));
                }
                children.push(body.parse()?);
            } else {
                return Err(body.error(
                    "expected an endpoint declaration or a child node `name { … }` / \
                     `name(var) { … }` inside a node block",
                ));
            }
        }
        Ok(Node {
            name,
            var,
            topics,
            children,
        })
    }
}

impl Parse for TopicDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // New endpoint direction is part of the declaration prefix:
        // `topic frame: Sample<Frame>` publishes from the owner,
        // `command target: Setpoint<Target>` is consumed by the owner, and
        // `query start: Req => Resp` is request/reply.
        let prefix = if input.peek(kw::topic) {
            input.parse::<kw::topic>()?;
            0u8
        } else if input.peek(kw::command) {
            input.parse::<kw::command>()?;
            1u8
        } else if input.peek(kw::query) {
            input.parse::<kw::query>()?;
            2u8
        } else {
            return Err(input.error("expected `topic`, `command`, or `query` endpoint"));
        };
        let leaf = if input.peek(Token![self]) {
            input.parse::<Token![self]>()?;
            TopicLeaf::Node
        } else {
            TopicLeaf::Named(input.parse()?)
        };
        input.parse::<Token![:]>()?;
        if prefix == 2 {
            let request = parse_body_path(input)?;
            input.parse::<Token![=>]>()?;
            let response = parse_body_path(input)?;
            input.parse::<Token![;]>()?;
            return Ok(TopicDef {
                leaf,
                kind: TopicKind::Query { request, response },
                semantic: SemanticKind::Query,
                owner_publishes: true,
            });
        }
        // The semantic descriptor spelling is `State<T>`, `Sample<T>`,
        // `Event<T>`, `Stream<T>`, or `Setpoint<T>`. It is deliberately
        // distinguished from the old lowercase role keywords.
        if input.peek(Ident)
            && !input.peek(kw::command)
            && !input.peek(kw::stream)
            && !input.peek(kw::state)
            && !input.peek(kw::event)
            && !input.peek(kw::sample)
            && !input.peek(kw::setpoint)
            && !input.peek(kw::query)
        {
            let descriptor: Ident = input.parse()?;
            if input.peek(Token![<]) {
                input.parse::<Token![<]>()?;
                let body = parse_body_path(input)?;
                input.parse::<Token![>]>()?;
                let (semantic, owner_publishes) = match descriptor.to_string().as_str() {
                    "State" if prefix == 0 => (SemanticKind::State, true),
                    "Sample" if prefix == 0 => (SemanticKind::Sample, true),
                    "Event" if prefix == 0 => (SemanticKind::Event, true),
                    "Stream" => (SemanticKind::Stream, prefix == 0),
                    "Setpoint" if prefix == 1 => (SemanticKind::Setpoint, false),
                    _ => {
                        return Err(syn::Error::new_spanned(
                            descriptor,
                            "expected semantic descriptor `State<T>`, `Sample<T>`, `Event<T>`, `Stream<T>`, or `Setpoint<T>`",
                        ));
                    }
                };
                input.parse::<Token![;]>()?;
                return Ok(TopicDef {
                    leaf,
                    kind: TopicKind::PubSub(body),
                    semantic,
                    owner_publishes,
                });
            }
            return Err(syn::Error::new_spanned(
                descriptor,
                "semantic endpoint descriptors must carry one payload type in angle brackets",
            ));
        }
        Err(input.error(
            "expected a semantic endpoint descriptor: `State<T>`, `Sample<T>`, `Event<T>`, `Stream<T>`, or `Setpoint<T>`",
        ))
    }
}

fn parse_body_path(input: ParseStream) -> syn::Result<BodyPath> {
    let ty: syn::TypePath = input.parse()?;
    Ok(BodyPath { path: ty.path })
}
