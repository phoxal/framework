//! Native MJCF composition through MuJoCo's model-editing API.
//!
//! Composition is a build-time operation.  A parent scene and every component
//! artifact are parsed into native specifications, then each explicitly named
//! component root is attached to one explicitly named parent site.  The
//! resulting [`Model`] is immutable and carries a digest of the complete
//! selection, while the native model owns the attached resources and names.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, CString};
use std::fmt;

use mujoco_rs::mujoco_c::{
    mj_compile, mj_deleteModel, mjs_attach, mjs_getError, mjs_setDeepCopy, mjsElement,
};
use mujoco_rs::prelude::MjSpec;
use mujoco_rs::wrappers::{MjVfs, SpecItem};
use sha2::{Digest, Sha256};

use crate::artifact::{ClosedModel, Resource};
use crate::error::ModelError;
use crate::model::{Model, ModelIdentity};

/// Separator used by the native namespace assigned to one component
/// instance.
pub const NAMESPACE_SEPARATOR: &str = "__";

/// One explicit component-to-scene native attachment.
///
/// The component root and parent target are model-local names.  They are never
/// inferred from a capability declaration or from a port name.  The default
/// namespace is `{instance}__`, which makes every attached component name
/// deterministic and prevents repeated component instances from colliding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComponentAttachment {
    instance: String,
    component: ClosedModel,
    target_site: String,
    component_root: String,
    prefix: String,
    suffix: String,
}

impl ComponentAttachment {
    /// Creates an attachment using the canonical `{instance}__` namespace.
    pub fn new(
        instance: impl Into<String>,
        component: ClosedModel,
        target_site: impl Into<String>,
        component_root: impl Into<String>,
    ) -> Result<Self, CompositionError> {
        let instance = instance.into();
        let prefix = format!("{instance}{NAMESPACE_SEPARATOR}");
        let attachment = Self {
            instance,
            component,
            target_site: target_site.into(),
            component_root: component_root.into(),
            prefix,
            suffix: String::new(),
        };
        validate_name(&attachment.instance, "component instance")?;
        validate_name(&attachment.target_site, "parent target site")?;
        validate_name(&attachment.component_root, "component root body")?;
        Ok(attachment)
    }

    /// Component instance identity selected by the scene definition.
    #[must_use]
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// Closed component artifact parsed for this attachment.
    #[must_use]
    pub fn component(&self) -> &ClosedModel {
        &self.component
    }

    /// Parent site receiving the component root body.
    #[must_use]
    pub fn target_site(&self) -> &str {
        &self.target_site
    }

    /// Component model-local root body to attach.
    #[must_use]
    pub fn component_root(&self) -> &str {
        &self.component_root
    }

    /// Exact native namespace prefix passed to `mjs_attach`.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Exact native namespace suffix passed to `mjs_attach`.
    #[must_use]
    pub fn suffix(&self) -> &str {
        &self.suffix
    }

    /// Returns the expected native name for one explicitly named component
    /// object after attachment.
    pub fn native_name(&self, local_name: &str) -> Result<String, CompositionError> {
        validate_name(local_name, "component native object")?;
        let name = format!("{}{local_name}{}", self.prefix, self.suffix);
        validate_composed_name(&name, "composed native object")?;
        Ok(name)
    }
}

/// A deterministic parent scene plus its fixed native component attachments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelComposition {
    scene: ClosedModel,
    attachments: Vec<ComponentAttachment>,
}

impl ModelComposition {
    /// Validates and canonicalizes a fixed composition selection.
    pub fn new(
        scene: ClosedModel,
        attachments: impl IntoIterator<Item = ComponentAttachment>,
    ) -> Result<Self, CompositionError> {
        let mut attachments = attachments.into_iter().collect::<Vec<_>>();
        let mut instances = BTreeSet::new();
        let mut targets = BTreeSet::new();
        for attachment in &attachments {
            if !instances.insert(attachment.instance.as_str()) {
                return Err(CompositionError::DuplicateInstance(
                    attachment.instance.clone(),
                ));
            }
            if !targets.insert(attachment.target_site.as_str()) {
                return Err(CompositionError::DuplicateTargetSite(
                    attachment.target_site.clone(),
                ));
            }
        }
        attachments.sort_by(|left, right| left.instance.cmp(&right.instance));
        Ok(Self { scene, attachments })
    }

    /// Root scene artifact used for native parsing.
    #[must_use]
    pub fn scene(&self) -> &ClosedModel {
        &self.scene
    }

    /// Canonical fixed attachment list.
    #[must_use]
    pub fn attachments(&self) -> &[ComponentAttachment] {
        &self.attachments
    }

    /// Stable digest of the complete source and attachment selection.
    ///
    /// This is a selection identity, distinct from the canonical
    /// [`Model::identity`] derived from the retained compiled artifact.
    #[must_use]
    pub fn identity(&self) -> ModelIdentity {
        ModelIdentity(composition_digest(&self.scene, &self.attachments))
    }

