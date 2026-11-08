# Recovery prerequisites

This is not an automatic backup or a production recovery procedure. Establish
an independently recoverable encrypted backup and key custody before service
use; the current Floci exercise is synthetic only.

1. Obtain an authorized snapshot and the original key. Restore the snapshot to
   a private, isolated offline source. Never point recovery at the live database.
2. Run `discord-mcp setup recovery` for the quarantined-copy workflow. Supply the
   original key securely; do not put it in arguments, logs or shell history.
3. Prepare a new destination and review it before explicitly activating it.
   Restored permissions, tokens and authorization codes are invalidated; reissue
   access deliberately after review. Never overwrite the original or live data.
4. Test key recovery, service identity, proxy/TLS and a full restore in an
   authorized isolated environment. Record evidence, recovery objectives and
   ownership. Synthetic tests alone do not establish RPO or RTO.
