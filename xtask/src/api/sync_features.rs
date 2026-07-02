use std::collections::BTreeSet;
use std::fs;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use proc_macro2::TokenStream;
use serde::{Deserialize, Serialize};
use syn::parse::{Parse, ParseStream};
use syn::{Attribute, Ident, Item, ItemMacro, ItemMod, Macro};
use toml_edit::{Array, DocumentMut, Item as TomlItem, Table, value};

use crate::workspace::Workspace;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GenerationChannel {
    Stable,
    Preview,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ApiGeneration {
    pub name: String,
    pub channel: GenerationChannel,
}

const BEGIN_MARKER: &str = "# @generated begin phoxal-api preview features";
const END_MARKER: &str = "# @generated end phoxal-api preview features";

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Fail if phoxal-api/Cargo.toml does not match the api tree.
    #[arg(long, conflicts_with = "write")]
    check: bool,
    /// Rewrite the managed feature block in phoxal-api/Cargo.toml.
    #[arg(long, conflicts_with = "check")]
    write: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Check,
    Write,
}

impl Args {
    fn mode(&self) -> Mode {
        match (self.check, self.write) {
            (_, true) => Mode::Write,
            _ => Mode::Check,
        }
    }
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let api_lib = workspace.root().join("phoxal-api/src/lib.rs");
    let manifest = workspace.root().join("phoxal-api/Cargo.toml");

    let source = fs::read_to_string(&api_lib)
        .with_context(|| format!("failed to read {}", api_lib.display()))?;
    let preview_features = preview_features_from_source(&source)
        .with_context(|| format!("failed to scan {}", api_lib.display()))?;

    let manifest_text = fs::read_to_string(&manifest)
        .with_context(|| format!("failed to read {}", manifest.display()))?;
    let synced = sync_manifest_text(&manifest_text, &preview_features)?;

    if synced == manifest_text {
        println!(
            "{} is in sync with {}",
            manifest.display(),
            api_lib.display()
        );
        return Ok(());
    }

    match args.mode() {
        Mode::Check => bail!(
            "{} preview feature block is out of sync; run `cargo xtask api sync-features --write`",
            manifest.display()
        ),
        Mode::Write => {
            fs::write(&manifest, synced)
                .with_context(|| format!("failed to write {}", manifest.display()))?;
            println!("rewrote {}", manifest.display());
            Ok(())
        }
    }
}

pub(crate) fn api_generations_from_workspace(workspace: &Workspace) -> Result<Vec<ApiGeneration>> {
    let api_lib = workspace.root().join("phoxal-api/src/lib.rs");
    let source = fs::read_to_string(&api_lib)
        .with_context(|| format!("failed to read {}", api_lib.display()))?;
    api_generations_from_source(&source)
        .with_context(|| format!("failed to scan {}", api_lib.display()))
}

fn preview_features_from_source(source: &str) -> Result<Vec<String>> {
    Ok(api_generations_from_source(source)?
        .into_iter()
        .filter(|generation| generation.channel == GenerationChannel::Preview)
        .map(|generation| format!("preview-{}", generation.name))
        .collect())
}

pub(crate) fn api_generations_from_source(source: &str) -> Result<Vec<ApiGeneration>> {
    let file = syn::parse_file(source).context("phoxal-api/src/lib.rs is not valid Rust")?;
    let mut scanner = TreeScanner::default();
    scanner.scan_items(&file.items, ScanContext::default())?;

    match scanner.invocations.len() {
        0 => bail!("expected exactly one production phoxal_api_tree! invocation, found none"),
        1 => parse_api_generations(scanner.invocations.remove(0)),
        count => {
            bail!("expected exactly one production phoxal_api_tree! invocation, found {count}")
        }
    }
}

#[derive(Default)]
struct TreeScanner {
    invocations: Vec<TokenStream>,
}

#[derive(Clone, Copy, Default)]
struct ScanContext {
    cfg_wrapped: bool,
    depth: usize,
}