    /// Compiles the fixed composition into one immutable native [`Model`].
    ///
    /// MuJoCo owns the attached native elements after compilation.  The
    /// returned model also retains the complete root and component resource
    /// closure in its artifact, together with a deterministic composition
    /// manifest that another native compiler can use to repeat the selection.
    pub fn compile(&self) -> Result<Model, ModelError> {
        validate_scene_policy(&self.scene)?;
        let resource_prefixes = self
            .attachments
            .iter()
            .map(|attachment| {
                (
                    &attachment.component,
                    format!("__phoxal_components__/{}/", attachment.instance()),
                )
            })
            .collect::<Vec<_>>();
        let ParsedSpec {
            spec: mut parent,
            _vfs: _parent_vfs,
        } = parse_spec_with_prefixed_resources(&self.scene, &resource_prefixes)?;
        enable_deep_copy(&parent)?;
        let mut native_names = BTreeSet::new();
        validate_native_names(&parent, None, &mut native_names)?;
        // Keep every child specification and VFS alive until the parent is
        // compiled. MuJoCo can retain shared references to attached children,
        // so deep-copy mode and this ownership are both deliberate.
        let mut children = Vec::with_capacity(self.attachments.len());

        for attachment in &self.attachments {
            validate_component_policy(&attachment.component, attachment.instance())?;
            let resource_prefix = format!("__phoxal_components__/{}/", attachment.instance());
            let child = parse_spec_with_prefix(&attachment.component, &resource_prefix)?;
            enable_deep_copy(&child.spec)?;
            validate_native_names(&child.spec, Some(attachment), &mut native_names)?;
            attach_spec(
                &parent,
                &child.spec,
                attachment.target_site(),
                attachment.component_root(),
                attachment.prefix(),
                attachment.suffix(),
                &format!(
                    "component {:?} root {:?} to site {:?}",
                    attachment.instance(),
                    attachment.component_root(),
                    attachment.target_site()
                ),
            )?;
            children.push(child);
        }

        compile_spec_with_vfs(&mut parent, &_parent_vfs)?;
        let serialized = serialize_spec(&parent)?;
        let artifact = self.composed_artifact(serialized)?;
        Model::from_closed(artifact)
    }

    fn composed_artifact(&self, serialized: Vec<u8>) -> Result<ClosedModel, ModelError> {
        // The serialized specification is the portable entry at the VFS root.
        // Preserve the original authored entry under a private name so the
        // retained source closure cannot collide with that entry.  Other
        // authored resources keep their names because compiled XML continues
        // to resolve them relative to the main MJCF directory.
        let mut resources = retained_scene_resources(&self.scene)?;

        for attachment in &self.attachments {
            let root = format!("__phoxal_components__/{}/", attachment.instance());
            for resource in attachment.component.resources() {
                let name = format!("{root}{}", resource.name());
                let resource = Resource::new(name, resource.bytes()).map_err(|error| {
                    composition_model_error(format!(
                        "component {:?} resource closure: {error}",
                        attachment.instance()
                    ))
                })?;
                resources.push(resource);
            }
        }

        let manifest = composition_manifest(self);
        resources.push(Resource::new("model.xml", serialized).map_err(|error| {
            composition_model_error(format!("serialized composition: {error}"))
        })?);
        resources.push(
            Resource::new("__phoxal_composition__/manifest.bin", manifest).map_err(|error| {
                composition_model_error(format!("composition manifest: {error}"))
            })?,
        );
        ClosedModel::new("model.xml", resources)
            .map_err(|error| composition_model_error(format!("composed artifact: {error}")))
    }
}

/// A fixed robot composition attached to one independently authored scene.
///
/// The robot closure is composed first, then its single direct root body is
/// attached to the scene target.  This keeps scene-owned global settings and
/// georeference metadata separate from robot/component authoring.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SceneComposition {
    scene: ClosedModel,
    robot: ClosedModel,
    robot_instance: String,
    scene_target_site: String,
    robot_root: String,
    attachments: Vec<ComponentAttachment>,
}

impl SceneComposition {
    /// Validates a fixed robot/component selection and scene attachment.
    pub fn new(
        scene: ClosedModel,
        robot: ClosedModel,
        robot_instance: impl Into<String>,
        scene_target_site: impl Into<String>,
        robot_root: impl Into<String>,
        attachments: impl IntoIterator<Item = ComponentAttachment>,
    ) -> Result<Self, CompositionError> {
        let robot_instance = robot_instance.into();
        let scene_target_site = scene_target_site.into();
        let robot_root = robot_root.into();
        validate_name(&robot_instance, "robot instance")?;
        validate_name(&scene_target_site, "scene target site")?;
        validate_name(&robot_root, "robot root body")?;
        let attachments = ModelComposition::new(robot.clone(), attachments)?.attachments;
        Ok(Self {
            scene,
            robot,
            robot_instance,
            scene_target_site,
            robot_root,
            attachments,
        })
    }

