//! 文件功能：标准锁访问辅助。
//!
//! - `std::sync::Mutex` 的锁中毒（持锁线程 panic）默认让之后每次 `lock()` 都返回
//!   `Err`，业务代码普遍 `.unwrap()` 会二次 panic，把这个后台线程的崩溃扩散成
//!   整个进程崩溃（常驻托盘时尤其明显）；
//! - 这里统一从中毒状态恢复：数据结构本身仍是完整的（写入要么完成要么未开始），
//!   继续使用比让应用整体挂掉更安全；
//! - 所有标准 `Mutex` 访问都应走 [`lock`]，不要直接 `.lock().unwrap()`。

use std::sync::{Mutex, MutexGuard};

/// 取标准互斥锁；锁中毒时取回内部数据继续使用，并记录一条警告。
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!("互斥锁已中毒（持锁线程曾 panic），从内部数据恢复继续使用");
            poisoned.into_inner()
        }
    }
}

/// 供方法链使用的等价形式：`self.tasks.lock_or_recover()`。
pub(crate) trait MutexExt<T> {
    fn lock_or_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn lock_or_recover(&self) -> MutexGuard<'_, T> {
        lock(self)
    }
}
