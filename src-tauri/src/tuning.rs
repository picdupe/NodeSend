//! 文件功能：传输与网络调参集中处。
//!
//! - 所有超时、批量、容量阈值都在这里命名并写明理由，避免散落在各模块的
//!   魔术数字（同一语义在不同文件里写出不同数值是历史 bug 的来源之一）；
//! - 只放"跨模块共享或需要解释"的值；纯局部常量仍留在各自模块内。

use std::time::Duration;

// --- 协议帧与应答 ---

/// 单个协议帧的读写超时（普通请求：Hello/Poll/Chunk 等）。
pub(crate) const FRAME_IO_TIMEOUT: Duration = Duration::from_secs(20);
/// 清单类请求（Offer/ManifestChunk/ManifestDone）的应答超时：接收端逐条落库，
/// 大清单 + 慢盘可能耗时很久，因此远大于普通帧。
pub(crate) const MANIFEST_ACK_TIMEOUT: Duration = Duration::from_secs(300);
/// Commit 应答超时：接收端可能要把几十万个文件从缓存盘搬到目标盘。
pub(crate) const COMMIT_ACK_TIMEOUT: Duration = Duration::from_secs(1800);
/// 单个帧的最大长度（含清单分片）。超过即认为报文非法。
pub(crate) const MAX_FRAME: usize = 8 * 1024 * 1024;

// --- 连接建立 ---

/// TCP 建连超时。
pub(crate) const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// TLS 握手超时（TCP 通道）。
pub(crate) const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(4);
/// QUIC 握手超时；失败即回退 TCP。
pub(crate) const QUIC_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// 服务端 accept 单个连接/流的超时。
pub(crate) const ACCEPT_TIMEOUT: Duration = Duration::from_secs(5);
/// 会话建立后写入对端证书（身份登记）的超时：这一步要写数据库，
/// 长时间等不到说明数据库锁被别的线程占住，宁可放弃这次会话也不拖死自己。
pub(crate) const IDENTITY_RECORD_TIMEOUT: Duration = Duration::from_secs(10);
/// QUIC 保活与空闲超时。
pub(crate) const QUIC_KEEPALIVE: Duration = Duration::from_secs(5);
pub(crate) const QUIC_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// 关闭写半连接时的等待上限（尽力而为，不阻塞退出）。
pub(crate) const SHUTDOWN_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

// --- 清单与分块 ---

/// 协议版本：帧格式变更必须同步提升，两端不一致时 HELLO 会直接拒绝。
/// v4：`Response::State.completed` 由 `Vec<usize>` 改为区间编码 `CompletedRanges`。
pub(crate) const PROTOCOL_VERSION: u16 = 4;
/// 单帧清单条数（**发送起点**）。小批量先行：选完目录立刻就能发第一批，
/// 不必等扫满整批落库批次（[`MANIFEST_PAGE`]）才开始发送。
pub(crate) const MANIFEST_BATCH: usize = 256;
/// 清单**落库批次**：扫描结果先在内存累计到该条数再一次性写数据库，
/// 减少事务次数；发送仍按 [`MANIFEST_BATCH`] 逐帧进行，两者互不阻塞。
pub(crate) const MANIFEST_PAGE: usize = 5000;
/// 选择目录后的预览提示阈值（含目录条目）：扫描过程中一旦超过该条数，
/// 界面改提示"文件数量过多 · 发送时逐步扫描"（后台仍继续扫描到结束）；
/// 扫完后一律显示最终的文件数与总大小。
pub(crate) const PREVIEW_SCAN_LIMIT: usize = 10_000;
/// 后台预览扫描每次向扫描器索取的条数（只影响进度刷新粒度）。
pub(crate) const PREVIEW_BATCH: usize = 2000;
/// 传输分块大小（协议允许 256 KiB / 1 MiB / 4 MiB）。真实场景是 1 TB 级、
/// 20 万文件：4 MiB 把块数从约 419 万降到约 26 万，每块一次数据库提交与一次
/// 回包往返的开销随之降一个数量级；小文件不受影响（每个文件仍是 1 块）。
pub(crate) const CHUNK_SIZE: u64 = 4 * 1024 * 1024;
/// 接收端同时处理的待处理请求上限（超过直接拒绝，避免无界堆积）。
pub(crate) const MAX_PENDING_REQUESTS: usize = 128;
/// 会话并发上限：达到上限时拒绝新连接并打 WARN。
pub(crate) const MAX_SESSIONS: usize = 64;
/// 同时进行的发送任务上限；超出任务在调度器中等待空位。
pub(crate) const MAX_ACTIVE_SENDS: usize = 10;

// --- 发送端 ---

/// 扫描循环里每轮用于发送"已扫描文件"的时间预算。边扫边传是刻意设计：
/// 数据发送不向扫描让路，只限制单个文件独占时间片。
pub(crate) const SEND_DATA_SLICE: Duration = Duration::from_millis(1500);
/// 等待接收端状态的轮询间隔。
pub(crate) const SEND_POLL_INTERVAL: Duration = Duration::from_millis(750);
/// 连接中断后的重试等待与冷却时间。
pub(crate) const SEND_RETRY_DELAY: Duration = Duration::from_secs(2);
pub(crate) const SEND_RETRY_COOLDOWN_SECS: i64 = 30;
/// 发送端保留的读句柄上限：机械盘顺序读需要复用句柄，但不能无界累积。
pub(crate) const SEND_HANDLES_LIMIT: usize = 32;
/// 单次请求耗时超过该值就打 WARN，便于区分"慢"与"卡死"。
pub(crate) const SLOW_REQUEST_WARN: Duration = Duration::from_secs(5);

// --- 接收端与写盘 ---

/// 写队列容量（块）。4 MiB × 16 ≈ 64 MiB/任务。
pub(crate) const QUEUE_CHUNKS: usize = 16;
/// 队列满时等待空间的上限（等待期间不持有任务锁）。
pub(crate) const QUEUE_WAIT_TIMEOUT: Duration = Duration::from_secs(300);
/// 顺序写等待缺口的上限；超过则降级为乱序写，避免永久等待。
pub(crate) const WRITE_GAP_TIMEOUT: Duration = Duration::from_secs(2);
/// 后台写入/转写巡检间隔。
pub(crate) const WRITE_TICK: Duration = Duration::from_millis(200);
/// 缓存盘满时轮询可用空间的间隔。
pub(crate) const CACHE_SPACE_POLL: Duration = Duration::from_millis(200);
/// 等待本任务已入队块落盘的上限。
pub(crate) const AWAIT_WRITES_TIMEOUT: Duration = Duration::from_secs(30);
/// 转写长时间无进展即判定失败（避免 commit 永久等待）。
pub(crate) const RELAY_STALL_TIMEOUT: Duration = Duration::from_secs(120);
/// 缓存转写的分段大小，以及触发转写的起始缓存量。
pub(crate) const RELAY_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const RELAY_START_BYTES: u64 = 256 * 1024 * 1024;
/// 缓存盘保留的最小可用空间：低于此值阻塞接收，等待转写释放。
pub(crate) const CACHE_FREE_FLOOR: u64 = 256 * 1024 * 1024;

// --- 发现与巡检 ---

/// 单次 UDP 探测等待应答的时间。
pub(crate) const DISCOVERY_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// 周期性重新发现设备的时间间隔。
pub(crate) const DISCOVERY_TICK: Duration = Duration::from_secs(3);