    /// Compiles the fixed robot and scene composition into one immutable model.
    pub fn compile(&self) -> Result<Model, ModelError> {
        validate_scene_policy(&self.scene)?;
        validate_component_policy(&self.robot, "robot")?;
        let robot_resource_prefix = "__phoxal_robot__/".to_owned();
        let component_resource_prefixes = self
            .attachments
            .iter()
            .map(|attachment| {
                (
                    &attachment.component,
                    format!("__phoxal_robot_components__/{}/", attachment.instance()),
                )
            })
            .collect::<Vec<_>>();
        let mut prefixed_resources = Vec::with_capacity(1 + component_resource_prefixes.len());
        prefixed_resources.push((&self.robot, robot_resource_prefix.clone()));
        prefixed_resources.extend(component_resource_prefixes.iter().cloned());
        let ParsedSpec {
            spec: mut scene_spec,
            _vfs: scene_vfs,
        } = parse_spec_with_prefixed_resources(&self.scene, &prefixed_resources)?;
        enable_deep_copy(&scene_spec)?;
        let mut native_names = BTreeSet::new();
        validate_native_names(&scene_spec, None, &mut native_names)?;

        let ParsedSpec {
            spec: robot_spec,
            _vfs: _robot_vfs,
        } = parse_spec_with_prefix(&self.robot, &robot_resource_prefix)?;
        enable_deep_copy(&robot_spec)?;
        let robot_root = direct_root_body_name(&robot_spec)?;
        if robot_root != self.robot_root {
            return Err(composition_model_error(format!(
                "robot root selection {:?} does not match its unique direct root body {:?}",
                self.robot_root, robot_root
            )));
        }
        let robot_prefix = format!("{}{}", self.robot_instance, NAMESPACE_SEPARATOR);
        validate_native_names_with_prefix(&robot_spec, &robot_prefix, &mut native_names)?;

        let mut children = Vec::with_capacity(self.attachments.len());
        for attachment in &self.attachments {
            validate_component_policy(&attachment.component, attachment.instance())?;
            let resource_prefix = format!("__phoxal_robot_components__/{}/", attachment.instance());
            let child = parse_spec_with_prefix(&attachment.component, &resource_prefix)?;
            enable_deep_copy(&child.spec)?;
            let final_prefix = format!("{}{}", robot_prefix, attachment.prefix());
            validate_native_names_with_prefix(&child.spec, &final_prefix, &mut native_names)?;
            attach_spec(
                &robot_spec,
                &child.spec,
                attachment.target_site(),
                attachment.component_root(),
                attachment.prefix(),
                attachment.suffix(),
                &format!(
                    "component {:?} root {:?} to robot site {:?}",
                    attachment.instance(),
                    attachment.component_root(),
                    attachment.target_site()
                ),
            )?;
            children.push(child);
        }

        if scene_spec
            .site_iter()
            .filter(|site| site.name() == self.scene_target_site)
            .count()
            != 1
        {
            return Err(composition_model_error(format!(
                "scene must contain exactly one persistent target site {:?}",
                self.scene_target_site
            )));
        }
        attach_spec(
            &scene_spec,
            &robot_spec,
            &self.scene_target_site,
            &robot_root,
            &robot_prefix,
            "",
            &format!(
                "robot {:?} root {:?} to scene site {:?}",
                self.robot_instance, robot_root, self.scene_target_site
            ),
        )?;
        compile_spec_with_vfs(&mut scene_spec, &scene_vfs)?;
        let serialized = serialize_spec(&scene_spec)?;
        let artifact = self.composed_artifact(serialized, &robot_resource_prefix)?;
        Model::from_closed(artifact)
    }

    fn composed_artifact(
        &self,
        serialized: Vec<u8>,
        robot_resource_prefix: &str,
    ) -> Result<ClosedModel, ModelError> {
        let mut resources = retained_scene_resources(&self.scene)?;
        for resource in self.robot.resources() {
            resources.push(
                Resource::new(
                    format!("{robot_resource_prefix}{}", resource.name()),
                    resource.bytes(),
                )
                .map_err(|error| {
                    composition_model_error(format!("robot resource closure: {error}"))
                })?,
            );
        }
        for attachment in &self.attachments {
            let root = format!("__phoxal_robot_components__/{}/", attachment.instance());
            for resource in attachment.component.resources() {
                resources.push(
                    Resource::new(format!("{root}{}", resource.name()), resource.bytes()).map_err(
                        |error| {
                            composition_model_error(format!(
                                "component {:?} resource closure: {error}",
                                attachment.instance()
                            ))
                        },
                    )?,
                );
            }
        }
        resources.push(Resource::new("model.xml", serialized).map_err(|error| {
            composition_model_error(format!("serialized composition: {error}"))
        })?);
        resources.push(
            Resource::new(
                "__phoxal_composition__/manifest.bin",
                scene_composition_manifest(self),
            )
            .map_err(|error| composition_model_error(format!("composition manifest: {error}")))?,
        );
        ClosedModel::new("model.xml", resources)
            .map_err(|error| composition_model_error(format!("composed artifact: {error}")))
    }
}

fn retained_scene_resources(scene: &ClosedModel) -> Result<Vec<Resource>, ModelError> {
    let main_directory = scene
        .entry()
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("");
    let mut resources = BTreeMap::<String, Vec<u8>>::new();
    for resource in scene.resources() {
        // Keep the authored bytes under a private provenance name.  When the
        // authored entry lived below a directory, also retain a root-relative
        // alias because the portable serialized specification is emitted as
        // `model.xml` at the VFS root.
        insert_retained_resource(
            &mut resources,
            format!("__phoxal_scene__/{}", resource.name()),
            resource.bytes(),
        )?;
        if resource.name() == scene.entry() {
            continue;
        }
        let root_name = if main_directory.is_empty() {
            resource.name()
        } else {
            resource
                .name()
                .strip_prefix(&format!("{main_directory}/"))
                .unwrap_or(resource.name())
        };
        if root_name != "model.xml" {
            insert_retained_resource(&mut resources, root_name.to_owned(), resource.bytes())?;
        }
    }
    resources
        .into_iter()
        .map(|(name, bytes)| {
            Resource::new(name, bytes).map_err(|error| {
                composition_model_error(format!("scene resource closure: {error}"))
            })
        })
        .collect()
}

