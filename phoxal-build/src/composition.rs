//! Exact, immutable prepared composition for local build-script consumption.
use crate::Error;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
const FILE: &str = "composition.json";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Composition {
    document: serde_json::Value,
    products: PathBuf,
    inputs: BTreeMap<PathBuf, String>,
}
fn read(path: &Path) -> Result<Vec<u8>, Error> {
    fs::read(path).map_err(|source| Error::Path {
        path: path.to_owned(),
        source,
    })
}
fn fingerprint(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
/// Publishes a resolved document and immutable copies of its coherent prepared products.
/// Input files are retained solely for freshness checks, never recomposed here.
pub fn publish_composition(
    root: &Path,
    document: &serde_json::Value,
    inputs: &[(PathBuf, Vec<u8>)],
    directories: &[PathBuf],
) -> Result<(), Error> {
    let store = crate::prepared::prepared_input_root(root)?;
    fs::create_dir_all(&store).map_err(|source| Error::Path {
        path: store.clone(),
        source,
    })?;
    let mut products = BTreeMap::new();
    for path in directories {
        // Read through the owner's pair lock, never independently copy potentially mixed files.
        let contract = crate::prepared::read_prepared(path)?;
        let name = path
            .file_name()
            .ok_or_else(|| Error::ApiInput {
                path: path.clone(),
                message: "missing product name".into(),
            })?
            .to_owned();
        products.insert(name, (contract.file, contract.descriptors));
    }
    let inputs = inputs
        .iter()
        .map(|(path, bytes)| {
            if read(path)? != *bytes {
                return Err(input(
                    path,
                    "authored input changed during preparation; retry the command",
                ));
            }
            Ok((path.clone(), fingerprint(bytes)))
        })
        .collect::<Result<BTreeMap<_, _>, Error>>()?;
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(document).map_err(|error| input(root, error))?);
    digest.update(serde_json::to_vec(&inputs).map_err(|error| input(root, error))?);
    for (name, (file, descriptors)) in &products {
        digest.update(name.as_encoded_bytes());
        digest.update(serde_json::to_vec(file).map_err(|error| input(root, error))?);
        digest.update(crate::encode_file_descriptor_set(descriptors));
    }
    let snapshot = store
        .join("compositions")
        .join(format!("{:x}", digest.finalize()));
    for (name, (file, descriptors)) in products {
        let destination = snapshot.join(name);
        if file.instance.is_some() {
            crate::prepared::publish_prepared(&destination, &file, &descriptors)?;
        } else {
            crate::prepared::write_prepared(
                &destination,
                &file.selection,
                file.binary.as_deref(),
                &file.executable,
                file.runtime.clone(),
                &descriptors,
            )?;
        }
    }
    // Empty compositions still need a stable snapshot directory.
    fs::create_dir_all(&snapshot).map_err(|source| Error::Path {
        path: snapshot.clone(),
        source,
    })?;
    let composition = Composition {
        document: document.clone(),
        products: snapshot,
        inputs,
    };
    let bytes = serde_json::to_vec(&composition).map_err(|error| input(root, error))?;
    let path = store.join(FILE);
    if fs::read(&path).ok().as_deref() == Some(&bytes) {
        return Ok(());
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&store).map_err(|source| Error::Path {
        path: store.clone(),
        source,
    })?;
    use std::io::Write;
    temporary.write_all(&bytes).map_err(|source| Error::Path {
        path: path.clone(),
        source,
    })?;
    temporary.persist(&path).map_err(|error| Error::Path {
        path,
        source: error.error,
    })?;
    Ok(())
}
fn input(path: &Path, error: impl std::fmt::Display) -> Error {
    Error::ApiInput {
        path: path.to_owned(),
        message: error.to_string(),
    }
}
pub(crate) fn read_composition(root: &Path) -> Result<(Vec<u8>, PathBuf), Error> {
    let path = crate::prepared::prepared_input_root(root)?.join(FILE);
    println!("cargo:rerun-if-changed={}", path.display());
    let composition: Composition = serde_json::from_slice(&read(&path).map_err(|_| {
        input(
            &path,
            "missing prepared composition; run `cargo phoxal prepare`",
        )
    })?)
    .map_err(|error| input(&path, error))?;
    for (file, expected) in &composition.inputs {
        println!("cargo:rerun-if-changed={}", file.display());
        if fingerprint(&read(file)?) != *expected {
            return Err(input(
                file,
                "authored composition changed; run `cargo phoxal prepare`",
            ));
        }
    }
    Ok((
        serde_json::to_vec(&composition.document).map_err(|error| input(&path, error))?,
        composition.products,
    ))
}
