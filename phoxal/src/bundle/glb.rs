//! Validation for the closed binary glTF form accepted in world bundles.

/// Validate the one closed binary glTF form accepted by world compilation and reopening.
pub(crate) fn validate_closed(bytes: &[u8]) -> Result<(), ClosedGlbError> {
    const JSON_CHUNK: u32 = 0x4E4F_534A;
    const BIN_CHUNK: u32 = 0x004E_4942;

    if bytes.len() < 20 || bytes.get(0..4) != Some(b"glTF") {
        return Err(ClosedGlbError("missing the GLB header".to_owned()));
    }
    let version = glb_u32(bytes, 4, "truncated GLB version")?;
    let declared = glb_u32(bytes, 8, "truncated GLB length")?;
    if version != 2 || usize::try_from(declared).ok() != Some(bytes.len()) {
        return Err(ClosedGlbError(format!(
            "expected GLB version 2 with declared length {}, found version {version} and length {declared}",
            bytes.len()
        )));
    }

    let mut offset = 12_usize;
    let mut json = None;
    let mut binary = None;
    while offset < bytes.len() {
        let header_end = offset
            .checked_add(8)
            .ok_or_else(|| ClosedGlbError("chunk header offset overflows".to_owned()))?;
        let header = bytes
            .get(offset..header_end)
            .ok_or_else(|| ClosedGlbError("truncated GLB chunk header".to_owned()))?;
        let length = glb_u32(header, 0, "invalid GLB chunk length")? as usize;
        let kind = glb_u32(header, 4, "invalid GLB chunk type")?;
        if !length.is_multiple_of(4) {
            return Err(ClosedGlbError(
                "GLB chunk length is not four-byte aligned".to_owned(),
            ));
        }
        let end = header_end
            .checked_add(length)
            .ok_or_else(|| ClosedGlbError("GLB chunk length overflows".to_owned()))?;
        let chunk = bytes
            .get(header_end..end)
            .ok_or_else(|| ClosedGlbError("truncated GLB chunk".to_owned()))?;
        match kind {
            JSON_CHUNK if offset == 12 && json.is_none() => json = Some(chunk),
            JSON_CHUNK => {
                return Err(ClosedGlbError(
                    "GLB JSON must be the first and only JSON chunk".to_owned(),
                ));
            }
            BIN_CHUNK if json.is_some() && binary.is_none() => binary = Some(chunk),
            BIN_CHUNK => {
                return Err(ClosedGlbError(
                    "GLB may contain at most one binary chunk after JSON".to_owned(),
                ));
            }
            _ => {
                return Err(ClosedGlbError(format!(
                    "unsupported GLB chunk type {kind:#010x}"
                )));
            }
        }
        offset = end;
    }

    let json = json.ok_or_else(|| ClosedGlbError("GLB has no JSON chunk".to_owned()))?;
    let json = std::str::from_utf8(json)
        .map_err(|source| ClosedGlbError(format!("JSON chunk is not UTF-8: {source}")))?;
    let json = json
        .trim_end_matches(|character: char| character == '\0' || character.is_ascii_whitespace());
    let document: serde_json::Value = serde_json::from_str(json)
        .map_err(|source| ClosedGlbError(format!("JSON chunk is invalid: {source}")))?;
    if document
        .get("asset")
        .and_then(|asset| asset.get("version"))
        .and_then(serde_json::Value::as_str)
        != Some("2.0")
    {
        return Err(ClosedGlbError(
            "JSON asset.version must be exactly '2.0'".to_owned(),
        ));
    }
    let buffers = match document.get("buffers") {
        Some(serde_json::Value::Array(buffers)) => buffers.as_slice(),
        Some(_) => {
            return Err(ClosedGlbError("JSON buffers must be an array".to_owned()));
        }
        None => &[],
    };
    let mut embedded_buffer_length = None;
    for (index, buffer) in buffers.iter().enumerate() {
        let buffer = buffer
            .as_object()
            .ok_or_else(|| ClosedGlbError(format!("buffers[{index}] must be an object")))?;
        let byte_length = buffer
            .get("byteLength")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                ClosedGlbError(format!(
                    "buffers[{index}].byteLength must be a non-negative integer"
                ))
            })?;
        let byte_length = usize::try_from(byte_length).map_err(|_| {
            ClosedGlbError(format!(
                "buffers[{index}].byteLength exceeds the supported size"
            ))
        })?;
        match buffer.get("uri") {
            Some(serde_json::Value::String(uri)) if uri.starts_with("data:") => {}
            Some(serde_json::Value::String(uri)) => {
                return Err(ClosedGlbError(format!(
                    "buffers contains external URI '{uri}'"
                )));
            }
            Some(_) => {
                return Err(ClosedGlbError(format!(
                    "buffers[{index}].uri must be a string"
                )));
            }
            None if index == 0 => embedded_buffer_length = Some(byte_length),
            None => {
                return Err(ClosedGlbError(
                    "only buffers[0] may omit uri and use the GLB binary chunk".to_owned(),
                ));
            }
        }
    }
    match (embedded_buffer_length, binary) {
        (None, None) => {}
        (None, Some(_)) => {
            return Err(ClosedGlbError(
                "GLB binary chunk has no matching buffers[0] without uri".to_owned(),
            ));
        }
        (Some(_), None) => {
            return Err(ClosedGlbError(
                "buffers[0] omits uri but the GLB binary chunk is missing".to_owned(),
            ));
        }
        (Some(byte_length), Some(binary)) => {
            let maximum = byte_length
                .checked_add(3)
                .ok_or_else(|| ClosedGlbError("buffer byte length overflows".to_owned()))?;
            if binary.len() < byte_length || binary.len() > maximum {
                return Err(ClosedGlbError(format!(
                    "GLB binary chunk length {} does not cover buffers[0].byteLength {byte_length} with at most three padding bytes",
                    binary.len()
                )));
            }
            if binary[byte_length..].iter().any(|byte| *byte != 0) {
                return Err(ClosedGlbError(
                    "GLB binary chunk padding bytes must be zero".to_owned(),
                ));
            }
        }
    }
    if let Some(images) = document.get("images") {
        let images = images
            .as_array()
            .ok_or_else(|| ClosedGlbError("JSON images must be an array".to_owned()))?;
        for (index, image) in images.iter().enumerate() {
            let image = image
                .as_object()
                .ok_or_else(|| ClosedGlbError(format!("images[{index}] must be an object")))?;
            if let Some(uri) = image.get("uri") {
                let uri = uri.as_str().ok_or_else(|| {
                    ClosedGlbError(format!("images[{index}].uri must be a string"))
                })?;
                if !uri.starts_with("data:") {
                    return Err(ClosedGlbError(format!(
                        "images contains external URI '{uri}'"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn glb_u32(bytes: &[u8], offset: usize, detail: &'static str) -> Result<u32, ClosedGlbError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| ClosedGlbError(detail.to_owned()))?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| ClosedGlbError(detail.to_owned()))?;
    Ok(u32::from_le_bytes(
        value
            .try_into()
            .map_err(|_| ClosedGlbError(detail.to_owned()))?,
    ))
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct ClosedGlbError(String);
