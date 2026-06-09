/// Defines a uniform topic leaf exposed by owner-local API crates.
#[macro_export]
macro_rules! topic_leaf {
    (
        pubsub $module:ident {
            path: $path:literal,
            payload: $payload:ty
        }
    ) => {
        $crate::topic_leaf! {
            pubsub $module() {
                path: $path,
                payload: $payload
            }
        }
    };
    (
        pubsub $module:ident($($key:ident: $kty:ty),*) {
            path: $path:literal,
            payload: $payload:ty
        }
    ) => {
        pub mod $module {
            use super::*;

            pub fn path($($key: $kty),*) -> String {
                format!($path $(, $key)*)
            }

            pub fn topic(bus: &$crate::bus::Bus $(, $key: $kty)*) -> String {
                bus.topic(&path($($key),*))
            }

            pub fn publisher<'a>(
                bus: &'a $crate::bus::Bus $(, $key: $kty)*
            ) -> $crate::bus::Result<
                $crate::bus::zenoh::TypedPublisherBuilder<
                    'a,
                    'static,
                    $crate::bus::pubsub::Stamped<$payload>,
                >,
            > {
                $crate::bus::pubsub::publisher_builder(bus, &path($($key),*))
            }

            pub fn subscriber_builder<'a>(
                bus: &'a $crate::bus::Bus $(, $key: $kty)*
            ) -> $crate::bus::zenoh::TypedSubscriberBuilder<
                'a,
                'static,
                $crate::bus::pubsub::Stamped<$payload>,
            > {
                $crate::bus::pubsub::subscriber_builder(bus, &path($($key),*))
            }

            pub fn schema_name() -> &'static str {
                <$payload as $crate::bus::zenoh::TypedSchema>::SCHEMA_NAME
            }

            pub fn schema_version() -> u32 {
                <$payload as $crate::bus::zenoh::TypedSchema>::SCHEMA_VERSION
            }
        }
    };
    (
        query $module:ident {
            path: $path:literal,
            request: $request:ty,
            response: $response:ty
        }
    ) => {
        $crate::topic_leaf! {
            query $module() {
                path: $path,
                request: $request,
                response: $response
            }
        }
    };
    (
        query $module:ident($($key:ident: $kty:ty),*) {
            path: $path:literal,
            request: $request:ty,
            response: $response:ty
        }
    ) => {
        pub mod $module {
            use super::*;

            pub fn path($($key: $kty),*) -> String {
                format!($path $(, $key)*)
            }

            pub fn topic(bus: &$crate::bus::Bus $(, $key: $kty)*) -> String {
                bus.topic(&path($($key),*))
            }

            pub fn get_builder<'a>(
                bus: &'a $crate::bus::Bus,
                $($key: $kty,)*
                request: &'a $request,
            ) -> $crate::bus::zenoh::TypedGetBuilder<'a, 'static, $response> {
                $crate::bus::query::get_builder(bus, &path($($key),*), request)
            }

            pub fn queryable_builder(
                bus: &$crate::bus::Bus $(, $key: $kty)*
            ) -> $crate::bus::Result<
                $crate::bus::zenoh::TypedQueryableBuilder<
                    '_,
                    'static,
                    $request,
                    $response,
                >,
            > {
                $crate::bus::query::queryable_builder(bus, &path($($key),*))
            }

            pub fn request_schema_name() -> &'static str {
                <$request as $crate::bus::zenoh::TypedSchema>::SCHEMA_NAME
            }

            pub fn request_schema_version() -> u32 {
                <$request as $crate::bus::zenoh::TypedSchema>::SCHEMA_VERSION
            }

            pub fn response_schema_name() -> &'static str {
                <$response as $crate::bus::zenoh::TypedSchema>::SCHEMA_NAME
            }

            pub fn response_schema_version() -> u32 {
                <$response as $crate::bus::zenoh::TypedSchema>::SCHEMA_VERSION
            }
        }
    };
}

/// Defines the standard stamped pub/sub topic leaf exposed by owner-local API crates.
#[macro_export]
macro_rules! pubsub_leaf {
    ($module:ident, $topic:ident, $payload:ident) => {
        pub mod $module {
            use super::*;
            use $crate::bus::pubsub::Stamped;
            use $crate::bus::zenoh::{TypedPublisherBuilder, TypedSubscriberBuilder};

            pub const TOPIC: &str = $topic;

            pub fn topic(bus: &$crate::bus::Bus) -> String {
                bus.topic(TOPIC)
            }

            pub fn publisher(
                bus: &$crate::bus::Bus,
            ) -> $crate::bus::Result<TypedPublisherBuilder<'_, 'static, Stamped<$payload>>> {
                $crate::bus::pubsub::publisher_builder(bus, TOPIC)
            }

            pub fn subscriber_builder(
                bus: &$crate::bus::Bus,
            ) -> TypedSubscriberBuilder<'_, 'static, Stamped<$payload>> {
                $crate::bus::pubsub::subscriber_builder(bus, TOPIC)
            }
        }
    };
}

/// Defines the standard request/queryable topic leaf exposed by owner-local API crates.
#[macro_export]
macro_rules! query_leaf {
    ($module:ident, $topic:ident, $request:ty, $response:ty) => {
        pub mod $module {
            use super::*;

            pub const TOPIC: &str = $topic;

            pub fn topic(bus: &$crate::bus::Bus) -> String {
                bus.topic(TOPIC)
            }

            pub fn get_builder<'a>(
                bus: &'a $crate::bus::Bus,
                request: &'a $request,
            ) -> $crate::bus::zenoh::TypedGetBuilder<'a, 'static, $response> {
                $crate::bus::query::get_builder(bus, TOPIC, request)
            }

            pub fn queryable_builder(
                bus: &$crate::bus::Bus,
            ) -> $crate::bus::Result<
                $crate::bus::zenoh::TypedQueryableBuilder<'_, 'static, $request, $response>,
            > {
                $crate::bus::query::queryable_builder(bus, TOPIC)
            }
        }
    };
}

/// Defines the transparent map-tile request schema wrapper used by query API leaves.
#[macro_export]
macro_rules! request_schema {
    ($name:ident, $schema:literal) => {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub MapTileRequest);

        impl TypedSchema for $name {
            const SCHEMA_NAME: &'static str = $schema;
            const SCHEMA_VERSION: u32 = 1;
        }
    };
}
