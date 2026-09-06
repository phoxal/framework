use super::*;

pub(super) async fn serve_bundle(bus: BusHandle, root: PathBuf) -> Result<()> {
    let server = declare(&bus, &supervisor::topics().bundle().get().owner()).await?;
    loop {
        let incoming = server.recv().await?;
        let request: supervisor::bundle::GetRequest = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        let entry_root = root.clone();
        let response = tokio::task::spawn_blocking(move || bundle_entry(&entry_root, &request))
            .await
            .context("the supervisor bundle reader worker stopped")?;
        reply(&incoming, &bus, &response).await?;
    }
}

/// Resolve one requested path against the bundle root.
///
/// The path has already passed the wire `BundlePath` parser, but a normalized
/// spelling can still escape through a symlink. Both sides are canonicalized,
/// and only regular files under the canonical root are eligible. The static
/// model and staged executables have dedicated contracts, so only immutable
/// assets are exposed through this reader.
pub(super) fn bundle_entry(
    root: &Path,
    request: &supervisor::bundle::GetRequest,
) -> supervisor::bundle::GetResponse {
    let canonical_root = match root.canonicalize() {
        Ok(root) => root,
        Err(_) => return supervisor::bundle::GetResponse::Refused,
    };
    let candidate = canonical_root.join(request.path.as_str().split('/').collect::<PathBuf>());
    let resolved = match candidate.canonicalize() {
        Ok(path) => path,
        Err(error) => {
            return classify_bundle_candidate_error(&canonical_root, request, &error);
        }
    };
    if !resolved.starts_with(&canonical_root) {
        return supervisor::bundle::GetResponse::InvalidPath;
    }
    if !resolved.is_file() {
        return supervisor::bundle::GetResponse::InvalidPath;
    }
    if !is_served_bundle_asset(&request.path) {
        return supervisor::bundle::GetResponse::Refused;
    }
    read_chunk(&resolved, request.offset)
}

/// Classify failure to resolve the requested entry without treating an existing
/// dangling path as if the bundle did not contain it.
fn classify_bundle_candidate_error(
    root: &Path,
    request: &supervisor::bundle::GetRequest,
    error: &std::io::Error,
) -> supervisor::bundle::GetResponse {
    match error.kind() {
        std::io::ErrorKind::NotFound => match requested_path_status(root, request) {
            Ok(RequestedPathStatus::Invalid) => supervisor::bundle::GetResponse::InvalidPath,
            Ok(RequestedPathStatus::Missing) => supervisor::bundle::GetResponse::Missing,
            Err(_) => supervisor::bundle::GetResponse::Refused,
        },
        std::io::ErrorKind::NotADirectory => supervisor::bundle::GetResponse::InvalidPath,
        _ if is_symlink_loop(error) => supervisor::bundle::GetResponse::InvalidPath,
        _ => supervisor::bundle::GetResponse::Refused,
    }
}

/// How a requested path failed to resolve below an otherwise canonical root.
enum RequestedPathStatus {
    /// A normal component is absent from the bundle.
    Missing,
    /// An existing link cannot produce an admissible path under the bundle root.
    Invalid,
}

/// Inspect every existing component to distinguish absence from a broken or
/// escaping symlink before a later component produces `NotFound`.
fn requested_path_status(
    root: &Path,
    request: &supervisor::bundle::GetRequest,
) -> std::io::Result<RequestedPathStatus> {
    let mut candidate = root.to_path_buf();
    for segment in request.path.as_str().split('/') {
        candidate.push(segment);
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => match candidate.canonicalize() {
                Ok(resolved) if resolved.starts_with(root) => {}
                Ok(_) => return Ok(RequestedPathStatus::Invalid),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) || is_symlink_loop(&error) =>
                {
                    return Ok(RequestedPathStatus::Invalid);
                }
                Err(error) => return Err(error),
            },
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RequestedPathStatus::Missing);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(RequestedPathStatus::Invalid)
}

/// `ErrorKind::FilesystemLoop` is still unstable, so Unix loop evidence stays
/// at the portable OS-error boundary until that standard-library variant is
/// available.
fn is_symlink_loop(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ELOOP)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

/// Preserve the distinction between an absent entry, an invalid resolved path,
/// and a bundle entry the supervisor could not serve.
pub(super) fn classify_bundle_path_error(
    error: &std::io::Error,
) -> supervisor::bundle::GetResponse {
    match error.kind() {
        std::io::ErrorKind::NotFound => supervisor::bundle::GetResponse::Missing,
        std::io::ErrorKind::NotADirectory => supervisor::bundle::GetResponse::InvalidPath,
        _ => supervisor::bundle::GetResponse::Refused,
    }
}

fn is_served_bundle_asset(path: &BundlePath) -> bool {
    path.as_str().starts_with("assets/")
}

fn read_chunk(path: &Path, offset: u64) -> supervisor::bundle::GetResponse {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => return classify_bundle_path_error(&error),
    };
    let length = match file.metadata().map(|metadata| metadata.len()) {
        Ok(length) => length,
        Err(_) => return supervisor::bundle::GetResponse::Refused,
    };
    if offset >= length {
        return supervisor::bundle::GetResponse::Chunk {
            bytes: Vec::new(),
            eof: true,
        };
    }
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return supervisor::bundle::GetResponse::Refused;
    }
    let remaining = usize::try_from(length.saturating_sub(offset)).unwrap_or(usize::MAX);
    let mut bytes = vec![0; remaining.min(MAX_BUNDLE_CHUNK_BYTES)];
    let read = match file.read(&mut bytes) {
        Ok(read) => read,
        Err(_) => return supervisor::bundle::GetResponse::Refused,
    };
    bytes.truncate(read);
    supervisor::bundle::GetResponse::Chunk {
        eof: read == 0 || offset.saturating_add(read as u64) >= length,
        bytes,
    }
}
