//! How a driven component instance is physically wired to the machine.
//!
//! This is the framework's own half of a `driver:` block. The other half is the
//! driver binary's `config`, which the model carries as opaque JSON because its
//! shape belongs to that binary; the connection does not, because the framework
//! validates it, an editor completes it, and a driver reads it as a typed value
//! rather than as a map it has to interpret for itself. One slot per owner.
//!
//! The vocabulary is a closed enum on purpose: a connection kind is a hardware
//! transport the framework knows how to talk about, not an open extension
//! point. [`Connection`] carries the kind and its payload as one internally
//! tagged value, exactly as an authored document spells it.
//!
//! # Reaching a payload from a driver
//!
//! `#[phoxal::driver(connection = serial)]` declares the one kind a driver
//! accepts. The declared payload type - [`Serial`] here - becomes the driver's
//! `ctx.connection()` return type, so the driver reads `port` and `baud` off a
//! struct instead of matching a variant it already knows it will get. A driver
//! that declares nothing gets the whole [`Connection`] and decides for itself.
//!
//! [`ConnectionPayload`] is what makes those two cases one signature: it is
//! implemented for each payload struct *and* for [`Connection`] itself, so the
//! declared type is the single source of both the accepted kind
//! ([`ConnectionPayload::KIND`]) and the conversion the runner performs.

use serde::{Deserialize, Serialize};

/// The sealing boundary for [`ConnectionPayload`].
///
/// Sealed for the same reason the role markers are (see
/// `phoxal::participant::surface`): the set of connection payloads is the
/// framework's closed vocabulary, and a downstream `impl ConnectionPayload for
/// MyThing` would make `#[phoxal::driver(connection = …)]`'s promise - that
/// `ctx.connection()` yields the kind the manifest actually authored - a claim
/// the framework no longer decides. The module is `#[doc(hidden)]` rather than
/// private because a role attribute's expansion names the trait from the
/// participant's own crate; it closes the accidental route, not a capability
/// boundary.
#[doc(hidden)]
pub mod sealing {
    pub trait Sealed {}
}

/// The hardware connection an authored `driver.connection` block states.
///
/// The wire form is internally tagged: `{"type": "can", "bus": 0, "node_id": 1}`
/// is one connection, tag and payload in one map, which is how a robot document
/// has always spelled it.
#[derive(
    phoxal_macros::DescribeWire,
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Connection {
    /// CAN bus connection.
    Can(Can),
    /// I2C connection.
    I2c(I2c),
    /// SPI connection.
    Spi(Spi),
    /// Serial port connection (RS-232/RS-485).
    Serial(Serial),
    /// UART connection (distinct from Serial for hardware-specific drivers).
    Uart(Uart),
    /// USB connection.
    Usb(Usb),
    /// GPIO pins.
    Gpio(Gpio),
}

impl Connection {
    /// Which kind this connection is, with the payload dropped.
    #[must_use]
    pub const fn kind(&self) -> ConnectionKind {
        match self {
            Self::Can(_) => ConnectionKind::Can,
            Self::I2c(_) => ConnectionKind::I2c,
            Self::Spi(_) => ConnectionKind::Spi,
            Self::Serial(_) => ConnectionKind::Serial,
            Self::Uart(_) => ConnectionKind::Uart,
            Self::Usb(_) => ConnectionKind::Usb,
            Self::Gpio(_) => ConnectionKind::Gpio,
        }
    }
}

/// A connection kind with no payload: what a driver *declares* it accepts, and
/// what the embedded participant metadata carries so build tooling can compare
/// a binary's declaration against an authored document without parsing either.
#[derive(
    phoxal_macros::DescribeWire, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    Can,
    I2c,
    Spi,
    Serial,
    Uart,
    Usb,
    Gpio,
}

impl ConnectionKind {
    /// The wire token for this kind, identical to the `snake_case` rename serde
    /// derives. Const so the role macro can splice it into the embedded
    /// participant-metadata document during const-eval.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Can => "can",
            Self::I2c => "i2c",
            Self::Spi => "spi",
            Self::Serial => "serial",
            Self::Uart => "uart",
            Self::Usb => "usb",
            Self::Gpio => "gpio",
        }
    }

    /// Every kind, so the wire declaration and `as_str` cannot cover different
    /// sets.
    pub const ALL: [Self; 7] = [
        Self::Can,
        Self::I2c,
        Self::Spi,
        Self::Serial,
        Self::Uart,
        Self::Usb,
        Self::Gpio,
    ];
}

