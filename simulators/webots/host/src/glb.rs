//! Closed GLB 2.0 decoding for Webots-native indexed geometry.
//!
//! Webots R2025a does not load GLB files through `CadShape` or `Mesh`. The adapter therefore
//! validates and decodes the bounded subset it can reproduce exactly, bakes the selected glTF
//! scene graph into vertex data, and lets both world and Robot generation emit native
//! `IndexedFaceSet` nodes.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Cursor;

use anyhow::{Context, Result, bail, ensure};
use image::ImageFormat;
use nalgebra::{Matrix3, Matrix4, Quaternion, Translation3, UnitQuaternion, Vector3, Vector4};
use serde_json::{Map, Value};

const MAGIC: &[u8; 4] = b"glTF";
const JSON_CHUNK: u32 = 0x4e4f_534a;
const BIN_CHUNK: u32 = 0x004e_4942;
const TRIANGLES: u64 = 4;
const FLOAT: u64 = 5126;
const UNSIGNED_BYTE: u64 = 5121;
const UNSIGNED_SHORT: u64 = 5123;
const UNSIGNED_INT: u64 = 5125;
const MAX_COLLISION_TRIANGLES: usize = 100_000;
const MAX_NODE_DEPTH: usize = 256;

#[derive(Clone, Debug)]
pub(crate) struct DecodedMesh {
    pub primitives: Vec<DecodedPrimitive>,
    pub images: Vec<DecodedImage>,
}

#[derive(Clone, Debug)]
pub(crate) struct DecodedPrimitive {
    pub positions: Vec<[f64; 3]>,
    pub normals: Option<Vec<[f64; 3]>>,
    pub texcoords: Option<Vec<[f64; 2]>>,
    pub indices: Vec<u32>,
    pub material: DecodedMaterial,
}

#[derive(Clone, Debug)]
pub(crate) struct DecodedMaterial {
    pub base_color: [f64; 4],
    pub metallic: f64,
    pub roughness: f64,
    pub emissive: [f64; 3],
    pub double_sided: bool,
    pub alpha_blend: bool,
    pub base_color_texture: Option<DecodedTexture>,
}

