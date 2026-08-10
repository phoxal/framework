//! Endpoint-typed handles over the family-rooted bus boundary.
//!
//! The handles are grouped by what they own:
//!
//! - [`stamp`] - the step tokens that let a publisher express robot time, and
//!   the timeline authority that mints world steps.
//! - [`publisher`] - the endpoint-kind publisher handles, plus the framework's
//!   own world-clock publisher.
//! - [`subscriber`] - the receiving side: [`Observed`](subscriber::Observed),
//!   [`StateView`](subscriber::StateView), and the delivery-specific
//!   [`SetpointReceiver`](subscriber::SetpointReceiver),
//!   [`SampleReceiver`](subscriber::SampleReceiver), and
//!   [`StreamReceiver`](subscriber::StreamReceiver), plus ordered
//!   [`EventReceiver`](subscriber::EventReceiver).
//! - [`querier`] - the caller side of the request/response leg.
//!
//! This module itself owns only the vocabulary all four share: turning one
//! Zenoh sample into a typed body plus its provenance.
//!
//! # Publishing is capability-gated
//!
//! There is no caller-supplied publish time. The robot time a publisher can
//! express is determined by what the contract *is*, and each publisher handle
//! is bounded by its contract's temporal-role marker, so reaching for the wrong
//! one is a compile error:
//!
//! - [`StatePublisher<B>`](publisher::StatePublisher) publishes at a step, and
//!   the step instant comes from a [`StepToken`](stamp::StepToken) the runner
//!   mints for every scheduled participant, or a
//!   [`WorldStepToken`](stamp::WorldStepToken) a
//!   [`TimelineAuthority`](stamp::TimelineAuthority) mints for the world
//!   authority alone.
//! - [`SamplePublisher<B>`](publisher::SamplePublisher) publishes with
//!   a [`CaptureStamp`](crate::time::CaptureStamp) the driver derived from its
//!   device clock, and honestly represents an untranslated capture rather than
//!   inventing one.
//! - [`SetpointPublisher<B>`](publisher::SetpointPublisher) and
//!   [`StreamPublisher<B>`](publisher::StreamPublisher) express no robot time
//!   at all.
//!
//! # Receiving is bus-stamped
//!
//! Every subscription stamps a [`LocalInstant`](crate::time::LocalInstant)
//! immediately after `recv_async()` returns and **before** decode, so ring
//! residence and decode cost are inside every consumer's measured age rather
//! than outside it. Observation time is process-local and receiver-specific, so
//! it rides on [`Observed`](subscriber::Observed) and never on the wire.
//!
//! # Delivery-family QoS
//!
//! Pub/sub admission is selected by the contract's
//! [`crate::DeliveryFamily`]. Every
//! publish returns immediately and the one session-owned drain remains the
//! only Zenoh publisher:
//!
//! - **State and setpoint** keep one newest unsent value per concrete topic.
//! - **Sample** keeps a bounded ordered lane and evicts its oldest values with
//!   explicit loss evidence when the lane is full.
//! - **Stream** keeps a bounded ordered lane and returns [`BusError::WouldBlock`]
//!   rather than silently evicting an older chunk.
//! - A body or ordered lane that cannot fit returns [`BusError::Saturated`]
//!   (stream publishers translate that to `WouldBlock`) without stalling the
//!   step loop.
//! - **Receivers bind their backlog by contract.** `StateView<B>` keeps only
//!   the last sample, setpoints retain one actionable value, samples use a
//!   bounded drop-oldest ring with loss evidence, and streams refuse to evict
//!   an older chunk when their ring is saturated.
//!
//! Contract identity lives entirely in the Zenoh key - the version is folded
//! into `<Endpoint as EndpointDescriptor>::TOPIC` - so a receiver's per-key subscription
//! is the fast-reject, and the decode path only still validates the codec. A
//! decode failure is counted (`decode_errors`) + logged as a health signal,
//! never a silent accept. Timeline-aware handles separately count purged or
//! retired-timeline samples in `timeline_filtered`, so quarantine churn is not
//! confused with active-buffer loss.

pub mod publisher;
pub mod querier;
pub mod stamp;
pub mod subscriber;

