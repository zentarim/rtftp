# Known findings

- Polling `tokio::fs::try_exists` during guest boot slowed the following
  transfer by about 10% (see the comment in `src/vm/mod.rs`).
