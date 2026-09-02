//! Participant-facing asset reads.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::bus::{BusHandle, DEFAULT_QUERY_TIMEOUT, Querier};
use crate::identity::ExecutionId;
use crate::model::AssetId;
use crate::supervisor::api as supervisor;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex as AsyncMutex, OnceCell};

use crate::bundle::{ASSETS_DIR, BundleError, BundlePath, BundleRoot, open_bundle_file};

/// Reads the files below `<bundle>/assets`.
///
/// There is no declared asset set to consult: an [`AssetId`] is already a
/// validated relative forward-slash path with no `.` or `..` segment, and
/// The bundle path type validates the joined path again, so a read cannot name
/// anything outside `assets/`. That pair of checks is the whole fence.
#[derive(Clone)]
pub struct ParticipantAssets {
    source: AssetSource,
}
impl std::fmt::Debug for ParticipantAssets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.source {
            AssetSource::Local { root } => formatter
                .debug_struct("ParticipantAssets")
                .field("source", &"local")
                .field("root", &root.path())
                .finish(),
            AssetSource::Remote(_) => formatter
                .debug_struct("ParticipantAssets")
                .field("source", &"supervisor")
                .finish(),
        }
    }
}

#[derive(Clone)]
enum AssetSource {
    Local { root: BundleRoot },
    Remote(Arc<RemoteAssets>),
}

struct RemoteAssets {
    reader: Querier<supervisor::bundle::GetRequest>,
    execution: ExecutionId,
    cache: OnceCell<AssetCache>,
}

/// One private cache retained by the participant runner. Its entries are
/// scoped to their execution before the asset path, so concurrent in-process
/// tests cannot reuse another execution's asset.
struct AssetCache {
    root: tempfile::TempDir,
    gates: Mutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>,
}

impl AssetCache {
    fn new(root: tempfile::TempDir) -> Self {
        Self {
            root,
            gates: Mutex::new(HashMap::new()),
        }
    }

    fn target(&self, execution: ExecutionId, path: &BundlePath) -> PathBuf {
        path.filesystem_path(&self.root.path().join(execution.to_string()))
    }

    fn gate(&self, path: PathBuf) -> Arc<AsyncMutex<()>> {
        self.gates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(path)
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }
}

impl ParticipantAssets {
    pub(crate) const fn new(root: BundleRoot) -> Self {
        Self {
            source: AssetSource::Local { root },
        }
    }

    pub(crate) fn relocate(&mut self, path: std::path::PathBuf) {
        if let AssetSource::Local { root } = &mut self.source {
            root.relocate(path);
        }
    }

    /// Bind supervisor-backed asset materialization to this process.
    ///
    /// Each asset is fetched lazily into one private process-local cache. The
    /// completed file is atomically published and remains available for the
    /// lifetime of this participant process.
    pub(crate) fn from_supervisor(bus: BusHandle) -> Result<Self, BundleError> {
        let reader = Querier::new(
            bus.clone(),
            &supervisor::topics().bundle().get().client(),
            DEFAULT_QUERY_TIMEOUT,
        )
        .map_err(|error| BundleError::Remote {
            detail: error.to_string(),
        })?;
        Ok(Self {
            source: AssetSource::Remote(Arc::new(RemoteAssets {
                reader,
                execution: bus.execution(),
                cache: OnceCell::const_new(),
            })),
        })
    }

    /// Materialize one asset as a stable local file path.
    ///
    /// Local bundles return their validated file directly. Supervisor-backed
    /// assets download lazily into this process-local cache and return the same
    /// path for the participant process lifetime.
    ///
    /// # Errors
    ///
    /// Returns [`BundleError`] when the asset is missing, cannot be read from a
    /// local bundle, or cannot be fetched and completely materialized from the
    /// supervisor.
    pub async fn materialize(&self, id: &AssetId) -> Result<PathBuf, BundleError> {
        match &self.source {
            AssetSource::Local { root } => {
                let (path, _) = Self::open_local_async(root.clone(), id.clone()).await?;
                Ok(path)
            }
            AssetSource::Remote(remote) => remote.materialize(id).await,
        }
    }

    /// Open one materialized asset for native or streaming consumers.
    ///
    /// # Errors
    ///
    /// Returns [`BundleError`] when the asset cannot be materialized or its
    /// local file cannot be opened.
    pub async fn open(&self, id: &AssetId) -> Result<File, BundleError> {
        let (_, file) = match &self.source {
            AssetSource::Local { root } => Self::open_local_async(root.clone(), id.clone()).await?,
            AssetSource::Remote(remote) => remote.open(id).await?,
        };
        Ok(file)
    }