fn insert_retained_resource(
    resources: &mut BTreeMap<String, Vec<u8>>,
    name: String,
    bytes: &[u8],
) -> Result<(), ModelError> {
    if let Some(existing) = resources.get(&name) {
        if existing.as_slice() != bytes {
            return Err(composition_model_error(format!(
                "scene resource name {name:?} has conflicting retained bytes"
            )));
        }
        return Ok(());
    }
    resources.insert(name, bytes.to_vec());
    Ok(())
}

/// Composes a robot closure into a scene after attaching its selected components.
pub fn compose_scene(
    scene: ClosedModel,
    robot: ClosedModel,
    robot_instance: impl Into<String>,
    scene_target_site: impl Into<String>,
    robot_root: impl Into<String>,
    attachments: impl IntoIterator<Item = ComponentAttachment>,
) -> Result<Model, ModelError> {
    SceneComposition::new(
        scene,
        robot,
        robot_instance,
        scene_target_site,
        robot_root,
        attachments,
    )
    .map_err(|error| composition_model_error(error.to_string()))?
    .compile()
}

/// Returns the sole direct body below an artifact's world body.
///
/// Robot authoring deliberately does not invent a root-body field.  The
/// native closure therefore has to provide exactly one direct body, and the
/// caller records that actual name in the admission selection before any
/// scene attachment is attempted.
pub fn unique_direct_root_body(artifact: &ClosedModel) -> Result<String, ModelError> {
    let ParsedSpec { spec, _vfs: _ } = parse_spec_with_prefix(artifact, "")?;
    direct_root_body_name(&spec)
}

fn attach_spec(
    parent: &MjSpec,
    child: &MjSpec,
    parent_site_name: &str,
    child_root_name: &str,
    prefix: &str,
    suffix: &str,
    description: &str,
) -> Result<(), ModelError> {
    let parent_target = parent
        .site(parent_site_name)
        .ok_or_else(|| {
            composition_model_error(format!("parent has no target site {parent_site_name:?}"))
        })?
        .element_pointer() as *mut mjsElement;
    let child_root = child
        .body(child_root_name)
        .ok_or_else(|| {
            composition_model_error(format!("child has no root body {child_root_name:?}"))
        })?
        .element_pointer();
    let prefix = CString::new(prefix)
        .map_err(|_| composition_model_error("namespace prefix contains NUL"))?;
    let suffix = CString::new(suffix)
        .map_err(|_| composition_model_error("namespace suffix contains NUL"))?;
    // SAFETY: both pointers refer to live elements owned by the corresponding
    // specifications.  The C API consumes the child root into the parent
    // specification and returns null when admission fails.
    let attached =
        unsafe { mjs_attach(parent_target, child_root, prefix.as_ptr(), suffix.as_ptr()) };
    if attached.is_null() {
        return Err(composition_model_error(format!(
            "MuJoCo rejected attachment of {description}"
        )));
    }
    Ok(())
}

fn direct_root_body_name(spec: &MjSpec) -> Result<String, ModelError> {
    let roots = spec
        .world_body()
        .body_iter(false)
        .map(|body| body.name().to_owned())
        .collect::<Vec<_>>();
    match roots.as_slice() {
        [root] if !root.is_empty() => Ok(root.clone()),
        [] => Err(composition_model_error(
            "robot must contain exactly one direct root body, found none",
        )),
        _ => Err(composition_model_error(format!(
            "robot must contain exactly one direct root body, found {}",
            roots.len()
        ))),
    }
}

struct ParsedSpec {
    spec: MjSpec,
    _vfs: MjVfs,
}

fn validate_native_names(
    spec: &MjSpec,
    attachment: Option<&ComponentAttachment>,
    names: &mut BTreeSet<(String, String)>,
) -> Result<(), ModelError> {
    match attachment {
        Some(attachment) => validate_native_names_with_affixes(
            spec,
            attachment.prefix(),
            attachment.suffix(),
            names,
        ),
        None => validate_native_names_with_affixes(spec, "", "", names),
    }
}

fn validate_native_names_with_prefix(
    spec: &MjSpec,
    prefix: &str,
    names: &mut BTreeSet<(String, String)>,
) -> Result<(), ModelError> {
    validate_native_names_with_affixes(spec, prefix, "", names)
}

fn validate_native_names_with_affixes(
    spec: &MjSpec,
    prefix: &str,
    suffix: &str,
    names: &mut BTreeSet<(String, String)>,
) -> Result<(), ModelError> {
    macro_rules! check_items {
        ($iterator:ident, $kind:literal) => {
            for item in spec.$iterator() {
                let local_name = item.name();
                if local_name.is_empty() {
                    continue;
                }
                let native_name = format!("{prefix}{local_name}{suffix}");
                validate_composed_name(&native_name, "composed native object")
                    .map_err(|error| composition_model_error(error.to_string()))?;
                if !names.insert(($kind.to_owned(), native_name.clone())) {
                    return Err(composition_model_error(format!(
                        "native {kind} name {native_name:?} is selected more than once",
                        kind = $kind,
                    )));
                }
            }
        };
    }

    check_items!(geom_iter, "geom");
    check_items!(joint_iter, "joint");
    check_items!(site_iter, "site");
    check_items!(camera_iter, "camera");
    check_items!(light_iter, "light");
    check_items!(frame_iter, "frame");
    check_items!(actuator_iter, "actuator");
    check_items!(sensor_iter, "sensor");
    check_items!(flex_iter, "flex");
    check_items!(pair_iter, "pair");
    check_items!(equality_iter, "equality");
    check_items!(exclude_iter, "exclude");
    check_items!(tendon_iter, "tendon");
    check_items!(numeric_iter, "numeric");
    check_items!(text_iter, "text");
    check_items!(tuple_iter, "tuple");
    check_items!(key_iter, "key");
    check_items!(mesh_iter, "mesh");
    check_items!(hfield_iter, "hfield");
    check_items!(skin_iter, "skin");
    check_items!(texture_iter, "texture");
    check_items!(material_iter, "material");
    check_items!(plugin_iter, "plugin");
    for item in spec.body_iter() {
        let local_name = item.name();
        if local_name.is_empty() {
            continue;
        }
        let native_name = format!("{prefix}{local_name}{suffix}");
        validate_composed_name(&native_name, "composed native object")
            .map_err(|error| composition_model_error(error.to_string()))?;
        if !names.insert(("body".to_owned(), native_name.clone())) {
            return Err(composition_model_error(format!(
                "native body name {native_name:?} is selected more than once"
            )));
        }
    }
    Ok(())
}

