//! Typed key identity across erased generated activation bindings.
use super::TransportValue;
use std::any::Any;

/// An owned activation key whose equality retains its original Rust type.
/// Keys stay local to one Runtime and require no wire representation.
pub struct ActivationKey {
    value: TransportValue,
    clone_value: fn(&dyn Any) -> TransportValue,
    equal: fn(&dyn Any, &dyn Any) -> bool,
}

impl ActivationKey {
    /// Erases a cloneable equality key for generated activation dispatch.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "the private clone function is paired with its concrete key type"
    )]
    pub fn new<K: Clone + Eq + Send + 'static>(key: K) -> Self {
        Self {
            value: Box::new(key),
            clone_value: |value| {
                // This function is stored with, and only applied to, its K value.
                Box::new(
                    value
                        .downcast_ref::<K>()
                        .expect("matching key clone type")
                        .clone(),
                )
            },
            equal: |left, right| match (left.downcast_ref::<K>(), right.downcast_ref::<K>()) {
                (Some(left), Some(right)) => left == right,
                _ => false,
            },
        }
    }

    pub(crate) fn matches_value(&self, value: &dyn Any) -> bool {
        (self.equal)(&*self.value, value)
    }

    pub(crate) fn into_value(self) -> TransportValue {
        self.value
    }
}

impl Clone for ActivationKey {
    fn clone(&self) -> Self {
        Self {
            value: (self.clone_value)(&*self.value),
            clone_value: self.clone_value,
            equal: self.equal,
        }
    }
}

impl PartialEq for ActivationKey {
    fn eq(&self, other: &Self) -> bool {
        (self.equal)(&*self.value, &*other.value)
    }
}

impl Eq for ActivationKey {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_preserves_type_and_value_without_serialization() {
        let key = ActivationKey::new(7_u64);
        assert!(key == key.clone());
        assert!(key != ActivationKey::new(8_u64));
        assert!(key != ActivationKey::new(7_u32));
        assert_eq!(*key.into_value().downcast::<u64>().unwrap(), 7);
    }
}