    /// Read one materialized asset into memory.
    ///
    /// # Errors
    ///
    /// Returns [`BundleError`] when the asset is missing, cannot be
    /// materialized, or its local file cannot be read.
    pub async fn read(&self, id: &AssetId) -> Result<Vec<u8>, BundleError> {
        let (path, mut file) = match &self.source {
            AssetSource::Local { root } => Self::open_local_async(root.clone(), id.clone()).await?,
            AssetSource::Remote(remote) => remote.open(id).await?,
        };
        tokio::task::spawn_blocking(move || {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|source| BundleError::ReadFile { path, source })?;
            Ok(bytes)
        })
        .await
        .map_err(|error| BundleError::AssetWorker {
            detail: error.to_string(),
        })?
    }

    pub(crate) fn read_local(&self, id: &AssetId) -> Result<Vec<u8>, BundleError> {
        match &self.source {
            AssetSource::Local { root } => Self::read_from_local(root, id),
            AssetSource::Remote(_) => Err(BundleError::Remote {
                detail: "a supervisor-backed asset must be read asynchronously".to_owned(),
            }),
        }
    }

    fn read_from_local(root: &BundleRoot, id: &AssetId) -> Result<Vec<u8>, BundleError> {
        let path = Self::path(id)?;
        let mut file = open_bundle_file(root, &path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|source| BundleError::ReadFile {
                path: path.filesystem_path(root.path()),
                source,
            })?;
        Ok(bytes)
    }

    async fn open_local_async(
        root: BundleRoot,
        id: AssetId,
    ) -> Result<(PathBuf, File), BundleError> {
        tokio::task::spawn_blocking(move || {
            let path = Self::path(&id)?;
            let filesystem_path = path.filesystem_path(root.path());
            let file = open_bundle_file(&root, &path)?;
            Ok((filesystem_path, file))
        })
        .await
        .map_err(|error| BundleError::AssetWorker {
            detail: error.to_string(),
        })?
    }

    /// Where one logical asset sits in the bundle.
    pub(crate) fn path(id: &AssetId) -> Result<BundlePath, BundleError> {
        Ok(BundlePath::new(format!("{ASSETS_DIR}/{}", id.as_str()))?)
    }
}

impl RemoteAssets {
    async fn materialize(&self, id: &AssetId) -> Result<PathBuf, BundleError> {
        let path = ParticipantAssets::path(id)?;
        let cache = self.cache().await?;
        let target = cache.target(self.execution, &path);
        if cached_file(&target).await? {
            return Ok(target);
        }

        let gate = cache.gate(target.clone());
        let _materialization = gate.lock().await;
        if cached_file(&target).await? {
            return Ok(target);
        }

        self.download_to(&path, &target).await?;
        Ok(target)
    }

    /// Lazily create the runner-owned cache. Every clone of this remote asset
    /// resolver shares the same `RemoteAssets`, so the root survives setup and
    /// shutdown but is removed when the runner releases its final clone.
    async fn cache(&self) -> Result<&AssetCache, BundleError> {
        self.cache
            .get_or_try_init(|| async {
                let root = tokio::task::spawn_blocking(tempfile::tempdir)
                    .await
                    .map_err(|error| BundleError::AssetWorker {
                        detail: error.to_string(),
                    })?
                    .map_err(|source| BundleError::Cache {
                        path: std::env::temp_dir(),
                        source,
                    })?;
                Ok(AssetCache::new(root))
            })
            .await
    }

    async fn open(&self, id: &AssetId) -> Result<(PathBuf, File), BundleError> {
        let path = self.materialize(id).await?;
        let opened_path = path.clone();
        let file = tokio::task::spawn_blocking(move || {
            File::open(&opened_path).map_err(|source| BundleError::ReadFile {
                path: opened_path,
                source,
            })
        })
        .await
        .map_err(|error| BundleError::AssetWorker {
            detail: error.to_string(),
        })??;
        Ok((path, file))
    }

