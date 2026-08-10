//! Canonical capture negotiation independent of process-callback handling.

use std::fmt;

/// Sample representation accepted at the capture boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureSampleFormat {
    /// Interleaved native-endian IEEE-754 32-bit float.
    F32Native,
}

/// Semantic channel position accepted by the Phase-3 capture stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureChannelPosition {
    /// One canonical mono channel.
    Mono,
}

/// Validated capture format published from the control-plane listener.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureFormat {
    /// Interleaved sample encoding.
    pub sample_format: CaptureSampleFormat,
    /// Negotiated graph-facing sample rate.
    pub sample_rate: u32,
    /// Interleaved channel count.
    pub channels: u16,
    /// Semantic position of the sole accepted channel.
    pub position: CaptureChannelPosition,
}

/// Canonical mono `f32` 48 kHz stream format requested from `PipeWire`.
pub const CANONICAL_CAPTURE_FORMAT: CaptureFormat = CaptureFormat {
    sample_format: CaptureSampleFormat::F32Native,
    sample_rate: 48_000,
    channels: 1,
    position: CaptureChannelPosition::Mono,
};

/// Why a negotiated stream format cannot enter the canonical pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NegotiatedFormatError {
    /// The format pod was absent or could not be parsed.
    Malformed,
    /// The media type/subtype is not raw audio.
    UnsupportedMedia,
    /// The sample representation is not native-endian interleaved `f32`.
    UnsupportedSampleFormat,
    /// The graph did not present the requested 48 kHz rate.
    UnsupportedSampleRate,
    /// The graph did not present exactly one channel.
    UnsupportedChannelCount,
    /// The channel is not semantically `MONO`.
    UnsupportedChannelPosition,
}

impl fmt::Display for NegotiatedFormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Malformed => "the negotiated PipeWire format is malformed",
            Self::UnsupportedMedia => "the negotiated PipeWire media type is not raw audio",
            Self::UnsupportedSampleFormat => {
                "the negotiated PipeWire sample format is not native-endian interleaved f32"
            }
            Self::UnsupportedSampleRate => "the negotiated PipeWire sample rate is not 48000 Hz",
            Self::UnsupportedChannelCount => "the negotiated PipeWire channel count is not mono",
            Self::UnsupportedChannelPosition => {
                "the negotiated PipeWire channel position is not MONO"
            }
        })
    }
}

impl std::error::Error for NegotiatedFormatError {}

/// Serializes the exact `EnumFormat` pod used to request canonical capture.
///
/// # Errors
///
/// Returns the serializer error if the fixed SPA object cannot be encoded.
#[cfg(feature = "pipewire-backend")]
pub fn build_capture_format_pod() -> Result<Vec<u8>, libspa::pod::serialize::GenError> {
    build_raw_audio_format_pod(CANONICAL_CAPTURE_FORMAT.sample_rate)
}

#[cfg(feature = "pipewire-backend")]
pub(crate) fn build_raw_audio_format_pod(
    sample_rate: u32,
) -> Result<Vec<u8>, libspa::pod::serialize::GenError> {
    use std::io::Cursor;

    use libspa::{
        param::{
            ParamType,
            audio::{AudioInfoRaw, MAX_CHANNELS},
        },
        pod::{Object, Value, serialize::PodSerializer},
        utils::SpaTypes,
    };

    let mut info = AudioInfoRaw::new();
    info.set_format(native_f32_format());
    info.set_rate(sample_rate);
    info.set_channels(u32::from(CANONICAL_CAPTURE_FORMAT.channels));
    let mut positions = [0; MAX_CHANNELS];
    positions[0] = libspa::sys::SPA_AUDIO_CHANNEL_MONO;
    info.set_position(positions);
    let object = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    let (cursor, _) = PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object))?;
    Ok(cursor.into_inner())
}

/// Parses and strictly validates the format received from the stream listener.
///
/// A 44.1 kHz physical device is supported when `PipeWire` performs graph
/// conversion and presents this requested canonical format to Noire.
///
/// # Errors
///
/// Returns a precise unsupported/malformed reason for control-plane recovery.
#[cfg(feature = "pipewire-backend")]
pub fn parse_negotiated_format(
    pod: &libspa::pod::Pod,
) -> Result<CaptureFormat, NegotiatedFormatError> {
    use libspa::param::{
        audio::AudioInfoRaw,
        format::{MediaSubtype, MediaType},
        format_utils,
    };

    let (media_type, media_subtype) =
        format_utils::parse_format(pod).map_err(|_| NegotiatedFormatError::Malformed)?;
    if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
        return Err(NegotiatedFormatError::UnsupportedMedia);
    }
    let mut info = AudioInfoRaw::new();
    info.parse(pod)
        .map_err(|_| NegotiatedFormatError::Malformed)?;
    if info.format() != native_f32_format() || !info.format().is_interleaved() {
        return Err(NegotiatedFormatError::UnsupportedSampleFormat);
    }
    if info.rate() != CANONICAL_CAPTURE_FORMAT.sample_rate {
        return Err(NegotiatedFormatError::UnsupportedSampleRate);
    }
    if info.channels() != u32::from(CANONICAL_CAPTURE_FORMAT.channels) {
        return Err(NegotiatedFormatError::UnsupportedChannelCount);
    }
    if info.position()[0] != libspa::sys::SPA_AUDIO_CHANNEL_MONO {
        return Err(NegotiatedFormatError::UnsupportedChannelPosition);
    }
    Ok(CANONICAL_CAPTURE_FORMAT)
}

#[cfg(all(feature = "pipewire-backend", target_endian = "little"))]
fn native_f32_format() -> libspa::param::audio::AudioFormat {
    libspa::param::audio::AudioFormat::F32LE
}

#[cfg(all(feature = "pipewire-backend", target_endian = "big"))]
fn native_f32_format() -> libspa::param::audio::AudioFormat {
    libspa::param::audio::AudioFormat::F32BE
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "pipewire-backend")]
    use super::CANONICAL_CAPTURE_FORMAT;
    use super::NegotiatedFormatError;

    #[test]
    fn unsupported_reason_has_stable_context() {
        assert_eq!(
            NegotiatedFormatError::UnsupportedSampleRate.to_string(),
            "the negotiated PipeWire sample rate is not 48000 Hz"
        );
    }

    #[cfg(feature = "pipewire-backend")]
    #[test]
    fn canonical_pod_round_trips_through_spa_parser() -> Result<(), &'static str> {
        let bytes = super::build_capture_format_pod().map_err(|_| "serialization failed")?;
        let pod = libspa::pod::Pod::from_bytes(&bytes).ok_or("pod parse failed")?;
        let parsed = super::parse_negotiated_format(pod).map_err(|_| "format parse failed")?;
        assert_eq!(parsed, CANONICAL_CAPTURE_FORMAT);
        Ok(())
    }
}
