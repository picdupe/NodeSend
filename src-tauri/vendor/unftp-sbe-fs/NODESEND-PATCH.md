Based on unftp-sbe-fs 0.4.0 (Apache-2.0), https://github.com/bolcom/libunftp.

Local patch: derive Clone for Filesystem. Each session clones the already opened
root capability, avoiding a fallible reopen (and a process-aborting panic) if the
shared directory is moved or removed after the listener starts.
