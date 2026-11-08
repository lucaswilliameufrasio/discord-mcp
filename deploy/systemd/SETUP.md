# Experimental systemd preparation

This bundle is a template, not an installer. No server or proxy has been changed.
Review the service unit, paths, permissions and service ownership before use.
The HTTP server has not been validated with Discord or for production.

1. Build and test an appropriate release artifact on a trusted build machine;
   verify its target architecture, libc compatibility and provenance. Do not
   compile on the server. Arrange a dedicated service account and a private
   state directory. Do not reuse another application's account or database.
2. Arrange independent custody and recovery of the encryption key and Discord
   OAuth secret. The existing unit expects a protected service environment file;
   environment variables are visible to privileged processes. Do not include
   secrets in this bundle, command arguments, shell history, logs or images.
   The environment file can override the unit's DB and listener settings.
3. Supply all variables listed by `discord-mcp setup remote --check` to the
   service. Run that preflight in the same service environment without exposing
   values. It does not validate Discord, filesystem access, TLS or recovery.
4. Plan a TLS reverse proxy, DNS and firewall separately. Preserve the public
   Host; redact authorization, cookies, callback URLs and request bodies. The
   application listens on loopback only. Register the OAuth callbacks and MCP
   client as described in the project operations guide.
5. Before installation, review the exact host changes and obtain explicit
   operator approval. Test identity isolation and an independent isolated
   restore. Confirm monitoring, upgrade and rollback procedures. Do not claim
   readiness based only on a preflight or service unit.

This service is a singleton using SQLite. Concurrent instances against one
database, blue-green deployments and database rollback are not established.
Do not install, enable or start the unit without host-specific review.
