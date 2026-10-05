//! Ordinary Cargo warm builds observe only existing configuration locations.
use std::{fs, path::Path, process::Command};

fn cargo(root: &Path, arguments: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    cargo_at_home(root, arguments, None)
}

fn cargo_at_home(
    root: &Path,
    arguments: &[&str],
    home: Option<&Path>,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut command = Command::new(env!("CARGO"));
    if let Some(home) = home {
        command.env("CARGO_HOME", home);
    }
    let output = command
        .current_dir(root)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .args(arguments)
        .args(["--offline", "--target-dir"])
        .arg(root.join("compiler-output"))
        .output()?;
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stderr)?)
}

fn assert_fresh(output: String, package: &str) {
    assert!(
        output.contains(&format!("Fresh {package}")),
        "expected unchanged {package} to stay Fresh; Cargo -vv output:\n{output}"
    );
}

#[test]
fn existing_config_changes_keep_unchanged_builds_fresh_and_recovery_is_package_scoped()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    scaffold(&root, "phoxal-config-watch-proof")?;
    fs::create_dir(root.join(".cargo"))?;
    let count = || -> Result<u32, Box<dyn std::error::Error>> {
        Ok(fs::read_to_string(root.join("build-count"))?.parse()?)
    };
    let build = || cargo(&root, &["build", "-vv"]);
    build()?;
    assert_eq!(count()?, 1);
    assert_fresh(build()?, "phoxal-config-watch-proof");
    assert_fresh(build()?, "phoxal-config-watch-proof");
    assert_eq!(count()?, 1);
    for (filename, value, expected) in [
        ("config.toml", "inputs-first", "inputs-first"),
        ("config.toml", "inputs-edited", "inputs-edited"),
        ("config", "inputs-legacy", "inputs-legacy"),
        ("config.toml", "inputs-shadowed", "inputs-legacy"),
    ] {
        let before = count()?;
        fs::write(
            root.join(".cargo").join(filename),
            format!("[build]\ntarget-dir={value:?}\n"),
        )?;
        build()?;
        assert_eq!(count()?, before + 1);
        assert!(
            fs::read_to_string(root.join("observed-root"))?
                .starts_with(root.join(expected).to_str().ok_or("non-UTF8 root")?)
        );
        assert_fresh(build()?, "phoxal-config-watch-proof");
        assert_eq!(count()?, before + 1);
    }
    fs::remove_file(root.join(".cargo/config"))?;
    build()?;
    assert!(
        fs::read_to_string(root.join("observed-root"))?
            .contains("inputs-shadowed/phoxal/prepared/")
    );
    fs::remove_file(root.join(".cargo/config.toml"))?;
    fs::remove_dir(root.join(".cargo"))?;
    build()?; // Removal removes the directory watch from the next fingerprint.
    let before = count()?;
    assert_fresh(build()?, "phoxal-config-watch-proof");
    fs::create_dir(root.join(".cargo"))?;
    fs::write(
        root.join(".cargo/config.toml"),
        "[build]\ntarget-dir='inputs-recovered'\n",
    )?;
    assert_fresh(build()?, "phoxal-config-watch-proof");
    assert_eq!(count()?, before); // The explicitly accepted absent-directory edge.
    // Preparation owns publication at the newly resolved input location.
    fs::create_dir_all(phoxal_build::prepared_input_root(&root)?)?;
    cargo(&root, &["clean", "-p", "phoxal-config-watch-proof"])?;
    build()?;
    assert!(
        fs::read_to_string(root.join("observed-root"))?
            .contains("inputs-recovered/phoxal/prepared/")
    );
    let recovered = count()?;
    assert_fresh(build()?, "phoxal-config-watch-proof");
    assert_eq!(count()?, recovered);
    Ok(())
}

fn scaffold(root: &Path, package: &str) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\nname='{package}'\nversion='0.1.0'\nedition='2024'\n[build-dependencies]\nphoxal-build={{path={:?}}}\n",
            env!("CARGO_MANIFEST_DIR")
        ),
    )?;
    fs::write(
        root.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot: { id: watch-proof }\n",
    )?;
    fs::write(root.join("src/main.rs"), "fn main() {}\n")?;
    fs::write(
        root.join("build.rs"),
        r#"fn main() {
        phoxal_build::api(Default::default()).unwrap();
        let root=std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let count=root.join("build-count");
        let previous=std::fs::read_to_string(&count).ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
        std::fs::write(count,(previous+1).to_string()).unwrap();
        std::fs::write(root.join("observed-root"), phoxal_build::prepared_input_root(&root).unwrap().to_string_lossy().as_bytes()).unwrap();
    }"#,
    )?;
    Ok(())
}

