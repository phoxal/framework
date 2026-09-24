//! Read sparse registry candidates without changing Cargo's cache or the robot.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use reqwest::blocking::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{CargoOptions, Error, PHOXAL_INDEX, ProjectLayout, Selection, Source, invalid};

#[derive(Deserialize)]
struct IndexEntry {
    vers: String,
    cksum: String,
    #[serde(default)]
    yanked: bool,
    #[serde(default)]
    deps: Vec<IndexDependency>,
}

#[derive(Deserialize)]
struct IndexDependency {
    name: String,
    #[serde(default)]
    package: Option<String>,
    req: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    optional: bool,
}

#[derive(Deserialize)]
struct RegistryConfig {
    dl: String,
}

pub(super) fn newest_eligible(
    layout: &ProjectLayout,
    options: &CargoOptions,
    selection: &Selection,
) -> Result<Option<String>, Error> {
    let registry = match &selection.source {
        None => "phoxal",
        Some(Source::Registry(source)) => source.registry.as_str(),
        Some(Source::Git(_)) | Some(Source::Path(_)) => return Ok(None),
    };
    if options.offline || matches!(options.lock, super::super::cargo::LockMode::Frozen) {
        return Err(invalid(
            layout.robot_manifest(),
            format!("cannot inspect registry `{registry}` for updates while offline"),
        ));
    }
    let current = semver::Version::parse(&selection.version)
        .map_err(|error| invalid(layout.robot_manifest(), error.to_string()))?;
    let index = index_url(layout.root(), registry)
        .map_err(|error| invalid(layout.robot_manifest(), error))?;
    let address = format!("{}{}", index, index_path(&selection.package));
    let client = Client::builder()
        .build()
        .map_err(|error| invalid(layout.robot_manifest(), error.to_string()))?;
    let token = std::env::var(format!(
        "CARGO_REGISTRIES_{}_TOKEN",
        registry.replace('-', "_").to_ascii_uppercase()
    ))
    .ok();
    let fetch = |url: &str| -> Result<Vec<u8>, Error> {
        let mut request = client.get(url);
        if let Some(token) = &token {
            request = request.header(reqwest::header::AUTHORIZATION, token);
        }
        let response = request
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| {
                invalid(
                    layout.robot_manifest(),
                    format!("registry `{registry}` request failed: {error}"),
                )
            })?;
        response
            .bytes()
            .map(|bytes| bytes.to_vec())
            .map_err(|error| {
                invalid(
                    layout.robot_manifest(),
                    format!("registry `{registry}` response failed: {error}"),
                )
            })
    };
    let entries = String::from_utf8(fetch(&address)?)
        .map_err(|error| invalid(layout.robot_manifest(), error.to_string()))?;
    let mut candidates = entries
        .lines()
        .map(serde_json::from_str::<IndexEntry>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            invalid(
                layout.robot_manifest(),
                format!("invalid registry index: {error}"),
            )
        })?
        .into_iter()
        .filter_map(|entry| {
            let version = semver::Version::parse(&entry.vers).ok()?;
            (version > current && !entry.yanked && supports_sdk(&entry)).then_some((version, entry))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    if candidates.is_empty() {
        return Ok(None);
    }
    let config: RegistryConfig = serde_json::from_slice(&fetch(&format!("{index}config.json"))?)
        .map_err(|error| {
            invalid(
                layout.robot_manifest(),
                format!("invalid registry config: {error}"),
            )
        })?;
    for (version, entry) in candidates {
        let url = download_url(&config.dl, &selection.package, &entry.vers);
        let archive = fetch(&url)?;
        let checksum = format!("{:x}", Sha256::digest(&archive));
        if checksum != entry.cksum {
            return Err(invalid(
                layout.robot_manifest(),
                format!(
                    "registry archive checksum mismatch for {} {}",
                    selection.package, entry.vers
                ),
            ));
        }
        if installable_archive(
            &archive,
            &selection.package,
            &entry.vers,
            selection.binary.as_deref(),
        )
        .map_err(|error| invalid(layout.robot_manifest(), error))?
        {
            return Ok(Some(version.to_string()));
        }
    }
    Ok(None)
}

fn supports_sdk(entry: &IndexEntry) -> bool {
    entry.deps.iter().any(|dependency| {
        dependency.package.as_deref().unwrap_or(&dependency.name) == "phoxal"
            && dependency.req == format!("={}", phoxal_build::SDK_VERSION)
            && dependency.kind.as_deref() == Some("build")
            && !dependency.optional
    })
}

fn index_url(root: &Path, registry: &str) -> Result<String, String> {
    let mut files = Vec::new();
    for directory in root.ancestors() {
        files.push(directory.join(".cargo/config.toml"));
        files.push(directory.join(".cargo/config"));
    }
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
    if let Some(home) = cargo_home {
        files.push(home.join("config.toml"));
        files.push(home.join("config"));
    }
    let configs = files
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|source| toml::from_str::<toml::Value>(&source).ok())
        .collect::<Vec<_>>();
    let url = configured_index(&configs, registry)?
        .or_else(|| (registry == "phoxal").then(|| PHOXAL_INDEX.to_owned()))
        .ok_or_else(|| format!("registry `{registry}` needs an index in Cargo configuration"))?;
    let url = url
        .strip_prefix("sparse+")
        .ok_or_else(|| format!("registry `{registry}` update requires a sparse index"))?;
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(format!(
            "registry `{registry}` has an invalid sparse index URL"
        ));
    }
    Ok(format!("{}/", url.trim_end_matches('/')))
}

