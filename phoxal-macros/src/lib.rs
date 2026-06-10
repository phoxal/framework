use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Ident, LitInt, Result, Token, Type, Visibility, braced, parenthesized};

mod kw {
    syn::custom_keyword!(pubsub);
    syn::custom_keyword!(query);
    syn::custom_keyword!(v);
}

#[proc_macro]
pub fn topic_tree(input: TokenStream) -> TokenStream {
    match syn::parse::<TopicTree>(input) {
        Ok(tree) => tree.expand().into(),
        Err(error) => error.to_compile_error().into(),
    }
}

struct TopicTree {
    visibility: Visibility,
    module: Ident,
    items: Vec<Item>,
}

enum Item {
    Node(Node),
    Leaf(Leaf),
}

struct Node {
    name: Ident,
    has_id: bool,
    children: Vec<Item>,
}

struct Leaf {
    name: Ident,
    interaction: Interaction,
    version: LitInt,
}

enum Interaction {
    PubSub(Type),
    Query { request: Type, response: Type },
}

impl Parse for TopicTree {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let visibility = input.parse()?;
        input.parse::<Token![mod]>()?;
        let module = input.parse()?;
        input.parse::<Token![;]>()?;

        let mut items = Vec::new();
        while !input.is_empty() {
            items.push(input.parse()?);
        }

        Ok(Self {
            visibility,
            module,
            items,
        })
    }
}

impl Parse for Item {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        if input.peek(kw::pubsub) {
            return Ok(Self::Leaf(parse_pubsub(input)?));
        }
        if input.peek(kw::query) {
            return Ok(Self::Leaf(parse_query(input)?));
        }
        Ok(Self::Node(input.parse()?))
    }
}

impl Parse for Node {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let name = input.parse()?;
        let mut has_id = false;

        if input.peek(syn::token::Paren) {
            let content;
            parenthesized!(content in input);
            let marker: Ident = content.parse()?;
            if marker != "id" {
                return Err(syn::Error::new(marker.span(), "expected `id`"));
            }
            if !content.is_empty() {
                return Err(content.error("unexpected tokens after `id`"));
            }
            has_id = true;
        }

        let content;
        braced!(content in input);
        let mut children = Vec::new();
        while !content.is_empty() {
            children.push(content.parse()?);
        }

        Ok(Self {
            name,
            has_id,
            children,
        })
    }
}

fn parse_pubsub(input: ParseStream<'_>) -> Result<Leaf> {
    input.parse::<kw::pubsub>()?;
    let name = input.parse()?;
    input.parse::<Token![:]>()?;
    let payload = input.parse()?;
    input.parse::<Token![,]>()?;
    input.parse::<kw::v>()?;
    input.parse::<Token![=]>()?;
    let version = input.parse()?;
    input.parse::<Token![;]>()?;

    Ok(Leaf {
        name,
        interaction: Interaction::PubSub(payload),
        version,
    })
}

fn parse_query(input: ParseStream<'_>) -> Result<Leaf> {
    input.parse::<kw::query>()?;
    let name = input.parse()?;
    input.parse::<Token![:]>()?;
    let request = input.parse()?;
    input.parse::<Token![=>]>()?;
    let response = input.parse()?;
    input.parse::<Token![,]>()?;
    input.parse::<kw::v>()?;
    input.parse::<Token![=]>()?;
    let version = input.parse()?;
    input.parse::<Token![;]>()?;

    Ok(Leaf {
        name,
        interaction: Interaction::Query { request, response },
        version,
    })
}

impl TopicTree {
    fn expand(&self) -> TokenStream2 {
        let visibility = &self.visibility;
        let module = &self.module;
        let root_methods = methods_for(&self.items, "", "");
        let root_modules = modules_for(&self.items, "", "");

        quote! {
            #visibility mod #module {
                pub fn new() -> __builders::Root {
                    __builders::Root::new(::std::vec::Vec::new())
                }

                #[doc(hidden)]
                pub mod __builders {
                    #[derive(Debug, Clone)]
                    pub struct Root {
                        slots: ::std::vec::Vec<crate::bus::topic::Slot>,
                    }

                    impl Root {
                        pub(super) fn new(
                            slots: ::std::vec::Vec<crate::bus::topic::Slot>,
                        ) -> Self {
                            Self { slots }
                        }
                    }

                    impl Root {
                        #(#root_methods)*
                    }

                    #(#root_modules)*
                }
            }
        }
    }
}