use zenoh::sample::Sample;

use crate::abi::{Codec, CodecId, EncodingError, EncodingMetadata, MessagePack};
use crate::contract::{EndpointDescriptor, Payload};
use crate::error::{BusError, MetadataProblem, Result};
use crate::metadata::BusMetadata;

/// Decode one Zenoh sample into the payload of endpoint `E`, validating the codec before
/// touching the payload.
///
/// Contract identity is not checked here: it is guaranteed by the Zenoh key
/// itself, and this function is only ever invoked for samples received on a
/// subscription already scoped to `E`'s family-rooted topic.
pub(crate) fn decode_sample<E: EndpointDescriptor>(
    sample: &Sample,
    topic: &str,
) -> Result<(E::Payload, BusMetadata)> {
    decode_payload::<E::Payload>(sample, topic)
}

pub(crate) fn decode_payload<B: Payload>(sample: &Sample, topic: &str) -> Result<(B, BusMetadata)> {
    let malformed = |problem: MetadataProblem| BusError::metadata(topic, problem);

    let encoding: EncodingMetadata = sample
        .encoding()
        .to_string()
        .parse()
        .map_err(|e: EncodingError| malformed(e.into()))?;
    if encoding.codec_id() != Some(CodecId::MessagePack) {
        return Err(BusError::UnsupportedCodec {
            codec: encoding.codec,
            topic: topic.to_string(),
        });
    }

    let attachment = sample
        .attachment()
        .ok_or_else(|| malformed(MetadataProblem::MissingAttachment))?;
    let metadata =
        BusMetadata::decode(attachment.to_bytes().as_ref()).map_err(|e| malformed(e.into()))?;

    if metadata.codec != encoding.codec {
        return Err(malformed(MetadataProblem::CodecMismatch {
            encoding: encoding.codec,
            attachment: metadata.codec,
        }));
    }
    if metadata.codec_id() != Some(CodecId::MessagePack) {
        return Err(BusError::UnsupportedCodec {
            codec: metadata.codec,
            topic: topic.to_string(),
        });
    }

    let body = MessagePack::decode::<B>(sample.payload().to_bytes().as_ref())?;
    Ok((body, metadata))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::CodecError;
    use crate::test_support::{Target, TargetEndpoint, sample, sample_with, sample_with_encoding};

    const TOPIC: &str = "yTEST/drive/target";

    #[test]
    fn decode_accepts_a_matching_sample() {
        let sample = sample(CodecId::MessagePack.as_u8());
        let (body, metadata) = decode_sample::<TargetEndpoint>(&sample, TOPIC).unwrap();
        assert_eq!(body.linear_x_mps, 1.0);
        assert_eq!(metadata.codec, CodecId::MessagePack.as_u8());
    }

    #[test]
    fn decode_rejects_encoding_attachment_codec_mismatch_before_body_decode() {
        let payload = rmp_serde::to_vec_named(&Target {
            linear_x_mps: 1.0,
            angular_z_radps: 0.5,
        })
        .unwrap();
        // The encoding string claims an unsupported codec even though the
        // attachment says MessagePack - the encoding string wins the
        // fast-reject.
        let sample = sample_with_encoding(
            CodecId::MessagePack.as_u8(),
            "phoxal/v0;codec=99".to_string(),
            payload,
        );

        let error = decode_sample::<TargetEndpoint>(&sample, TOPIC).unwrap_err();
        assert!(matches!(
            error,
            BusError::UnsupportedCodec { codec: 99, .. }
        ));
    }

    #[test]
    fn decode_rejects_unsupported_codec() {
        let error = decode_sample::<TargetEndpoint>(&sample(99), TOPIC).unwrap_err();
        assert!(matches!(
            error,
            BusError::UnsupportedCodec { codec: 99, .. }
        ));
    }

    #[test]
    fn decode_rejects_corrupt_payload() {
        let sample = sample_with(CodecId::MessagePack.as_u8(), vec![0xc1, 0xc1, 0xc1]);
        let error = decode_sample::<TargetEndpoint>(&sample, TOPIC).unwrap_err();
        assert!(matches!(error, BusError::Codec(CodecError::Decode(_))));
    }
}
