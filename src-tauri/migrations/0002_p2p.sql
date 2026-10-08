-- Discovery hints are independent from Trust. Offline nodes are retained.
CREATE TABLE nodes (node_id TEXT PRIMARY KEY, record TEXT NOT NULL);
CREATE TABLE node_certificates (node_id TEXT PRIMARY KEY, certificate BLOB NOT NULL, updated_at TEXT NOT NULL);
-- Task manifest, durable chunk bitmap, decisions and local paths; never advertised.
CREATE TABLE transfer_tasks (id TEXT PRIMARY KEY, record TEXT NOT NULL);
INSERT INTO settings (key,value) VALUES ('receive_policy','ask');
