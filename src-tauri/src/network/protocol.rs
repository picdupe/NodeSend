//! Versioned, length-bounded NodeSend native protocol; no HTTP dependency.
use crate::error::{AppError, AppResult};
use crate::transfer::model::{FileEntry, OfferSummary};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
/// v3 adds incremental scan summaries and partial-manifest resume offsets.
pub const VERSION: u16 = crate::tuning::PROTOCOL_VERSION;
pub const MAX_FRAME: usize = crate::tuning::MAX_FRAME;
pub const IO_TIMEOUT: std::time::Duration = crate::tuning::FRAME_IO_TIMEOUT;
/// 清单帧（Offer / ManifestChunk / ManifestDone）的应答超时。
/// 接收端要逐条落库并规划目标路径，几万条清单在 NAS、HDD 或网络盘上
/// 可能远超普通请求的 20 秒；用普通超时会把"扫描慢"误报成"网络读取超时"。
pub const MANIFEST_TIMEOUT: std::time::Duration = crate::tuning::MANIFEST_ACK_TIMEOUT;

/// 已完成块集合的紧凑编码：按升序、互不重叠的闭区间 `[start, end]`。
///
/// 原先是 `Vec<usize>`，每次 `Poll` 都要回传整份块号列表：1 TB / 4 MiB 分块
/// 等于 26 万个块，JSON 约 2 MB，且每 750ms 一次。顺序传输下块号是连续的，
/// 用区间表示通常只剩几个元素；缺口多时最坏退化为"每块一个区间"，
/// 也比逐个数字小得多（每个区间 2 个 32 位整数）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CompletedRanges {
    #[serde(default)]
    ranges: Vec<[u32; 2]>,
}

// Read legacy v3 arrays as well as the v4 compact object. Serialization remains
// the v4 compact shape so this build interoperates with the committed GitHub build.
impl<'de> Deserialize<'de> for CompletedRanges {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Compact {
                #[serde(default)]
                ranges: Vec<[u32; 2]>,
            },
            Legacy(Vec<usize>),
        }
        match Repr::deserialize(deserializer)? {
            Repr::Compact { ranges } => Ok(Self { ranges }),
            Repr::Legacy(indices) => Ok(Self::from_sorted(indices)),
        }
    }
}

impl CompletedRanges {
    /// 由**升序**块号构造（`BTreeSet` 迭代天然升序）。乱序输入只会多出区间，不会出错。
    pub fn from_sorted(iter: impl IntoIterator<Item = usize>) -> Self {
        let mut ranges: Vec<[u32; 2]> = Vec::new();
        for index in iter {
            let value = index.min(u32::MAX as usize) as u32;
            match ranges.last_mut() {
                Some(last) if value == last[1].saturating_add(1) => last[1] = value,
                Some(last) if value <= last[1] => {}
                _ => ranges.push([value, value]),
            }
        }
        Self { ranges }
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// 已完成块的精确数量（区间长度之和）。
    pub fn len(&self) -> usize {
        self.ranges
            .iter()
            .map(|range| (range[1] - range[0]) as usize + 1)
            .sum()
    }

    /// 是否包含某块（区间有序且互不重叠，二分查找）。
    pub fn contains(&self, index: usize) -> bool {
        let value = index.min(u32::MAX as usize) as u32;
        self.ranges
            .binary_search_by(|range| {
                if range[1] < value {
                    std::cmp::Ordering::Less
                } else if range[0] > value {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }

    /// 最大块号（用于校验对端是否返回了超出清单的块）。
    pub fn max(&self) -> Option<usize> {
        self.ranges.last().map(|range| range[1] as usize)
    }

    /// 展开为逐块迭代器（顺序即升序）。
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.ranges
            .iter()
            .flat_map(|range| (range[0]..=range[1]).map(|value| value as usize))
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    SkipFile {
        id: String,
        file: usize,
    },
    Addresses,
    Hello {
        version: u16,
        node_id: String,
        display_name: String,
    },
    /// 建立/复用接收任务：只带清单摘要与指纹，返 `Ready` 表示需继续接收分片清单。
    Offer {
        id: String,
        summary: OfferSummary,
    },
    /// Sender has automatically reconnected after an interruption; resume only
    /// recoverable interrupted receive tasks, never an intentional user pause.
    Reconnect {
        id: String,
    },
    /// 清单分片：`first` 为本片首条在整份清单中的下标（0 起，按序递增）。
    ManifestChunk {
        id: String,
        first: usize,
        entries: Vec<FileEntry>,
    },
    /// 清单发送完毕：最终计数和摘要只在扫描结束后确定，接收端据此校验清单。
    ManifestDone {
        id: String,
        file_count: usize,
        total_size: u64,
        digest: String,
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
    Addresses {
        addresses: Vec<String>,
        tcp_port: u16,
        quic_port: u16,
    },
    Hello {
        version: u16,
        node_id: String,
        display_name: String,
        tcp_port: u16,
        quic_port: u16,
    },
    State {
        status: String,
        completed: CompletedRanges,
    },
    Ack,
    /// 清单分片已写入（逐片确认，形成背压，避免接收端无界缓冲）。
    ManifestAck,
    Listing {
        entries: Vec<NativeEntry>,
    },
    File {
        length: u64,
    },
    Ready,
    ManifestResume {
        next_index: usize,
    },
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
    r: &mut R,
    timeout: std::time::Duration,
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

#[cfg(test)]
mod compatibility_tests {
    use super::CompletedRanges;

    #[test]
    fn completed_ranges_accepts_legacy_and_compact_wire_shapes() {
        let legacy: CompletedRanges = serde_json::from_str("[0,1,2,5]").unwrap();
        assert_eq!(legacy.iter().collect::<Vec<_>>(), vec![0, 1, 2, 5]);

        let compact: CompletedRanges = serde_json::from_str(r#"{"ranges":[[0,2],[5,5]]}"#).unwrap();
        assert_eq!(compact.iter().collect::<Vec<_>>(), vec![0, 1, 2, 5]);

        let encoded = serde_json::to_string(&compact).unwrap();
        assert_eq!(encoded, r#"{"ranges":[[0,2],[5,5]]}"#);
    }
}
