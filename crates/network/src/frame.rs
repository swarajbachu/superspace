use quinn::{RecvStream, SendStream};
use superspace_protocol::Message;
use thiserror::Error;

/// Maximum encoded protocol frame, large enough for one bounded transfer chunk plus metadata.
pub const MAX_FRAME_SIZE: usize = 512 * 1024;
const LENGTH_BYTES: usize = 4;

/// Encode one versioned protocol message with a network-byte-order length prefix.
///
/// # Errors
///
/// Returns [`FrameError::TooLarge`] or a serialization failure.
pub fn encode_frame(message: &Message) -> Result<Vec<u8>, FrameError> {
    let payload = serde_json::to_vec(message)?;
    if payload.len() > MAX_FRAME_SIZE {
        return Err(FrameError::TooLarge);
    }
    let length = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge)?;
    let mut frame = Vec::with_capacity(LENGTH_BYTES + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Decode exactly one complete bounded frame.
///
/// # Errors
///
/// Returns [`FrameError`] for truncation, trailing bytes, size violations, or invalid messages.
pub fn decode_frame(frame: &[u8]) -> Result<Message, FrameError> {
    if frame.len() < LENGTH_BYTES {
        return Err(FrameError::Truncated);
    }
    let length = u32::from_be_bytes(
        frame[..LENGTH_BYTES]
            .try_into()
            .map_err(|_| FrameError::Truncated)?,
    ) as usize;
    if length > MAX_FRAME_SIZE {
        return Err(FrameError::TooLarge);
    }
    if frame.len() != LENGTH_BYTES + length {
        return Err(if frame.len() < LENGTH_BYTES + length {
            FrameError::Truncated
        } else {
            FrameError::TrailingData
        });
    }
    serde_json::from_slice(&frame[LENGTH_BYTES..]).map_err(FrameError::Codec)
}

/// Write one framed message to a QUIC stream.
///
/// # Errors
///
/// Returns serialization, size-limit, or QUIC stream failures.
pub async fn write_frame(stream: &mut SendStream, message: &Message) -> Result<(), FrameError> {
    stream
        .write_all(&encode_frame(message)?)
        .await
        .map_err(|_| FrameError::Write)
}

/// Read one framed message from a QUIC stream with allocation limits applied before allocation.
///
/// # Errors
///
/// Returns truncation, size-limit, decoding, or QUIC stream failures.
pub async fn read_frame(stream: &mut RecvStream) -> Result<Message, FrameError> {
    let mut prefix = [0_u8; LENGTH_BYTES];
    stream
        .read_exact(&mut prefix)
        .await
        .map_err(|_| FrameError::Read)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length > MAX_FRAME_SIZE {
        return Err(FrameError::TooLarge);
    }
    let mut payload = vec![0_u8; length];
    stream
        .read_exact(&mut payload)
        .await
        .map_err(|_| FrameError::Read)?;
    serde_json::from_slice(&payload).map_err(FrameError::Codec)
}

/// Protocol framing and stream failures.
#[derive(Debug, Error)]
pub enum FrameError {
    /// Encoded or announced payload exceeds [`MAX_FRAME_SIZE`].
    #[error("network frame exceeds the size limit")]
    TooLarge,
    /// In-memory frame ended before its declared boundary.
    #[error("network frame is truncated")]
    Truncated,
    /// In-memory frame contains bytes after its declared boundary.
    #[error("network frame has trailing data")]
    TrailingData,
    /// Message serialization or parsing failed.
    #[error("network frame contains an invalid protocol message")]
    Codec(#[from] serde_json::Error),
    /// QUIC receive stream failed.
    #[error("network frame read failed")]
    Read,
    /// QUIC send stream failed.
    #[error("network frame write failed")]
    Write,
}

#[cfg(test)]
mod tests {
    use superspace_protocol::{DeviceInfo, PROTOCOL_VERSION};
    use uuid::Uuid;

    use super::*;

    fn message() -> Message {
        Message::Hello(DeviceInfo {
            id: Uuid::nil(),
            name: "Linux Desktop".into(),
            platform: "linux".into(),
            protocol_versions: vec![PROTOCOL_VERSION],
        })
    }

    #[test]
    fn frame_round_trips_and_rejects_bad_boundaries() {
        let frame = encode_frame(&message()).expect("encode");
        assert_eq!(decode_frame(&frame).expect("decode"), message());
        assert!(matches!(
            decode_frame(&frame[..frame.len() - 1]),
            Err(FrameError::Truncated)
        ));
        let mut trailing = frame;
        trailing.push(0);
        assert!(matches!(
            decode_frame(&trailing),
            Err(FrameError::TrailingData)
        ));
    }

    #[test]
    fn declared_oversize_is_rejected_before_allocation() {
        let prefix = u32::try_from(MAX_FRAME_SIZE + 1)
            .expect("frame bound fits u32")
            .to_be_bytes();
        assert!(matches!(decode_frame(&prefix), Err(FrameError::TooLarge)));
    }
}