fn modules_for(items: &[Item], template_prefix: &str, schema_prefix: &str) -> Vec<TokenStream2> {
    items
        .iter()
        .filter_map(|item| match item {
            Item::Node(node) => Some(module_for(node, template_prefix, schema_prefix)),
            Item::Leaf(_) => None,
        })
        .collect()
}

fn module_for(node: &Node, template_prefix: &str, schema_prefix: &str) -> TokenStream2 {
    let name = &node.name;
    let (template, schema) = node_prefix(node, template_prefix, schema_prefix);
    let methods = methods_for(&node.children, &template, &schema);
    let modules = modules_for(&node.children, &template, &schema);

    quote! {
        pub mod #name {
            #[derive(Debug, Clone)]
            pub struct Builder {
                slots: ::std::vec::Vec<crate::bus::topic::Slot>,
            }

            impl Builder {
                pub(super) fn new(
                    slots: ::std::vec::Vec<crate::bus::topic::Slot>,
                ) -> Self {
                    Self { slots }
                }
            }

            impl Builder {
                #(#methods)*
            }

            #(#modules)*
        }
    }
}

fn methods_for(items: &[Item], template_prefix: &str, schema_prefix: &str) -> Vec<TokenStream2> {
    items
        .iter()
        .map(|item| match item {
            Item::Node(node) => child_method_for(node),
            Item::Leaf(leaf) => leaf_method_for(leaf, template_prefix, schema_prefix),
        })
        .collect()
}

fn child_method_for(node: &Node) -> TokenStream2 {
    let name = &node.name;

    if node.has_id {
        let any_name = format_ident!("{}_any", name);
        quote! {
            pub fn #name(
                mut self,
                id: impl Into<::std::borrow::Cow<'static, str>>,
            ) -> #name::Builder {
                self.slots.push(crate::bus::topic::Slot::Bound(id.into()));
                #name::Builder::new(self.slots)
            }

            pub fn #any_name(mut self) -> #name::Builder {
                self.slots.push(crate::bus::topic::Slot::Any);
                #name::Builder::new(self.slots)
            }
        }
    } else {
        quote! {
            pub fn #name(self) -> #name::Builder {
                #name::Builder::new(self.slots)
            }
        }
    }
}

fn leaf_method_for(leaf: &Leaf, template_prefix: &str, schema_prefix: &str) -> TokenStream2 {
    let name = &leaf.name;
    let segment = ident_segment(name);
    let template = syn::LitStr::new(&join_path(template_prefix, &segment), name.span());
    let schema = syn::LitStr::new(&join_path(schema_prefix, &segment), name.span());
    let version = &leaf.version;

    match &leaf.interaction {
        Interaction::PubSub(payload) => quote! {
            pub fn #name(
                self,
            ) -> crate::bus::topic::Topic<crate::bus::topic::PubSub<#payload>> {
                crate::bus::topic::Topic::new(#template, #schema, #version, self.slots)
            }
        },
        Interaction::Query { request, response } => quote! {
            pub fn #name(
                self,
            ) -> crate::bus::topic::Topic<crate::bus::topic::Query<#request, #response>> {
                crate::bus::topic::Topic::new(#template, #schema, #version, self.slots)
            }
        },
    }
}

fn node_prefix(node: &Node, template_prefix: &str, schema_prefix: &str) -> (String, String) {
    let segment = ident_segment(&node.name);
    let schema = join_path(schema_prefix, &segment);
    let template = if node.has_id {
        join_path(&join_path(template_prefix, &segment), "*")
    } else {
        join_path(template_prefix, &segment)
    };
    (template, schema)
}

fn ident_segment(ident: &Ident) -> String {
    ident.to_string().trim_start_matches("r#").to_string()
}

fn join_path(prefix: &str, segment: &str) -> String {
    if prefix.is_empty() {
        segment.to_string()
    } else {
        format!("{prefix}/{segment}")
    }
}