impl std::fmt::Display for ConnectionKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An authored connection is not the kind the driver declared it accepts.
///
/// This is a launch mistake in the robot document, not a runtime condition: the
/// binary the author wired to this component instance speaks a different
/// transport than the instance was authored with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "this driver accepts a {expected} connection, but the component instance authors {authored}"
)]
pub struct ConnectionKindMismatch {
    /// The one kind the driver declared.
    pub expected: ConnectionKind,
    /// The kind the authored `driver.connection` block actually carries.
    pub authored: ConnectionKind,
}

/// What a driver may declare as its accepted connection: one payload struct, or
/// [`Connection`] itself for a driver that accepts every kind.
///
/// The declared type is the single source of both facts the framework needs -
/// the kind to enforce ([`Self::KIND`]) and the value to hand the driver
/// ([`Self::from_connection`]) - so the two can never disagree.
pub trait ConnectionPayload: sealing::Sealed + Sized {
    /// The one kind this type accepts, or `None` when it accepts every kind.
    const KIND: Option<ConnectionKind>;

    /// Take this payload out of an authored connection.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionKindMismatch`] when the authored connection is a
    /// different kind than [`Self::KIND`].
    fn from_connection(connection: Connection) -> Result<Self, ConnectionKindMismatch>;
}

impl sealing::Sealed for Connection {}

impl ConnectionPayload for Connection {
    // A driver that declared no kind accepts whatever the document authored,
    // which is exactly what "no declaration" has to mean.
    const KIND: Option<ConnectionKind> = None;

    fn from_connection(connection: Connection) -> Result<Self, ConnectionKindMismatch> {
        Ok(connection)
    }
}

/// Declare one payload struct's membership in the closed vocabulary: its kind,
/// its seal, and the one variant it comes out of. Written once rather than
/// seven times because every line of it is decided by the variant name, and a
/// hand-copied version is seven chances to pair a kind with the wrong payload.
macro_rules! payloads {
    ($($variant:ident),+ $(,)?) => {
        $(
            impl sealing::Sealed for $variant {}

            impl ConnectionPayload for $variant {
                const KIND: Option<ConnectionKind> = Some(ConnectionKind::$variant);

                fn from_connection(
                    connection: Connection,
                ) -> Result<Self, ConnectionKindMismatch> {
                    match connection {
                        Connection::$variant(payload) => Ok(payload),
                        other => Err(ConnectionKindMismatch {
                            expected: ConnectionKind::$variant,
                            authored: other.kind(),
                        }),
                    }
                }
            }
        )+
    };
}

payloads!(Can, I2c, Spi, Serial, Uart, Usb, Gpio);

/// CAN bus connection.
#[derive(
    phoxal_macros::DescribeWire,
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct Can {
    pub bus: u8,
    pub node_id: u8,
}

/// I2C connection.
#[derive(
    phoxal_macros::DescribeWire,
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct I2c {
    pub bus: u8,
    pub address: u16,
}

/// SPI connection.
#[derive(
    phoxal_macros::DescribeWire,
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct Spi {
    pub bus: u8,
    pub chip_select: u8,
}

/// Serial port connection (RS-232/RS-485).
#[derive(
    phoxal_macros::DescribeWire,
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct Serial {
    pub port: String,
    pub baud: u32,
}

/// UART connection (distinct from Serial for hardware-specific drivers).
#[derive(
    phoxal_macros::DescribeWire,
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct Uart {
    pub port: String,
    pub baud_rate: u32,
}

/// USB connection.
#[derive(
    phoxal_macros::DescribeWire,
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct Usb {
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
}

/// GPIO pins.
#[derive(
    phoxal_macros::DescribeWire,
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct Gpio {
    pub chip: String,
    pub pins: Vec<GpioPin>,
}

/// One GPIO line: which line on the chip it is, which way it is driven, and
/// whether it reads and drives inverted.
#[derive(
    phoxal_macros::DescribeWire,
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct GpioPin {
    pub line: u16,
    pub direction: GpioDirection,
    #[serde(default)]
    pub active_low: bool,
}