fn configured_index(configs: &[toml::Value], registry: &str) -> Result<Option<String>, String> {
    let mut source = registry;
    let mut seen = BTreeSet::new();
    loop {
        if !seen.insert(source.to_owned()) {
            return Err(format!(
                "registry `{registry}` has a cyclic Cargo source replacement"
            ));
        }
        let settings = configs
            .iter()
            .find_map(|config| config.get("source").and_then(|value| value.get(source)));
        let Some(settings) = settings else {
            return Ok(registry_index(configs, source).or_else(|| {
                (source == "crates-io").then(|| "sparse+https://index.crates.io/".to_owned())
            }));
        };
        if let Some(next) = settings.get("replace-with").and_then(toml::Value::as_str) {
            source = next;
            continue;
        }
        return settings
            .get("registry")
            .and_then(toml::Value::as_str)
            .map(str::to_owned)
            .map(Some)
            .ok_or_else(|| {
                format!(
                    "registry `{registry}` is replaced by `{source}`, which is not a sparse registry"
                )
            });
    }
}

fn registry_index(configs: &[toml::Value], registry: &str) -> Option<String> {
    let env = format!(
        "CARGO_REGISTRIES_{}_INDEX",
        registry.replace('-', "_").to_ascii_uppercase()
    );
    std::env::var(env).ok().or_else(|| {
        configs.iter().find_map(|config| {
            config
                .get("registries")
                .and_then(|value| value.get(registry))
                .and_then(|value| value.get("index"))
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
        })
    })
}

fn index_path(package: &str) -> String {
    let name = package.to_ascii_lowercase();
    match name.len() {
        0 => String::new(),
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{name}", &name[..1]),
        _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
    }
}

fn download_url(template: &str, package: &str, version: &str) -> String {
    let lower = package.to_ascii_lowercase();
    let lowerprefix = index_path(&lower)
        .rsplit_once('/')
        .map(|(prefix, _)| prefix.to_owned())
        .unwrap_or_default();
    let prefix = index_path(package)
        .rsplit_once('/')
        .map(|(prefix, _)| prefix.to_owned())
        .unwrap_or_default();
    if template.contains('{') {
        template
            .replace("{lowerprefix}", &lowerprefix)
            .replace("{prefix}", &prefix)
            .replace("{crate}", package)
            .replace("{version}", version)
    } else {
        format!(
            "{}/{package}/{version}/download",
            template.trim_end_matches('/')
        )
    }
}

fn installable_archive(
    bytes: &[u8],
    package: &str,
    version: &str,
    binary: Option<&str>,
) -> Result<bool, String> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let prefix = format!("{package}-{version}/");
    let mut paths = BTreeSet::new();
    let mut manifest = None;
    for entry in archive.entries().map_err(|error| error.to_string())? {
        let mut entry = entry.map_err(|error| error.to_string())?;
        let path = entry
            .path()
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .into_owned();
        let Some(path) = path.strip_prefix(&prefix) else {
            continue;
        };
        if path == "Cargo.toml" {
            let mut source = String::new();
            entry
                .read_to_string(&mut source)
                .map_err(|error| error.to_string())?;
            manifest = Some(source);
        }
        paths.insert(path.to_owned());
    }
    let Some(manifest) = manifest else {
        return Ok(false);
    };
    let manifest = toml::from_str::<toml::Value>(&manifest).map_err(|error| error.to_string())?;
    let selected_binary = binary.unwrap_or(package);
    let explicit_binary = manifest
        .get("bin")
        .and_then(toml::Value::as_array)
        .is_some_and(|targets| {
            targets.iter().any(|target| {
                target.get("name").and_then(toml::Value::as_str) == Some(selected_binary)
            })
        });
    Ok(manifest["package"]["name"].as_str() == Some(package)
        && manifest["package"]["version"].as_str() == Some(version)
        && paths.contains("Cargo.lock")
        && paths.contains("build.rs")
        && paths
            .iter()
            .any(|path| path.starts_with("api/") && path.ends_with(".proto"))
        && (explicit_binary || (selected_binary == package && paths.contains("src/main.rs"))))
}

#[cfg(test)]
mod tests {
    use super::configured_index;

    #[test]
    fn update_uses_the_cargo_replacement_instead_of_the_original_registry() {
        let project = toml::from_str(
            "[registries.proof_source_replacement_test]\nindex = 'sparse+https://origin.example/'\n[source.proof_source_replacement_test]\nreplace-with = 'mirror'\n",
        )
        .expect("valid project configuration");
        let home = toml::from_str("[source.mirror]\nregistry = 'sparse+https://mirror.example/'\n")
            .expect("valid home configuration");
        assert_eq!(
            configured_index(&[project, home], "proof_source_replacement_test")
                .expect("sparse mirror"),
            Some("sparse+https://mirror.example/".to_owned())
        );
    }

    #[test]
    fn update_reports_an_unsupported_cargo_directory_replacement() {
        let config = toml::from_str(
            "[registries.proof_directory_replacement_test]\nindex = 'sparse+https://origin.example/'\n[source.proof_directory_replacement_test]\nreplace-with = 'vendor'\n[source.vendor]\ndirectory = 'vendor'\n",
        )
        .expect("valid configuration");
        let error = configured_index(&[config], "proof_directory_replacement_test")
            .expect_err("directory source cannot answer sparse update queries");
        assert!(error.contains("not a sparse registry"), "{error}");
    }
}
