-- =============================================================================
-- 文件功能：0001 初始 Schema（基线 §18/§4/§11/§16）。
-- 对应模块：
--   trusted_nodes —— Trust 关系（§3/§16，以 Node ID 为单位）
--   shares        —— 共享中心目录配置（§11）与访问模式（§16）
--   settings      —— 应用设置键值表（HTTP 端口等，§23）
-- 规则：本文件一经发布不可修改；后续变更追加 0002_xxx.sql。
-- =============================================================================

-- 已信任 Node 列表：Trust 以 Node ID 为单位（基线 §3）。
CREATE TABLE trusted_nodes (
    node_id     TEXT PRIMARY KEY,
    display_name TEXT,
    created_at  TEXT NOT NULL
);

-- 共享目录配置（共享虚拟根，不暴露绝对路径；基线 §11）。
CREATE TABLE shares (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,           -- 自定义共享名称
    path         TEXT NOT NULL,                 -- 本机绝对路径（仅服务端使用）
    enabled      INTEGER NOT NULL DEFAULT 1,    -- 0/1
    access_mode  TEXT NOT NULL DEFAULT 'token', -- open | token | trusted | deny（§16）
    access_token TEXT,                          -- token 模式下的访问令牌
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

-- 通用应用设置（键值对）。
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- 初始默认设置：内嵌 HTTP 共享服务监听地址与开关（§12）。
-- HTTP 共享是【附加功能】，默认关闭；软件启动不自动共享，由用户在仪表盘手动开启。
INSERT INTO settings (key, value) VALUES
    ('http_enabled', '0'),
    ('http_listen',   '0.0.0.0:58080');
