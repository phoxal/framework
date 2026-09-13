//! Closed MJCF/resource artifacts and deterministic identity material.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::ArtifactError;

const DEFAULT_MAX_RESOURCE_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_CLOSURE_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_MAX_RESOURCES: usize = 4096;

/// Admission limits for a closed native model/resource closure.
///
/// The limits are checked before native parsing so a malformed or oversized
/// source cannot make the native process allocate an unbounded amount of
/// memory through a model-loading path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceLimits {
    /// Maximum size of one resource in bytes.
    pub max_resource_bytes: usize,
    /// Maximum sum of resource sizes in bytes.
    pub max_closure_bytes: usize,
    /// Maximum number of resources in a closure.
    pub max_resources: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_resource_bytes: DEFAULT_MAX_RESOURCE_BYTES,
            max_closure_bytes: DEFAULT_MAX_CLOSURE_BYTES,
            max_resources: DEFAULT_MAX_RESOURCES,
        }
    }
}

/// One named byte resource supplied to an MJCF closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resource {
    name: String,
    bytes: Vec<u8>,
}

impl Resource {
    /// Creates a resource with a normalized relative VFS name.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError`] when the name is absolute, contains `.` or
    /// `..` path components, uses backslashes, or contains a NUL byte.
    pub fn new(name: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Result<Self, ArtifactError> {
        let name = name.into();
        validate_resource_name(&name)?;
        Ok(Self {
            name,
            bytes: bytes.into(),
        })
    }

    /// Returns the normalized VFS name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the resource bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// An immutable, validated native model/resource closure.
///
/// The digest is computed from the entry name and every sorted resource name
/// and byte sequence with explicit length prefixes.
/// It is an artifact identity, not a claim that two native MuJoCo versions
/// will produce bit-identical compiled models.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosedModel {
    entry: String,
    resources: Vec<Resource>,
    digest: [u8; 32],
}

impl ClosedModel {
    /// Builds and validates a closed model from named resources.
    ///
    /// The resource names are sorted for deterministic VFS insertion and
    /// digesting.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError`] for invalid names, duplicates, missing entry,
    /// invalid entry text, or an exceeded closure limit.
    pub fn new(
        entry: impl Into<String>,
        resources: impl IntoIterator<Item = Resource>,
    ) -> Result<Self, ArtifactError> {
        Self::with_limits(entry, resources, ResourceLimits::default())
    }

    /// Builds and validates a closed model with explicit admission limits.
    pub fn with_limits(
        entry: impl Into<String>,
        resources: impl IntoIterator<Item = Resource>,
        limits: ResourceLimits,
    ) -> Result<Self, ArtifactError> {
        let entry = entry.into();
        if entry.is_empty() {
            return Err(ArtifactError::EmptyEntry);
        }
        validate_resource_name(&entry).map_err(|error| match error {
            ArtifactError::InvalidResourceName(_) => {
                ArtifactError::InvalidResourceName(entry.clone())
            }
            ArtifactError::ResourceNameContainsNul(_) => {
                ArtifactError::ResourceNameContainsNul(entry.clone())
            }
            other => other,
        })?;

        let mut by_name = BTreeMap::new();
        let mut total_bytes = 0usize;
        for resource in resources {
            let name = resource.name.clone();
            if by_name.contains_key(&name) {
                return Err(ArtifactError::DuplicateResource(name));
            }
            if by_name.len() >= limits.max_resources {
                return Err(ArtifactError::TooManyResources {
                    actual: by_name.len() + 1,
                    limit: limits.max_resources,
                });
            }
            let size = resource.bytes.len();
            if size > limits.max_resource_bytes {
                return Err(ArtifactError::ResourceTooLarge {
                    name,
                    actual: size,
                    limit: limits.max_resource_bytes,
                });
            }
            total_bytes = total_bytes
                .checked_add(size)
                .ok_or(ArtifactError::ClosureTooLarge {
                    actual: usize::MAX,
                    limit: limits.max_closure_bytes,
                })?;
            by_name.insert(resource.name.clone(), resource);
        }

        if by_name.len() > limits.max_resources {
            return Err(ArtifactError::TooManyResources {
                actual: by_name.len(),
                limit: limits.max_resources,
            });
        }
        if total_bytes > limits.max_closure_bytes {
            return Err(ArtifactError::ClosureTooLarge {
                actual: total_bytes,
                limit: limits.max_closure_bytes,
            });
        }

        let entry_resource = by_name
            .get(&entry)
            .ok_or_else(|| ArtifactError::EntryMissing(entry.clone()))?;
        std::str::from_utf8(&entry_resource.bytes).map_err(|source| {
            ArtifactError::EntryNotUtf8 {
                path: entry.clone(),
                source,
            }
        })?;

        let resources: Vec<_> = by_name.into_values().collect();
        validate_xml_references(&entry, &resources)?;
        let digest = digest(&entry, &resources);
        Ok(Self {
            entry,
            resources,
            digest,
        })
    }