/// Compose one root scene and explicit component attachments.
pub fn compose_model(
    scene: ClosedModel,
    attachments: impl IntoIterator<Item = ComponentAttachment>,
) -> Result<Model, ModelError> {
    ModelComposition::new(scene, attachments)
        .map_err(|error| composition_model_error(error.to_string()))?
        .compile()
}

fn parse_spec_with_prefix(artifact: &ClosedModel, prefix: &str) -> Result<ParsedSpec, ModelError> {
    let mut vfs = MjVfs::new();
    for resource in artifact.resources() {
        let name = format!("{prefix}{}", resource.name());
        vfs.add_from_buffer(&name, resource.bytes())
            .map_err(|error| composition_model_error(format!("resource admission: {error}")))?;
    }
    let entry = format!("{prefix}{}", artifact.entry());
    let mut spec = MjSpec::from_xml_vfs(&entry, &vfs)
        .map_err(|error| composition_model_error(format!("native parse: {error}")))?;
    if !prefix.is_empty() {
        namespace_asset_files(&mut spec, prefix)?;
    }
    Ok(ParsedSpec { spec, _vfs: vfs })
}

fn parse_spec_with_prefixed_resources(
    artifact: &ClosedModel,
    extras: &[(&ClosedModel, String)],
) -> Result<ParsedSpec, ModelError> {
    let mut vfs = MjVfs::new();
    for resource in artifact.resources() {
        vfs.add_from_buffer(resource.name(), resource.bytes())
            .map_err(|error| composition_model_error(format!("resource admission: {error}")))?;
    }
    for (extra, prefix) in extras {
        for resource in extra.resources() {
            let name = format!("{prefix}{}", resource.name());
            vfs.add_from_buffer(&name, resource.bytes())
                .map_err(|error| {
                    composition_model_error(format!("prefixed resource admission: {error}"))
                })?;
        }
    }
    let spec = MjSpec::from_xml_vfs(artifact.entry(), &vfs)
        .map_err(|error| composition_model_error(format!("native parse: {error}")))?;
    Ok(ParsedSpec { spec, _vfs: vfs })
}

fn namespace_asset_files(spec: &mut MjSpec, prefix: &str) -> Result<(), ModelError> {
    macro_rules! prefix_files {
        ($iterator:ident) => {
            for item in spec.$iterator() {
                let file = item.file().to_owned();
                if file.is_empty() {
                    continue;
                }
                let path = prefixed_asset_path(prefix, &file)?;
                item.set_file(&path);
            }
        };
    }

    prefix_files!(mesh_iter_mut);
    prefix_files!(hfield_iter_mut);
    prefix_files!(skin_iter_mut);
    prefix_files!(texture_iter_mut);
    Ok(())
}

fn prefixed_asset_path(prefix: &str, file: &str) -> Result<String, ModelError> {
    let path = std::path::Path::new(file);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err(composition_model_error(format!(
            "asset file {file:?} is absolute or escapes its closed resource root"
        )));
    }
    Ok(format!("{prefix}{file}"))
}

fn serialize_spec(spec: &MjSpec) -> Result<Vec<u8>, ModelError> {
    let mut buffer_size = 64 * 1024;
    loop {
        match spec.save_xml_string(buffer_size) {
            Ok(xml) => return Ok(xml.into_bytes()),
            Err(mujoco_rs::error::MjEditError::XmlBufferTooSmall { required_size }) => {
                buffer_size = required_size.checked_add(1).ok_or_else(|| {
                    composition_model_error("composed XML size overflows native buffer")
                })?;
            }
            Err(error) => {
                return Err(composition_model_error(format!(
                    "serialize composed specification: {error}"
                )));
            }
        }
    }
}

fn enable_deep_copy(spec: &MjSpec) -> Result<(), ModelError> {
    // SAFETY: the specification owns the pointer returned by `ffi`, and no
    // other mutable native operation is in flight while composition runs.
    let result = unsafe { mjs_setDeepCopy(spec.ffi() as *const _ as *mut _, 1) };
    if result == 0 {
        Ok(())
    } else {
        Err(composition_model_error(format!(
            "MuJoCo rejected deep-copy attachment mode with status {result}"
        )))
    }
}

