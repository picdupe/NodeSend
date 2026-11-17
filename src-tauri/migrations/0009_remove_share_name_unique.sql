CREATE TABLE shares_without_unique_name (
    id                  TEXT PRIMARY KEY,
    name                TEXT NOT NULL,
    path                TEXT NOT NULL,
    enabled             INTEGER NOT NULL DEFAULT 1,
    access_mode         TEXT NOT NULL DEFAULT 'token',
    access_token        TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    protocol            TEXT NOT NULL DEFAULT 'nodesend',
    port                INTEGER NOT NULL DEFAULT 0,
    protocol_share_name TEXT NOT NULL DEFAULT ''
);

INSERT INTO shares_without_unique_name (
    id, name, path, enabled, access_mode, access_token,
    created_at, updated_at, protocol, port, protocol_share_name
)
SELECT
    id, name, path, enabled, access_mode, access_token,
    created_at, updated_at, protocol, port, protocol_share_name
FROM shares;

DROP TABLE shares;
ALTER TABLE shares_without_unique_name RENAME TO shares;