// Copy only fixture lockfile dependencies into a disposable home. The real
// registry is read-only; no symlink points back to mutable shared caches.
fn seed_offline_home(root: &Path, destination: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let source = cargo_config2::cargo_home_with_cwd(root).ok_or("no Cargo home")?;
    let registry = fs::read_dir(source.join("registry/index"))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("index.crates.io-")
        })
        .ok_or("no cached crates.io registry")?;
    let registry_name = registry.file_name();
    let index = destination.join("registry/index").join(&registry_name);
    let cache = destination.join("registry/cache").join(&registry_name);
    fs::create_dir_all(&index)?;
    fs::create_dir_all(&cache)?;
    fs::copy(
        registry.path().join("config.json"),
        index.join("config.json"),
    )?;
    let lock = fs::read_to_string(root.join("Cargo.lock"))?;
    for package in lock.split("[[package]]").skip(1) {
        if !package.contains("registry+https://github.com/rust-lang/crates.io-index") {
            continue;
        }
        let field = |key: &str| -> Result<&str, Box<dyn std::error::Error>> {
            Ok(package
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .ok_or("missing lockfile field")?
                .trim_matches('"'))
        };
        let name = field("name = ")?;
        let version = field("version = ")?;
        let archive = format!("{name}-{version}.crate");
        fs::copy(
            source
                .join("registry/cache")
                .join(&registry_name)
                .join(&archive),
            cache.join(archive),
        )?;
        let relative = match name.len() {
            1 => format!("1/{name}"),
            2 => format!("2/{name}"),
            3 => format!("3/{}/{name}", &name[..1]),
            _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
        };
        let file = index.join(".cache").join(&relative);
        fs::create_dir_all(file.parent().ok_or("index entry has no parent")?)?;
        fs::copy(registry.path().join(".cache").join(relative), file)?;
    }
    Ok(())
}

#[test]
fn global_config_files_track_edits_deletion_and_recovery_without_watching_caches()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?.join("robot");
    fs::create_dir(&root)?;
    scaffold(&root, "phoxal-global-config-proof")?;
    cargo(&root, &["build"])?;
    let home = temporary.path().join("isolated-cargo-home");
    seed_offline_home(&root, &home)?;
    let configure = |file: &str, selected: &str| -> Result<(), Box<dyn std::error::Error>> {
        fs::write(
            home.join(file),
            format!("[build]\ntarget-dir={:?}\n", root.join(selected)),
        )?;
        Ok(())
    };
    let build = || cargo_at_home(&root, &["build", "-vv"], Some(&home));
    let count = || -> Result<u32, Box<dyn std::error::Error>> {
        Ok(fs::read_to_string(root.join("build-count"))?.parse()?)
    };
    let observed = |selected: &str| -> Result<(), Box<dyn std::error::Error>> {
        assert!(
            fs::read_to_string(root.join("observed-root"))?
                .starts_with(root.join(selected).to_str().ok_or("non-UTF8 root")?)
        );
        Ok(())
    };
    configure("config.toml", "global-initial")?;
    build()?;
    observed("global-initial")?;
    let before = count()?;
    assert_fresh(build()?, "phoxal-global-config-proof");
    fs::write(home.join("registry/cache-unrelated"), "cache changed")?;
    assert_fresh(build()?, "phoxal-global-config-proof");
    assert_eq!(count()?, before);
    configure("config.toml", "global-edited")?;
    build()?;
    observed("global-edited")?;
    configure("config", "global-legacy")?;
    assert_fresh(build()?, "phoxal-global-config-proof");
    observed("global-edited")?; // Accepted, explicitly documented new-file exception.
    cargo_at_home(
        &root,
        &["clean", "-p", "phoxal-global-config-proof"],
        Some(&home),
    )?;
    build()?;
    observed("global-legacy")?;
    assert_fresh(build()?, "phoxal-global-config-proof");
    configure("config", "global-legacy-edited")?;
    build()?;
    observed("global-legacy-edited")?;
    fs::remove_file(home.join("config"))?;
    build()?;
    observed("global-edited")?;
    fs::remove_file(home.join("config.toml"))?;
    build()?;
    observed("target")?;
    assert_fresh(build()?, "phoxal-global-config-proof");
    Ok(())
}
