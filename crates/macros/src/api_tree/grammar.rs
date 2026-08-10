//! The semantic family grammar: the keyword table and every `Parse` impl that
//! turns an endpoint declaration's tokens into the [`super::model`] tree, plus
//! the diagnostics that name why a form is not accepted where it was written.

use syn::parse::{Parse, ParseStream};
use syn::{Ident, Token};

use super::model::{BodyPath, SemanticKind, TopicDef, TopicKind, TopicLeaf};

mod kw {
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