impl Default for DecodedMaterial {
    fn default() -> Self {
        Self {
            base_color: [1.0; 4],
            metallic: 1.0,
            roughness: 1.0,
            emissive: [0.0; 3],
            double_sided: false,
            alpha_blend: false,
            base_color_texture: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DecodedTexture {
    pub image: usize,
    pub repeat_s: bool,
    pub repeat_t: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImageKind {
    Png,
    Jpeg,
}

impl ImageKind {
    const fn format(self) -> ImageFormat {
        match self {
            Self::Png => ImageFormat::Png,
            Self::Jpeg => ImageFormat::Jpeg,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DecodedImage {
    pub kind: ImageKind,
    pub bytes: Vec<u8>,
}

#[derive(Clone)]
struct MeshPrimitive {
    positions: Vec<[f64; 3]>,
    normals: Option<Vec<[f64; 3]>>,
    texcoords: Option<Vec<[f64; 2]>>,
    indices: Vec<u32>,
    material: DecodedMaterial,
}

#[derive(Clone, Copy)]
struct BufferView {
    buffer: usize,
    offset: usize,
    length: usize,
    stride: Option<usize>,
}

type AccessorData<'a> = (&'a [u8], usize, usize, u64, String, bool);

impl DecodedMesh {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let (document, binary) = container(bytes)?;
        validate_top_level(&document)?;
        let buffers = buffers(&document, binary)?;
        let views = buffer_views(&document, &buffers)?;
        validate_accessors(&document, views.len())?;
        let images = images(&document, &buffers, &views)?;
        let textures = textures(&document, images.len())?;
        let materials = materials(&document, &textures)?;
        let meshes = meshes(&document, &buffers, &views, &materials)?;
        let primitives = scene_primitives(&document, &meshes)?;
        ensure!(
            !primitives.is_empty(),
            "GLB selected scene contains no mesh primitive"
        );
        Ok(Self { primitives, images })
    }

    pub fn validate_collision(&self) -> Result<()> {
        let triangles = self
            .primitives
            .iter()
            .try_fold(0_usize, |total, primitive| {
                ensure!(
                    primitive.indices.len().is_multiple_of(3),
                    "collision primitive is not a triangle list"
                );
                for triangle in primitive.indices.as_chunks::<3>().0 {
                    let a = primitive.positions[triangle[0] as usize];
                    let b = primitive.positions[triangle[1] as usize];
                    let c = primitive.positions[triangle[2] as usize];
                    ensure!(
                        a != b && b != c && a != c,
                        "collision GLB contains a triangle with coincident vertices"
                    );
                }
                total
                    .checked_add(primitive.indices.len() / 3)
                    .context("collision triangle count overflowed")
            })?;
        ensure!(
            triangles <= MAX_COLLISION_TRIANGLES,
            "collision GLB has {triangles} triangles, exceeding {MAX_COLLISION_TRIANGLES}"
        );
        Ok(())
    }

    pub fn render_visual(
        &self,
        out: &mut String,
        indent: usize,
        texture_url: impl Fn(usize) -> Result<String>,
    ) -> Result<()> {
        for (primitive_index, primitive) in self.primitives.iter().enumerate() {
            writeln!(out, "{:indent$}Shape {{", "")?;
            render_appearance(
                out,
                &primitive.material,
                primitive_index,
                indent + 2,
                &texture_url,
            )?;
            writeln!(
                out,
                "{:width$}geometry IndexedFaceSet {{",
                "",
                width = indent + 2
            )?;
            render_indexed_face_set(out, primitive, indent + 4, true)?;
            writeln!(out, "{:width$}}}", "", width = indent + 2)?;
            writeln!(out, "{:indent$}}}", "")?;
        }
        Ok(())
    }

    pub fn staged_texture(&self, primitive: usize) -> Result<Option<Vec<u8>>> {
        let primitive = self
            .primitives
            .get(primitive)
            .context("decoded GLB texture names an absent primitive")?;
        let Some(texture) = &primitive.material.base_color_texture else {
            return Ok(None);
        };
        let image = self
            .images
            .get(texture.image)
            .context("decoded GLB material names an absent image")?;
        let mut pixels = image::load_from_memory_with_format(&image.bytes, image.kind.format())
            .context("validated GLB image could not be decoded for material baking")?
            .to_rgba8();
        for pixel in pixels.pixels_mut() {
            for (sample, factor) in pixel.0[..3]
                .iter_mut()
                .zip(primitive.material.base_color[..3].iter())
            {
                let linear = srgb_to_linear(f64::from(*sample) / 255.0) * factor;
                *sample = (linear_to_srgb(linear).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            pixel.0[3] = if primitive.material.alpha_blend {
                (f64::from(pixel.0[3]) * primitive.material.base_color[3]).round() as u8
            } else {
                255
            };
        }
        let mut encoded = Cursor::new(Vec::new());
        let staged = if primitive.material.alpha_blend {
            image::DynamicImage::ImageRgba8(pixels)
        } else {
            image::DynamicImage::ImageRgb8(image::DynamicImage::ImageRgba8(pixels).to_rgb8())
        };
        staged
            .write_to(&mut encoded, ImageFormat::Png)
            .context("failed to encode baked GLB material texture")?;
        Ok(Some(encoded.into_inner()))
    }

    pub fn render_collision(&self, out: &mut String, indent: usize) -> Result<()> {
        self.render_collision_scaled(out, indent, [1.0; 3])
    }

    pub fn render_collision_scaled(
        &self,
        out: &mut String,
        indent: usize,
        scale: [f64; 3],
    ) -> Result<()> {
        ensure!(
            scale.iter().all(|value| value.is_finite() && *value > 0.0),
            "collision GLB scale must contain finite positive values"
        );
        self.validate_collision()?;
        if self.primitives.len() > 1 {
            writeln!(out, "{:indent$}Group {{", "")?;
            writeln!(out, "{:width$}children [", "", width = indent + 2)?;
        }
        let geometry_indent = if self.primitives.len() > 1 {
            indent + 4
        } else {
            indent
        };
        for primitive in &self.primitives {
            let mut primitive = primitive.clone();
            for position in &mut primitive.positions {
                for axis in 0..3 {
                    position[axis] *= scale[axis];
                    ensure!(
                        position[axis].is_finite(),
                        "collision GLB scale overflowed a vertex"
                    );
                }
            }
            writeln!(out, "{:geometry_indent$}IndexedFaceSet {{", "")?;
            render_indexed_face_set(out, &primitive, geometry_indent + 2, false)?;
            writeln!(out, "{:geometry_indent$}}}", "")?;
        }
        if self.primitives.len() > 1 {
            writeln!(out, "{:width$}]", "", width = indent + 2)?;
            writeln!(out, "{:indent$}}}", "")?;
        }
        Ok(())
    }
}

fn render_appearance(
    out: &mut String,
    material: &DecodedMaterial,
    primitive_index: usize,
    indent: usize,
    texture_url: &impl Fn(usize) -> Result<String>,
) -> Result<()> {
    writeln!(out, "{:indent$}appearance PBRAppearance {{", "")?;
    let base_color = if material.base_color_texture.is_some() {
        [1.0; 3]
    } else {
        [
            material.base_color[0],
            material.base_color[1],
            material.base_color[2],
        ]
    };
    writeln!(
        out,
        "{:width$}baseColor {} {} {}",
        "",
        crate::generation::number(base_color[0]),
        crate::generation::number(base_color[1]),
        crate::generation::number(base_color[2]),
        width = indent + 2
    )?;
    writeln!(
        out,
        "{:width$}metalness {}",
        "",
        crate::generation::number(material.metallic),
        width = indent + 2
    )?;
    writeln!(
        out,
        "{:width$}roughness {}",
        "",
        crate::generation::number(material.roughness),
        width = indent + 2
    )?;
    if material.base_color_texture.is_none() && material.alpha_blend && material.base_color[3] < 1.0
    {
        writeln!(
            out,
            "{:width$}transparency {}",
            "",
            crate::generation::number(1.0 - material.base_color[3]),
            width = indent + 2
        )?;
    }
    if material.emissive != [0.0; 3] {
        writeln!(
            out,
            "{:width$}emissiveColor {} {} {}",
            "",
            crate::generation::number(material.emissive[0]),
            crate::generation::number(material.emissive[1]),
            crate::generation::number(material.emissive[2]),
            width = indent + 2
        )?;
    }
    if let Some(texture) = &material.base_color_texture {
        let image = texture_url(primitive_index)?;
        writeln!(
            out,
            "{:width$}baseColorMap ImageTexture {{",
            "",
            width = indent + 2
        )?;
        writeln!(
            out,
            "{:width$}url [\"{}\"]",
            "",
            crate::generation::quoted(&image),
            width = indent + 4
        )?;
        writeln!(
            out,
            "{:width$}repeatS {}",
            "",
            if texture.repeat_s { "TRUE" } else { "FALSE" },
            width = indent + 4
        )?;
        writeln!(
            out,
            "{:width$}repeatT {}",
            "",
            if texture.repeat_t { "TRUE" } else { "FALSE" },
            width = indent + 4
        )?;
        writeln!(out, "{:width$}}}", "", width = indent + 2)?;
    }
    writeln!(out, "{:indent$}}}", "")?;
    Ok(())
}

fn srgb_to_linear(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f64) -> f64 {
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn render_indexed_face_set(
    out: &mut String,
    primitive: &DecodedPrimitive,
    indent: usize,
    visual: bool,
) -> Result<()> {
    let backface_offset = if visual && primitive.material.double_sided {
        Some(u32::try_from(primitive.positions.len()).context("GLB vertex count exceeds u32")?)
    } else {
        None
    };
    writeln!(out, "{:indent$}coord Coordinate {{", "")?;
    writeln!(out, "{:width$}point [", "", width = indent + 2)?;
    for point in &primitive.positions {
        writeln!(
            out,
            "{:width$}{} {} {}",
            "",
            crate::generation::number(point[0]),
            crate::generation::number(point[1]),
            crate::generation::number(point[2]),
            width = indent + 4
        )?;
    }
    if backface_offset.is_some() {
        for point in &primitive.positions {
            writeln!(
                out,
                "{:width$}{} {} {}",
                "",
                crate::generation::number(point[0]),
                crate::generation::number(point[1]),
                crate::generation::number(point[2]),
                width = indent + 4
            )?;
        }
    }
    writeln!(out, "{:width$}]", "", width = indent + 2)?;
    writeln!(out, "{:indent$}}}", "")?;
    render_indices(
        out,
        "coordIndex",
        &primitive.indices,
        indent,
        backface_offset,
    )?;
    if visual {
        if let Some(normals) = &primitive.normals {
            writeln!(out, "{:indent$}normal Normal {{", "")?;
            writeln!(out, "{:width$}vector [", "", width = indent + 2)?;
            for normal in normals {
                writeln!(
                    out,
                    "{:width$}{} {} {}",
                    "",
                    crate::generation::number(normal[0]),
                    crate::generation::number(normal[1]),
                    crate::generation::number(normal[2]),
                    width = indent + 4
                )?;
            }
            if backface_offset.is_some() {
                for normal in normals {
                    writeln!(
                        out,
                        "{:width$}{} {} {}",
                        "",
                        crate::generation::number(-normal[0]),
                        crate::generation::number(-normal[1]),
                        crate::generation::number(-normal[2]),
                        width = indent + 4
                    )?;
                }
            }
            writeln!(out, "{:width$}]", "", width = indent + 2)?;
            writeln!(out, "{:indent$}}}", "")?;
            render_indices(
                out,
                "normalIndex",
                &primitive.indices,
                indent,
                backface_offset,
            )?;
            writeln!(out, "{:indent$}normalPerVertex TRUE", "")?;
        }
        if let Some(texcoords) = &primitive.texcoords {
            writeln!(out, "{:indent$}texCoord TextureCoordinate {{", "")?;
            writeln!(out, "{:width$}point [", "", width = indent + 2)?;
            for texcoord in texcoords {
                writeln!(
                    out,
                    "{:width$}{} {}",
                    "",
                    crate::generation::number(texcoord[0]),
                    crate::generation::number(1.0 - texcoord[1]),
                    width = indent + 4
                )?;
            }
            if backface_offset.is_some() {
                for texcoord in texcoords {
                    writeln!(
                        out,
                        "{:width$}{} {}",
                        "",
                        crate::generation::number(texcoord[0]),
                        crate::generation::number(1.0 - texcoord[1]),
                        width = indent + 4
                    )?;
                }
            }
            writeln!(out, "{:width$}]", "", width = indent + 2)?;
            writeln!(out, "{:indent$}}}", "")?;
            render_indices(
                out,
                "texCoordIndex",
                &primitive.indices,
                indent,
                backface_offset,
            )?;
        }
    }
    Ok(())
}

fn render_indices(
    out: &mut String,
    field: &str,
    indices: &[u32],
    indent: usize,
    backface_offset: Option<u32>,
) -> Result<()> {
    writeln!(out, "{:indent$}{field} [", "")?;
    for triangle in indices.as_chunks::<3>().0 {
        writeln!(
            out,
            "{:width$}{} {} {} -1",
            "",
            triangle[0],
            triangle[1],
            triangle[2],
            width = indent + 2
        )?;
    }
    if let Some(offset) = backface_offset {
        for triangle in indices.as_chunks::<3>().0 {
            let first = triangle[0]
                .checked_add(offset)
                .context("double-sided GLB index overflowed")?;
            let second = triangle[2]
                .checked_add(offset)
                .context("double-sided GLB index overflowed")?;
            let third = triangle[1]
                .checked_add(offset)
                .context("double-sided GLB index overflowed")?;
            writeln!(
                out,
                "{:width$}{first} {second} {third} -1",
                "",
                width = indent + 2
            )?;
        }
    }
    writeln!(out, "{:indent$}]", "")?;
    Ok(())
}

fn container(bytes: &[u8]) -> Result<(serde_json::Value, Option<&[u8]>)> {
    ensure!(bytes.len() >= 20, "GLB header is truncated");
    ensure!(&bytes[..4] == MAGIC, "asset is not a GLB container");
    ensure!(read_u32(bytes, 4)? == 2, "only GLB version 2 is supported");
    ensure!(
        usize::try_from(read_u32(bytes, 8)?)? == bytes.len(),
        "GLB declared length is inconsistent"
    );

    let mut offset = 12_usize;
    let mut json = None;
    let mut binary = None;
    while offset < bytes.len() {
        ensure!(offset + 8 <= bytes.len(), "GLB chunk header is truncated");
        let length = usize::try_from(read_u32(bytes, offset)?)?;
        let kind = read_u32(bytes, offset + 4)?;
        ensure!(
            length.is_multiple_of(4),
            "GLB chunk is not four-byte aligned"
        );
        let first = offset == 12;
        offset = offset
            .checked_add(8)
            .context("GLB chunk offset overflowed")?;
        let end = offset
            .checked_add(length)
            .context("GLB chunk length overflowed")?;
        ensure!(end <= bytes.len(), "GLB chunk is truncated");
        match kind {
            JSON_CHUNK if first && json.is_none() => json = Some(&bytes[offset..end]),
            JSON_CHUNK => bail!("GLB JSON must be the first and only JSON chunk"),
            BIN_CHUNK if json.is_some() && binary.is_none() => binary = Some(&bytes[offset..end]),
            BIN_CHUNK => bail!("GLB may contain at most one BIN chunk after JSON"),
            _ => bail!("unsupported GLB chunk type {kind:#010x}"),
        }
        offset = end;
    }
    let padded_json = json.context("GLB has no JSON chunk")?;
    let json_end = padded_json
        .iter()
        .rposition(|byte| *byte != b' ')
        .map_or(0, |index| index + 1);
    ensure!(json_end > 0, "GLB JSON chunk is empty");
    let padding = &padded_json[json_end..];
    ensure!(
        padding.len() <= 3,
        "GLB JSON has more than three padding bytes"
    );
    ensure!(
        padding.iter().all(|byte| *byte == b' '),
        "GLB JSON padding must contain only spaces"
    );
    let document = serde_json::from_slice(&padded_json[..json_end])?;
    Ok((document, binary))
}

fn validate_top_level(document: &serde_json::Value) -> Result<()> {
    let root = document
        .as_object()
        .context("glTF document is not an object")?;
    ensure_keys(
        root,
        &[
            "accessors",
            "asset",
            "bufferViews",
            "buffers",
            "extensionsRequired",
            "extensionsUsed",
            "extras",
            "images",
            "materials",
            "meshes",
            "nodes",
            "samplers",
            "scene",
            "scenes",
            "textures",
        ],
        "glTF document",
    )?;
    let asset = root
        .get("asset")
        .and_then(Value::as_object)
        .context("glTF asset is not an object")?;
    ensure_keys(
        asset,
        &["copyright", "extras", "generator", "minVersion", "version"],
        "glTF asset",
    )?;
    ensure!(
        asset.get("version").and_then(Value::as_str) == Some("2.0"),
        "glTF asset.version must be exactly 2.0"
    );
    if let Some(minimum) = asset.get("minVersion") {
        ensure!(
            minimum.as_str() == Some("2.0"),
            "glTF asset.minVersion must be exactly 2.0 when present"
        );
    }
    let allowed_extensions = BTreeSet::from(["KHR_materials_clearcoat"]);
    for field in ["extensionsUsed", "extensionsRequired"] {
        if let Some(extensions) = root.get(field) {
            let extensions = extensions
                .as_array()
                .with_context(|| format!("glTF {field} is not an array"))?;
            for extension in extensions {
                let extension = extension
                    .as_str()
                    .with_context(|| format!("glTF {field} contains a non-string"))?;
                ensure!(
                    allowed_extensions.contains(extension),
                    "unsupported glTF extension {extension}"
                );
            }
        }
    }
    Ok(())
}

fn buffers(document: &serde_json::Value, binary: Option<&[u8]>) -> Result<Vec<Vec<u8>>> {
    let entries = document
        .get("buffers")
        .and_then(serde_json::Value::as_array)
        .context("glTF buffers must be a non-empty array")?;
    ensure!(!entries.is_empty(), "glTF buffers array is empty");
    let mut decoded = Vec::with_capacity(entries.len());
    let mut binary_owned = false;
    for (index, entry) in entries.iter().enumerate() {
        let entry = entry
            .as_object()
            .with_context(|| format!("glTF buffer[{index}] is not an object"))?;
        ensure_keys(
            entry,
            &["byteLength", "extras", "name", "uri"],
            &format!("glTF buffer[{index}]"),
        )?;
        let byte_length = required_usize(entry.get("byteLength"), "buffer byteLength")?;
        ensure!(byte_length > 0, "glTF buffer[{index}] byteLength is zero");
        let body = match entry.get("uri") {
            None => {
                ensure!(index == 0, "only glTF buffer[0] may omit uri");
                ensure!(!binary_owned, "multiple buffers claim the GLB BIN chunk");
                let binary = binary.context("glTF buffer[0] omits uri but GLB has no BIN chunk")?;
                ensure!(
                    binary.len() >= byte_length,
                    "GLB BIN chunk is shorter than buffer[0]"
                );
                let padding = &binary[byte_length..];
                ensure!(
                    padding.len() <= 3,
                    "GLB BIN has more than three padding bytes"
                );
                ensure!(
                    padding.iter().all(|byte| *byte == 0),
                    "GLB BIN padding must contain only zero bytes"
                );
                binary_owned = true;
                binary[..byte_length].to_vec()
            }
            Some(uri) => {
                let uri = uri
                    .as_str()
                    .with_context(|| format!("glTF buffer[{index}].uri is not a string"))?;
                let (mime, body) = data_uri(uri, None)
                    .with_context(|| format!("glTF buffer[{index}] has an invalid data URI"))?;
                ensure!(
                    matches!(
                        mime.as_str(),
                        "application/octet-stream" | "application/gltf-buffer"
                    ),
                    "glTF buffer[{index}] has unsupported data URI mimeType '{mime}'"
                );
                ensure!(
                    body.len() == byte_length,
                    "glTF buffer[{index}] data length does not equal byteLength"
                );
                body
            }
        };
        decoded.push(body);
    }
    ensure!(
        binary.is_none() || binary_owned,
        "GLB contains a stray BIN chunk not owned by buffer[0]"
    );
    Ok(decoded)
}

fn buffer_views(document: &serde_json::Value, buffers: &[Vec<u8>]) -> Result<Vec<BufferView>> {
    let Some(entries) = document.get("bufferViews") else {
        return Ok(Vec::new());
    };
    let entries = entries
        .as_array()
        .context("glTF bufferViews is not an array")?;
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let entry = entry
                .as_object()
                .with_context(|| format!("glTF bufferView[{index}] is not an object"))?;
            ensure_keys(
                entry,
                &[
                    "buffer",
                    "byteLength",
                    "byteOffset",
                    "byteStride",
                    "extras",
                    "name",
                    "target",
                ],
                &format!("glTF bufferView[{index}]"),
            )?;
            let buffer = required_usize(entry.get("buffer"), "bufferView buffer")?;
            let offset =
                optional_usize(entry.get("byteOffset"), "bufferView byteOffset")?.unwrap_or(0);
            let length = required_usize(entry.get("byteLength"), "bufferView byteLength")?;
            ensure!(length > 0, "glTF bufferView[{index}] byteLength is zero");
            let stride = optional_usize(entry.get("byteStride"), "bufferView byteStride")?;
            let body = buffers
                .get(buffer)
                .with_context(|| format!("bufferView[{index}] names absent buffer {buffer}"))?;
            ensure!(
                offset
                    .checked_add(length)
                    .is_some_and(|end| end <= body.len()),
                "bufferView[{index}] exceeds its buffer"
            );
            if let Some(stride) = stride {
                ensure!(
                    (4..=252).contains(&stride) && stride.is_multiple_of(4),
                    "invalid bufferView byteStride"
                );
            }
            if let Some(target) = entry.get("target") {
                ensure!(
                    matches!(target.as_u64(), Some(34_962) | Some(34_963)),
                    "invalid bufferView target"
                );
            }
            Ok(BufferView {
                buffer,
                offset,
                length,
                stride,
            })
        })
        .collect()
}

fn validate_accessors(document: &Value, view_count: usize) -> Result<()> {
    let entries = document
        .get("accessors")
        .and_then(Value::as_array)
        .context("glTF accessors is not an array")?;
    for (index, accessor) in entries.iter().enumerate() {
        let accessor = accessor
            .as_object()
            .with_context(|| format!("glTF accessor[{index}] is not an object"))?;
        ensure_keys(
            accessor,
            &[
                "bufferView",
                "byteOffset",
                "componentType",
                "count",
                "extras",
                "max",
                "min",
                "name",
                "normalized",
                "type",
            ],
            &format!("glTF accessor[{index}]"),
        )?;
        let view = required_usize(accessor.get("bufferView"), "accessor bufferView")?;
        ensure!(
            view < view_count,
            "accessor[{index}] names absent bufferView"
        );
        optional_usize(accessor.get("byteOffset"), "accessor byteOffset")?;
        ensure!(
            required_usize(accessor.get("count"), "accessor count")? > 0,
            "accessor[{index}] count is zero"
        );
        ensure!(
            matches!(
                required_u64(accessor.get("componentType"), "accessor componentType")?,
                FLOAT | UNSIGNED_BYTE | UNSIGNED_SHORT | UNSIGNED_INT
            ),
            "accessor[{index}] has an unsupported componentType"
        );
        let dimensions = match accessor.get("type").and_then(Value::as_str) {
            Some("SCALAR") => 1,
            Some("VEC2") => 2,
            Some("VEC3") => 3,
            Some(other) => bail!("accessor[{index}] has unsupported type '{other}'"),
            None => bail!("accessor[{index}] type is missing or not a string"),
        };
        optional_bool(accessor.get("normalized"), "accessor normalized")?;
        for field in ["min", "max"] {
            if let Some(values) = accessor.get(field) {
                let values = values
                    .as_array()
                    .with_context(|| format!("accessor[{index}] {field} is not an array"))?;
                ensure!(
                    values.len() == dimensions,
                    "accessor[{index}] {field} has the wrong dimensions"
                );
                for value in values {
                    let value = value.as_f64().with_context(|| {
                        format!("accessor[{index}] {field} contains a non-number")
                    })?;
                    ensure!(
                        value.is_finite(),
                        "accessor[{index}] {field} contains a non-finite number"
                    );
                }
            }
        }
    }
    Ok(())
}

fn images(
    document: &serde_json::Value,
    buffers: &[Vec<u8>],
    views: &[BufferView],
) -> Result<Vec<DecodedImage>> {
    let Some(entries) = document.get("images") else {
        return Ok(Vec::new());
    };
    let entries = entries.as_array().context("glTF images is not an array")?;
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let entry = entry
                .as_object()
                .with_context(|| format!("glTF image[{index}] is not an object"))?;
            ensure_keys(
                entry,
                &["bufferView", "extras", "mimeType", "name", "uri"],
                &format!("glTF image[{index}]"),
            )?;
            let (mime, bytes) = match (entry.get("uri"), entry.get("bufferView")) {
                (Some(uri), None) => {
                    let uri = uri
                        .as_str()
                        .with_context(|| format!("glTF image[{index}].uri is not a string"))?;
                    let decoded = data_uri(uri, Some("image/"))?;
                    if let Some(declared) = entry.get("mimeType") {
                        let declared = declared.as_str().with_context(|| {
                            format!("glTF image[{index}].mimeType is not a string")
                        })?;
                        ensure!(
                            declared == decoded.0,
                            "glTF image[{index}] mimeType conflicts with its data URI"
                        );
                    }
                    decoded
                }
                (None, Some(view)) => {
                    let view = required_usize(Some(view), "image bufferView")?;
                    let mime = entry
                        .get("mimeType")
                        .and_then(serde_json::Value::as_str)
                        .context("bufferView-backed glTF image has no mimeType")?
                        .to_owned();
                    let view = views
                        .get(view)
                        .with_context(|| format!("glTF image[{index}] names absent bufferView"))?;
                    let body = &buffers[view.buffer][view.offset..view.offset + view.length];
                    (mime, body.to_vec())
                }
                _ => bail!("glTF image[{index}] must use exactly one of uri or bufferView"),
            };
            let kind = match mime.as_str() {
                "image/png" => ImageKind::Png,
                "image/jpeg" => ImageKind::Jpeg,
                _ => bail!("glTF image[{index}] has unsupported mimeType '{mime}'"),
            };
            let decoded = image::load_from_memory_with_format(&bytes, kind.format())
                .with_context(|| format!("glTF image[{index}] is not decodable {mime}"))?;
            ensure!(
                decoded.width() > 0 && decoded.height() > 0,
                "glTF image[{index}] has zero dimensions"
            );
            Ok(DecodedImage { kind, bytes })
        })
        .collect()
}

fn textures(document: &serde_json::Value, image_count: usize) -> Result<Vec<DecodedTexture>> {
    let samplers = document.get("samplers").map_or(Ok(&[][..]), |value| {
        value
            .as_array()
            .map(Vec::as_slice)
            .context("glTF samplers is not an array")
    })?;
    for (index, sampler) in samplers.iter().enumerate() {
        let sampler = sampler
            .as_object()
            .with_context(|| format!("glTF sampler[{index}] is not an object"))?;
        ensure_keys(
            sampler,
            &["extras", "magFilter", "minFilter", "name", "wrapS", "wrapT"],
            &format!("glTF sampler[{index}]"),
        )?;
        ensure!(
            sampler.get("magFilter").is_none() && sampler.get("minFilter").is_none(),
            "explicit glTF texture filtering cannot be reproduced exactly"
        );
        texture_wrap(sampler.get("wrapS"), "wrapS")?;
        texture_wrap(sampler.get("wrapT"), "wrapT")?;
    }
    let Some(entries) = document.get("textures") else {
        return Ok(Vec::new());
    };
    let entries = entries
        .as_array()
        .context("glTF textures is not an array")?;
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let entry = entry
                .as_object()
                .with_context(|| format!("glTF texture[{index}] is not an object"))?;
            ensure_keys(
                entry,
                &["extras", "name", "sampler", "source"],
                &format!("glTF texture[{index}]"),
            )?;
            let image = required_usize(entry.get("source"), "texture source")?;
            ensure!(
                image < image_count,
                "glTF texture[{index}] names absent image {image}"
            );
            let sampler = entry
                .get("sampler")
                .map(|value| required_usize(Some(value), "texture sampler"))
                .transpose()?
                .map(|sampler| {
                    samplers
                        .get(sampler)
                        .with_context(|| format!("texture[{index}] names absent sampler {sampler}"))
                })
                .transpose()?;
            let (repeat_s, repeat_t) = if let Some(sampler) = sampler {
                let sampler = sampler
                    .as_object()
                    .with_context(|| format!("glTF texture[{index}] sampler is not an object"))?;
                ensure_keys(
                    sampler,
                    &["extras", "magFilter", "minFilter", "name", "wrapS", "wrapT"],
                    &format!("glTF texture[{index}] sampler"),
                )?;
                ensure!(
                    sampler.get("magFilter").is_none() && sampler.get("minFilter").is_none(),
                    "explicit glTF texture filtering cannot be reproduced exactly"
                );
                (
                    texture_wrap(sampler.get("wrapS"), "wrapS")?,
                    texture_wrap(sampler.get("wrapT"), "wrapT")?,
                )
            } else {
                (true, true)
            };
            Ok(DecodedTexture {
                image,
                repeat_s,
                repeat_t,
            })
        })
        .collect()
}

fn texture_wrap(value: Option<&serde_json::Value>, field: &str) -> Result<bool> {
    let value = value
        .map(|value| {
            value
                .as_u64()
                .with_context(|| format!("glTF {field} is not an unsigned integer"))
        })
        .transpose()?
        .unwrap_or(10_497);
    match value {
        10_497 => Ok(true),
        33_071 => Ok(false),
        33_648 => bail!("glTF mirrored-repeat {field} is unsupported"),
        other => bail!("invalid glTF {field} value {other}"),
    }
}

fn materials(
    document: &serde_json::Value,
    textures: &[DecodedTexture],
) -> Result<Vec<DecodedMaterial>> {
    let Some(entries) = document.get("materials") else {
        return Ok(Vec::new());
    };
    let entries = entries
        .as_array()
        .context("glTF materials is not an array")?;
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let entry = entry
                .as_object()
                .with_context(|| format!("glTF material[{index}] is not an object"))?;
            ensure_keys(
                entry,
                &[
                    "alphaCutoff",
                    "alphaMode",
                    "doubleSided",
                    "emissiveFactor",
                    "emissiveTexture",
                    "extensions",
                    "extras",
                    "name",
                    "normalTexture",
                    "occlusionTexture",
                    "pbrMetallicRoughness",
                ],
                &format!("glTF material[{index}]"),
            )?;
            for unsupported in ["normalTexture", "occlusionTexture", "emissiveTexture"] {
                ensure!(
                    entry.get(unsupported).is_none(),
                    "glTF material[{index}] {unsupported} is unsupported"
                );
            }
            if let Some(extensions) = entry.get("extensions") {
                let extensions = extensions
                    .as_object()
                    .context("glTF material extensions is not an object")?;
                ensure!(
                    extensions.len() == 1 && extensions.contains_key("KHR_materials_clearcoat"),
                    "glTF material[{index}] has unsupported extensions"
                );
                let clearcoat = extensions["KHR_materials_clearcoat"]
                    .as_object()
                    .context("KHR_materials_clearcoat is not an object")?;
                ensure_keys(
                    clearcoat,
                    &[
                        "clearcoatFactor",
                        "clearcoatNormalTexture",
                        "clearcoatRoughnessFactor",
                        "clearcoatRoughnessTexture",
                        "clearcoatTexture",
                        "extras",
                    ],
                    "KHR_materials_clearcoat",
                )?;
                let factor =
                    finite_factor(clearcoat.get("clearcoatFactor"), 0.0, "clearcoatFactor")?;
                let _roughness = finite_factor(
                    clearcoat.get("clearcoatRoughnessFactor"),
                    0.0,
                    "clearcoatRoughnessFactor",
                )?;
                ensure!(
                    factor == 0.0
                        && clearcoat.get("clearcoatTexture").is_none()
                        && clearcoat.get("clearcoatRoughnessTexture").is_none()
                        && clearcoat.get("clearcoatNormalTexture").is_none(),
                    "only semantically inactive KHR_materials_clearcoat is supported"
                );
            }
            let pbr = entry
                .get("pbrMetallicRoughness")
                .map(|value| {
                    value
                        .as_object()
                        .context("pbrMetallicRoughness is not an object")
                })
                .transpose()?;
            if let Some(pbr) = pbr {
                ensure_keys(
                    pbr,
                    &[
                        "baseColorFactor",
                        "baseColorTexture",
                        "extras",
                        "metallicFactor",
                        "metallicRoughnessTexture",
                        "roughnessFactor",
                    ],
                    "glTF pbrMetallicRoughness",
                )?;
                ensure!(
                    pbr.get("metallicRoughnessTexture").is_none(),
                    "glTF metallic-roughness textures are unsupported"
                );
            }
            let base_color = pbr
                .and_then(|pbr| pbr.get("baseColorFactor"))
                .map(|value| finite_array::<4>(value, "baseColorFactor"))
                .transpose()?
                .unwrap_or([1.0; 4]);
            ensure!(
                base_color.iter().all(|value| (0.0..=1.0).contains(value)),
                "glTF baseColorFactor is outside [0, 1]"
            );
            let metallic = finite_factor(
                pbr.and_then(|pbr| pbr.get("metallicFactor")),
                1.0,
                "metallicFactor",
            )?;
            let roughness = finite_factor(
                pbr.and_then(|pbr| pbr.get("roughnessFactor")),
                1.0,
                "roughnessFactor",
            )?;
            let base_color_texture = pbr
                .and_then(|pbr| pbr.get("baseColorTexture"))
                .map(|texture| material_texture(texture, textures, "baseColorTexture"))
                .transpose()?;
            let emissive = entry
                .get("emissiveFactor")
                .map(|value| finite_array::<3>(value, "emissiveFactor"))
                .transpose()?
                .unwrap_or([0.0; 3]);
            ensure!(
                emissive.iter().all(|value| (0.0..=1.0).contains(value)),
                "glTF emissiveFactor is outside [0, 1]"
            );
            let alpha_mode = entry
                .get("alphaMode")
                .map(|value| value.as_str().context("glTF alphaMode is not a string"))
                .transpose()?
                .unwrap_or("OPAQUE");
            let alpha_blend = match alpha_mode {
                "OPAQUE" => false,
                "BLEND" => true,
                "MASK" => bail!("glTF alpha MASK cannot be reproduced exactly"),
                other => bail!("invalid glTF alphaMode '{other}'"),
            };
            ensure!(
                entry.get("alphaCutoff").is_none() || alpha_mode == "MASK",
                "glTF alphaCutoff is present without alpha MASK"
            );
            Ok(DecodedMaterial {
                base_color,
                metallic,
                roughness,
                emissive,
                double_sided: optional_bool(entry.get("doubleSided"), "doubleSided")?
                    .unwrap_or(false),
                alpha_blend,
                base_color_texture,
            })
        })
        .collect()
}

