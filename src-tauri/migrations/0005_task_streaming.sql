-- 清单流式化（Stage 2）：把「清单明细 / 完成块 / 转写进度 / 跳过文件 / 目标路径」
-- 从 transfer_tasks.record 的整份 JSON 中拆出，改为按行增量读写。
-- 目标：edit()（改状态、记完成、记转写进度）的持久化开销与文件数无关，
--       百万级清单不再出现「每收一块就重写上百 MB JSON」。

-- 清单条目，按 index_no 与内存中 entries 的顺序一一对应。
CREATE TABLE task_entries (
    task_id     TEXT    NOT NULL,
    index_no    INTEGER NOT NULL,
    path        TEXT    NOT NULL,
    size        INTEGER NOT NULL,
    modified_ms INTEGER NOT NULL,
    directory   INTEGER NOT NULL,
    PRIMARY KEY (task_id, index_no)
);

-- 已完成块：仅在数据真正落盘（并 sync）后写入。
CREATE TABLE task_chunks (
    task_id     TEXT    NOT NULL,
    chunk_index INTEGER NOT NULL,
    PRIMARY KEY (task_id, chunk_index)
);

-- 缓存中转进度：每个文件已转写并 sync 的字节数；只保存非零项（稀疏）。
CREATE TABLE task_relay (
    task_id    TEXT    NOT NULL,
    file_index INTEGER NOT NULL,
    relayed    INTEGER NOT NULL,
    PRIMARY KEY (task_id, file_index)
);

-- 被跳过的文件（源文件已变化，或接收方未选中）。
CREATE TABLE task_skipped (
    task_id    TEXT    NOT NULL,
    file_index INTEGER NOT NULL,
    PRIMARY KEY (task_id, file_index)
);

-- 接收端规划出的目标路径；只保存有值（Some）的条目，未选中的不落行。
CREATE TABLE task_targets (
    task_id    TEXT    NOT NULL,
    file_index INTEGER NOT NULL,
    target     TEXT    NOT NULL,
    PRIMARY KEY (task_id, file_index)
);