fn compile_spec_with_vfs(spec: &mut MjSpec, vfs: &MjVfs) -> Result<(), ModelError> {
    // SAFETY: both pointers are owned by the live specification/VFS values,
    // and MuJoCo only reads the VFS while compiling this specification.
    let model = unsafe { mj_compile(spec.ffi() as *const _ as *mut _, vfs.ffi()) };
    if model.is_null() {
        // SAFETY: a failed compile leaves the specification alive and exposes
        // its NUL-terminated diagnostic through mjs_getError.
        let message = unsafe {
            let error = mjs_getError(spec.ffi() as *const _ as *mut _);
            if error.is_null() {
                "unknown native compilation failure".to_owned()
            } else {
                CStr::from_ptr(error).to_string_lossy().into_owned()
            }
        };
        return Err(composition_model_error(format!(
            "native compile: {message}"
        )));
    }
    // SAFETY: mj_compile returned one owned model pointer, which is not needed
    // after it marks the specification as compiled for serialization.
    unsafe { mj_deleteModel(model) };
    Ok(())
}

fn validate_scene_policy(scene: &ClosedModel) -> Result<(), ModelError> {
    validate_xml_policy(scene, "scene", &["keyframe"])
}

fn validate_component_policy(component: &ClosedModel, instance: &str) -> Result<(), ModelError> {
    validate_xml_policy(
        component,
        &format!("component {instance:?}"),
        &["option", "keyframe", "visual", "statistic", "size"],
    )
}

fn validate_xml_policy(
    artifact: &ClosedModel,
    owner: &str,
    forbidden_tags: &[&str],
) -> Result<(), ModelError> {
    for resource in artifact.resources() {
        let Ok(text) = std::str::from_utf8(resource.bytes()) else {
            continue;
        };
        for tag in forbidden_tags {
            if contains_xml_tag(text, tag) {
                return Err(composition_model_error(format!(
                    "{owner} resource {:?} contains forbidden global <{tag}>; global scene settings and keyframes are owned by the root scene",
                    resource.name()
                )));
            }
        }
    }
    Ok(())
}

fn contains_xml_tag(xml: &str, expected: &str) -> bool {
    let bytes = xml.as_bytes();
    let mut index = 0;
    while let Some(relative) = bytes[index..].iter().position(|byte| *byte == b'<') {
        index += relative + 1;
        if index >= bytes.len() || matches!(bytes[index], b'!' | b'?' | b'/') {
            continue;
        }
        let start = index;
        while index < bytes.len()
            && !matches!(bytes[index], b'>' | b'/' | b' ' | b'\t' | b'\r' | b'\n')
        {
            index += 1;
        }
        if &bytes[start..index] == expected.as_bytes() {
            return true;
        }
    }
    false
}

fn composition_manifest(composition: &ModelComposition) -> Vec<u8> {
    let mut manifest = Vec::new();
    manifest.extend_from_slice(b"phoxal-mujoco-composition-v1\0");
    append_manifest_field(&mut manifest, composition.scene.entry().as_bytes());
    manifest.extend_from_slice(&(composition.attachments.len() as u64).to_be_bytes());
    for attachment in &composition.attachments {
        append_manifest_field(&mut manifest, attachment.instance.as_bytes());
        append_manifest_field(&mut manifest, attachment.component.entry().as_bytes());
        append_manifest_field(&mut manifest, attachment.target_site.as_bytes());
        append_manifest_field(&mut manifest, attachment.component_root.as_bytes());
        append_manifest_field(&mut manifest, attachment.prefix.as_bytes());
        append_manifest_field(&mut manifest, attachment.suffix.as_bytes());
        manifest.extend_from_slice(&attachment.component.digest());
    }
    manifest
}

fn scene_composition_manifest(composition: &SceneComposition) -> Vec<u8> {
    let mut manifest = Vec::new();
    manifest.extend_from_slice(b"phoxal-mujoco-scene-composition-v1\0");
    append_manifest_field(&mut manifest, composition.scene.entry().as_bytes());
    append_manifest_field(&mut manifest, composition.robot.entry().as_bytes());
    append_manifest_field(&mut manifest, composition.robot_instance.as_bytes());
    append_manifest_field(&mut manifest, composition.scene_target_site.as_bytes());
    append_manifest_field(&mut manifest, composition.robot_root.as_bytes());
    manifest.extend_from_slice(&(composition.attachments.len() as u64).to_be_bytes());
    for attachment in &composition.attachments {
        append_manifest_field(&mut manifest, attachment.instance.as_bytes());
        append_manifest_field(&mut manifest, attachment.component.entry().as_bytes());
        append_manifest_field(&mut manifest, attachment.target_site.as_bytes());
        append_manifest_field(&mut manifest, attachment.component_root.as_bytes());
        append_manifest_field(&mut manifest, attachment.prefix.as_bytes());
        append_manifest_field(&mut manifest, attachment.suffix.as_bytes());
        manifest.extend_from_slice(&attachment.component.digest());
    }
    manifest
}

fn append_manifest_field(manifest: &mut Vec<u8>, bytes: &[u8]) {
    manifest.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    manifest.extend_from_slice(bytes);
}

fn composition_model_error(detail: impl Into<String>) -> ModelError {
    ModelError::Composition(detail.into())
}

