Based on smb-server 0.4.0 (MIT), https://github.com/paltaio/rust-smb-server.

Local patch: src/fs/local.rs selects Unix FileExt::read_at/write_at or Windows
FileExt::seek_read/seek_write so the embedded local filesystem backend builds
on Windows.

Compound signing fixes in src/dispatch.rs: include alignment padding in response
signatures and verify original request bytes before inherited compound context
changes the session/tree/file IDs. Covered by the independent smb2 client test.