impl TreeScanner {
    fn scan_items(&mut self, items: &[Item], context: ScanContext) -> Result<()> {
        for item in items {
            if is_cfg_test_module(item) {
                continue;
            }

            let cfg_wrapped = context.cfg_wrapped || has_cfg_indirection(item_attrs(item));
            let context = ScanContext {
                cfg_wrapped,
                depth: context.depth,
            };

            match item {
                Item::Macro(item_macro) => self.scan_item_macro(item_macro, context)?,
                Item::Mod(item_mod) => self.scan_item_mod(item_mod, context)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn scan_item_macro(&mut self, item_macro: &ItemMacro, context: ScanContext) -> Result<()> {
        if is_macro_path(&item_macro.mac, "include") {
            bail!("include! indirection is not allowed around the production phoxal_api_tree!");
        }

        if !is_macro_path(&item_macro.mac, "phoxal_api_tree") {
            return Ok(());
        }

        if context.cfg_wrapped {
            bail!("cfg indirection is not allowed around the production phoxal_api_tree!");
        }
        if context.depth != 0 {
            bail!("the production phoxal_api_tree! invocation must be at crate root");
        }

        self.invocations.push(item_macro.mac.tokens.clone());
        Ok(())
    }

    fn scan_item_mod(&mut self, item_mod: &ItemMod, context: ScanContext) -> Result<()> {
        let Some((_, items)) = &item_mod.content else {
            return Ok(());
        };

        self.scan_items(
            items,
            ScanContext {
                cfg_wrapped: context.cfg_wrapped,
                depth: context.depth + 1,
            },
        )
    }
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        Item::Verbatim(_) => &[],
        _ => &[],
    }
}

fn is_cfg_test_module(item: &Item) -> bool {
    matches!(item, Item::Mod(item_mod) if item_mod.attrs.iter().any(is_cfg_test_attr))
}

fn has_cfg_indirection(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .any(|attr| attr.path().is_ident("cfg") || attr.path().is_ident("cfg_attr"))
}

fn is_cfg_test_attr(attr: &Attribute) -> bool {
    attr.path().is_ident("cfg")
        && attr
            .parse_args::<syn::Path>()
            .is_ok_and(|path| path.is_ident("test"))
}

fn is_macro_path(mac: &Macro, name: &str) -> bool {
    mac.path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == name)
}

mod kw {
    syn::custom_keyword!(preview);
    syn::custom_keyword!(version);
    syn::custom_keyword!(extends);
}

struct ParsedApiGenerations {
    generations: Vec<ApiGeneration>,
}

impl Parse for ParsedApiGenerations {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut generations = Vec::new();
        while !input.is_empty() {
            let is_preview = if input.peek(kw::preview) {
                input.parse::<kw::preview>()?;
                true
            } else {
                false
            };
            input.parse::<kw::version>()?;
            let name: Ident = input.parse()?;
            if input.peek(kw::extends) {
                input.parse::<kw::extends>()?;
                let _parent: Ident = input.parse()?;
            }

            let body;
            syn::braced!(body in input);
            let _tokens: TokenStream = body.parse()?;

            generations.push(ApiGeneration {
                name: name.to_string(),
                channel: if is_preview {
                    GenerationChannel::Preview
                } else {
                    GenerationChannel::Stable
                },
            });
        }
        Ok(ParsedApiGenerations { generations })
    }
}

fn parse_api_generations(tokens: TokenStream) -> Result<Vec<ApiGeneration>> {
    let parsed: ParsedApiGenerations =
        syn::parse2(tokens).context("failed to parse phoxal_api_tree! versions")?;
    let mut seen = BTreeSet::new();
    for generation in &parsed.generations {
        if !seen.insert(generation.name.clone()) {
            bail!("duplicate API generation {}", generation.name);
        }
    }
    Ok(parsed.generations)
}

fn sync_manifest_text(manifest: &str, preview_features: &[String]) -> Result<String> {
    let block = managed_block(preview_features)?;
    match marker_range(manifest)? {
        Some((start, end)) => {
            let mut out = String::with_capacity(manifest.len() + block.len());
            out.push_str(&manifest[..start]);
            out.push_str(&block);
            out.push_str(&manifest[end..]);
            Ok(out)
        }
        None => insert_managed_block(manifest, &block),
    }
}

fn managed_block(preview_features: &[String]) -> Result<String> {
    let mut features = Table::new();
    for feature in preview_features {
        features[feature.as_str()] = value(Array::new());
    }

    let mut doc = DocumentMut::new();
    doc["features"] = TomlItem::Table(features);

    Ok(format!("{BEGIN_MARKER}\n{}{END_MARKER}\n", doc))
}

fn marker_range(manifest: &str) -> Result<Option<(usize, usize)>> {
    let mut begin = None;
    let mut end = None;
    let mut offset = 0;

    for line in manifest.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed == BEGIN_MARKER {
            if begin.is_some() {
                bail!("found multiple begin markers for the generated preview feature block");
            }
            begin = Some(offset);
        } else if trimmed == END_MARKER {
            if end.is_some() {
                bail!("found multiple end markers for the generated preview feature block");
            }
            end = Some(offset + line.len());
        }
        offset += line.len();
    }

    match (begin, end) {
        (None, None) => Ok(None),
        (Some(begin), Some(end)) if begin < end => Ok(Some((begin, end))),
        (Some(_), Some(_)) => bail!("generated preview feature block markers are out of order"),
        (Some(_), None) => bail!("generated preview feature block is missing its end marker"),
        (None, Some(_)) => bail!("generated preview feature block is missing its begin marker"),
    }
}

