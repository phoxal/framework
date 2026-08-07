//! Every internal runtime-metric variant `phoxal-bus` declares has somewhere to
//! land on the wire.
//!
//! `phoxal-bus` owns the internal accounting vocabulary and this crate owns the
//! serialized enums the supervisor reads; the `phoxal` engine maps one onto the
//! other when it builds a rollup. That layering is deliberate - the internal
//! vocabulary is free to change without a wire revision - but it means a variant
//! added on one side and nowhere else is invisible to an operator with nothing
//! failing to say so.
//!
//! `phoxal-bus` cannot check the pairing: it would have to name the wire type,
//! and this crate depends on it, not the other way round. The engine can see
//! both but is two hops from either definition. This crate is one hop from both,
//! so the guard lives here.
//!
//! The matches below are exhaustive in both directions, so a variant added on
//! either side stops compiling until it is paired here, in the wire enum, and in
//! the engine's rollup.

use crate::v0_1::supervisor as wire;

/// The pairing the engine's rollup must implement. Deliberately test-local: the
/// engine owns the real conversion, and a public one here would become a second
/// place for it to live.
fn wire_direction(direction: phoxal_bus::RuntimeDirection) -> wire::RuntimeDirection {
    match direction {
        phoxal_bus::RuntimeDirection::Publish => wire::RuntimeDirection::Publish,
        phoxal_bus::RuntimeDirection::Subscribe => wire::RuntimeDirection::Subscribe,
    }
}

fn wire_buffer_kind(buffer_kind: phoxal_bus::RuntimeBufferKind) -> wire::RuntimeBufferKind {
    match buffer_kind {
        phoxal_bus::RuntimeBufferKind::Outbound => wire::RuntimeBufferKind::Outbound,
        phoxal_bus::RuntimeBufferKind::Latest => wire::RuntimeBufferKind::Latest,
        phoxal_bus::RuntimeBufferKind::Subscriber => wire::RuntimeBufferKind::Subscriber,
    }
}

#[test]
fn every_internal_direction_has_a_distinct_wire_variant() {
    const INTERNAL: [phoxal_bus::RuntimeDirection; 2] = [
        phoxal_bus::RuntimeDirection::Publish,
        phoxal_bus::RuntimeDirection::Subscribe,
    ];

    let mapped: Vec<wire::RuntimeDirection> = INTERNAL.into_iter().map(wire_direction).collect();
    let mut distinct = mapped.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        mapped.len(),
        "two internal directions collapsed onto one wire variant, so an operator \
         cannot tell them apart"
    );

    // The reverse direction, exhaustively: a wire variant that is not the image
    // of an internal one has to be one nothing measures.
    for direction in [
        wire::RuntimeDirection::Publish,
        wire::RuntimeDirection::Subscribe,
        wire::RuntimeDirection::Mixed,
    ] {
        let measured = mapped.contains(&direction);
        match direction {
            wire::RuntimeDirection::Publish | wire::RuntimeDirection::Subscribe => assert!(
                measured,
                "{direction:?} is on the wire but no internal buffer reports it"
            ),
            // `Mixed` exists only on the wire: the engine synthesizes it for the
            // bounded overflow row, which combines omitted rows from both
            // directions. No buffer is ever `Mixed`.
            wire::RuntimeDirection::Mixed => assert!(!measured),
        }
    }
}

#[test]
fn every_internal_buffer_kind_has_a_distinct_wire_variant() {
    const INTERNAL: [phoxal_bus::RuntimeBufferKind; 3] = [
        phoxal_bus::RuntimeBufferKind::Outbound,
        phoxal_bus::RuntimeBufferKind::Latest,
        phoxal_bus::RuntimeBufferKind::Subscriber,
    ];

    let mapped: Vec<wire::RuntimeBufferKind> = INTERNAL.into_iter().map(wire_buffer_kind).collect();
    let mut distinct = mapped.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        mapped.len(),
        "two internal buffer kinds collapsed onto one wire variant, so an \
         operator cannot tell which buffer is under pressure"
    );

    for buffer_kind in [
        wire::RuntimeBufferKind::Outbound,
        wire::RuntimeBufferKind::Latest,
        wire::RuntimeBufferKind::Subscriber,
        wire::RuntimeBufferKind::Mixed,
    ] {
        let measured = mapped.contains(&buffer_kind);
        match buffer_kind {
            wire::RuntimeBufferKind::Outbound
            | wire::RuntimeBufferKind::Latest
            | wire::RuntimeBufferKind::Subscriber => assert!(
                measured,
                "{buffer_kind:?} is on the wire but no internal buffer reports it"
            ),
            // As with `RuntimeDirection::Mixed`, the overflow row only.
            wire::RuntimeBufferKind::Mixed => assert!(!measured),
        }
    }
}