    /// Creates a closure containing one UTF-8 MJCF document named `model.xml`.
    pub fn from_xml(xml: impl AsRef<[u8]>) -> Result<Self, ArtifactError> {
        Self::new(
            "model.xml",
            [Resource::new("model.xml", xml.as_ref().to_vec())?],
        )
    }

    /// Reads one model directory into a closed resource closure.
    ///
    /// `root` is an explicit resource boundary.
    /// Every regular file below it is included, directory traversal is sorted
    /// by normalized path, and symlinks are refused so the closure cannot
    /// escape the selected root.
    pub fn from_directory(
        root: impl AsRef<Path>,
        entry: impl AsRef<Path>,
    ) -> Result<Self, ArtifactError> {
        Self::from_directory_with_limits(root, entry, ResourceLimits::default())
    }

    /// Reads one model directory with explicit closure admission limits.
    pub fn from_directory_with_limits(
        root: impl AsRef<Path>,
        entry: impl AsRef<Path>,
        limits: ResourceLimits,
    ) -> Result<Self, ArtifactError> {
        let root = root.as_ref();
        let root_metadata = fs::symlink_metadata(root).map_err(|source| ArtifactError::Io {
            path: root.to_owned(),
            source,
        })?;
        if root_metadata.file_type().is_symlink() {
            return Err(ArtifactError::InvalidResourceName(
                root.to_string_lossy().into_owned(),
            ));
        }
        let root = root.canonicalize().map_err(|source| ArtifactError::Io {
            path: root.to_owned(),
            source,
        })?;
        let root_metadata = fs::metadata(&root).map_err(|source| ArtifactError::Io {
            path: root.clone(),
            source,
        })?;
        if !root_metadata.is_dir() {
            return Err(ArtifactError::UnsupportedFileType(root));
        }
        let entry_path = entry.as_ref();
        let entry_name = normalized_relative_path(entry_path).map_err(|_| {
            ArtifactError::InvalidResourceName(entry_path.to_string_lossy().into_owned())
        })?;

        let mut files = Vec::new();
        let mut total_bytes = 0usize;
        collect_files(&root, &root, &mut files, limits, &mut total_bytes)?;
        let resources = files
            .into_iter()
            .map(|(name, path)| {
                let bytes = fs::read(&path).map_err(|source| ArtifactError::Io {
                    path: path.clone(),
                    source,
                })?;
                Resource::new(name, bytes)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::with_limits(entry_name, resources, limits)
    }

    /// Reads the parent directory of `path` as the explicit resource root.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ArtifactError> {
        Self::from_file_with_limits(path, ResourceLimits::default())
    }

    /// Reads the parent resource root of `path` with explicit closure limits.
    pub fn from_file_with_limits(
        path: impl AsRef<Path>,
        limits: ResourceLimits,
    ) -> Result<Self, ArtifactError> {
        let path = path.as_ref();
        let file_name = path.file_name().ok_or_else(|| {
            ArtifactError::InvalidResourceName(path.to_string_lossy().into_owned())
        })?;
        let root = path.parent().unwrap_or_else(|| Path::new("."));
        Self::from_directory_with_limits(root, file_name, limits)
    }

    /// Returns the entry name passed to the native parser.
    #[must_use]
    pub fn entry(&self) -> &str {
        &self.entry
    }

    /// Returns resources in deterministic normalized-name order.
    pub fn resources(&self) -> impl ExactSizeIterator<Item = &Resource> {
        self.resources.iter()
    }

    /// Looks up one resource by normalized name.
    #[must_use]
    pub fn resource(&self, name: &str) -> Option<&Resource> {
        self.resources.iter().find(|resource| resource.name == name)
    }

    /// Returns the deterministic artifact digest.
    #[must_use]
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    /// Returns the digest as lowercase hexadecimal.
    #[must_use]
    pub fn digest_hex(&self) -> String {
        self.digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

#[derive(Default)]
struct AssetDirectories {
    mesh: Option<String>,
    texture: Option<String>,
}

struct XmlTag {
    name: String,
    attributes: BTreeMap<String, String>,
}

fn validate_xml_references(entry: &str, resources: &[Resource]) -> Result<(), ArtifactError> {
    let resources = resources
        .iter()
        .map(|resource| (resource.name().to_owned(), resource.bytes()))
        .collect::<BTreeMap<_, _>>();
    validate_model_document(
        entry,
        &resources,
        &mut BTreeSet::new(),
        &mut BTreeSet::new(),
    )
}

fn validate_model_document(
    entry: &str,
    resources: &BTreeMap<String, &[u8]>,
    active_models: &mut BTreeSet<String>,
    validated_models: &mut BTreeSet<String>,
) -> Result<(), ArtifactError> {
    if active_models.len() >= 128 || !active_models.insert(entry.to_owned()) {
        return Err(invalid_xml_reference(
            entry,
            "cyclic or excessively nested model attachment",
        ));
    }
    if validated_models.contains(entry) {
        active_models.remove(entry);
        return Ok(());
    }
    let directory = resource_parent(entry);
    let mut tags = Vec::new();
    expand_includes(
        entry,
        &directory,
        resources,
        &mut BTreeSet::new(),
        &mut BTreeSet::new(),
        &mut tags,
    )?;
    // Includes splice XML into one model. Compiler settings govern the entire
    // expanded model, including assets before or inside an included document.
    let directories = asset_directories(entry, tags.iter().map(|(_, tag)| tag))?;
    for (source, tag) in &tags {
        if tag.name == "plugin"
            || tag.attributes.contains_key("plugin")
            || tag.attributes.contains_key("plugin_instance")
        {
            return Err(invalid_xml_reference(
                source,
                "native plugins and custom resource providers are not admitted",
            ));
        }
        let files = file_attributes(tag, &directories);
        for attribute in tag
            .attributes
            .keys()
            .filter(|name| name.starts_with("file"))
        {
            if !files.iter().any(|(known, _)| *known == attribute) {
                return Err(invalid_xml_reference(
                    source,
                    format!("unadmitted file attribute {}.{attribute}", tag.name),
                ));
            }
        }
        for (attribute, asset_directory) in files {
            let reference = &tag.attributes[attribute];
            validate_file_format(source, &tag.name, attribute, reference, tag)?;
            let resolved =
                resolve_resource_reference(source, &directory, asset_directory, reference)?;
            if !resources.contains_key(&resolved) {
                return Err(invalid_xml_reference(
                    source,
                    format!("reference {reference:?} resolves to missing resource {resolved:?}"),
                ));
            }
            if tag.name == "model" {
                // A model asset starts its own include/compiler scope.
                validate_model_document(&resolved, resources, active_models, validated_models)?;
            }
        }
    }
    active_models.remove(entry);
    validated_models.insert(entry.to_owned());
    Ok(())
}

fn expand_includes(
    source: &str,
    directory: &str,
    resources: &BTreeMap<String, &[u8]>,
    active: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
    expanded: &mut Vec<(String, XmlTag)>,
) -> Result<(), ArtifactError> {
    if active.len() >= 128 || !active.insert(source.to_owned()) {
        return Err(invalid_xml_reference(
            source,
            "cyclic or excessively nested XML include",
        ));
    }
    if !visited.insert(source.to_owned()) {
        return Err(invalid_xml_reference(
            source,
            "duplicate XML include is not admitted",
        ));
    }
    let bytes = resources.get(source).ok_or_else(|| {
        invalid_xml_reference(
            source,
            "referenced XML resource is missing from the closure",
        )
    })?;
    let document = std::str::from_utf8(bytes)
        .map_err(|e| invalid_xml_reference(source, format!("XML resource is not UTF-8: {e}")))?;
    if !document.trim_start().starts_with('<') {
        return Err(invalid_xml_reference(
            source,
            "resource is not an XML document",
        ));
    }
    for tag in parse_xml_tags(source, document)? {
        if tag.name == "include" {
            let reference = tag
                .attributes
                .get("file")
                .ok_or_else(|| invalid_xml_reference(source, "include has no file"))?;
            validate_file_format(source, "include", "file", reference, &tag)?;
            let resolved = resolve_resource_reference(source, directory, "", reference)?;
            expand_includes(&resolved, directory, resources, active, visited, expanded)?;
        } else {
            expanded.push((source.to_owned(), tag));
        }
    }
    active.remove(source);
    Ok(())
}

fn resource_parent(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(parent, _)| parent.to_owned())
        .unwrap_or_default()
}

fn asset_directories<'a>(
    source: &str,
    tags: impl Iterator<Item = &'a XmlTag>,
) -> Result<AssetDirectories, ArtifactError> {
    let mut assetdir = None;
    let mut meshdir = None;
    let mut texturedir = None;
    for tag in tags.filter(|tag| tag.name == "compiler") {
        if let Some(value) = tag.attributes.get("strippath") {
            return Err(invalid_xml_reference(
                source,
                format!("compiler strippath={value:?} is not admitted"),
            ));
        }
        if tag.attributes.contains_key("hfielddir") {
            return Err(invalid_xml_reference(
                source,
                "compiler hfielddir is not a MuJoCo resource directory; use meshdir",
            ));
        }
        if let Some(value) = tag.attributes.get("assetdir") {
            assetdir = Some(validate_directory(source, value, "assetdir")?);
        }
        if let Some(value) = tag.attributes.get("meshdir") {
            meshdir = Some(validate_directory(source, value, "meshdir")?);
        }
        if let Some(value) = tag.attributes.get("texturedir") {
            texturedir = Some(validate_directory(source, value, "texturedir")?);
        }
    }
    Ok(AssetDirectories {
        mesh: meshdir.or_else(|| assetdir.clone()),
        texture: texturedir.or(assetdir),
    })
}

fn validate_directory(source: &str, value: &str, attribute: &str) -> Result<String, ArtifactError> {
    if value.is_empty() {
        return Ok(String::new());
    }
    let mut components = Vec::new();
    append_reference_components(source, value, &mut components).map_err(|_| {
        invalid_xml_reference(
            source,
            format!("compiler {attribute}={value:?} is not a normalized relative directory"),
        )
    })?;
    Ok(components.join("/"))
}

fn file_attributes<'a>(
    tag: &'a XmlTag,
    directories: &'a AssetDirectories,
) -> Vec<(&'a str, &'a str)> {
    let directory = match tag.name.as_str() {
        "mesh" | "hfield" | "skin" => directories.mesh.as_deref().unwrap_or(""),
        "texture" => directories.texture.as_deref().unwrap_or(""),
        "include" | "model" => "",
        _ => return Vec::new(),
    };
    let names: &[&str] = match tag.name.as_str() {
        "texture" => &[
            "file",
            "fileright",
            "fileleft",
            "fileup",
            "filedown",
            "filefront",
            "fileback",
        ],
        "mesh" | "hfield" | "skin" | "include" | "model" => &["file"],
        _ => &[],
    };
    names
        .iter()
        .filter(|name| tag.attributes.contains_key(**name))
        .map(|name| (*name, directory))
        .collect()
}

fn validate_file_format(
    source: &str,
    tag_name: &str,
    attribute: &str,
    reference: &str,
    tag: &XmlTag,
) -> Result<(), ArtifactError> {
    if reference.contains('&') {
        return Err(invalid_xml_reference(
            source,
            format!("resource reference {reference:?} uses an XML entity"),
        ));
    }
    let extension = reference
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, extension)| extension.to_ascii_lowercase());
    let content_type = tag.attributes.get("content_type").map(String::as_str);
    let supported = match tag_name {
        "mesh" => match content_type {
            Some(value) => matches!(value, "model/vnd.mujoco.msh" | "model/obj" | "model/stl"),
            None => matches!(extension.as_deref(), Some("msh" | "obj" | "stl")),
        },
        "hfield" => match content_type {
            Some(value) => matches!(value, "image/png" | "image/vnd.mujoco.hfield"),
            // Any non-PNG extension is MuJoCo's documented binary hfield
            // format, so extension alone cannot be rejected here.
            None => true,
        },
        "texture" => match content_type {
            Some(value) => matches!(value, "image/png" | "image/vnd.mujoco.texture"),
            // Any non-PNG extension selects MuJoCo's documented custom binary
            // texture format.
            None => true,
        },
        "skin" => content_type.is_none(),
        "model" => content_type.is_none(),
        "include" => content_type.is_none(),
        _ => true,
    };
    if !supported {
        return Err(invalid_xml_reference(
            source,
            format!(
                "unsupported {tag_name} {attribute}={reference:?} with content_type {content_type:?}"
            ),
        ));
    }
    if tag_name == "skin" && content_type.is_some() {
        return Err(invalid_xml_reference(
            source,
            "skin assets do not support content_type",
        ));
    }
    Ok(())
}

