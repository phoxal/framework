use std::borrow::Cow;
use std::marker::PhantomData;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PubSub<T>(PhantomData<T>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Query<Req, Resp>(PhantomData<(Req, Resp)>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    Any,
    Bound(Cow<'static, str>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotArg {
    Any,
    Bound(Cow<'static, str>),
}

pub const ANY: SlotArg = SlotArg::Any;

impl From<SlotArg> for Slot {
    fn from(value: SlotArg) -> Self {
        match value {
            SlotArg::Any => Self::Any,
            SlotArg::Bound(value) => Self::Bound(value),
        }
    }
}

impl From<&str> for SlotArg {
    fn from(value: &str) -> Self {
        if value == "*" {
            Self::Any
        } else {
            Self::Bound(Cow::Owned(value.to_string()))
        }
    }
}

impl From<&String> for SlotArg {
    fn from(value: &String) -> Self {
        value.as_str().into()
    }
}

impl From<String> for SlotArg {
    fn from(value: String) -> Self {
        if value == "*" {
            Self::Any
        } else {
            Self::Bound(Cow::Owned(value))
        }
    }
}

impl From<Cow<'static, str>> for SlotArg {
    fn from(value: Cow<'static, str>) -> Self {
        if value.as_ref() == "*" {
            Self::Any
        } else {
            Self::Bound(value)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topic<Kind> {
    template: &'static str,
    schema: &'static str,
    slots: Vec<Slot>,
    _kind: PhantomData<Kind>,
}

impl<Kind> Topic<Kind> {
    pub fn new(template: &'static str, schema: &'static str, slots: Vec<Slot>) -> Self {
        Self {
            template,
            schema,
            slots,
            _kind: PhantomData,
        }
    }

    pub fn key(&self) -> Cow<'static, str> {
        if self.slots.is_empty() {
            return Cow::Borrowed(self.template);
        }

        let mut slots = self.slots.iter();
        let mut parts = self.template.split('*').peekable();
        let mut key = String::with_capacity(self.template.len());

        while let Some(part) = parts.next() {
            key.push_str(part);
            if parts.peek().is_some() {
                match slots.next() {
                    Some(Slot::Any) => key.push('*'),
                    Some(Slot::Bound(value)) => key.push_str(value),
                    None => key.push('*'),
                }
            }
        }

        Cow::Owned(key)
    }

    pub fn schema(&self) -> &'static str {
        self.schema
    }

    pub fn is_concrete(&self) -> bool {
        self.slots.iter().all(|slot| matches!(slot, Slot::Bound(_))) && !self.key().contains('*')
    }

    pub fn publish_key(&self) -> crate::bus::Result<Cow<'static, str>> {
        let key = self.key();
        if !self.is_concrete() || key.contains('*') {
            Err(crate::bus::Error::InvalidTopic(
                "topic has wildcard '*' slot(s); bind all ids before publishing".to_string(),
            ))
        } else {
            Ok(key)
        }
    }
}

#[macro_export]
macro_rules! topic_tree {
    ($visibility:vis mod $module:ident; $($items:tt)*) => {
        $visibility mod $module {
            pub fn new() -> __builders::Root {
                __builders::Root::new(::std::vec::Vec::new())
            }

            #[doc(hidden)]
            pub mod __builders {
                #[derive(Debug, Clone)]
                pub struct Root {
                    slots: ::std::vec::Vec<$crate::bus::topic::Slot>,
                }

                impl Root {
                    pub(super) fn new(
                        slots: ::std::vec::Vec<$crate::bus::topic::Slot>,
                    ) -> Self {
                        Self { slots }
                    }
                }

                impl Root {
                    $crate::topic_tree!(@methods [] [] $($items)*);
                }

                $crate::topic_tree!(@modules [] [] $($items)*);
            }
        }
    };

    (@methods [$($template:expr),*] [$($schema:expr),*]) => {};

    (
        @methods [$($template:expr),*] [$($schema:expr),*]
        pubsub r#static: $payload:ty;
        $($rest:tt)*
    ) => {
        $crate::topic_tree!(
            @pubsub_method r#static, "static", [$($template),*] [$($schema),*] $payload
        );
        $crate::topic_tree!(@methods [$($template),*] [$($schema),*] $($rest)*);
    };

    (
        @methods [$($template:expr),*] [$($schema:expr),*]
        pubsub $leaf:ident: $payload:ty;
        $($rest:tt)*
    ) => {
        $crate::topic_tree!(
            @pubsub_method $leaf, stringify!($leaf), [$($template),*] [$($schema),*] $payload
        );
        $crate::topic_tree!(@methods [$($template),*] [$($schema),*] $($rest)*);
    };

    (
        @methods [$($template:expr),*] [$($schema:expr),*]
        query $leaf:ident: $request:ty => $response:ty;
        $($rest:tt)*
    ) => {
        $crate::topic_tree!(
            @query_method $leaf, stringify!($leaf), [$($template),*] [$($schema),*]
            $request => $response
        );
        $crate::topic_tree!(@methods [$($template),*] [$($schema),*] $($rest)*);
    };

    (
        @methods [$($template:expr),*] [$($schema:expr),*]
        $name:ident(id) { $($children:tt)* }
        $($rest:tt)*
    ) => {
        pub fn $name(
            mut self,
            id: impl ::std::convert::Into<$crate::bus::topic::SlotArg>,
        ) -> $name::Builder {
            self.slots.push($crate::bus::topic::Slot::from(id.into()));
            $name::Builder::new(self.slots)
        }

        $crate::topic_tree!(@methods [$($template),*] [$($schema),*] $($rest)*);
    };

    (
        @methods [$($template:expr),*] [$($schema:expr),*]
        $name:ident { $($children:tt)* }
        $($rest:tt)*
    ) => {
        pub fn $name(self) -> $name::Builder {
            $name::Builder::new(self.slots)
        }

        $crate::topic_tree!(@methods [$($template),*] [$($schema),*] $($rest)*);
    };

    (
        @pubsub_method $method:ident, $segment:expr,
        [$($template:expr),*] [$($schema:expr),*]
        $payload:ty
    ) => {
        pub fn $method(
            self,
        ) -> $crate::bus::topic::Topic<$crate::bus::topic::PubSub<$payload>> {
            $crate::bus::topic::Topic::new(
                concat!($($template, "/",)* $segment),
                concat!($($schema, "/",)* $segment),
                self.slots,
            )
        }
    };

    (
        @query_method $method:ident, $segment:expr,
        [$($template:expr),*] [$($schema:expr),*]
        $request:ty => $response:ty
    ) => {
        pub fn $method(
            self,
        ) -> $crate::bus::topic::Topic<$crate::bus::topic::Query<$request, $response>> {
            $crate::bus::topic::Topic::new(
                concat!($($template, "/",)* $segment),
                concat!($($schema, "/",)* $segment),
                self.slots,
            )
        }
    };

    (@modules [$($template:expr),*] [$($schema:expr),*]) => {};

    (
        @modules [$($template:expr),*] [$($schema:expr),*]
        pubsub r#static: $payload:ty;
        $($rest:tt)*
    ) => {
        $crate::topic_tree!(@modules [$($template),*] [$($schema),*] $($rest)*);
    };

    (
        @modules [$($template:expr),*] [$($schema:expr),*]
        pubsub $leaf:ident: $payload:ty;
        $($rest:tt)*
    ) => {
        $crate::topic_tree!(@modules [$($template),*] [$($schema),*] $($rest)*);
    };

    (
        @modules [$($template:expr),*] [$($schema:expr),*]
        query $leaf:ident: $request:ty => $response:ty;
        $($rest:tt)*
    ) => {
        $crate::topic_tree!(@modules [$($template),*] [$($schema),*] $($rest)*);
    };

    (
        @modules [$($template:expr),*] [$($schema:expr),*]
        $name:ident(id) { $($children:tt)* }
        $($rest:tt)*
    ) => {
        $crate::topic_tree!(@module $name [$($template),*] [$($schema),*] [id] { $($children)* });
        $crate::topic_tree!(@modules [$($template),*] [$($schema),*] $($rest)*);
    };

    (
        @modules [$($template:expr),*] [$($schema:expr),*]
        $name:ident { $($children:tt)* }
        $($rest:tt)*
    ) => {
        $crate::topic_tree!(@module $name [$($template),*] [$($schema),*] [] { $($children)* });
        $crate::topic_tree!(@modules [$($template),*] [$($schema),*] $($rest)*);
    };

    (@module $name:ident [$($template:expr),*] [$($schema:expr),*] [id] { $($children:tt)* }) => {
        pub mod $name {
            #[derive(Debug, Clone)]
            pub struct Builder {
                slots: ::std::vec::Vec<$crate::bus::topic::Slot>,
            }

            impl Builder {
                pub(super) fn new(
                    slots: ::std::vec::Vec<$crate::bus::topic::Slot>,
                ) -> Self {
                    Self { slots }
                }
            }

            impl Builder {
                $crate::topic_tree!(
                    @methods [$($template,)* stringify!($name), "*"] [$($schema,)* stringify!($name)]
                    $($children)*
                );
            }

            $crate::topic_tree!(
                @modules [$($template,)* stringify!($name), "*"] [$($schema,)* stringify!($name)]
                $($children)*
            );
        }
    };

    (@module $name:ident [$($template:expr),*] [$($schema:expr),*] [] { $($children:tt)* }) => {
        pub mod $name {
            #[derive(Debug, Clone)]
            pub struct Builder {
                slots: ::std::vec::Vec<$crate::bus::topic::Slot>,
            }

            impl Builder {
                pub(super) fn new(
                    slots: ::std::vec::Vec<$crate::bus::topic::Slot>,
                ) -> Self {
                    Self { slots }
                }
            }

            impl Builder {
                $crate::topic_tree!(
                    @methods [$($template,)* stringify!($name)] [$($schema,)* stringify!($name)]
                    $($children)*
                );
            }

            $crate::topic_tree!(
                @modules [$($template,)* stringify!($name)] [$($schema,)* stringify!($name)]
                $($children)*
            );
        }
    };
}

#[cfg(test)]
mod tests {
    use crate::api::{
        asset,
        component::capability::{gnss, motor},
        drive, simulation, topic,
    };
    use crate::bus::topic::{ANY, PubSub, Query, Topic};

    #[test]
    fn topic_builder_keys_match_tree_paths() {
        assert_eq!(topic::new().drive().target().key(), "drive/target");
        assert_eq!(
            topic::new()
                .component("base")
                .motor("left_wheel")
                .command()
                .key(),
            "component/base/motor/left_wheel/command"
        );
        assert_eq!(
            topic::new().component(ANY).motor(ANY).command().key(),
            "component/*/motor/*/command"
        );
        assert_eq!(
            topic::new().component("*").motor("*").command().key(),
            "component/*/motor/*/command"
        );
        assert_eq!(topic::new().asset().get().key(), "asset/get");
        assert_eq!(topic::new().simulation().clock().key(), "simulation/clock");
        assert_eq!(
            topic::new().simulation().robot("r1").pose().key(),
            "simulation/robot/r1/pose"
        );
        assert_eq!(
            topic::new().simulation().robot(ANY).pose().key(),
            "simulation/robot/*/pose"
        );
    }

    #[test]
    fn topic_builder_schemas_elide_holes() {
        let motor = topic::new().component("base").motor("left_wheel").command();
        let gnss = topic::new().component("base").gnss("receiver").data();
        let pose = topic::new().simulation().robot("r1").pose();
        let target = topic::new().drive().target();

        assert_eq!(motor.schema(), "component/motor/command");
        assert_eq!(gnss.schema(), "component/gnss/data");
        assert_eq!(pose.schema(), "simulation/robot/pose");
        assert_eq!(target.schema(), "drive/target");
    }

    #[test]
    fn topic_publish_keys_require_bound_slots() {
        assert!(
            topic::new()
                .component(ANY)
                .motor(ANY)
                .command()
                .publish_key()
                .is_err()
        );
        assert_eq!(
            topic::new()
                .component("base")
                .motor("left_wheel")
                .command()
                .publish_key()
                .unwrap(),
            "component/base/motor/left_wheel/command"
        );
    }

    #[test]
    fn rendered_wildcards_are_not_concrete() {
        let wildcard = topic::new().component("*").motor("left").command();
        let manually_under_bound = Topic::<PubSub<motor::Command>>::new(
            "component/*/motor/*/command",
            "component/motor/command",
            Vec::new(),
        );

        assert!(!wildcard.is_concrete());
        assert!(!manually_under_bound.is_concrete());
    }

    #[test]
    fn topic_payload_types_are_carried_by_leaf_methods() {
        fn want<T>(_: Topic<PubSub<T>>) {}
        fn want_command(_: Topic<PubSub<motor::Command>>) {}
        fn want_asset_get(_: Topic<Query<asset::GetRequest, asset::GetResponse>>) {}

        want::<drive::Target>(topic::new().drive().target());
        want::<drive::State>(topic::new().drive().state());
        want::<gnss::Sample>(topic::new().component("base").gnss("receiver").data());
        want::<simulation::clock::Clock>(topic::new().simulation().clock());
        want::<simulation::pose::Pose>(topic::new().simulation().robot("r1").pose());
        want_command(topic::new().component("base").motor("left_wheel").command());
        want_asset_get(topic::new().asset().get());
    }
}
