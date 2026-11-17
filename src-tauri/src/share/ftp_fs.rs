use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use unftp_core::{
    auth::{Principal, UserDetail, UserDetailError, UserDetailProvider},
    storage::{Error, ErrorKind, Fileinfo, Result, StorageBackend},
};

const PRINCIPAL_PREFIX: &str = "nodesend-share-ids:";

#[derive(Debug, Clone)]
pub struct FtpUser {
    pub allowed_share_ids: HashSet<String>,
}

impl UserDetail for FtpUser {}

impl fmt::Display for FtpUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NodeSend FTP user")
    }
}

#[derive(Debug)]
pub struct FtpUserDetailProvider;

#[async_trait]
impl UserDetailProvider for FtpUserDetailProvider {
    type User = FtpUser;

    async fn provide_user_detail(
        &self,
        principal: &Principal,
    ) -> std::result::Result<FtpUser, UserDetailError> {
        let ids = principal
            .username
            .strip_prefix(PRINCIPAL_PREFIX)
            .ok_or_else(|| UserDetailError::new("invalid FTP identity"))?;
        let allowed_share_ids = ids
            .split(',')
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect();
        Ok(FtpUser { allowed_share_ids })
    }
}

#[derive(Clone, Debug)]
pub struct FtpShare {
    pub id: String,
    pub name: String,
    pub filesystem: unftp_sbe_fs::Filesystem,
}

pub type FtpShareMap = Arc<RwLock<HashMap<String, FtpShare>>>;

#[derive(Debug, Clone)]
pub struct FtpMultiplexFs {
    shares: FtpShareMap,
}

impl FtpMultiplexFs {
    pub fn new(shares: FtpShareMap) -> Self {
        Self { shares }
    }

    fn route<'a>(
        &self,
        user: &FtpUser,
        path: &'a Path,
    ) -> Result<Option<(FtpShare, PathBuf)>> {
        let raw = path.to_string_lossy();
        let raw = raw.trim_start_matches(|character| character == '/' || character == '\\');
        if raw.is_empty() {
            return Ok(None);
        }
        let (name, tail) = match raw.find(|character| character == '/' || character == '\\') {
            Some(index) => (&raw[..index], &raw[index + 1..]),
            None => (raw, ""),
        };
        let share = self
            .shares
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .find(|share| {
                share.name.eq_ignore_ascii_case(name)
                    && user.allowed_share_ids.contains(&share.id)
            })
            .cloned()
            .ok_or_else(not_found)?;
        let inner = if tail.is_empty() {
            PathBuf::from("/")
        } else {
            PathBuf::from(tail)
        };
        Ok(Some((share, inner)))
    }

}

fn not_found() -> Error {
    Error::new(
        ErrorKind::PermanentFileNotAvailable,
        std::io::Error::new(std::io::ErrorKind::NotFound, "FTP share path not found"),
    )
}

fn permission_denied() -> Error {
    Error::new(
        ErrorKind::PermissionDenied,
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "FTP share access denied"),
    )
}

#[async_trait]
impl StorageBackend<FtpUser> for FtpMultiplexFs {
    type Metadata = unftp_sbe_fs::Meta;

    fn supported_features(&self) -> u32 {
        unftp_core::storage::FEATURE_RESTART | unftp_core::storage::FEATURE_SITEMD5
    }

    async fn metadata<P: AsRef<Path> + Send + fmt::Debug>(
        &self,
        user: &FtpUser,
        path: P,
    ) -> Result<Self::Metadata> {
        let path = path.as_ref();
        if path == Path::new("/") || path == Path::new("\\") || path.as_os_str().is_empty() {
            return Err(not_found());
        }
        let (share, inner) = self.route(user, path)?.ok_or_else(not_found)?;
        share.filesystem.metadata(user, inner).await
    }

    async fn list<P: AsRef<Path> + Send + fmt::Debug>(
        &self,
        user: &FtpUser,
        path: P,
    ) -> Result<Vec<Fileinfo<PathBuf, Self::Metadata>>> {
        let path = path.as_ref();
        if path == Path::new("/") || path == Path::new("\\") || path.as_os_str().is_empty() {
            return Err(not_found());
        }
        let (share, inner) = self.route(user, path)?.ok_or_else(not_found)?;
        share.filesystem.list(user, inner).await
    }

    async fn get<P: AsRef<Path> + Send + fmt::Debug>(
        &self,
        user: &FtpUser,
        path: P,
        start_pos: u64,
    ) -> Result<Box<dyn tokio::io::AsyncRead + Send + Sync + Unpin>> {
        let (share, inner) = self.route(user, path.as_ref())?.ok_or_else(not_found)?;
        share.filesystem.get(user, inner, start_pos).await
    }

    async fn put<P, R>(
        &self,
        user: &FtpUser,
        input: R,
        path: P,
        start_pos: u64,
    ) -> Result<u64>
    where
        P: AsRef<Path> + Send + fmt::Debug,
        R: tokio::io::AsyncRead + Send + Sync + Unpin + 'static,
    {
        let (share, inner) = self.route(user, path.as_ref())?.ok_or_else(not_found)?;
        share.filesystem.put(user, input, inner, start_pos).await
    }

    async fn del<P: AsRef<Path> + Send + fmt::Debug>(
        &self,
        user: &FtpUser,
        path: P,
    ) -> Result<()> {
        let (share, inner) = self.route(user, path.as_ref())?.ok_or_else(not_found)?;
        share.filesystem.del(user, inner).await
    }

    async fn mkd<P: AsRef<Path> + Send + fmt::Debug>(
        &self,
        user: &FtpUser,
        path: P,
    ) -> Result<()> {
        let (share, inner) = self.route(user, path.as_ref())?.ok_or_else(not_found)?;
        share.filesystem.mkd(user, inner).await
    }

    async fn rename<P: AsRef<Path> + Send + fmt::Debug>(
        &self,
        user: &FtpUser,
        from: P,
        to: P,
    ) -> Result<()> {
        let (from_share, from_path) = self.route(user, from.as_ref())?.ok_or_else(not_found)?;
        let (to_share, to_path) = self.route(user, to.as_ref())?.ok_or_else(not_found)?;
        if from_share.id != to_share.id {
            return Err(permission_denied());
        }
        from_share.filesystem.rename(user, from_path, to_path).await
    }

    async fn rmd<P: AsRef<Path> + Send + fmt::Debug>(
        &self,
        user: &FtpUser,
        path: P,
    ) -> Result<()> {
        let (share, inner) = self.route(user, path.as_ref())?.ok_or_else(not_found)?;
        share.filesystem.rmd(user, inner).await
    }

    async fn cwd<P: AsRef<Path> + Send + fmt::Debug>(
        &self,
        user: &FtpUser,
        path: P,
    ) -> Result<()> {
        let path = path.as_ref();
        if path == Path::new("/") || path == Path::new("\\") || path.as_os_str().is_empty() {
            return Err(not_found());
        }
        let (share, inner) = self.route(user, path)?.ok_or_else(not_found)?;
        share.filesystem.cwd(user, inner).await
    }
}

pub fn principal_name(share_ids: impl IntoIterator<Item = String>) -> String {
    format!(
        "{PRINCIPAL_PREFIX}{}",
        share_ids.into_iter().collect::<Vec<_>>().join(",")
    )
}