    /// Fetch bounded supervisor ranges into a temporary sibling and atomically
    /// publish the completed cache file.
    async fn download_to(&self, path: &BundlePath, target: &Path) -> Result<(), BundleError> {
        let parent = target.parent().ok_or_else(|| BundleError::Cache {
            path: target.to_path_buf(),
            source: std::io::Error::other("an asset cache target has no parent directory"),
        })?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| BundleError::Cache {
                path: parent.to_path_buf(),
                source,
            })?;
        let temporary_parent = parent.to_path_buf();
        let temporary = tokio::task::spawn_blocking(move || {
            tempfile::NamedTempFile::new_in(temporary_parent).map(|file| file.into_temp_path())
        })
        .await
        .map_err(|error| BundleError::AssetWorker {
            detail: error.to_string(),
        })?
        .map_err(|source| BundleError::Cache {
            path: parent.to_path_buf(),
            source,
        })?;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&temporary)
            .await
            .map_err(|source| BundleError::Cache {
                path: temporary.to_path_buf(),
                source,
            })?;
        let mut offset = 0_u64;
        loop {
            let response = self
                .reader
                .query(supervisor::bundle::GetRequest {
                    path: path.clone(),
                    offset,
                })
                .await
                .map_err(|error| BundleError::Remote {
                    detail: error.to_string(),
                })?;
            match response {
                supervisor::bundle::GetResponse::Chunk { bytes: chunk, eof } => {
                    if !eof && chunk.is_empty() {
                        return Err(BundleError::Remote {
                            detail: format!(
                                "supervisor returned a non-final empty chunk for {} at offset {offset}",
                                path
                            ),
                        });
                    }
                    offset = offset.checked_add(chunk.len() as u64).ok_or_else(|| {
                        BundleError::Remote {
                            detail: format!("asset offset overflow while reading {path}"),
                        }
                    })?;
                    file.write_all(&chunk)
                        .await
                        .map_err(|source| BundleError::Cache {
                            path: temporary.to_path_buf(),
                            source,
                        })?;
                    if eof {
                        break;
                    }
                }
                supervisor::bundle::GetResponse::Missing => {
                    return Err(BundleError::MissingFile {
                        path: PathBuf::from(path.as_str()),
                    });
                }
                supervisor::bundle::GetResponse::InvalidPath => {
                    return Err(BundleError::Remote {
                        detail: format!("supervisor rejected invalid asset path {path}"),
                    });
                }
                supervisor::bundle::GetResponse::Refused => {
                    return Err(BundleError::Remote {
                        detail: format!("supervisor refused asset path {path}"),
                    });
                }
            }
        }
        file.sync_all().await.map_err(|source| BundleError::Cache {
            path: temporary.to_path_buf(),
            source,
        })?;
        drop(file);
        let published_target = target.to_path_buf();
        tokio::task::spawn_blocking(move || {
            temporary
                .persist(&published_target)
                .map_err(|error| error.error)
        })
        .await
        .map_err(|error| BundleError::AssetWorker {
            detail: error.to_string(),
        })?
        .map_err(|source| BundleError::Cache {
            path: target.to_path_buf(),
            source,
        })?;
        Ok(())
    }
}

