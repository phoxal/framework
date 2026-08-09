//! The semantic API/protocol grammar: the keyword table and every `Parse` impl
//! that turns an invocation's tokens into the [`super::model`] tree, plus the
//! diagnostics that name why a form is not accepted where it was written.

use syn::parse::{Parse, ParseStream};
use syn::{Ident, Token};

use super::ApiTree;
use super::model::{
    BodyPath, Node, Protocol, Removal, SemanticKind, TopicDef, TopicKind, TopicLeaf, Version,
};

mod kw {
    syn::custom_keyword!(extends);
    syn::custom_keyword!(latest);
    syn::custom_keyword!(protocol);
    syn::custom_keyword!(remove);
    syn::custom_keyword!(replace);
    syn::custom_keyword!(version);
    syn::custom_keyword!(topic);
    syn::custom_keyword!(command);
    syn::custom_keyword!(stream);
    syn::custom_keyword!(state);
    syn::custom_keyword!(measurement);
    syn::custom_keyword!(diagnostic);
    syn::custom_keyword!(delivery);
    syn::custom_keyword!(sample);
    syn::custom_keyword!(setpoint);
    syn::custom_keyword!(event);
    syn::custom_keyword!(query);
}

/// The diagnostic for an invocation that reaches a `protocol` tree from inside
/// the other mode. The protocol-first direction has its own wording at the top
/// of [`ApiTree::parse`]; this is every other direction, so a misplaced
/// `protocol` never lands on the generic "expected a child node" error.
const MODES_DO_NOT_MIX: &str = "a `protocol <name> { … }` tree stands alone at the top of its own semantic macro \
     invocation: it has no `version` revisions and no `latest` selection, so the two modes never \
     mix in one invocation";

/// The diagnostic tail for a `replace`/`remove` inside a revision that has no
/// parent to delta against.
pub(super) const VERSION_HAS_NO_PARENT: &str =
    "is only valid inside a revision that `extends` another revision";

/// The diagnostic tail for a `replace`/`remove` inside a protocol tree. A
/// protocol has no revision history at all, so there is nothing to delta - the
/// declaration is edited in place.
pub(super) const PROTOCOL_HAS_NO_DELTAS: &str = "is not valid inside a `protocol` tree: a protocol has no revision history, so edit the \
     declaration in place";

impl Parse for ApiTree {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.peek(kw::protocol) {
            let mut protocols = Vec::new();
            while input.peek(kw::protocol) {
                protocols.push(input.parse()?);
            }
            if !input.is_empty() {
                return Err(input.error(
                    "a `protocol` invocation declares only `protocol <name> { … }` trees: it has \
                     no `version` revisions and no `latest` selection",
                ));
            }
            return Ok(ApiTree::Protocols(protocols));
        }
        let mut versions: Vec<Version> = Vec::new();
        // The semantic API grammar makes selection part of the declaration:
        // `latest version v0.2 extends v0.1 { ... }`.
        if input.peek(kw::latest) {
            input.parse::<kw::latest>()?;
            let mut version: Version = input.parse()?;
            version.latest = true;
            version.latest_prefix = true;
            versions.push(version);
        }
        while input.peek(kw::version) {
            versions.push(input.parse()?);
        }
        if input.peek(kw::latest) && input.peek2(kw::version) {
            input.parse::<kw::latest>()?;
            let mut version: Version = input.parse()?;
            version.latest = true;
            version.latest_prefix = true;
            versions.push(version);
            while input.peek(kw::version) {
                versions.push(input.parse()?);
            }
        }
        if input.peek(kw::protocol) {
            return Err(input.error(MODES_DO_NOT_MIX));
        }
        if versions.is_empty() {
            return Err(input.error(
                "the semantic API/protocol macros require at least one `version` block or one \
                 `protocol <name> { … }` tree",
            ));
        }
        let latest = if versions.iter().any(|version| version.latest) {
            if versions.iter().filter(|version| version.latest).count() != 1 {
                return Err(
                    input.error("exactly one final `version ... latest` declaration is required")
                );
            }
            let selected = versions
                .iter()
                .position(|version| version.latest)
                .ok_or_else(|| input.error("exactly one latest revision is required"))?;
            if selected + 1 != versions.len() {
                return Err(syn::Error::new_spanned(
                    &versions[selected].name,
                    "`latest` must belong to the final version declaration",
                ));
            }
            if !input.is_empty() {
                return Err(input.error(
                    "`latest` belongs on the final version declaration; remove the trailing selection",
                ));
            }
            versions[selected].name.clone()
        } else {
            input.parse::<kw::latest>()?;
            let latest = input.parse()?;
            input.parse::<Token![;]>()?;
            if let Some(version) = versions.iter_mut().find(|version| version.name == latest) {
                version.latest = true;
            }
            latest
        };
        if input.peek(kw::protocol) {
            return Err(input.error(MODES_DO_NOT_MIX));
        }
        if !input.is_empty() {
            return Err(input.error("expected exactly one final `latest <revision>;` declaration"));
        }
        Ok(ApiTree::Api { versions, latest })
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
            if body.peek(kw::remove) {
                return Err(body.error(format!("`remove` {PROTOCOL_HAS_NO_DELTAS}")));
            }
            nodes.push(body.parse()?);
        }
        Ok(Protocol { name, nodes })
    }
}

