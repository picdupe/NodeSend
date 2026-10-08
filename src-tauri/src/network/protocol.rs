//! Versioned, length-bounded NodeSend native protocol; no HTTP dependency.
use crate::error::{AppError, AppResult};
use crate::transfer::model::Manifest;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
pub const VERSION: u16 = 1;
pub const MAX_FRAME: usize = 8 * 1024 * 1024;
pub const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    SkipFile { id: String, file: usize },
    Addresses,
    Hello {
        version: u16,
        node_id: String,
        display_name: String,
    },
    Offer {
        id: String,
        manifest: Manifest,
    },
    Poll {
        id: String,
    },
    Chunk {
        id: String,
        index: usize,
        length: usize,
        sha256: Option<String>,
    },
    Commit {
        id: String,
    },
    Cancel {
        id: String,
    },
    Browse {
        share_id: Option<String>,
        path: String,
        token: Option<String>,
    },
    Fetch {
        share_id: String,
        path: String,
        token: Option<String>,
    },
    Put {
        share_id: String,
        path: String,
        length: u64,
        token: Option<String>,
    },
}
#[derive(Debug, Serialize, Deserialize)]
pub struct NativeEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Addresses { addresses: Vec<String>, tcp_port: u16, quic_port: u16 },
    Hello {
        version: u16,
        node_id: String,
        display_name: String,
        tcp_port: u16,
        quic_port: u16,
    },
    State {
        status: String,
        completed: Vec<usize>,
    },
    Ack,
    Listing {
        entries: Vec<NativeEntry>,
    },
    File {
        length: u64,
    },
    Ready,
    Error {
        message: String,
    },
}
pub async fn write_frame<W: AsyncWrite + Unpin + ?Sized, T: Serialize>(
    w: &mut W,
    value: &T,
) -> AppResult<()> {
    let body = serde_json::to_vec(value)?;
    if body.len() > MAX_FRAME {
        return Err(AppError::BadRequest("协议消息过大".into()));
    }
    tokio::time::timeout(IO_TIMEOUT, async {
        w.write_u32(body.len() as u32).await?;
        w.write_all(&body).await?;
        w.flush().await
    })
    .await
    .map_err(|_| AppError::Other("网络写入超时".into()))??;
    Ok(())
}
pub async fn read_frame<R: AsyncRead + Unpin + ?Sized, T: DeserializeOwned>(
    r: &mut R,
) -> AppResult<T> {
    read_frame_with_timeout(r, IO_TIMEOUT).await
}
pub async fn read_frame_with_timeout<R: AsyncRead + Unpin + ?Sized, T: DeserializeOwned>(
    r: &mut R, timeout: std::time::Duration,
) -> AppResult<T> {
    tokio::time::timeout(timeout, async {
        let length = r.read_u32().await? as usize;
        if length == 0 || length > MAX_FRAME {
            return Err(AppError::BadRequest("协议帧长度非法".into()));
        }
        let mut buf = vec![0; length];
        r.read_exact(&mut buf).await?;
        Ok(serde_json::from_slice(&buf)?)
    })
    .await
    .map_err(|_| AppError::Other("网络读取超时".into()))?
}