fn parse_xml_tags(path: &str, document: &str) -> Result<Vec<XmlTag>, ArtifactError> {
    if document.contains('&') {
        return Err(invalid_xml_reference(
            path,
            "XML entities are not admitted in a closed MJCF resource",
        ));
    }
    let bytes = document.as_bytes();
    let mut tags = Vec::new();
    let mut cursor = 0usize;
    while let Some(relative) = bytes[cursor..].iter().position(|byte| *byte == b'<') {
        let start = cursor + relative;
        if bytes[start..].starts_with(b"<!--") {
            let end = document[start + 4..]
                .find("-->")
                .map(|offset| start + 4 + offset + 3)
                .ok_or_else(|| invalid_xml_reference(path, "unterminated XML comment"))?;
            cursor = end;
            continue;
        }
        if bytes[start..].starts_with(b"<!") {
            return Err(invalid_xml_reference(
                path,
                "DTD, entity, CDATA, and other XML declarations are not admitted",
            ));
        }
        if bytes[start..].starts_with(b"<?") {
            let end = find_tag_end(document, start + 1)
                .ok_or_else(|| invalid_xml_reference(path, "unterminated XML declaration"))?;
            cursor = end + 1;
            continue;
        }
        let end = find_tag_end(document, start + 1)
            .ok_or_else(|| invalid_xml_reference(path, "unterminated XML element"))?;
        let body = document[start + 1..end].trim_end();
        if body.starts_with('/') {
            cursor = end + 1;
            continue;
        }
        tags.push(parse_xml_tag(path, body)?);
        cursor = end + 1;
    }
    Ok(tags)
}