impl Parse for Version {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<kw::version>()?;
        let (name, wire_hint) = parse_revision_ident(input)?;
        let name_text = name.to_string();
        let Some(parts) = name_text.strip_prefix('v') else {
            return Err(syn::Error::new(
                name.span(),
                "API revisions use Rust identifiers such as `v0_1` or `v1_0`",
            ));
        };
        let Some((major, minor)) = parts.split_once('_') else {
            return Err(syn::Error::new(
                name.span(),
                "API revisions use two-part Rust identifiers such as `v0_1` or `v1_0`",
            ));
        };
        let valid_part = |part: &str| {
            !part.is_empty()
                && (part == "0" || !part.starts_with('0'))
                && part.bytes().all(|byte| byte.is_ascii_digit())
        };
        if !valid_part(major) || !valid_part(minor) {
            return Err(syn::Error::new(
                name.span(),
                "API revision components must be canonical decimal numbers, e.g. `v0_1`",
            ));
        }
        let wire_id = if wire_hint.contains('.') {
            wire_hint
        } else {
            format!("v{major}.{minor}")
        };
        // `latest` is attached to the final revision. Accept it on either side
        // of `extends` so both natural declaration orders remain readable.
        let mut latest = input.peek(kw::latest);
        if latest {
            input.parse::<kw::latest>()?;
        }
        let parent = if input.peek(kw::extends) {
            input.parse::<kw::extends>()?;
            Some(parse_revision_ident(input)?.0)
        } else {
            None
        };
        if input.peek(kw::latest) {
            if latest {
                return Err(input.error("a version can declare `latest` only once"));
            }
            input.parse::<kw::latest>()?;
            latest = true;
        }
        let body;
        syn::braced!(body in input);
        let mut nodes = Vec::new();
        let mut removals = Vec::new();
        while !body.is_empty() {
            if body.peek(kw::remove) {
                removals.push(body.parse()?);
            } else {
                nodes.push(body.parse()?);
            }
        }
        Ok(Version {
            name,
            wire_id,
            latest,
            latest_prefix: false,
            parent,
            nodes,
            removals,
        })
    }
}

fn parse_revision_ident(input: ParseStream) -> syn::Result<(Ident, String)> {
    let major_or_name: Ident = input.parse()?;
    let text = major_or_name.to_string();
    if input.peek(Token![.]) {
        input.parse::<Token![.]>()?;
        let minor: syn::LitInt = input.parse()?;
        let minor_text = minor.base10_digits();
        let name = quote::format_ident!("{}_{}", text, minor_text);
        let wire_id = format!("{text}.{minor_text}");
        return Ok((name, wire_id));
    }
    Ok((major_or_name, text))
}

impl Parse for Node {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let replace = if input.peek(kw::replace) {
            input.parse::<kw::replace>()?;
            true
        } else {
            false
        };
        // `protocol <name> {` here is a protocol tree nested inside another
        // tree. A node literally called `protocol` is followed by `{` or `(`,
        // never by a second identifier, so this only catches the mix-up.
        if input.peek(kw::protocol) && input.peek2(Ident) {
            return Err(input.error(MODES_DO_NOT_MIX));
        }
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
        let mut removals = Vec::new();
        while !body.is_empty() {
            // Leading doc-comments / attributes apply to the next item; `topic`
            // declarations take none.
            let attrs = body.call(syn::Attribute::parse_outer)?;
            let replace_item = if body.peek(kw::replace) {
                body.parse::<kw::replace>()?;
                true
            } else {
                false
            };
            if body.peek(kw::remove) {
                if replace_item {
                    return Err(body.error("`replace remove` is not valid; use `remove <path>;`"));
                }
                if let Some(attr) = attrs.first() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "attributes are not allowed on a `remove` declaration",
                    ));
                }
                removals.push(body.parse()?);
            } else if body.peek(kw::topic) || body.peek(kw::command) || body.peek(kw::query) {
                if let Some(attr) = attrs.first() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "attributes are not allowed on a `topic` declaration",
                    ));
                }
                let mut topic: TopicDef = body.parse()?;
                topic.replace = replace_item;
                topics.push(topic);
            } else if body.peek(Token![struct]) || body.peek(Token![enum]) {
                return Err(body.error(
                    "semantic API/protocol trees declare endpoints over ordinary Rust domain types; move this struct/enum into its domain module",
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
                let mut child: Node = body.parse()?;
                child.replace = replace_item;
                children.push(child);
            } else if body.peek(kw::protocol) && body.peek2(Ident) {
                return Err(body.error(MODES_DO_NOT_MIX));
            } else {
                return Err(body.error(
                    "expected an endpoint declaration or a child node `name { … }` / \
                     `name(var) { … }` inside an API node block",
                ));
            }
        }
        Ok(Node {
            name,
            replace,
            var,
            topics,
            children,
            removals,
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
                replace: false,
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
            && !input.peek(kw::measurement)
            && !input.peek(kw::diagnostic)
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
                    replace: false,
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

impl Parse for Removal {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<kw::remove>()?;
        let head = input.parse()?;
        let mut rest = Vec::new();
        while input.peek(Token![::]) {
            input.parse::<Token![::]>()?;
            rest.push(input.parse()?);
        }
        input.parse::<Token![;]>()?;
        Ok(Removal { head, rest })
    }
}
