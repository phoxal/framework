use super::*;

pub(super) const MAX_FRAME_BYTES: usize = MAX_ROBOT_SOURCE_BYTES + 1024;

/// Encode one bounded private-link frame.
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), LinkError> {
    let body = rmp_serde::to_vec_named(value)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(LinkError::FrameTooLarge {
            bytes: body.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }
    let length = u32::try_from(body.len()).map_err(|_| LinkError::FrameTooLarge {
        bytes: body.len(),
        maximum: MAX_FRAME_BYTES,
    })?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

/// Decode one bounded private-link frame.
pub fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, LinkError> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let bytes = u32::from_be_bytes(length) as usize;
    if bytes > MAX_FRAME_BYTES {
        return Err(LinkError::FrameTooLarge {
            bytes,
            maximum: MAX_FRAME_BYTES,
        });
    }
    let mut body = vec![0_u8; bytes];
    reader.read_exact(&mut body)?;
    Ok(rmp_serde::from_slice(&body)?)
}