fn insert_managed_block(manifest: &str, block: &str) -> Result<String> {
    if manifest
        .lines()
        .any(|line| line.trim_start().starts_with("[features]"))
    {
        bail!("refusing to insert generated block because an unmanaged [features] table exists");
    }

    let insert_at = find_table(manifest, "lints").unwrap_or(manifest.len());
    let (prefix, suffix) = manifest.split_at(insert_at);
    let mut out = String::with_capacity(manifest.len() + block.len() + 2);
    out.push_str(prefix.trim_end());
    out.push_str("\n\n");
    out.push_str(block);
    if !suffix.is_empty() {
        out.push('\n');
        out.push_str(suffix.trim_start_matches('\n'));
    }
    Ok(out)
}

fn find_table(manifest: &str, name: &str) -> Option<usize> {
    let mut offset = 0;
    let needle = format!("[{name}]");
    for line in manifest.split_inclusive('\n') {
        if line.trim() == needle {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(body: &str) -> String {
        format!(
            r#"
use phoxal_macros::phoxal_api_tree;

{body}

#[cfg(test)]
mod tests {{
    phoxal_api_tree! {{
        preview version ignored {{
            sample {{
                struct Body {{ value: u8 }}
                topic body: state Body;
            }}
        }}
    }}
}}
"#
        )
    }

    fn manifest_with_block(block_body: &str) -> String {
        format!(
            r#"[package]
name = "phoxal-api"

{BEGIN_MARKER}
[features]
{block_body}{END_MARKER}

[lints]
workspace = true
"#
        )
    }

    #[test]
    fn scanner_reads_single_root_invocation_and_ignores_test_modules() {
        let source = source(
            r#"
phoxal_api_tree! {
    version y2026_1 {
        sample {
            struct Body { value: u8 }
            topic body: state Body;
        }
    }
    preview version y2026_2 extends y2026_1 {}
}
"#,
        );

        assert_eq!(
            preview_features_from_source(&source).unwrap(),
            vec!["preview-y2026_2"]
        );
    }

    #[test]
    fn scanner_errors_on_zero_or_multiple_production_invocations() {
        let zero = source("pub fn no_tree() {}");
        let zero_err = preview_features_from_source(&zero).unwrap_err().to_string();
        assert!(zero_err.contains("found none"), "{zero_err}");

        let multiple = source(
            r#"
phoxal_api_tree! { version y2026_1 { sample { struct Body { value: u8 } topic body: state Body; } } }
phoxal_api_tree! { version y2026_2 { sample { struct Body { value: u8 } topic body: state Body; } } }
"#,
        );
        let multiple_err = preview_features_from_source(&multiple)
            .unwrap_err()
            .to_string();
        assert!(multiple_err.contains("found 2"), "{multiple_err}");
    }

    #[test]
    fn scanner_rejects_include_and_cfg_indirection() {
        let include_err = preview_features_from_source(&source(
            r#"
include!("api_tree.rs");
"#,
        ))
        .unwrap_err()
        .to_string();
        assert!(
            include_err.contains("include! indirection"),
            "{include_err}"
        );

        let cfg_err = preview_features_from_source(&source(
            r#"
#[cfg(feature = "generated-tree")]
phoxal_api_tree! {
    version y2026_1 {
        sample {
            struct Body { value: u8 }
            topic body: state Body;
        }
    }
}
"#,
        ))
        .unwrap_err()
        .to_string();
        assert!(cfg_err.contains("cfg indirection"), "{cfg_err}");
    }

    #[test]
    fn sync_check_accepts_empty_managed_block_when_no_previews_exist() {
        let manifest = manifest_with_block("");
        let synced = sync_manifest_text(&manifest, &[]).unwrap();
        assert_eq!(synced, manifest);
    }

    #[test]
    fn sync_write_replaces_managed_block_with_current_preview_features_only() {
        let manifest = manifest_with_block("preview-y2025_9 = []\n");
        let synced = sync_manifest_text(
            &manifest,
            &["preview-y2026_2".to_string(), "preview-y2026_3".to_string()],
        )
        .unwrap();

        assert!(synced.contains("preview-y2026_2 = []"));
        assert!(synced.contains("preview-y2026_3 = []"));
        assert!(!synced.contains("preview-y2025_9"));
        assert!(synced.contains("[lints]\nworkspace = true"));
    }

    #[test]
    fn sync_write_inserts_block_before_lints_when_missing() {
        let manifest = r#"[package]
name = "phoxal-api"

[lints]
workspace = true
"#;
        let synced = sync_manifest_text(manifest, &["preview-y2026_2".to_string()]).unwrap();

        assert!(synced.contains(BEGIN_MARKER));
        assert!(synced.contains("[features]\npreview-y2026_2 = []"));
        assert!(synced.contains("[lints]\nworkspace = true"));
    }
}
