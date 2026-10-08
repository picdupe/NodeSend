//! Apply the same canonical path boundary to every DAV filesystem operation.
use dav_server::{
    davpath::DavPath,
    fs::{
        DavDirEntry, DavFile, DavFileSystem, DavMetaData, FsError, FsFuture, FsResult, FsStream,
        OpenOptions, ReadDirMeta,
    },
    localfs::LocalFs,
};
use futures_util::StreamExt;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct GuardedFs {
    root: PathBuf,
    inner: Box<LocalFs>,
}
impl GuardedFs {
    pub fn new(root: &Path) -> std::io::Result<Self> {
        let root = root.canonicalize()?;
        Ok(Self {
            inner: LocalFs::new(&root, false, false, false),
            root,
        })
    }
    fn check(&self, path: &DavPath) -> FsResult<()> {
        let relative = path.as_rel_ospath();
        let rel = relative.to_str().ok_or(FsError::Forbidden)?;
        crate::httpapi::security::validate_virtual(rel).map_err(|_| FsError::Forbidden)?;
        let candidate = self.root.join(relative);
        let real = match std::fs::symlink_metadata(&candidate) {
            Ok(_) => candidate.canonicalize().map_err(|_| FsError::Forbidden)?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => candidate
                .parent()
                .ok_or(FsError::Forbidden)?
                .canonicalize()
                .map_err(|_| FsError::NotFound)?,
            Err(_) => return Err(FsError::Forbidden),
        };
        if real.starts_with(&self.root) {
            Ok(())
        } else {
            Err(FsError::Forbidden)
        }
    }
}
impl DavFileSystem for GuardedFs {
    fn open<'a>(
        &'a self,
        path: &'a DavPath,
        options: OpenOptions,
    ) -> FsFuture<'a, Box<dyn DavFile>> {
        Box::pin(async move {
            self.check(path)?;
            self.inner.open(path, options).await
        })
    }
    fn metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        Box::pin(async move {
            self.check(path)?;
            self.inner.metadata(path).await
        })
    }
    fn read_dir<'a>(
        &'a self,
        path: &'a DavPath,
        _: ReadDirMeta,
    ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        Box::pin(async move {
            self.check(path)?;
            let entries = self.inner.read_dir(path, ReadDirMeta::DataSymlink).await?;
            Ok(Box::pin(entries.filter_map(|entry| async move {
                match entry {
                    Ok(item) => match item.metadata().await {
                        Ok(meta) if !meta.is_symlink() => Some(Ok(item)),
                        _ => None,
                    },
                    Err(err) => Some(Err(err)),
                }
            })) as FsStream<Box<dyn DavDirEntry>>)
        })
    }
    fn create_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            self.check(path)?;
            self.inner.create_dir(path).await
        })
    }
    fn remove_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            self.check(path)?;
            self.inner.remove_dir(path).await
        })
    }
    fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            self.check(path)?;
            self.inner.remove_file(path).await
        })
    }
    fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            self.check(from)?;
            self.check(to)?;
            self.inner.rename(from, to).await
        })
    }
    fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            self.check(from)?;
            self.check(to)?;
            self.inner.copy(from, to).await
        })
    }
}