/// Which way one GPIO line is driven.
#[derive(
    phoxal_macros::DescribeWire,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GpioDirection {
    Input,
    Output,
}

#[cfg(test)]
mod tests {
    use super::{
        Can, Connection, ConnectionKind, ConnectionKindMismatch, ConnectionPayload, Gpio,
        GpioDirection, GpioPin, Serial,
    };

    fn can() -> Connection {
        Connection::Can(Can { bus: 0, node_id: 1 })
    }

    /// The authored spelling is one map carrying the tag beside the payload
    /// fields, which is the form every robot document already uses.
    #[test]
    fn a_connection_is_one_internally_tagged_map() {
        let json = serde_json::to_value(can()).expect("a connection serializes");
        assert_eq!(
            json,
            serde_json::json!({"type": "can", "bus": 0, "node_id": 1})
        );
        let decoded: Connection = serde_json::from_value(json).expect("its own output parses");
        assert_eq!(decoded, can());
    }

    /// An internally tagged newtype variant delegates the payload's own field
    /// rules, so `deny_unknown_fields` on the payload is what rejects a
    /// misspelled key inside a connection block rather than silently dropping
    /// it. This is the pairing the split has to preserve, so it is checked
    /// rather than assumed.
    #[test]
    fn an_unknown_key_inside_a_connection_is_rejected() {
        let error = serde_json::from_value::<Connection>(
            serde_json::json!({"type": "can", "bus": 0, "node_id": 1, "nod_id": 2}),
        )
        .expect_err("an undeclared key inside a connection must not parse");
        assert!(
            format!("{error}").contains("unknown field `nod_id`"),
            "{error}"
        );
    }

    #[test]
    fn every_kind_round_trips_through_its_wire_token() {
        for kind in ConnectionKind::ALL {
            let json = serde_json::to_string(&kind).expect("a unit variant serializes");
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            let decoded: ConnectionKind =
                serde_json::from_str(&json).expect("its own token parses back");
            assert_eq!(decoded, kind);
        }
    }

    /// The payload type a driver declares decides both facts, so a declaration
    /// cannot promise one kind and hand over another.
    #[test]
    fn a_declared_payload_accepts_only_its_own_kind() {
        assert_eq!(
            <Serial as ConnectionPayload>::KIND,
            Some(ConnectionKind::Serial)
        );
        let serial = Connection::Serial(Serial {
            port: "/dev/ttyUSB0".to_owned(),
            baud: 115_200,
        });
        assert_eq!(serial.kind(), ConnectionKind::Serial);
        assert_eq!(
            Serial::from_connection(serial).expect("the declared kind converts"),
            Serial {
                port: "/dev/ttyUSB0".to_owned(),
                baud: 115_200,
            }
        );
        assert_eq!(
            Serial::from_connection(can()).expect_err("another kind must not convert"),
            ConnectionKindMismatch {
                expected: ConnectionKind::Serial,
                authored: ConnectionKind::Can,
            }
        );
    }

    /// An undeclared driver takes the whole vocabulary and decides for itself,
    /// which is what makes one `ctx.connection()` signature serve both cases.
    #[test]
    fn the_undeclared_case_is_the_enum_itself() {
        assert_eq!(<Connection as ConnectionPayload>::KIND, None);
        assert_eq!(
            Connection::from_connection(can()).expect("the enum accepts every kind"),
            can()
        );
    }

    /// `active_low` defaults and the pin list is ordinary, so a GPIO block
    /// keeps parsing exactly as authored documents spell it today.
    #[test]
    fn a_gpio_block_keeps_its_authored_spelling() {
        let connection: Connection = serde_json::from_value(serde_json::json!({
            "type": "gpio",
            "chip": "gpiochip0",
            "pins": [{"line": 1, "direction": "output"}],
        }))
        .expect("a gpio connection parses");
        assert_eq!(
            connection,
            Connection::Gpio(Gpio {
                chip: "gpiochip0".to_owned(),
                pins: vec![GpioPin {
                    line: 1,
                    direction: GpioDirection::Output,
                    active_low: false,
                }],
            })
        );
    }
}