fn material_texture(
    value: &serde_json::Value,
    textures: &[DecodedTexture],
    name: &str,
) -> Result<DecodedTexture> {
    let value = value
        .as_object()
        .with_context(|| format!("glTF {name} is not an object"))?;
    ensure_keys(
        value,
        &["extensions", "extras", "index", "texCoord"],
        &format!("glTF {name}"),
    )?;
    ensure!(
        value.get("extensions").is_none(),
        "glTF {name} extensions are unsupported"
    );
    ensure!(
        value
            .get("texCoord")
            .map(|value| required_u64(Some(value), "material texture texCoord"))
            .transpose()?
            .unwrap_or(0)
            == 0,
        "only glTF TEXCOORD_0 is supported"
    );
    let index = required_usize(value.get("index"), "material texture index")?;
    textures
        .get(index)
        .cloned()
        .with_context(|| format!("glTF {name} names absent texture {index}"))
}

fn meshes(
    document: &serde_json::Value,
    buffers: &[Vec<u8>],
    views: &[BufferView],
    materials: &[DecodedMaterial],
) -> Result<Vec<Vec<MeshPrimitive>>> {
    let entries = document
        .get("meshes")
        .and_then(serde_json::Value::as_array)
        .context("glTF meshes is not an array")?;
    entries
        .iter()
        .enumerate()
        .map(|(mesh_index, mesh)| {
            let mesh = mesh
                .as_object()
                .with_context(|| format!("glTF mesh[{mesh_index}] is not an object"))?;
            ensure_keys(
                mesh,
                &["extras", "name", "primitives", "weights"],
                &format!("glTF mesh[{mesh_index}]"),
            )?;
            ensure!(
                mesh.get("weights").is_none(),
                "glTF mesh weights are unsupported"
            );
            let primitives = mesh
                .get("primitives")
                .and_then(serde_json::Value::as_array)
                .context("glTF mesh primitives is not an array")?;
            ensure!(!primitives.is_empty(), "glTF mesh has no primitives");
            primitives
                .iter()
                .enumerate()
                .map(|(primitive_index, primitive)| {
                    let primitive = primitive.as_object().with_context(|| {
                        format!(
                            "glTF mesh[{mesh_index}] primitive[{primitive_index}] is not an object"
                        )
                    })?;
                    ensure_keys(
                        primitive,
                        &[
                            "attributes",
                            "extensions",
                            "extras",
                            "indices",
                            "material",
                            "mode",
                            "targets",
                        ],
                        &format!("glTF mesh[{mesh_index}] primitive[{primitive_index}]"),
                    )?;
                    ensure!(
                        primitive.get("targets").is_none(),
                        "glTF morph targets are unsupported"
                    );
                    ensure!(
                        primitive.get("extensions").is_none(),
                        "glTF primitive extensions are unsupported"
                    );
                    ensure!(
                        primitive
                            .get("mode")
                            .map(|value| required_u64(Some(value), "primitive mode"))
                            .transpose()?
                            .unwrap_or(TRIANGLES)
                            == TRIANGLES,
                        "only glTF TRIANGLES primitives are supported"
                    );
                    let attributes = primitive
                        .get("attributes")
                        .and_then(serde_json::Value::as_object)
                        .context("glTF primitive attributes is not an object")?;
                    for attribute in attributes.keys() {
                        ensure!(
                            matches!(attribute.as_str(), "POSITION" | "NORMAL" | "TEXCOORD_0"),
                            "unsupported glTF vertex attribute {attribute}"
                        );
                    }
                    let positions = read_f32_vectors::<3>(
                        document,
                        buffers,
                        views,
                        required_usize(attributes.get("POSITION"), "POSITION accessor")?,
                        "VEC3",
                    )?;
                    ensure!(
                        !positions.is_empty(),
                        "glTF selected primitive has zero POSITION count"
                    );
                    let normals = attributes
                        .get("NORMAL")
                        .map(|value| {
                            read_f32_vectors::<3>(
                                document,
                                buffers,
                                views,
                                required_usize(Some(value), "NORMAL accessor")?,
                                "VEC3",
                            )
                        })
                        .transpose()?;
                    let texcoords = attributes
                        .get("TEXCOORD_0")
                        .map(|value| {
                            read_f32_vectors::<2>(
                                document,
                                buffers,
                                views,
                                required_usize(Some(value), "TEXCOORD_0 accessor")?,
                                "VEC2",
                            )
                        })
                        .transpose()?;
                    if let Some(normals) = &normals {
                        ensure!(
                            normals.len() == positions.len(),
                            "NORMAL count differs from POSITION"
                        );
                        ensure!(
                            normals.iter().all(|normal| {
                                let length = (normal[0] * normal[0]
                                    + normal[1] * normal[1]
                                    + normal[2] * normal[2])
                                    .sqrt();
                                (length - 1.0).abs() <= 1.0e-4
                            }),
                            "glTF NORMAL is not unit length"
                        );
                    }
                    if let Some(texcoords) = &texcoords {
                        ensure!(
                            texcoords.len() == positions.len(),
                            "TEXCOORD_0 count differs from POSITION"
                        );
                    }
                    let indices = if let Some(value) = primitive.get("indices") {
                        read_indices(
                            document,
                            buffers,
                            views,
                            required_usize(Some(value), "indices accessor")?,
                        )?
                    } else {
                        (0..positions.len())
                            .map(|index| {
                                u32::try_from(index)
                                    .context("unindexed glTF primitive exceeds u32 indices")
                            })
                            .collect::<Result<Vec<_>>>()?
                    };
                    ensure!(
                        !indices.is_empty(),
                        "glTF selected primitive has zero triangle index count"
                    );
                    ensure!(
                        indices.len().is_multiple_of(3),
                        "triangle index count is not divisible by three"
                    );
                    ensure!(
                        indices.iter().all(|index| usize::try_from(*index)
                            .is_ok_and(|index| index < positions.len())),
                        "triangle index is outside POSITION"
                    );
                    let material = primitive
                        .get("material")
                        .map(|value| required_usize(Some(value), "primitive material"))
                        .transpose()?
                        .map(|index| {
                            materials
                                .get(index)
                                .cloned()
                                .with_context(|| format!("primitive names absent material {index}"))
                        })
                        .transpose()?
                        .unwrap_or_default();
                    ensure!(
                        material.base_color_texture.is_none() || texcoords.is_some(),
                        "textured glTF primitive has no TEXCOORD_0"
                    );
                    Ok(MeshPrimitive {
                        positions,
                        normals,
                        texcoords,
                        indices,
                        material,
                    })
                })
                .collect()
        })
        .collect()
}