fn validate_name(value: &str, field: &'static str) -> Result<(), CompositionError> {
    if value.is_empty()
        || value.len() > 64
        || !value.is_ascii()
        || value.contains(NAMESPACE_SEPARATOR)
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte == 0)
    {
        return Err(CompositionError::InvalidName {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

fn validate_composed_name(value: &str, field: &'static str) -> Result<(), CompositionError> {
    if value.is_empty()
        || value.len() > 64
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte == 0)
    {
        return Err(CompositionError::InvalidName {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

fn composition_digest(scene: &ClosedModel, attachments: &[ComponentAttachment]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"phoxal-mujoco-composition-v1");
    feed_bytes(&mut digest, scene.entry().as_bytes());
    digest.update(scene.digest());
    digest.update((attachments.len() as u64).to_be_bytes());
    for attachment in attachments {
        feed_bytes(&mut digest, attachment.instance.as_bytes());
        feed_bytes(&mut digest, attachment.target_site.as_bytes());
        feed_bytes(&mut digest, attachment.component_root.as_bytes());
        feed_bytes(&mut digest, attachment.prefix.as_bytes());
        feed_bytes(&mut digest, attachment.suffix.as_bytes());
        digest.update(attachment.component.digest());
    }
    digest.finalize().into()
}

fn feed_bytes(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

/// A fixed-composition configuration was not admissible.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CompositionError {
    /// One public/native composition identifier is malformed.
    #[error("{field} {value:?} is invalid")]
    InvalidName {
        /// Identifier role.
        field: &'static str,
        /// Supplied identifier.
        value: String,
    },
    /// An instance identity appears more than once.
    #[error("component instance {0:?} is selected more than once")]
    DuplicateInstance(String),
    /// A parent target site receives more than one root body.
    #[error("parent target site {0:?} is selected more than once")]
    DuplicateTargetSite(String),
}

impl fmt::Display for ModelComposition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelComposition")
            .field("scene", &self.scene.digest_hex())
            .field("attachments", &self.attachments)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_namespace_is_explicit_and_deterministic() {
        let component = ClosedModel::from_xml(
            br#"<mujoco model="component"><worldbody><body name="mount"/></worldbody></mujoco>"#,
        )
        .expect("component artifact");
        let attachment = ComponentAttachment::new("left", component, "left_mount", "mount")
            .expect("valid attachment");
        assert_eq!(attachment.prefix(), "left__");
        assert_eq!(
            attachment.native_name("motor").expect("native name"),
            "left__motor"
        );
        assert!(matches!(
            ComponentAttachment::new(
                "left__again",
                ClosedModel::from_xml("<mujoco/>").unwrap(),
                "mount",
                "mount"
            ),
            Err(CompositionError::InvalidName { .. })
        ));
    }

    #[test]
    fn duplicate_instances_and_targets_are_rejected_before_native_work() {
        let scene = ClosedModel::from_xml(
            br#"<mujoco model="scene"><worldbody><site name="left_mount"/><site name="right_mount"/></worldbody></mujoco>"#,
        )
        .expect("scene artifact");
        let component = ClosedModel::from_xml(
            br#"<mujoco model="component"><worldbody><body name="mount"/></worldbody></mujoco>"#,
        )
        .expect("component artifact");
        let left = ComponentAttachment::new("wheel", component.clone(), "left_mount", "mount")
            .expect("left attachment");
        let right_same_instance =
            ComponentAttachment::new("wheel", component.clone(), "right_mount", "mount")
                .expect("right attachment");
        assert!(matches!(
            ModelComposition::new(scene.clone(), [left, right_same_instance]),
            Err(CompositionError::DuplicateInstance(_))
        ));

        let left = ComponentAttachment::new("left", component.clone(), "left_mount", "mount")
            .expect("left attachment");
        let right_same_target = ComponentAttachment::new("right", component, "left_mount", "mount")
            .expect("right attachment");
        assert!(matches!(
            ModelComposition::new(scene, [left, right_same_target]),
            Err(CompositionError::DuplicateTargetSite(_))
        ));
    }

    #[test]
    fn nested_scene_entries_retain_root_relative_resources_for_portable_output() {
        let scene = ClosedModel::new(
            "sub/model.xml",
            [
                Resource::new(
                    "sub/model.xml",
                    br#"<mujoco><asset><mesh name="payload" file="assets/payload.obj"/></asset><worldbody/></mujoco>"#,
                )
                .expect("nested scene XML"),
                Resource::new("sub/assets/payload.obj", b"mesh".to_vec())
                    .expect("nested scene asset"),
            ],
        )
        .expect("nested scene closure");
        let retained = retained_scene_resources(&scene).expect("retained scene closure");
        assert!(
            retained
                .iter()
                .any(|resource| resource.name() == "__phoxal_scene__/sub/model.xml")
        );
        assert!(
            retained
                .iter()
                .any(|resource| resource.name() == "assets/payload.obj")
        );
    }

    #[test]
    fn global_component_settings_and_keyframes_are_rejected() {
        let scene = ClosedModel::from_xml(br#"<mujoco model="scene"><worldbody/></mujoco>"#)
            .expect("scene artifact");
        let component = ClosedModel::from_xml(
            br#"<mujoco model="component"><option gravity="0 0 0"/><worldbody><body name="mount"/></worldbody></mujoco>"#,
        )
        .expect("component artifact");
        let attachment =
            ComponentAttachment::new("sensor", component, "mount", "mount").expect("attachment");
        let error = ModelComposition::new(scene, [attachment])
            .expect("selection")
            .compile()
            .expect_err("component global options must be refused");
        assert!(error.to_string().contains("forbidden global <option>"));
    }

    #[cfg(feature = "native")]
    #[test]
    fn native_attach_compiles_prefixed_component_objects() {
        let scene = ClosedModel::from_xml(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/composition/scene.xml"
        )))
        .expect("scene artifact");
        let component = component_with_mesh();
        let attachment =
            ComponentAttachment::new("sensor", component, "mount", "mount").expect("attach");
        let model = ModelComposition::new(scene, [attachment])
            .expect("composition selection")
            .compile()
            .expect("native composition");
        assert!(model.body("sensor__mount").expect("body lookup").is_some());
        assert!(
            model
                .body("sensor__payload")
                .expect("body lookup")
                .is_some()
        );
        assert!(
            model
                .joint("sensor__joint")
                .expect("joint lookup")
                .is_some()
        );
        assert!(
            model
                .artifact()
                .resource("__phoxal_components__/sensor/model.xml")
                .is_some(),
            "composed artifacts must retain the component entry"
        );
        assert!(
            model
                .artifact()
                .resource("__phoxal_components__/sensor/assets/payload.obj")
                .is_some(),
            "composed artifacts must retain attached mesh resources under final names"
        );
        let serialized = std::str::from_utf8(
            model
                .artifact()
                .resource("model.xml")
                .expect("serialized composition")
                .bytes(),
        )
        .expect("serialized XML");
        assert!(serialized.contains("__phoxal_components__/sensor/assets/payload.obj"));
        assert!(
            model
                .artifact()
                .resource("__phoxal_composition__/manifest.bin")
                .is_some(),
            "composed artifacts must retain composition selection metadata"
        );
        let rebuilt = Model::from_closed(model.artifact().clone())
            .expect("serialized composition artifact must compile independently");
        assert_eq!(rebuilt.counts(), model.counts());
        assert_eq!(rebuilt.identity(), model.identity());
    }

    #[cfg(feature = "native")]
    fn component_with_mesh() -> ClosedModel {
        ClosedModel::new(
            "model.xml",
            [
                Resource::new(
                    "model.xml",
                    br#"<mujoco model="composition-component"><asset><mesh name="payload_mesh" file="assets/payload.obj"/></asset><worldbody><body name="mount"><body name="payload"><joint name="joint" type="hinge"/><geom name="payload_visual" type="mesh" mesh="payload_mesh" contype="0" conaffinity="0"/></body></body></worldbody></mujoco>"#.to_vec(),
                )
                .expect("component XML resource"),
                Resource::new(
                    "assets/payload.obj",
                    b"v 0 0 0\nv 0.02 0 0\nv 0 0.02 0\nv 0 0 0.02\nf 1 2 3\nf 1 3 4\n".to_vec(),
                )
                .expect("component mesh resource"),
            ],
        )
        .expect("component artifact")
    }

    #[cfg(feature = "native")]
    #[test]
    fn independent_compositions_do_not_share_native_children() {
        let scene = ClosedModel::from_xml(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/composition/scene.xml"
        )))
        .expect("scene artifact");
        let component = ClosedModel::from_xml(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/composition/component.xml"
        )))
        .expect("component artifact");

        let first = ModelComposition::new(
            scene.clone(),
            [
                ComponentAttachment::new("left", component.clone(), "mount", "mount")
                    .expect("left attachment"),
            ],
        )
        .expect("first selection")
        .compile()
        .expect("first composition");
        let second = ModelComposition::new(
            scene,
            [
                ComponentAttachment::new("right", component, "right_mount", "mount")
                    .expect("right attachment"),
            ],
        )
        .expect("second selection")
        .compile()
        .expect("second composition");

        assert_ne!(first.identity(), second.identity());
        assert!(first.body("left__payload").expect("first body").is_some());
        assert!(first.body("right__payload").expect("first body").is_none());
        assert!(
            second
                .body("right__payload")
                .expect("second body")
                .is_some()
        );
        assert!(second.body("left__payload").expect("second body").is_none());
    }

    #[cfg(feature = "native")]
    #[test]
    fn scene_composition_attaches_robot_after_component_composition() {
        let scene = ClosedModel::from_xml(
            br#"<mujoco model="outer-scene"><option timestep="0.01"/><worldbody><site name="robot_mount"/></worldbody></mujoco>"#,
        )
        .expect("scene artifact");
        let robot = ClosedModel::from_xml(
            br#"<mujoco model="robot"><worldbody><body name="base"><site name="component_mount"/></body></worldbody></mujoco>"#,
        )
        .expect("robot artifact");
        let component = ClosedModel::from_xml(
            br#"<mujoco model="component"><worldbody><body name="mount"><body name="payload"><geom type="sphere" size="0.01"/></body></body></worldbody></mujoco>"#,
        )
        .expect("component artifact");
        let attachment = ComponentAttachment::new("sensor", component, "component_mount", "mount")
            .expect("component attachment");
        let model =
            SceneComposition::new(scene, robot, "bench", "robot_mount", "base", [attachment])
                .expect("scene composition selection")
                .compile()
                .expect("scene composition");
        assert!(model.body("bench__base").unwrap().is_some());
        assert!(model.body("bench__sensor__payload").unwrap().is_some());
        let rebuilt = Model::from_closed(model.artifact().clone()).expect("independent rebuild");
        assert_eq!(rebuilt.identity(), model.identity());
        assert_eq!(rebuilt.counts(), model.counts());
    }
}