fn find_tag_end(document: &str, start: usize) -> Option<usize> {
    let bytes = document.as_bytes();
    let mut quote = None;
    for (index, byte) in bytes.iter().copied().enumerate().skip(start) {
        match (quote, byte) {
            (None, b'\'' | b'"') => quote = Some(bytes[index]),
            (Some(expected), byte) if byte == expected => quote = None,
            (None, b'>') => return Some(index),
            _ => {}
        }
    }
    None
}

fn parse_xml_tag(path: &str, body: &str) -> Result<XmlTag, ArtifactError> {
    let body = body.trim_end_matches('/').trim_end();
    let name_end = body
        .find(|character: char| character.is_ascii_whitespace())
        .unwrap_or(body.len());
    let name = &body[..name_end];
    if name.is_empty() {
        return Err(invalid_xml_reference(path, "XML element has no name"));
    }
    let mut attributes = BTreeMap::new();
    let mut cursor = name_end;
    while cursor < body.len() {
        while cursor < body.len() && body.as_bytes()[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= body.len() {
            break;
        }
        let key_start = cursor;
        while cursor < body.len()
            && !body.as_bytes()[cursor].is_ascii_whitespace()
            && body.as_bytes()[cursor] != b'='
        {
            cursor += 1;
        }
        let key = &body[key_start..cursor];
        if key.is_empty() {
            return Err(invalid_xml_reference(path, "XML attribute has no name"));
        }
        while cursor < body.len() && body.as_bytes()[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= body.len() || body.as_bytes()[cursor] != b'=' {
            return Err(invalid_xml_reference(
                path,
                format!("XML attribute {key:?} has no '='"),
            ));
        }
        cursor += 1;
        while cursor < body.len() && body.as_bytes()[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= body.len() || !matches!(body.as_bytes()[cursor], b'\'' | b'"') {
            return Err(invalid_xml_reference(
                path,
                format!("XML attribute {key:?} is not quoted"),
            ));
        }
        let quote = body.as_bytes()[cursor];
        cursor += 1;
        let value_start = cursor;
        while cursor < body.len() && body.as_bytes()[cursor] != quote {
            cursor += 1;
        }
        if cursor >= body.len() {
            return Err(invalid_xml_reference(
                path,
                format!("XML attribute {key:?} is unterminated"),
            ));
        }
        let value = &body[value_start..cursor];
        cursor += 1;
        if attributes
            .insert(key.to_owned(), value.to_owned())
            .is_some()
        {
            return Err(invalid_xml_reference(
                path,
                format!("XML attribute {key:?} appears more than once"),
            ));
        }
    }
    Ok(XmlTag {
        name: name.to_owned(),
        attributes,
    })
}

fn resolve_resource_reference(
    source: &str,
    main_directory: &str,
    directory: &str,
    reference: &str,
) -> Result<String, ArtifactError> {
    if reference.is_empty()
        || reference.contains('\0')
        || reference.contains('\\')
        || reference.contains("://")
    {
        return Err(invalid_xml_reference(
            source,
            format!("resource reference {reference:?} is not a local normalized path"),
        ));
    }
    let mut components = Vec::new();
    append_reference_components(source, main_directory, &mut components)?;
    append_reference_components(source, directory, &mut components)?;
    append_reference_components(source, reference, &mut components)?;
    if components.is_empty() {
        return Err(invalid_xml_reference(
            source,
            format!("resource reference {reference:?} is empty after normalization"),
        ));
    }
    Ok(components.join("/"))
}

fn append_reference_components(
    source: &str,
    value: &str,
    components: &mut Vec<String>,
) -> Result<(), ArtifactError> {
    if value.is_empty() {
        return Ok(());
    }
    if value.starts_with('/') || value.ends_with('/') {
        return Err(invalid_xml_reference(
            source,
            format!("resource path {value:?} is absolute or not a file path"),
        ));
    }
    for component in value.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(invalid_xml_reference(
                source,
                format!("resource path {value:?} is not normalized"),
            ));
        }
        if component.contains(':') {
            return Err(invalid_xml_reference(
                source,
                format!("resource path {value:?} has a URI or platform prefix"),
            ));
        }
        components.push(component.to_owned());
    }
    Ok(())
}

fn invalid_xml_reference(path: &str, detail: impl Into<String>) -> ArtifactError {
    ArtifactError::InvalidXmlReference {
        path: path.to_owned(),
        detail: detail.into(),
    }
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<(String, PathBuf)>,
    limits: ResourceLimits,
    total_bytes: &mut usize,
) -> Result<(), ArtifactError> {
    let mut entries = fs::read_dir(directory)
        .map_err(|source| ArtifactError::Io {
            path: directory.to_owned(),
            source,
        })?
        .map(|entry| {
            entry
                .map_err(|source| ArtifactError::Io {
                    path: directory.to_owned(),
                    source,
                })
                .and_then(|entry| {
                    let path = entry.path();
                    let file_type = entry.file_type().map_err(|source| ArtifactError::Io {
                        path: path.clone(),
                        source,
                    })?;
                    if file_type.is_symlink() {
                        return Err(ArtifactError::InvalidResourceName(
                            path.strip_prefix(root)
                                .unwrap_or(&path)
                                .to_string_lossy()
                                .into_owned(),
                        ));
                    }
                    let relative = path.strip_prefix(root).map_err(|_| {
                        ArtifactError::InvalidResourceName(path.to_string_lossy().into_owned())
                    })?;
                    let name = normalized_relative_path(relative).map_err(|_| {
                        ArtifactError::InvalidResourceName(relative.to_string_lossy().into_owned())
                    })?;
                    Ok((name, path, file_type))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    for (name, path, file_type) in entries {
        if file_type.is_dir() {
            collect_files(root, &path, files, limits, total_bytes)?;
        } else if !file_type.is_file() {
            return Err(ArtifactError::UnsupportedFileType(path));
        } else {
            let size = fs::metadata(&path)
                .map_err(|source| ArtifactError::Io {
                    path: path.clone(),
                    source,
                })?
                .len();
            let size = usize::try_from(size).unwrap_or(usize::MAX);
            if size > limits.max_resource_bytes {
                return Err(ArtifactError::ResourceTooLarge {
                    name,
                    actual: size,
                    limit: limits.max_resource_bytes,
                });
            }
            if files.len() >= limits.max_resources {
                return Err(ArtifactError::TooManyResources {
                    actual: files.len() + 1,
                    limit: limits.max_resources,
                });
            }
            *total_bytes = total_bytes
                .checked_add(size)
                .ok_or(ArtifactError::ClosureTooLarge {
                    actual: usize::MAX,
                    limit: limits.max_closure_bytes,
                })?;
            if *total_bytes > limits.max_closure_bytes {
                return Err(ArtifactError::ClosureTooLarge {
                    actual: *total_bytes,
                    limit: limits.max_closure_bytes,
                });
            }
            files.push((name, path));
        }
    }
    Ok(())
}

fn normalized_relative_path(path: &Path) -> Result<String, ()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(());
    }
    if path.to_string_lossy().contains('\\') {
        return Err(());
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str().ok_or(())?;
                if part.is_empty() || part.contains('\0') || part.contains('\\') {
                    return Err(());
                }
                parts.push(part);
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => return Err(()),
        }
    }
    if parts.is_empty() {
        return Err(());
    }
    Ok(parts.join("/"))
}

fn validate_resource_name(name: &str) -> Result<(), ArtifactError> {
    if name.contains('\0') {
        return Err(ArtifactError::ResourceNameContainsNul(name.to_owned()));
    }
    normalized_relative_path(Path::new(name))
        .map(|_| ())
        .map_err(|_| ArtifactError::InvalidResourceName(name.to_owned()))
}

fn digest(entry: &str, resources: &[Resource]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    update_bytes(&mut hasher, entry.as_bytes());
    for resource in resources {
        update_bytes(&mut hasher, resource.name.as_bytes());
        update_bytes(&mut hasher, &resource.bytes);
    }
    hasher.finalize().into()
}

fn update_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const XML: &[u8] = br#"<mujoco><worldbody/></mujoco>"#;

    #[test]
    fn sorts_resources_and_computes_stable_digest() {
        let first = ClosedModel::new(
            "model.xml",
            [
                Resource::new("assets/floor.stl", b"floor".to_vec()).unwrap(),
                Resource::new("model.xml", XML.to_vec()).unwrap(),
            ],
        )
        .unwrap();
        let second = ClosedModel::new(
            "model.xml",
            [
                Resource::new("model.xml", XML.to_vec()).unwrap(),
                Resource::new("assets/floor.stl", b"floor".to_vec()).unwrap(),
            ],
        )
        .unwrap();

        assert_eq!(first, second);
        assert_eq!(
            first.resources().map(Resource::name).collect::<Vec<_>>(),
            ["assets/floor.stl", "model.xml",]
        );
        assert_eq!(first.digest_hex().len(), 64);
    }

    #[test]
    fn rejects_escape_and_duplicate_names() {
        assert!(matches!(
            Resource::new("../model.xml", XML),
            Err(ArtifactError::InvalidResourceName(_))
        ));
        assert!(matches!(
            Resource::new("assets\\mesh.stl", XML),
            Err(ArtifactError::InvalidResourceName(_))
        ));
        assert!(matches!(
            ClosedModel::new(
                "model.xml",
                [
                    Resource::new("model.xml", XML).unwrap(),
                    Resource::new("model.xml", XML).unwrap(),
                ]
            ),
            Err(ArtifactError::DuplicateResource(_))
        ));
    }

    #[test]
    fn rejects_missing_or_non_utf8_entry() {
        assert!(matches!(
            ClosedModel::new("", std::iter::empty::<Resource>()),
            Err(ArtifactError::EmptyEntry)
        ));
        assert!(matches!(
            ClosedModel::new("model.xml", [Resource::new("other.xml", XML).unwrap()]),
            Err(ArtifactError::EntryMissing(_))
        ));
        assert!(matches!(
            ClosedModel::new(
                "model.xml",
                [Resource::new("model.xml", vec![0xff]).unwrap()]
            ),
            Err(ArtifactError::EntryNotUtf8 { .. })
        ));
    }

    #[test]
    fn rejects_xml_references_that_are_not_in_the_closed_closure() {
        let missing_include =
            ClosedModel::from_xml(br#"<mujoco><include file="beside.xml"/><worldbody/></mujoco>"#)
                .expect_err("a missing include must not fall back to the host filesystem");
        assert!(matches!(
            missing_include,
            ArtifactError::InvalidXmlReference { .. }
        ));

        let missing_mesh = ClosedModel::from_xml(
            br#"<mujoco><asset><mesh name="mesh" file="beside.obj"/></asset><worldbody/></mujoco>"#,
        )
        .expect_err("a missing mesh must not fall back to the host filesystem");
        assert!(matches!(
            missing_mesh,
            ArtifactError::InvalidXmlReference { .. }
        ));
    }

    #[test]
    fn rejects_escaping_and_unapproved_native_resource_references() {
        let absolute = ClosedModel::from_xml(
            br#"<mujoco><asset><mesh name="mesh" file="/tmp/mesh.obj"/></asset><worldbody/></mujoco>"#,
        )
        .expect_err("absolute resources are outside the closure");
        assert!(matches!(
            absolute,
            ArtifactError::InvalidXmlReference { .. }
        ));

        let plugin = ClosedModel::from_xml(
            br#"<mujoco><extension><plugin plugin="host-provider"/></extension><worldbody/></mujoco>"#,
        )
        .expect_err("custom native providers are not admitted");
        assert!(matches!(plugin, ArtifactError::InvalidXmlReference { .. }));
    }

    #[test]
    fn resolves_includes_and_assets_from_the_main_mjcf_directory() {
        let nested = ClosedModel::new(
            "robot/model.xml",
            [
                Resource::new(
                    "robot/model.xml",
                    br#"<mujoco><include file="parts/first.xml"/><worldbody/></mujoco>"#,
                )
                .unwrap(),
                Resource::new(
                    "robot/parts/first.xml",
                    br#"<mujoco><include file="parts/second.xml"/></mujoco>"#,
                )
                .unwrap(),
                Resource::new("robot/parts/second.xml", b"<mujoco/>".to_vec()).unwrap(),
            ],
        )
        .expect("nested includes resolve relative to the main entry directory");
        assert_eq!(nested.entry(), "robot/model.xml");

        ClosedModel::new(
            "robot/model.xml",
            [
                Resource::new(
                    "robot/model.xml",
                    br#"<mujoco><compiler assetdir="assets"/><asset><mesh file="mesh.obj"/><texture file="texture.bin"/></asset><worldbody/></mujoco>"#,
                )
                .unwrap(),
                Resource::new("robot/assets/mesh.obj", b"mesh".to_vec()).unwrap(),
                Resource::new("robot/assets/texture.bin", b"texture".to_vec()).unwrap(),
            ],
        )
        .expect("assetdir supplies both meshdir and texturedir defaults");
    }

    #[test]
    fn rejects_duplicate_and_cyclic_includes() {
        let duplicate = ClosedModel::new(
            "model.xml",
            [
                Resource::new(
                    "model.xml",
                    br#"<mujoco><include file="part.xml"/><include file="part.xml"/></mujoco>"#,
                )
                .unwrap(),
                Resource::new("part.xml", b"<mujoco/>".to_vec()).unwrap(),
            ],
        )
        .expect_err("an include may not be admitted twice");
        assert!(matches!(
            duplicate,
            ArtifactError::InvalidXmlReference { .. }
        ));

        let cycle = ClosedModel::new(
            "model.xml",
            [
                Resource::new(
                    "model.xml",
                    br#"<mujoco><include file="part.xml"/></mujoco>"#,
                )
                .unwrap(),
                Resource::new(
                    "part.xml",
                    br#"<mujoco><include file="model.xml"/></mujoco>"#,
                )
                .unwrap(),
            ],
        )
        .expect_err("cyclic includes are not admitted");
        assert!(matches!(cycle, ArtifactError::InvalidXmlReference { .. }));
    }

    #[test]
    fn rejects_unsupported_asset_formats_and_path_options() {
        for xml in [
            br#"<mujoco><asset><mesh file="mesh.ply"/></asset></mujoco>"#.as_slice(),
            br#"<mujoco><asset><hfield file="height.bin" content_type="image/jpeg"/></asset></mujoco>"#
                .as_slice(),
            br#"<mujoco><asset><texture file="texture.bin" content_type="image/jpeg"/></asset></mujoco>"#
                .as_slice(),
            br#"<mujoco><compiler strippath="true"/></mujoco>"#.as_slice(),
            br#"<mujoco><compiler hfielddir="height"/></mujoco>"#.as_slice(),
            br#"<!DOCTYPE mujoco [<!ENTITY mesh "mesh.obj">]><mujoco><asset><mesh file="&mesh;"/></asset></mujoco>"#
                .as_slice(),
            br#"<mujoco><![CDATA[unsafe]]></mujoco>"#.as_slice(),
        ] {
            let result = ClosedModel::from_xml(xml);
            assert!(matches!(result, Err(ArtifactError::InvalidXmlReference { .. })), "{xml:?}");
        }
    }

    #[test]
    fn does_not_use_host_filesystem_for_closed_xml_references() {
        let parent = tempfile::tempdir().unwrap();
        let root_path = parent.path().join("root");
        fs::create_dir(&root_path).unwrap();
        fs::write(
            root_path.join("model.xml"),
            br#"<mujoco><asset><mesh file="outside.obj"/></asset></mujoco>"#,
        )
        .unwrap();
        // This file is intentionally next to, rather than inside, the explicit
        // resource root.  A native parser fallback must not make admission pass.
        fs::write(parent.path().join("outside.obj"), b"outside").unwrap();
        let result = ClosedModel::from_directory(&root_path, "model.xml");
        assert!(matches!(
            result,
            Err(ArtifactError::InvalidXmlReference { .. })
        ));
    }

    #[test]
    fn custom_limits_are_enforced_before_digesting() {
        let limits = ResourceLimits {
            max_resource_bytes: 2,
            max_closure_bytes: 2,
            max_resources: 1,
        };
        assert!(matches!(
            ClosedModel::with_limits(
                "model.xml",
                [Resource::new("model.xml", XML).unwrap()],
                limits,
            ),
            Err(ArtifactError::ResourceTooLarge { .. })
        ));
    }

    #[test]
    fn directory_closure_is_sorted_and_respects_limits_before_reading() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("assets")).unwrap();
        fs::write(root.path().join("model.xml"), XML).unwrap();
        fs::write(root.path().join("assets/floor.stl"), b"floor").unwrap();

        let first = ClosedModel::from_directory(root.path(), "model.xml").unwrap();
        let second = ClosedModel::from_file(root.path().join("model.xml")).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first.resources().map(Resource::name).collect::<Vec<_>>(),
            ["assets/floor.stl", "model.xml"]
        );

        let limits = ResourceLimits {
            max_resource_bytes: 2,
            max_closure_bytes: 64,
            max_resources: 2,
        };
        assert!(matches!(
            ClosedModel::from_directory_with_limits(root.path(), "model.xml", limits),
            Err(ArtifactError::ResourceTooLarge { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn directory_closure_refuses_symlinks() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("model.xml"), XML).unwrap();
        std::os::unix::fs::symlink(root.path().join("model.xml"), root.path().join("alias.xml"))
            .unwrap();
        assert!(matches!(
            ClosedModel::from_directory(root.path(), "model.xml"),
            Err(ArtifactError::InvalidResourceName(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn directory_closure_refuses_a_symlinked_root() {
        let parent = tempfile::tempdir().unwrap();
        let actual = parent.path().join("actual");
        fs::create_dir(&actual).unwrap();
        fs::write(actual.join("model.xml"), XML).unwrap();
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&actual, &link).unwrap();

        assert!(matches!(
            ClosedModel::from_directory(&link, "model.xml"),
            Err(ArtifactError::InvalidResourceName(_))
        ));
    }
}

#[cfg(test)]
mod closed_scope_tests {
    use super::*;
    fn resource(name: &str, xml: &str) -> Resource {
        Resource::new(name, xml.as_bytes()).unwrap()
    }

    #[test]
    fn included_assets_use_the_main_models_compiler_settings() {
        ClosedModel::new("robot/main.xml", [
            resource("robot/main.xml", r#"<mujoco><compiler meshdir="assets"/><include file="parts/assets.xml"/><worldbody/></mujoco>"#),
            resource("robot/parts/assets.xml", r#"<mujoco><asset><mesh name="m" file="mesh.obj"/></asset></mujoco>"#),
            resource("robot/assets/mesh.obj", "mesh bytes"),
        ]).expect("compiler asset directory applies across includes");
    }

    #[cfg(feature = "native")]
    #[test]
    fn native_compiler_resolves_included_assets_with_the_same_scope() {
        let artifact = ClosedModel::new("robot/main.xml", [
            resource("robot/main.xml", r#"<mujoco><compiler meshdir="assets"/><include file="parts/assets.xml"/><worldbody><geom type="mesh" mesh="tetra"/></worldbody></mujoco>"#),
            resource("robot/parts/assets.xml", r#"<mujoco><asset><mesh name="tetra" file="tetra.obj"/></asset></mujoco>"#),
            resource("robot/assets/tetra.obj", "v 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 1\nf 1 3 2\nf 1 2 4\nf 1 4 3\nf 2 3 4\n"),
        ]).unwrap();
        crate::Model::from_closed(artifact).expect("native and admission path resolution agree");
    }

    #[test]
    fn attached_model_cannot_hide_a_filesystem_escape() {
        let result = ClosedModel::new(
            "scene.xml",
            [
                resource(
                    "scene.xml",
                    r#"<mujoco><asset><model name="child" file="child/robot.xml"/></asset><worldbody><attach model="child" body="mount" prefix="robot_"/></worldbody></mujoco>"#,
                ),
                resource(
                    "child/robot.xml",
                    r#"<mujoco><asset><mesh file="/tmp/outside.obj"/></asset><worldbody><body name="mount"/></worldbody></mujoco>"#,
                ),
            ],
        );
        assert!(
            result.is_err(),
            "attached models must receive the same closed-resource admission"
        );
    }

    #[test]
    fn unimplemented_file_reading_elements_fail_before_native_parsing() {
        assert!(ClosedModel::from_xml(r#"<mujoco><worldbody><flexcomp name="cloth" type="gmsh" file="/tmp/outside.msh"/></worldbody></mujoco>"#).is_err());
    }
}