fn scene_primitives(
    document: &serde_json::Value,
    meshes: &[Vec<MeshPrimitive>],
) -> Result<Vec<DecodedPrimitive>> {
    let scenes = document
        .get("scenes")
        .and_then(serde_json::Value::as_array)
        .context("glTF scenes is not an array")?;
    ensure!(!scenes.is_empty(), "glTF scenes array is empty");
    for (index, scene) in scenes.iter().enumerate() {
        let scene = scene
            .as_object()
            .with_context(|| format!("glTF scene[{index}] is not an object"))?;
        ensure_keys(
            scene,
            &["extras", "name", "nodes"],
            &format!("glTF scene[{index}]"),
        )?;
    }
    let scene_index = document
        .get("scene")
        .map(|value| required_usize(Some(value), "default scene"))
        .transpose()?
        .unwrap_or(0);
    let scene = scenes
        .get(scene_index)
        .and_then(serde_json::Value::as_object)
        .with_context(|| format!("glTF default scene {scene_index} is absent"))?;
    ensure_keys(
        scene,
        &["extras", "name", "nodes"],
        &format!("glTF scene[{scene_index}]"),
    )?;
    let roots = scene
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .context("glTF scene nodes is not an array")?;
    let nodes = document
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .context("glTF nodes is not an array")?;
    for (index, node) in nodes.iter().enumerate() {
        let node = node
            .as_object()
            .with_context(|| format!("glTF node[{index}] is not an object"))?;
        ensure_keys(
            node,
            &[
                "camera",
                "children",
                "extensions",
                "extras",
                "matrix",
                "mesh",
                "name",
                "rotation",
                "scale",
                "skin",
                "translation",
                "weights",
            ],
            &format!("glTF node[{index}]"),
        )?;
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut primitives = Vec::new();
    for root in roots {
        let root = required_usize(Some(root), "scene root node")?;
        visit_node(
            root,
            &Matrix4::identity(),
            nodes,
            meshes,
            &mut visiting,
            &mut visited,
            &mut primitives,
        )?;
    }
    Ok(primitives)
}

#[allow(
    clippy::too_many_arguments,
    reason = "scene traversal carries immutable node/mesh tables plus cycle and output state"
)]
fn visit_node(
    index: usize,
    parent: &Matrix4<f64>,
    nodes: &[serde_json::Value],
    meshes: &[Vec<MeshPrimitive>],
    visiting: &mut BTreeSet<usize>,
    visited: &mut BTreeSet<usize>,
    output: &mut Vec<DecodedPrimitive>,
) -> Result<()> {
    ensure!(
        visiting.len() < MAX_NODE_DEPTH,
        "glTF scene exceeds {MAX_NODE_DEPTH} nested nodes"
    );
    ensure!(
        visiting.insert(index),
        "glTF node graph contains a cycle at {index}"
    );
    ensure!(
        visited.insert(index),
        "glTF node {index} has multiple selected-scene parents"
    );
    let node = nodes
        .get(index)
        .and_then(serde_json::Value::as_object)
        .with_context(|| format!("glTF node {index} is absent or not an object"))?;
    ensure_keys(
        node,
        &[
            "camera",
            "children",
            "extensions",
            "extras",
            "matrix",
            "mesh",
            "name",
            "rotation",
            "scale",
            "skin",
            "translation",
            "weights",
        ],
        &format!("glTF node {index}"),
    )?;
    for unsupported in ["camera", "skin", "weights", "extensions"] {
        ensure!(
            node.get(unsupported).is_none(),
            "glTF node {index} {unsupported} is unsupported"
        );
    }
    let transform = parent * node_transform(node)?;
    ensure!(
        transform.iter().all(|value| value.is_finite()),
        "glTF composed node transform overflowed"
    );
    if let Some(mesh) = node.get("mesh") {
        let mesh_index = required_usize(Some(mesh), "node mesh")?;
        let mesh = meshes
            .get(mesh_index)
            .with_context(|| format!("glTF node {index} names absent mesh {mesh_index}"))?;
        for primitive in mesh {
            output.push(transform_primitive(primitive, &transform)?);
        }
    }
    if let Some(children) = node.get("children") {
        let children = children
            .as_array()
            .context("glTF node children is not an array")?;
        for child in children {
            visit_node(
                required_usize(Some(child), "child node")?,
                &transform,
                nodes,
                meshes,
                visiting,
                visited,
                output,
            )?;
        }
    }
    visiting.remove(&index);
    Ok(())
}

