ALTER TABLE shares ADD COLUMN requested_port INTEGER NOT NULL DEFAULT 0;
UPDATE shares
SET requested_port = CASE protocol
    WHEN 'http' THEN 8193
    WHEN 'ftp' THEN 21
    WHEN 'smb' THEN 445
    ELSE port
END;
