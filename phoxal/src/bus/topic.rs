use std::borrow::Cow;
use std::marker::PhantomData;

pub use phoxal_macros::topic_tree;

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
pub struct Topic<Kind> {
    template: &'static str,
    schema: &'static str,
    version: u32,
    slots: Vec<Slot>,
    _kind: PhantomData<Kind>,
}

impl<Kind> Topic<Kind> {
    pub fn new(
        template: &'static str,
        schema: &'static str,
        version: u32,
        slots: Vec<Slot>,
    ) -> Self {
        Self {
            template,
            schema,
            version,
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

    pub const fn version(&self) -> u32 {
        self.version
    }

    pub fn is_concrete(&self) -> bool {
        self.slots.iter().all(|slot| matches!(slot, Slot::Bound(_)))
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

#[cfg(test)]
mod tests {
    use crate::api::v1::{asset, component, drive, simulation, topic};
    use crate::bus::topic::{PubSub, Query, Topic};

    #[test]
    fn topic_builder_keys_match_tree_paths() {
        assert_eq!(topic::new().v1().drive().target().key(), "v1/drive/target");
        assert_eq!(
            topic::new().v1().component("base").motor().key(),
            "v1/component/base/motor"
        );
        assert_eq!(
            topic::new().v1().component_any().motor().key(),
            "v1/component/*/motor"
        );
        assert_eq!(topic::new().v1().asset().get().key(), "v1/asset/get");
        assert_eq!(
            topic::new().v1().simulation().clock().key(),
            "v1/simulation/clock"
        );
        assert_eq!(
            topic::new().v1().simulation().robot("r1").pose().key(),
            "v1/simulation/robot/r1/pose"
        );
        assert_eq!(
            topic::new().v1().simulation().robot_any().pose().key(),
            "v1/simulation/robot/*/pose"
        );
    }

    #[test]
    fn topic_builder_schemas_elide_holes() {
        let motor = topic::new().v1().component("base").motor();
        let gnss = topic::new().v1().component("base").gnss();
        let pose = topic::new().v1().simulation().robot("r1").pose();
        let target = topic::new().v1().drive().target();

        assert_eq!(motor.schema(), "v1/component/motor");
        assert_eq!(gnss.schema(), "v1/component/gnss");
        assert_eq!(pose.schema(), "v1/simulation/robot/pose");
        assert_eq!(target.schema(), "v1/drive/target");
        assert_eq!(motor.version(), 1);
        assert_eq!(gnss.version(), 1);
        assert_eq!(pose.version(), 1);
        assert_eq!(target.version(), 1);
    }

    #[test]
    fn topic_publish_keys_require_bound_slots() {
        assert!(
            topic::new()
                .v1()
                .component_any()
                .motor()
                .publish_key()
                .is_err()
        );
        assert_eq!(
            topic::new()
                .v1()
                .component("base")
                .motor()
                .publish_key()
                .unwrap(),
            "v1/component/base/motor"
        );
    }

    #[test]
    fn topic_payload_types_are_carried_by_leaf_methods() {
        fn want<T>(_: Topic<PubSub<T>>) {}
        fn want_command(_: Topic<PubSub<component::motor::Command>>) {}
        fn want_asset_get(_: Topic<Query<asset::get::Request, asset::get::Response>>) {}

        want::<drive::target::Target>(topic::new().v1().drive().target());
        want::<drive::state::State>(topic::new().v1().drive().state());
        want::<component::gnss::Sample>(topic::new().v1().component("base").gnss());
        want::<simulation::clock::Clock>(topic::new().v1().simulation().clock());
        want::<simulation::robot::pose::Pose>(topic::new().v1().simulation().robot("r1").pose());
        want_command(topic::new().v1().component("base").motor());
        want_asset_get(topic::new().v1().asset().get());
    }
}