fn node_transform(node: &serde_json::Map<String, serde_json::Value>) -> Result<Matrix4<f64>> {
    if let Some(matrix) = node.get("matrix") {
        ensure!(
            node.get("translation").is_none()
                && node.get("rotation").is_none()
                && node.get("scale").is_none(),
            "glTF node may not combine matrix with TRS"
        );
        let values = finite_array::<16>(matrix, "node matrix")?;
        ensure!(
            values[3] == 0.0 && values[7] == 0.0 && values[11] == 0.0 && values[15] == 1.0,
            "glTF node matrix must be affine"
        );
        return Ok(Matrix4::from_column_slice(&values));
    }
    let translation = node
        .get("translation")
        .map(|value| finite_array::<3>(value, "node translation"))
        .transpose()?
        .unwrap_or([0.0; 3]);
    let rotation = node
        .get("rotation")
        .map(|value| finite_array::<4>(value, "node rotation"))
        .transpose()?
        .unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let scale = node
        .get("scale")
        .map(|value| finite_array::<3>(value, "node scale"))
        .transpose()?
        .unwrap_or([1.0; 3]);
    ensure!(
        scale.iter().all(|value| *value != 0.0),
        "glTF node scale is singular"
    );
    let quaternion = Quaternion::new(rotation[3], rotation[0], rotation[1], rotation[2]);
    ensure!(
        (quaternion.norm() - 1.0).abs() <= 1.0e-5,
        "glTF node quaternion is not normalized"
    );
    let rotation = UnitQuaternion::from_quaternion(quaternion);
    Ok(
        Translation3::new(translation[0], translation[1], translation[2]).to_homogeneous()
            * rotation.to_homogeneous()
            * Matrix4::new_nonuniform_scaling(&Vector3::new(scale[0], scale[1], scale[2])),
    )
}