async fn cached_file(path: &Path) -> Result<bool, BundleError> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(BundleError::Cache {
            path: path.to_path_buf(),
            source: std::io::Error::other("asset cache path is not a regular file"),
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(BundleError::Cache {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::bus::{BusConfig, BusOwner, Codec, MessagePack, SourceLabel};
    use crate::identity::ExecutionId;

    use super::*;

    /// Concurrent first use atomically materializes every supervisor-sized
    /// range once; later reads and native opens reuse that stable local file.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remote_assets_materialize_once_for_concurrent_reads_and_opens() {
        let (owner, bus) = BusOwner::open(BusConfig::for_external(
            ExecutionId::mint(),
            Some(SourceLabel::new("asset-test").expect("a valid label")),
            Vec::new(),
        ))
        .await
        .expect("the in-process bus opens");
        let server = bus
            .declare_server(supervisor::topics().bundle().get().owner().key())
            .await
            .expect("the bundle server declares");
        let requests = Arc::new(AtomicUsize::new(0));
        let server_bus = bus.clone();
        let server_requests = Arc::clone(&requests);
        let server_task = tokio::spawn(async move {
            for (offset, bytes, eof) in
                [(0, b"abc".as_slice(), false), (3, b"def".as_slice(), true)]
            {
                let incoming = server.recv().await.expect("the client queries the server");
                let request = MessagePack::decode::<supervisor::bundle::GetRequest>(
                    &incoming.request_bytes().expect("the request has bytes"),
                )
                .expect("the request decodes");
                assert_eq!(request.path.as_str(), "assets/mesh.bin");
                assert_eq!(request.offset, offset);
                server_requests.fetch_add(1, Ordering::Relaxed);
                incoming
                    .reply(
                        &server_bus,
                        MessagePack::encode(&supervisor::bundle::GetResponse::Chunk {
                            bytes: bytes.to_vec(),
                            eof,
                        })
                        .expect("the response encodes"),
                    )
                    .await
                    .expect("the response reaches the client");
            }
        });

        let assets = ParticipantAssets::from_supervisor(bus.clone())
            .expect("the participant reader binds to the supervisor");
        let cloned_assets = assets.clone();
        let runner_assets = assets.clone();
        let id = AssetId::new("mesh.bin").expect("a valid asset id");
        let (first_path, second_path) =
            tokio::join!(assets.materialize(&id), cloned_assets.materialize(&id),);
        let materialized = first_path.expect("the first materialization succeeds");
        assert_eq!(
            materialized,
            second_path.expect("the concurrent materialization reuses the same file")
        );
        assert_eq!(requests.load(Ordering::Relaxed), 2);
        assert_eq!(
            std::fs::read(&materialized).expect("the cached file reads"),
            b"abcdef"
        );
        let (first_read, second_read) = tokio::join!(assets.read(&id), cloned_assets.read(&id));
        assert_eq!(
            first_read.expect("the original reader uses the cache"),
            b"abcdef"
        );
        assert_eq!(second_read.expect("the clone uses the cache"), b"abcdef");
        let mut opened = cloned_assets
            .open(&id)
            .await
            .expect("a native consumer opens the cached file");
        let mut opened_bytes = Vec::new();
        opened
            .read_to_end(&mut opened_bytes)
            .expect("the opened cached file reads");
        assert_eq!(opened_bytes, b"abcdef");
        server_task.await.expect("the server completes");
        assert_eq!(
            requests.load(Ordering::Relaxed),
            2,
            "one shared materialization serves every later read and open"
        );
        drop(opened);
        drop(assets);
        drop(cloned_assets);
        assert_eq!(
            std::fs::read(&materialized)
                .expect("the runner cache outlives the participant asset facade"),
            b"abcdef"
        );
        drop(runner_assets);
        assert!(
            !materialized.exists(),
            "the temporary cache must be removed after the participant runner releases it"
        );

        owner.close().await;
    }

    /// A supervisor response that claims the file continues but makes no
    /// progress cannot cause an unbounded retry loop in a participant.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remote_assets_refuse_a_nonfinal_empty_range() {
        let (owner, bus) = BusOwner::open(BusConfig::for_external(
            ExecutionId::mint(),
            Some(SourceLabel::new("asset-test").expect("a valid label")),
            Vec::new(),
        ))
        .await
        .expect("the in-process bus opens");
        let server = bus
            .declare_server(supervisor::topics().bundle().get().owner().key())
            .await
            .expect("the bundle server declares");
        let server_bus = bus.clone();
        let server_task = tokio::spawn(async move {
            let incoming = server.recv().await.expect("the client queries the server");
            incoming
                .reply(
                    &server_bus,
                    MessagePack::encode(&supervisor::bundle::GetResponse::Chunk {
                        bytes: Vec::new(),
                        eof: false,
                    })
                    .expect("the response encodes"),
                )
                .await
                .expect("the response reaches the client");
        });

        let assets = ParticipantAssets::from_supervisor(bus.clone())
            .expect("the participant reader binds to the supervisor");
        let id = AssetId::new("mesh.bin").expect("a valid asset id");
        let error = assets
            .read(&id)
            .await
            .expect_err("a non-final empty range is invalid");
        assert!(
            error.to_string().contains("non-final empty chunk"),
            "{error}"
        );
        server_task.await.expect("the server completes");
        owner.close().await;
    }

    /// A failed partial transfer never publishes a cache file, so a later read
    /// starts again from byte zero rather than returning truncated data.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remote_assets_do_not_publish_an_interrupted_download() {
        let (owner, bus) = BusOwner::open(BusConfig::for_external(
            ExecutionId::mint(),
            Some(SourceLabel::new("asset-test").expect("a valid label")),
            Vec::new(),
        ))
        .await
        .expect("the in-process bus opens");
        let server = bus
            .declare_server(supervisor::topics().bundle().get().owner().key())
            .await
            .expect("the bundle server declares");
        let server_bus = bus.clone();
        let server_task = tokio::spawn(async move {
            for (offset, response) in [
                (
                    0,
                    supervisor::bundle::GetResponse::Chunk {
                        bytes: b"partial".to_vec(),
                        eof: false,
                    },
                ),
                (7, supervisor::bundle::GetResponse::Missing),
                (
                    0,
                    supervisor::bundle::GetResponse::Chunk {
                        bytes: b"complete".to_vec(),
                        eof: true,
                    },
                ),
            ] {
                let incoming = server.recv().await.expect("the client queries the server");
                let request = MessagePack::decode::<supervisor::bundle::GetRequest>(
                    &incoming.request_bytes().expect("the request has bytes"),
                )
                .expect("the request decodes");
                assert_eq!(request.offset, offset);
                incoming
                    .reply(
                        &server_bus,
                        MessagePack::encode(&response).expect("the response encodes"),
                    )
                    .await
                    .expect("the response reaches the client");
            }
        });

        let assets = ParticipantAssets::from_supervisor(bus.clone())
            .expect("the participant reader binds to the supervisor");
        let id = AssetId::new("mesh.bin").expect("a valid asset id");
        assert!(matches!(
            assets.read(&id).await,
            Err(BundleError::MissingFile { .. })
        ));
        assert_eq!(
            assets
                .read(&id)
                .await
                .expect("the second download completes"),
            b"complete"
        );
        server_task.await.expect("the server completes");
        owner.close().await;
    }
}
