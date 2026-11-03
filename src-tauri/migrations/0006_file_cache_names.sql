-- 稳定文件名映射：清单条目记录显示文件名与缓存安全名称。
--
-- `name`       清单条目的显示文件名（路径最后一段），接收端落库时写入；
-- `cache_name` 缓存盘上的稳定文件名（`<NodeID>-<文件名>`，同名追加序号），
--              接收端在条目落库前生成并持久化，之后所有阶段只按文件索引读取。
-- 旧记录两列为空，仅保留为历史，不支持恢复传输。
ALTER TABLE task_entries ADD COLUMN name TEXT NOT NULL DEFAULT '';
ALTER TABLE task_entries ADD COLUMN cache_name TEXT NOT NULL DEFAULT '';
