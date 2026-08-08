//! The `phoxal_api_tree!` grammar: the keyword table and every `Parse` impl
//! that turns an invocation's tokens into the [`super::model`] tree, plus the
//! diagnostics that name why a form is not accepted where it was written.

use syn::parse::{Parse, ParseStream};
use syn::{Ident, ItemEnum, ItemStruct, Token};

use super::ApiTree;
use super::model::{
    Node, Protocol, Removal, TopicDef, TopicKind, TopicLeaf, TopicRole, TypeDef, TypeItem, Version,
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
    syn::custom_keyword!(state);
    syn::custom_keyword!(measurement);
    syn::custom_keyword!(diagnostic);
    syn::custom_keyword!(query);
    syn::custom_keyword!(world_clock);
}

/// The diagnostic for an invocation that reaches a `protocol` tree from inside
/// the other mode. The protocol-first direction has its own wording at the top
/// of [`ApiTree::parse`]; this is every other direction, so a misplaced
/// `protocol` never lands on the generic "expected a child node" error.
const MODES_DO_NOT_MIX: &str = "a `protocol <name> { … }` tree stands alone at the top of its own `phoxal_api_tree!` \
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
        let mut versions = Vec::new();
        while input.peek(kw::version) {
            versions.push(input.parse()?);
        }
        if input.peek(kw::protocol) {
            return Err(input.error(MODES_DO_NOT_MIX));
        }
        if versions.is_empty() {
            return Err(input.error(
                "phoxal_api_tree! requires at least one `version` block or one \
                 `protocol <name> { … }` tree",
            ));
        }
        input.parse::<kw::latest>()?;
        let latest = input.parse()?;
        input.parse::<Token![;]>()?;
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
        let name: Ident = input.parse()?;
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
        let wire_id = format!("v{major}.{minor}");
        let parent = if input.peek(kw::extends) {
            input.parse::<kw::extends>()?;
            Some(input.parse()?)
        } else {
            None
        };
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
            parent,
            nodes,
            removals,
        })
    }
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
        let mut types = Vec::new();
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
            } else if body.peek(kw::topic) {
                if let Some(attr) = attrs.first() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "attributes are not allowed on a `topic` declaration",
                    ));
                }
                let mut topic: TopicDef = body.parse()?;
                topic.replace = replace_item;
                topics.push(topic);
            } else if body.peek(Token![struct]) {
                let mut item: ItemStruct = body.parse()?;
                item.attrs = attrs;
                item.vis = syn::Visibility::Public(syn::token::Pub::default());
                types.push(TypeDef {
                    replace: replace_item,
                    item: TypeItem::Struct(item),
                });
            } else if body.peek(Token![enum]) {
                let mut item: ItemEnum = body.parse()?;
                item.attrs = attrs;
                item.vis = syn::Visibility::Public(syn::token::Pub::default());
                types.push(TypeDef {
                    replace: replace_item,
                    item: TypeItem::Enum(item),
                });
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
                    "expected `struct`, `enum`, `topic …;`, or a child node `name { … }` / \
                     `name(var) { … }` inside an API node block",
                ));
            }
        }
        Ok(Node {
            name,
            replace,
            var,
            types,
            topics,
            children,
            removals,
        })
    }
}

impl Parse for TopicDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<kw::topic>()?;
        let leaf = if input.peek(Token![self]) {
            input.parse::<Token![self]>()?;
            TopicLeaf::Node
        } else {
            TopicLeaf::Named(input.parse()?)
        };
        input.parse::<Token![:]>()?;
        // Every topic declares a role. `command`, `state`, `measurement`, and
        // `diagnostic` carry a single pub/sub body and differ by role; `query`
        // carries request/response. The role rides alongside the kind and
        // selects the side brand in the generated builders: a `command` leaf is
        // `Publish` on the public builder and `Subscribe` on the owner builder;
        // every owner-published role is the reverse.
        let (kind, role) = if input.peek(kw::command) {
            input.parse::<kw::command>()?;
            let body: Ident = input.parse()?;
            (TopicKind::PubSub(body), TopicRole::Command)
        } else if input.peek(kw::state) {
            input.parse::<kw::state>()?;
            let body: Ident = input.parse()?;
            (TopicKind::PubSub(body), TopicRole::State)
        } else if input.peek(kw::measurement) {
            input.parse::<kw::measurement>()?;
            let body: Ident = input.parse()?;
            (TopicKind::PubSub(body), TopicRole::Measurement)
        } else if input.peek(kw::diagnostic) {
            input.parse::<kw::diagnostic>()?;
            let body: Ident = input.parse()?;
            (TopicKind::PubSub(body), TopicRole::Diagnostic)
        } else if input.peek(kw::query) {
            input.parse::<kw::query>()?;
            let request: Ident = input.parse()?;
            input.parse::<Token![=>]>()?;
            let response: Ident = input.parse()?;
            (TopicKind::Query { request, response }, TopicRole::Query)
        } else if input.peek(kw::world_clock) {
            // Framework-reserved: see `TopicRole::WorldClock`'s docs. There is
            // exactly one production use (`simulation::Clock` in
            // `phoxal-api/src/lib.rs`) and no reason for a second.
            input.parse::<kw::world_clock>()?;
            let body: Ident = input.parse()?;
            (TopicKind::PubSub(body), TopicRole::WorldClock)
        } else {
            return Err(input.error(
                "expected a topic role: `command <Body>`, `state <Body>`, \
                 `measurement <Body>`, `diagnostic <Body>`, `world_clock <Body>` \
                 (framework-reserved), or `query <Req> => <Resp>`",
            ));
        };
        input.parse::<Token![;]>()?;
        Ok(TopicDef {
            replace: false,
            leaf,
            kind,
            role,
        })
    }
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
