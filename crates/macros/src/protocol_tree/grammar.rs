//! The descriptor-only endpoint grammar and its diagnostics.

use syn::parse::{Parse, ParseStream};
use syn::{Ident, Token};

use super::model::{BodyPath, Descriptor, StreamDirection, TopicDef, TopicLeaf};

impl Parse for TopicDef {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let leaf = if input.peek(Token![self]) {
            input.parse::<Token![self]>()?;
            TopicLeaf::Node
        } else {
            TopicLeaf::Named(input.parse()?)
        };
        input.parse::<Token![:]>()?;

        let name: Ident = input.parse()?;
        input.parse::<Token![<]>()?;
        let descriptor = match name.to_string().as_str() {
            "State" => Descriptor::State(parse_one(input, &name, "State<T>")?),
            "Sample" => Descriptor::Sample(parse_one(input, &name, "Sample<T>")?),
            "Event" => Descriptor::Event(parse_one(input, &name, "Event<T>")?),
            "Setpoint" => Descriptor::Setpoint(parse_one(input, &name, "Setpoint<T>")?),
            "Query" => {
                let request = parse_body_path(input)?;
                input.parse::<Token![,]>().map_err(|_| {
                    syn::Error::new_spanned(&name, "`Query` requires `Query<Request, Response>`")
                })?;
                let response = parse_body_path(input)?;
                reject_extra_argument(input, &name, "Query<Request, Response>")?;
                Descriptor::Query { request, response }
            }
            "Stream" => {
                let body = parse_body_path(input)?;
                input.parse::<Token![,]>().map_err(|_| {
                    syn::Error::new_spanned(&name, "`Stream` requires `Stream<Payload, In|Out>`")
                })?;
                let direction: Ident = input.parse()?;
                let direction = match direction.to_string().as_str() {
                    "In" => StreamDirection::In,
                    "Out" => StreamDirection::Out,
                    _ => {
                        return Err(syn::Error::new_spanned(
                            direction,
                            "stream direction is exactly `In` or `Out`",
                        ));
                    }
                };
                reject_extra_argument(input, &name, "Stream<Payload, In|Out>")?;
                Descriptor::Stream { body, direction }
            }
            _ => {
                return Err(syn::Error::new_spanned(
                    name,
                    "expected descriptor `State<T>`, `Sample<T>`, `Event<T>`, `Setpoint<T>`, `Query<Request, Response>`, or `Stream<T, In|Out>`",
                ));
            }
        };
        input.parse::<Token![>]>()?;
        input.parse::<Token![;]>()?;
        Ok(Self { leaf, descriptor })
    }
}

fn parse_one(input: ParseStream<'_>, name: &Ident, expected: &str) -> syn::Result<BodyPath> {
    let body = parse_body_path(input)?;
    reject_extra_argument(input, name, expected)?;
    Ok(body)
}

fn reject_extra_argument(input: ParseStream<'_>, name: &Ident, expected: &str) -> syn::Result<()> {
    if input.peek(Token![,]) {
        return Err(syn::Error::new_spanned(
            name,
            format!("descriptor requires exactly `{expected}`"),
        ));
    }
    Ok(())
}

fn parse_body_path(input: ParseStream<'_>) -> syn::Result<BodyPath> {
    let ty: syn::TypePath = input.parse()?;
    Ok(BodyPath { path: ty.path })
}