fn transform_primitive(
    primitive: &MeshPrimitive,
    transform: &Matrix4<f64>,
) -> Result<DecodedPrimitive> {
    let positions = primitive
        .positions
        .iter()
        .map(|position| {
            let transformed = transform * Vector4::new(position[0], position[1], position[2], 1.0);
            ensure!(
                transformed.w != 0.0,
                "glTF node transform produced a point at infinity"
            );
            let position = [
                transformed.x / transformed.w,
                transformed.y / transformed.w,
                transformed.z / transformed.w,
            ];
            ensure!(
                position.iter().all(|value| value.is_finite()),
                "glTF node transform overflowed a vertex"
            );
            Ok(position)
        })
        .collect::<Result<Vec<_>>>()?;
    let linear = transform.fixed_view::<3, 3>(0, 0).into_owned();
    let determinant = linear.determinant();
    ensure!(
        determinant.is_finite() && determinant != 0.0,
        "glTF node transform is singular or overflowed"
    );
    let normal_matrix: Matrix3<f64> = linear
        .try_inverse()
        .context("glTF normal transform is singular")?
        .transpose();
    let normals = primitive
        .normals
        .as_ref()
        .map(|normals| {
            normals
                .iter()
                .map(|normal| {
                    let transformed = normal_matrix * Vector3::new(normal[0], normal[1], normal[2]);
                    let norm = transformed.norm();
                    ensure!(
                        norm.is_finite() && norm > 0.0,
                        "glTF normal is zero or overflowed after transform"
                    );
                    let transformed = transformed / norm;
                    Ok([transformed.x, transformed.y, transformed.z])
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    let mut indices = primitive.indices.clone();
    if determinant.is_sign_negative() {
        for triangle in indices.as_chunks_mut::<3>().0 {
            triangle.swap(1, 2);
        }
    }
    Ok(DecodedPrimitive {
        positions,
        normals,
        texcoords: primitive.texcoords.clone(),
        indices,
        material: primitive.material.clone(),
    })
}

fn read_f32_vectors<const N: usize>(
    document: &serde_json::Value,
    buffers: &[Vec<u8>],
    views: &[BufferView],
    index: usize,
    expected_type: &str,
) -> Result<Vec<[f64; N]>> {
    let (bytes, count, stride, component_type, accessor_type, normalized) =
        accessor(document, buffers, views, index, N * 4)?;
    ensure!(component_type == FLOAT, "accessor {index} is not FLOAT");
    ensure!(
        accessor_type == expected_type,
        "accessor {index} is not {expected_type}"
    );
    ensure!(!normalized, "FLOAT accessor {index} may not be normalized");
    let vectors = (0..count)
        .map(|element| {
            let start = element * stride;
            let mut vector = [0.0; N];
            for (component, value) in vector.iter_mut().enumerate() {
                let offset = start + component * 4;
                let parsed = f32::from_le_bytes(bytes[offset..offset + 4].try_into()?);
                ensure!(
                    parsed.is_finite(),
                    "accessor {index} contains non-finite FLOAT"
                );
                *value = f64::from(parsed);
            }
            Ok(vector)
        })
        .collect::<Result<Vec<_>>>()?;
    validate_declared_bounds(document, index, &vectors)?;
    Ok(vectors)
}

fn validate_declared_bounds<const N: usize>(
    document: &Value,
    index: usize,
    values: &[[f64; N]],
) -> Result<()> {
    let accessor = document
        .get("accessors")
        .and_then(Value::as_array)
        .and_then(|accessors| accessors.get(index))
        .and_then(Value::as_object)
        .with_context(|| format!("glTF accessor {index} is absent or not an object"))?;
    for (field, minimum) in [("min", true), ("max", false)] {
        let Some(declared) = accessor.get(field) else {
            continue;
        };
        let declared = finite_array::<N>(declared, &format!("accessor {field}"))?;
        for component in 0..N {
            let observed = values
                .iter()
                .map(|value| value[component])
                .reduce(if minimum { f64::min } else { f64::max })
                .context("zero-count accessor cannot declare bounds")?;
            let tolerance = declared[component].abs().max(1.0) * f64::EPSILON * 4.0;
            ensure!(
                (observed - declared[component]).abs() <= tolerance,
                "glTF accessor {index} {field}[{component}] is {}, decoded data is {observed}",
                declared[component]
            );
        }
    }
    Ok(())
}

fn read_indices(
    document: &serde_json::Value,
    buffers: &[Vec<u8>],
    views: &[BufferView],
    index: usize,
) -> Result<Vec<u32>> {
    let accessors = document
        .get("accessors")
        .and_then(serde_json::Value::as_array)
        .context("glTF accessors is not an array")?;
    let metadata = accessors
        .get(index)
        .and_then(serde_json::Value::as_object)
        .with_context(|| format!("glTF accessor {index} is absent or not an object"))?;
    let component_type = required_u64(metadata.get("componentType"), "accessor componentType")?;
    let width = match component_type {
        UNSIGNED_BYTE => 1,
        UNSIGNED_SHORT => 2,
        UNSIGNED_INT => 4,
        _ => bail!("index accessor {index} has unsupported componentType {component_type}"),
    };
    let (bytes, count, stride, _, accessor_type, normalized) =
        accessor(document, buffers, views, index, width)?;
    ensure!(
        accessor_type == "SCALAR",
        "index accessor {index} is not SCALAR"
    );
    ensure!(!normalized, "index accessor {index} may not be normalized");
    let indices = (0..count)
        .map(|element| {
            let start = element * stride;
            Ok(match width {
                1 => u32::from(bytes[start]),
                2 => u32::from(u16::from_le_bytes(bytes[start..start + 2].try_into()?)),
                4 => u32::from_le_bytes(bytes[start..start + 4].try_into()?),
                _ => unreachable!(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    for (field, minimum) in [("min", true), ("max", false)] {
        let Some(declared) = metadata.get(field) else {
            continue;
        };
        let declared = finite_array::<1>(declared, &format!("index accessor {field}"))?[0];
        let observed = indices
            .iter()
            .copied()
            .reduce(if minimum { u32::min } else { u32::max })
            .context("zero-count index accessor cannot declare bounds")?;
        ensure!(
            f64::from(observed) == declared,
            "glTF index accessor {index} {field} does not match decoded data"
        );
    }
    Ok(indices)
}

fn accessor<'a>(
    document: &serde_json::Value,
    buffers: &'a [Vec<u8>],
    views: &[BufferView],
    index: usize,
    element_size: usize,
) -> Result<AccessorData<'a>> {
    let accessors = document
        .get("accessors")
        .and_then(serde_json::Value::as_array)
        .context("glTF accessors is not an array")?;
    let accessor = accessors
        .get(index)
        .and_then(serde_json::Value::as_object)
        .with_context(|| format!("glTF accessor {index} is absent or not an object"))?;
    ensure_keys(
        accessor,
        &[
            "bufferView",
            "byteOffset",
            "componentType",
            "count",
            "extras",
            "max",
            "min",
            "name",
            "normalized",
            "type",
        ],
        &format!("glTF accessor {index}"),
    )?;
    ensure!(
        accessor.get("sparse").is_none(),
        "sparse accessors are unsupported"
    );
    let view_index = required_usize(accessor.get("bufferView"), "accessor bufferView")?;
    let view = views
        .get(view_index)
        .with_context(|| format!("accessor {index} names absent bufferView {view_index}"))?;
    let offset = optional_usize(accessor.get("byteOffset"), "accessor byteOffset")?.unwrap_or(0);
    let count = required_usize(accessor.get("count"), "accessor count")?;
    let component_type = required_u64(accessor.get("componentType"), "accessor componentType")?;
    let component_width = match component_type {
        UNSIGNED_BYTE => 1,
        UNSIGNED_SHORT => 2,
        UNSIGNED_INT | FLOAT => 4,
        _ => bail!("accessor {index} has unsupported componentType {component_type}"),
    };
    let accessor_type = accessor
        .get("type")
        .and_then(serde_json::Value::as_str)
        .context("accessor type is not a string")?
        .to_owned();
    let normalized =
        optional_bool(accessor.get("normalized"), "accessor normalized")?.unwrap_or(false);
    let stride = view.stride.unwrap_or(element_size);
    ensure!(
        stride >= element_size,
        "accessor stride is smaller than its element"
    );
    let absolute_offset = view
        .offset
        .checked_add(offset)
        .context("accessor absolute byte offset overflowed")?;
    ensure!(
        stride.is_multiple_of(component_width) && absolute_offset.is_multiple_of(component_width),
        "accessor {index} is not aligned to its component width"
    );
    let required = if count == 0 {
        offset
    } else {
        offset
            .checked_add(
                (count - 1)
                    .checked_mul(stride)
                    .context("accessor stride overflowed")?,
            )
            .and_then(|end| end.checked_add(element_size))
            .context("accessor byte range overflowed")?
    };
    ensure!(
        required <= view.length,
        "accessor {index} exceeds its bufferView"
    );
    let buffer = &buffers[view.buffer];
    let start = absolute_offset;
    Ok((
        &buffer[start..start + required.saturating_sub(offset)],
        count,
        stride,
        component_type,
        accessor_type,
        normalized,
    ))
}

fn data_uri(uri: &str, required_prefix: Option<&str>) -> Result<(String, Vec<u8>)> {
    let (metadata, payload) = uri
        .strip_prefix("data:")
        .and_then(|uri| uri.split_once(','))
        .context("URI is external or is not a data URI")?;
    let mime = metadata
        .strip_suffix(";base64")
        .context("only base64 data URIs are supported")?;
    if let Some(prefix) = required_prefix {
        ensure!(
            mime.starts_with(prefix),
            "data URI mimeType '{mime}' is invalid"
        );
    }
    Ok((mime.to_owned(), decode_base64(payload)?))
}

fn decode_base64(value: &str) -> Result<Vec<u8>> {
    ensure!(
        value.len().is_multiple_of(4),
        "base64 length is not divisible by four"
    );
    let mut output = Vec::with_capacity(value.len() / 4 * 3);
    for (chunk_index, chunk) in value.as_bytes().as_chunks::<4>().0.iter().enumerate() {
        let last = chunk_index + 1 == value.len() / 4;
        let a = base64_value(chunk[0])?;
        let b = base64_value(chunk[1])?;
        ensure!(
            chunk[0] != b'=' && chunk[1] != b'=',
            "invalid base64 padding"
        );
        let c = if chunk[2] == b'=' {
            0
        } else {
            base64_value(chunk[2])?
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            base64_value(chunk[3])?
        };
        ensure!(
            last || (chunk[2] != b'=' && chunk[3] != b'='),
            "base64 padding appears before the final quantum"
        );
        ensure!(
            chunk[2] != b'=' || chunk[3] == b'=',
            "invalid base64 padding order"
        );
        ensure!(
            chunk[2] != b'=' || b & 0x0f == 0,
            "nonzero base64 padding bits"
        );
        ensure!(
            chunk[3] != b'=' || c & 0x03 == 0,
            "nonzero base64 padding bits"
        );
        output.push((a << 2) | (b >> 4));
        if chunk[2] != b'=' {
            output.push((b << 4) | (c >> 2));
        }
        if chunk[3] != b'=' {
            output.push((c << 6) | d);
        }
    }
    Ok(output)
}

fn base64_value(value: u8) -> Result<u8> {
    match value {
        b'A'..=b'Z' => Ok(value - b'A'),
        b'a'..=b'z' => Ok(value - b'a' + 26),
        b'0'..=b'9' => Ok(value - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        b'=' => Ok(0),
        _ => bail!("invalid base64 character"),
    }
}

fn finite_array<const N: usize>(value: &serde_json::Value, name: &str) -> Result<[f64; N]> {
    let values = value
        .as_array()
        .with_context(|| format!("glTF {name} is not an array"))?;
    ensure!(values.len() == N, "glTF {name} must have {N} values");
    let mut result = [0.0; N];
    for (target, value) in result.iter_mut().zip(values) {
        *target = value
            .as_f64()
            .with_context(|| format!("glTF {name} contains a non-number"))?;
        ensure!(
            target.is_finite(),
            "glTF {name} contains a non-finite number"
        );
    }
    Ok(result)
}

fn finite_factor(value: Option<&serde_json::Value>, default: f64, name: &str) -> Result<f64> {
    let value = value.map_or(Ok(default), |value| {
        value
            .as_f64()
            .with_context(|| format!("glTF {name} is not a number"))
    })?;
    ensure!(
        value.is_finite() && (0.0..=1.0).contains(&value),
        "glTF {name} is outside [0, 1]"
    );
    Ok(value)
}

fn optional_bool(value: Option<&serde_json::Value>, name: &str) -> Result<Option<bool>> {
    value
        .map(|value| {
            value
                .as_bool()
                .with_context(|| format!("glTF {name} is not a boolean"))
        })
        .transpose()
}

fn required_usize(value: Option<&serde_json::Value>, name: &str) -> Result<usize> {
    usize::try_from(required_u64(value, name)?)
        .with_context(|| format!("glTF {name} exceeds usize"))
}

fn optional_usize(value: Option<&serde_json::Value>, name: &str) -> Result<Option<usize>> {
    value
        .map(|value| required_usize(Some(value), name))
        .transpose()
}

fn required_u64(value: Option<&serde_json::Value>, name: &str) -> Result<u64> {
    value
        .and_then(serde_json::Value::as_u64)
        .with_context(|| format!("glTF {name} is missing or not an unsigned integer"))
}

fn ensure_keys(object: &Map<String, Value>, accepted: &[&str], name: &str) -> Result<()> {
    for key in object.keys() {
        ensure!(
            accepted.contains(&key.as_str()),
            "{name} contains unsupported field '{key}'"
        );
    }
    Ok(())
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let end = offset
        .checked_add(4)
        .context("GLB integer offset overflowed")?;
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..end)
            .context("GLB integer is truncated")?
            .try_into()?,
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    fn glb(json: &serde_json::Value, binary: Option<&[u8]>) -> Vec<u8> {
        let mut json = serde_json::to_vec(json).expect("JSON");
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let binary_length = binary.map_or(0, |binary| 8 + binary.len().div_ceil(4) * 4);
        let total = 12 + 8 + json.len() + binary_length;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&2_u32.to_le_bytes());
        bytes.extend_from_slice(&u32::try_from(total).expect("length").to_le_bytes());
        bytes.extend_from_slice(&u32::try_from(json.len()).expect("length").to_le_bytes());
        bytes.extend_from_slice(&JSON_CHUNK.to_le_bytes());
        bytes.extend_from_slice(&json);
        if let Some(binary) = binary {
            let padded = binary.len().div_ceil(4) * 4;
            bytes.extend_from_slice(&u32::try_from(padded).expect("length").to_le_bytes());
            bytes.extend_from_slice(&BIN_CHUNK.to_le_bytes());
            bytes.extend_from_slice(binary);
            bytes.resize(total, 0);
        }
        bytes
    }

    fn triangle_document(buffer: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "asset": { "version": "2.0" },
            "scene": 0,
            "scenes": [{ "nodes": [0] }],
            "nodes": [{ "mesh": 0 }],
            "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0 }, "indices": 1 }] }],
            "buffers": [buffer],
            "bufferViews": [
                { "buffer": 0, "byteOffset": 0, "byteLength": 36 },
                { "buffer": 0, "byteOffset": 36, "byteLength": 6 }
            ],
            "accessors": [
                { "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3" },
                { "bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR" }
            ]
        })
    }

    fn triangle_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for index in [0_u16, 1, 2] {
            bytes.extend_from_slice(&index.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn strict_container_correlates_buffer_zero_and_bin_padding() {
        let binary = triangle_bytes();
        let document = triangle_document(serde_json::json!({ "byteLength": binary.len() }));
        DecodedMesh::decode(&glb(&document, Some(&binary))).expect("closed triangle");

        let missing = glb(&document, None);
        assert!(DecodedMesh::decode(&missing).is_err());
        let short = triangle_document(serde_json::json!({ "byteLength": binary.len() + 3 }));
        assert!(DecodedMesh::decode(&glb(&short, Some(&binary))).is_err());
        let embedded = triangle_document(serde_json::json!({
            "byteLength": binary.len(),
            "uri": format!("data:application/octet-stream;base64,{}", encode_base64(&binary))
        }));
        DecodedMesh::decode(&glb(&embedded, None)).expect("embedded buffer data URI");
        assert!(DecodedMesh::decode(&glb(&embedded, Some(&binary))).is_err());

        let too_much_padding = triangle_document(serde_json::json!({
            "byteLength": binary.len() - 4
        }));
        assert!(DecodedMesh::decode(&glb(&too_much_padding, Some(&binary))).is_err());

        let mut nonzero_padding = binary.clone();
        nonzero_padding[41] = 1;
        let nonzero_padding_document = triangle_document(serde_json::json!({
            "byteLength": binary.len() - 1
        }));
        assert!(
            DecodedMesh::decode(&glb(&nonzero_padding_document, Some(&nonzero_padding))).is_err()
        );
    }

    #[test]
    fn collision_triangles_require_three_distinct_points() {
        let mut binary = triangle_bytes();
        let first = binary[..12].to_vec();
        binary[12..24].copy_from_slice(&first);
        let document = triangle_document(serde_json::json!({ "byteLength": binary.len() }));
        let decoded = DecodedMesh::decode(&glb(&document, Some(&binary)))
            .expect("degenerate visual geometry still decodes");
        let error = decoded
            .validate_collision()
            .expect_err("coincident collision vertices are rejected");
        assert!(error.to_string().contains("coincident vertices"));
    }

    #[test]
    fn strict_container_requires_json_first_and_rejects_unknown_chunks() {
        let document = triangle_document(serde_json::json!({
            "byteLength": 42,
            "uri": format!(
                "data:application/octet-stream;base64,{}",
                encode_base64(&triangle_bytes())
            )
        }));
        let mut misordered = glb(&document, None);
        misordered[16..20].copy_from_slice(&BIN_CHUNK.to_le_bytes());
        assert!(container(&misordered).is_err());

        let mut unknown = glb(&document, None);
        unknown.extend_from_slice(&0_u32.to_le_bytes());
        unknown.extend_from_slice(&0x1234_5678_u32.to_le_bytes());
        let length = u32::try_from(unknown.len()).expect("test length");
        unknown[8..12].copy_from_slice(&length.to_le_bytes());
        assert!(container(&unknown).is_err());

        let mut json = serde_json::to_vec(&document).expect("JSON");
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        json.extend_from_slice(b"    ");
        let total = 12 + 8 + json.len();
        let mut overpadded = Vec::new();
        overpadded.extend_from_slice(MAGIC);
        overpadded.extend_from_slice(&2_u32.to_le_bytes());
        overpadded.extend_from_slice(&u32::try_from(total).expect("length").to_le_bytes());
        overpadded.extend_from_slice(&u32::try_from(json.len()).expect("length").to_le_bytes());
        overpadded.extend_from_slice(&JSON_CHUNK.to_le_bytes());
        overpadded.extend_from_slice(&json);
        assert!(container(&overpadded).is_err());
    }

    #[test]
    fn buffers_are_typed_nonempty_and_byte_length_is_exact() {
        let binary = triangle_bytes();
        for buffers in [
            serde_json::json!([]),
            serde_json::json!([null]),
            serde_json::json!([{}]),
            serde_json::json!([{ "byteLength": "42" }]),
        ] {
            let mut document = triangle_document(serde_json::json!({
                "byteLength": binary.len()
            }));
            document["buffers"] = buffers;
            assert!(DecodedMesh::decode(&glb(&document, Some(&binary))).is_err());
        }
    }

    #[test]
    fn external_uris_and_invalid_images_are_rejected() {
        let mut document = triangle_document(serde_json::json!({
            "byteLength": 42,
            "uri": "other.bin"
        }));
        assert!(DecodedMesh::decode(&glb(&document, None)).is_err());
        document["buffers"][0] = serde_json::json!({
            "byteLength": 42,
            "uri": format!("data:application/octet-stream;base64,{}", encode_base64(&triangle_bytes()))
        });
        document["images"] = serde_json::json!([{
            "uri": format!(
                "data:image/png;base64,{}",
                encode_base64(b"\x89PNG\r\n\x1a\ntruncated")
            )
        }]);
        assert!(DecodedMesh::decode(&glb(&document, None)).is_err());
    }

    #[test]
    fn embedded_png_and_jpeg_are_fully_decoded_and_mime_checked() {
        for (mime, format) in [
            ("image/png", ImageFormat::Png),
            ("image/jpeg", ImageFormat::Jpeg),
        ] {
            let mut bytes = Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(1, 1)
                .write_to(&mut bytes, format)
                .expect("test image encodes");
            let mut document = triangle_document(serde_json::json!({
                "byteLength": 42,
                "uri": format!(
                    "data:application/octet-stream;base64,{}",
                    encode_base64(&triangle_bytes())
                )
            }));
            document["images"] = serde_json::json!([{
                "mimeType": mime,
                "uri": format!("data:{mime};base64,{}", encode_base64(bytes.get_ref()))
            }]);
            let decoded = DecodedMesh::decode(&glb(&document, None))
                .expect("decodable embedded image is accepted");
            assert_eq!(decoded.images.len(), 1);

            document["images"][0]["mimeType"] = serde_json::json!(if mime == "image/png" {
                "image/jpeg"
            } else {
                "image/png"
            });
            assert!(DecodedMesh::decode(&glb(&document, None)).is_err());
        }
    }

    #[test]
    fn embedded_texture_renders_and_extracts_with_native_uv_coordinates() {
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([128, 64, 255, 128]),
        ))
        .write_to(&mut png, ImageFormat::Png)
        .expect("test PNG encodes");
        let mut binary = triangle_bytes();
        binary.extend_from_slice(&[0, 0]);
        for value in [0.0_f32, 0.0, 1.0, 0.0, 0.0, 1.0] {
            binary.extend_from_slice(&value.to_le_bytes());
        }
        let mut document = triangle_document(serde_json::json!({
            "byteLength": binary.len()
        }));
        document["bufferViews"]
            .as_array_mut()
            .expect("views")
            .push(serde_json::json!({
                "buffer": 0,
                "byteOffset": 44,
                "byteLength": 24
            }));
        document["accessors"]
            .as_array_mut()
            .expect("accessors")
            .push(serde_json::json!({
                "bufferView": 2,
                "componentType": 5126,
                "count": 3,
                "type": "VEC2"
            }));
        document["images"] = serde_json::json!([{
            "uri": format!(
                "data:image/png;base64,{}",
                encode_base64(png.get_ref())
            )
        }]);
        document["samplers"] = serde_json::json!([{ "wrapS": 33071, "wrapT": 10497 }]);
        document["textures"] = serde_json::json!([{ "source": 0, "sampler": 0 }]);
        document["materials"] = serde_json::json!([{
            "alphaMode": "BLEND",
            "pbrMetallicRoughness": {
                "baseColorFactor": [0.5, 0.25, 0.75, 0.5],
                "baseColorTexture": { "index": 0 }
            }
        }]);
        document["meshes"][0]["primitives"][0]["attributes"]["TEXCOORD_0"] = serde_json::json!(2);
        document["meshes"][0]["primitives"][0]["material"] = serde_json::json!(0);

        let decoded = DecodedMesh::decode(&glb(&document, Some(&binary)))
            .expect("textured primitive decodes");
        let mut source = String::from("Group { children [\n");
        decoded
            .render_visual(&mut source, 2, |image| Ok(format!("textures/{image}.png")))
            .expect("textured geometry renders");
        source.push_str("] }\n");
        assert!(source.contains("baseColorMap ImageTexture"));
        assert!(source.contains("baseColor 1 1 1"));
        assert!(!source.contains("transparency"));
        assert!(source.contains("url [\"textures/0.png\"]"));
        assert!(source.contains("repeatS FALSE"));
        assert!(source.contains("repeatT TRUE"));
        assert!(source.contains("texCoord TextureCoordinate"));
        assert!(
            source.contains("0 1"),
            "glTF V is flipped into Webots UV space"
        );
        let _: webots_proto_ast::Proto = source.parse().expect("textured source parses");

        let staged = tempfile::tempdir().expect("texture staging root");
        crate::generation::stage_decoded_images(staged.path(), "fixture.glb", &decoded)
            .expect("texture extracts");
        let extracted = staged.path().join("fixture.glb.images/0.png");
        let extracted = image::load_from_memory_with_format(
            &fs::read(extracted).expect("extracted PNG"),
            ImageFormat::Png,
        )
        .expect("extracted PNG decodes")
        .to_rgba8();
        assert_eq!(extracted.get_pixel(0, 0).0, [92, 30, 225, 64]);

        document["materials"][0]["alphaMode"] = serde_json::json!("OPAQUE");
        let opaque = DecodedMesh::decode(&glb(&document, Some(&binary)))
            .expect("opaque textured primitive decodes")
            .staged_texture(0)
            .expect("opaque texture bakes")
            .expect("textured primitive has staged texture");
        let opaque = image::load_from_memory_with_format(&opaque, ImageFormat::Png)
            .expect("opaque texture decodes");
        assert!(!opaque.color().has_alpha());
        let opaque = opaque.to_rgba8();
        assert_eq!(opaque.get_pixel(0, 0).0, [92, 30, 225, 255]);
    }

    #[test]
    fn selected_primitives_must_have_vertices_and_triangles() {
        let binary = triangle_bytes();
        let mut document = triangle_document(serde_json::json!({
            "byteLength": binary.len()
        }));
        document["accessors"][0]["count"] = serde_json::json!(0);
        assert!(DecodedMesh::decode(&glb(&document, Some(&binary))).is_err());

        let mut document = triangle_document(serde_json::json!({
            "byteLength": binary.len()
        }));
        document["accessors"][1]["count"] = serde_json::json!(0);
        assert!(DecodedMesh::decode(&glb(&document, Some(&binary))).is_err());
    }

    #[test]
    fn selected_node_transform_is_baked_into_positions() {
        let binary = triangle_bytes();
        let mut document = triangle_document(serde_json::json!({
            "byteLength": binary.len()
        }));
        document["nodes"][0]["translation"] = serde_json::json!([1.0, 2.0, 3.0]);
        document["nodes"][0]["scale"] = serde_json::json!([2.0, 3.0, 4.0]);
        let decoded = DecodedMesh::decode(&glb(&document, Some(&binary)))
            .expect("transformed primitive decodes");
        assert_eq!(
            decoded.primitives[0].positions,
            vec![[1.0, 2.0, 3.0], [3.0, 2.0, 3.0], [1.0, 5.0, 3.0]]
        );
    }

    #[test]
    fn finite_authored_transforms_must_not_overflow_native_geometry() {
        let binary = triangle_bytes();
        let mut document = triangle_document(serde_json::json!({
            "byteLength": binary.len()
        }));
        document["nodes"] = serde_json::json!([
            { "translation": [1.0e308, 0.0, 0.0], "children": [1] },
            { "translation": [1.0e308, 0.0, 0.0], "mesh": 0 }
        ]);
        assert!(DecodedMesh::decode(&glb(&document, Some(&binary))).is_err());

        document["nodes"] = serde_json::json!([{ "scale": [2.0, 2.0, 2.0], "mesh": 0 }]);
        let decoded =
            DecodedMesh::decode(&glb(&document, Some(&binary))).expect("finite visual positions");
        assert!(
            decoded
                .render_collision_scaled(&mut String::new(), 0, [f64::MAX; 3])
                .is_err()
        );
    }

    #[test]
    fn scene_depth_is_bounded_before_recursive_expansion() {
        let binary = triangle_bytes();
        let mut document = triangle_document(serde_json::json!({ "byteLength": binary.len() }));
        let mut nodes: Vec<_> = (0..MAX_NODE_DEPTH)
            .map(|index| serde_json::json!({ "children": [index + 1] }))
            .collect();
        nodes.push(serde_json::json!({ "mesh": 0 }));
        document["nodes"] = serde_json::json!(nodes);
        let error = DecodedMesh::decode(&glb(&document, Some(&binary))).expect_err("bounded depth");
        assert!(error.to_string().contains("nested nodes"));
    }

    #[test]
    fn double_sided_material_emits_a_reversed_native_backface() {
        let binary = triangle_bytes();
        let mut document = triangle_document(serde_json::json!({
            "byteLength": binary.len()
        }));
        document["materials"] = serde_json::json!([{ "doubleSided": true }]);
        document["meshes"][0]["primitives"][0]["material"] = serde_json::json!(0);
        let decoded = DecodedMesh::decode(&glb(&document, Some(&binary)))
            .expect("double-sided primitive decodes");
        let mut source = String::new();
        decoded
            .render_visual(&mut source, 0, |_| bail!("fixture has no texture"))
            .expect("double-sided primitive renders");
        assert!(source.contains("0 1 2 -1"));
        assert!(source.contains("3 5 4 -1"));
        assert!(!source.contains("solid "));
    }

    #[test]
    fn unknown_semantic_fields_and_malformed_inactive_clearcoat_are_rejected() {
        let binary = triangle_bytes();
        for (array, index) in [
            ("buffers", 0),
            ("bufferViews", 0),
            ("accessors", 0),
            ("meshes", 0),
            ("scenes", 0),
            ("nodes", 0),
        ] {
            let mut document = triangle_document(serde_json::json!({
                "byteLength": binary.len()
            }));
            document[array][index]["typo"] = serde_json::json!(true);
            assert!(DecodedMesh::decode(&glb(&document, Some(&binary))).is_err());
        }

        let mut primitive = triangle_document(serde_json::json!({
            "byteLength": binary.len()
        }));
        primitive["meshes"][0]["primitives"][0]["typo"] = serde_json::json!(true);
        assert!(
            DecodedMesh::decode(&glb(&primitive, Some(&binary)))
                .expect_err("primitive typo")
                .to_string()
                .contains("unsupported field")
        );

        for mutation in [
            serde_json::json!({ "images": [{ "typo": true }] }),
            serde_json::json!({ "samplers": [{ "typo": true }] }),
            serde_json::json!({ "textures": [{ "typo": true }] }),
            serde_json::json!({ "materials": [{ "typo": true }] }),
            serde_json::json!({
                "materials": [{ "pbrMetallicRoughness": { "typo": true } }]
            }),
            serde_json::json!({
                "extensionsUsed": ["KHR_materials_clearcoat"],
                "materials": [{
                    "extensions": { "KHR_materials_clearcoat": { "typo": true } }
                }]
            }),
        ] {
            let mut document = triangle_document(serde_json::json!({
                "byteLength": binary.len()
            }));
            for (key, value) in mutation.as_object().expect("mutation object") {
                document[key] = value.clone();
            }
            assert!(
                DecodedMesh::decode(&glb(&document, Some(&binary)))
                    .expect_err("semantic typo")
                    .to_string()
                    .contains("unsupported field")
            );
        }

        let mut document = triangle_document(serde_json::json!({
            "byteLength": binary.len()
        }));
        document["materials"] = serde_json::json!([{
            "extensions": { "KHR_materials_clearcoat": {
                "clearcoatFactor": 0,
                "clearcoatRoughnessFactor": 2
            } }
        }]);
        document["extensionsUsed"] = serde_json::json!(["KHR_materials_clearcoat"]);
        document["meshes"][0]["primitives"][0]["material"] = serde_json::json!(0);
        assert!(DecodedMesh::decode(&glb(&document, Some(&binary))).is_err());
    }

    #[test]
    fn required_framework_glbs_decode_to_webots_native_geometry() {
        for (name, bytes) in [
            (
                "ddsm115",
                &include_bytes!("../../../../components/ddsm115/meshes/ddsm115.glb")[..],
            ),
            (
                "drive_motor",
                &include_bytes!(
                    "../../../../fixture/components/drive_motor/meshes/drive_motor.glb"
                )[..],
            ),
        ] {
            let decoded = DecodedMesh::decode(bytes)
                .unwrap_or_else(|error| panic!("{name} must decode: {error:#}"));
            assert!(!decoded.primitives.is_empty());
            assert!(
                decoded
                    .primitives
                    .iter()
                    .all(|primitive| !primitive.positions.is_empty())
            );
            let mut source = String::from("Group { children [\n");
            decoded
                .render_visual(&mut source, 2, |_| bail!("fixture has no image"))
                .expect("native visual renders");
            source.push_str("] }\n");
            assert!(source.contains("IndexedFaceSet"));
            assert!(!source.contains("CadShape"));
            assert!(!source.contains("url [\""));
            let _: webots_proto_ast::Proto = source
                .parse()
                .unwrap_or_else(|error| panic!("{name} native geometry parses: {error}"));
        }
    }

    #[test]
    #[ignore = "requires an installed Webots R2025a runtime"]
    fn installed_webots_loads_native_decoded_geometry_without_asset_warnings() {
        let webots = webots_executable().expect("WEBOTS_HOME or installed Webots R2025a");
        let output = Command::new(&webots)
            .arg("--version")
            .output()
            .expect("Webots version runs");
        let version = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success() && version.contains("R2025a"),
            "native renderer proof requires runnable Webots R2025a; {} returned {}:\n{version}",
            webots.display(),
            output.status
        );

        let bundle = crate::generation::tests::compile_mesh_world(
            include_bytes!("../../../../fixture/components/drive_motor/meshes/drive_motor.glb"),
            None,
        );
        let project = tempfile::tempdir().expect("temporary Webots project");
        let root = project.path().join("generated");
        let executable = std::env::current_exe().expect("test executable");
        let generated = crate::generation::stage_project(
            &bundle,
            &root,
            "tcp://127.0.0.1:7000",
            &crate::generation::ControllerExecutables {
                world: executable.clone(),
                robot: executable,
            },
        )
        .expect("production project staging");
        let world = fs::read_to_string(generated.world())
            .expect("generated world")
            .replace(crate::WORLD_CONTROLLER_PACKAGE, "renderer_probe");
        assert!(!world.contains("CadShape"));
        assert!(!world.contains("url [\""));
        let _: webots_proto_ast::Proto = world.parse().expect("probe world parses");

        let worlds = root.join("worlds");
        let controller = root.join("controllers/renderer_probe");
        fs::create_dir_all(&worlds).expect("worlds directory");
        fs::create_dir_all(&controller).expect("controller directory");
        fs::write(worlds.join("native_renderer.wbt"), world).expect("probe world");
        let controller_path = controller.join("renderer_probe.py");
        let imported_controller = root.join("controllers/import_probe");
        fs::create_dir_all(&imported_controller).expect("imported controller directory");
        fs::write(
            imported_controller.join("import_probe.py"),
            r#"from controller import Robot
from pathlib import Path
import time
robot = Robot()
Path("started").write_text("ready")
while True:
    robot.step(0)
    time.sleep(0.01)
"#,
        )
        .expect("imported controller");
        fs::write(
            &controller_path,
            r#"from controller import Supervisor
from pathlib import Path
import time
supervisor = Supervisor()
supervisor.simulationSetMode(Supervisor.SIMULATION_MODE_PAUSE)
supervisor.step(0)
before = supervisor.getTime()
supervisor.getRoot().getField("children").importMFNodeFromString(-1, 'DEF IMPORT_PROBE Robot { controller "import_probe" synchronization TRUE }')
supervisor.step(0)
time.sleep(0.05)
assert not Path("../import_probe/started").exists()
supervisor.simulationSetMode(Supervisor.SIMULATION_MODE_REAL_TIME)
print("PHOXAL_MODE_BEFORE_FLUSH", supervisor.simulationGetMode(), flush=True)
supervisor.step(0)
print("PHOXAL_MODE_AFTER_FLUSH", supervisor.simulationGetMode(), flush=True)
assert supervisor.getTime() == before
assert supervisor.simulationGetMode() == Supervisor.SIMULATION_MODE_REAL_TIME
deadline = time.monotonic() + 5
while not Path("../import_probe/started").exists():
    assert time.monotonic() < deadline, "imported native controller did not start"
    supervisor.step(0)
    assert supervisor.getTime() == before, "controller startup advanced physics"
    time.sleep(0.01)
supervisor.simulationSetMode(Supervisor.SIMULATION_MODE_PAUSE)
supervisor.step(0)
assert supervisor.getTime() == before
supervisor.getFromDef("IMPORT_PROBE").remove()
supervisor.step(0)
print("PHOXAL_ZERO_TIME_IMPORT_OK", flush=True)
supervisor.simulationSetMode(Supervisor.SIMULATION_MODE_REAL_TIME)
supervisor.step(0)
probe = supervisor.getFromDef("PHOXAL_EXHIBIT_0")
children = probe.getField("children") if probe is not None else None
bounding = probe.getField("boundingObject").getSFNode() if probe is not None else None
if children is None or children.getCount() <= 0 or bounding is None:
    print("PHOXAL_NATIVE_GEOMETRY_MISSING", flush=True)
    supervisor.simulationQuit(2)
else:
    print("PHOXAL_NATIVE_GEOMETRY_OK", flush=True)
    supervisor.step(int(supervisor.getBasicTimeStep()))
    supervisor.simulationQuit(0)
"#,
        )
        .expect("probe controller");

        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("available native port")
            .local_addr()
            .expect("native port address")
            .port();
        let mut child = Command::new(webots)
            .args([
                "--batch",
                "--no-rendering",
                "--mode=fast",
                "--stdout",
                "--stderr",
            ])
            .arg(format!("--port={port}"))
            .arg(worlds.join("native_renderer.wbt"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Webots starts");
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            if child.try_wait().expect("Webots status").is_some() {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().expect("terminate hung Webots proof");
                panic!("Webots native renderer proof exceeded 45 seconds");
            }
            thread::sleep(Duration::from_millis(100));
        }
        let output = child.wait_with_output().expect("Webots output");
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "Webots failed:\n{combined}");
        assert!(
            combined.contains("PHOXAL_NATIVE_GEOMETRY_OK"),
            "supervisor did not prove emitted geometry:\n{combined}"
        );
        for line in combined.lines().filter(|line| line.contains("WARNING:")) {
            assert!(
                is_software_renderer_warning(line, &combined),
                "Webots reported an unexpected warning:\n{combined}"
            );
            eprintln!("native geometry proof used software rendering: {line}");
        }
        for rejected in [
            "ERROR:",
            "Invalid URL",
            "Invalid data",
            "invalid IndexedFaceSet",
            "PHOXAL_NATIVE_GEOMETRY_MISSING",
        ] {
            assert!(
                !combined.contains(rejected),
                "Webots reported asset failure '{rejected}':\n{combined}"
            );
        }
    }

    // Headless Linux uses Mesa software rendering. Its exact performance
    // notice is not an asset-loader diagnostic or visual-quality acceptance.
    fn is_software_renderer_warning(line: &str, output: &str) -> bool {
        let mut message = line.trim();
        while let Some(rest) = message.strip_prefix("WARNING:") {
            message = rest.trim_start();
        }
        message == "System below the minimal requirements."
            && output.contains("GPU vendor is 'Mesa'")
            && output.contains("slow 3D software rendering system")
    }

    #[test]
    fn native_geometry_proof_rejects_asset_warnings_despite_software_rendering() {
        let software = "GPU vendor is 'Mesa'; slow 3D software rendering system";
        for line in [
            "WARNING: System below the minimal requirements.",
            "WARNING: WARNING: System below the minimal requirements.",
        ] {
            assert!(is_software_renderer_warning(line, software));
            assert!(!is_software_renderer_warning(line, "hardware renderer"));
        }
        for line in [
            "WARNING: Invalid URL",
            "WARNING: invalid IndexedFaceSet",
            "WARNING: System below the minimal requirements. Invalid URL",
        ] {
            assert!(!is_software_renderer_warning(line, software));
        }
    }

    fn webots_executable() -> Option<PathBuf> {
        let configured = std::env::var_os("WEBOTS_HOME").map(PathBuf::from);
        let candidates = configured
            .iter()
            .flat_map(|home| [home.join("webots"), home.join("Contents/MacOS/webots")])
            .chain([
                PathBuf::from("/Applications/Webots.app/Contents/MacOS/webots"),
                PathBuf::from("/usr/local/webots/webots"),
                PathBuf::from("/usr/bin/webots"),
            ]);
        candidates
            .into_iter()
            .find(|path| Path::new(path).is_file())
    }

    fn encode_base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut encoded = String::new();
        for chunk in bytes.chunks(3) {
            let a = chunk[0];
            let b = chunk.get(1).copied().unwrap_or(0);
            let c = chunk.get(2).copied().unwrap_or(0);
            encoded.push(char::from(ALPHABET[usize::from(a >> 2)]));
            encoded.push(char::from(ALPHABET[usize::from((a & 0x03) << 4 | b >> 4)]));
            encoded.push(if chunk.len() > 1 {
                char::from(ALPHABET[usize::from((b & 0x0f) << 2 | c >> 6)])
            } else {
                '='
            });
            encoded.push(if chunk.len() > 2 {
                char::from(ALPHABET[usize::from(c & 0x3f)])
            } else {
                '='
            });
        }
        encoded
    }
}
