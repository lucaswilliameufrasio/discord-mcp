//! Read-only setup guidance; never prints environment values or contacts a host.

pub fn run(mode: &str) -> Result<(), Box<dyn std::error::Error>> {
    match mode {
        "local" => {
            println!("Local setup (no files or settings are changed):");
            println!("1. Choose a private directory for DISCORD_MCP_DB.");
            println!(
                "2. Supply DISCORD_MCP_KEY (32 random bytes in hex) through your secret manager; keep a separate recovery copy."
            );
            for name in ["DISCORD_MCP_DB", "DISCORD_MCP_KEY"] {
                let status = if std::env::var_os(name).is_some() {
                    "present (not validated)"
                } else {
                    "missing"
                };
                println!("   {name}: {status}");
            }
            if let Some(value) = std::env::var_os("DISCORD_MCP_KEY") {
                let valid = value
                    .to_str()
                    .and_then(|text| hex::decode(text).ok())
                    .is_some_and(|bytes| bytes.len() == 32);
                if !valid {
                    return Err(
                        "DISCORD_MCP_KEY must be 32 bytes encoded in hex; value not shown".into(),
                    );
                }
                println!("   DISCORD_MCP_KEY format: valid (custody not verified)");
            }
            println!(
                "3. Run `discord-mcp setup local --init` to initialize storage, then `discord-mcp web` to register channels."
            );
            println!(
                "4. Configure your MCP client to run `discord-mcp` with the same environment."
            );
            println!(
                "Never put keys or webhooks in command arguments or shell history. See README.md and docs/operations.md."
            );
        }
        "remote" => {
            println!("Remote setup planning (no connection or server changes):");
            println!(
                "1. Build and test a compatible release artifact locally; do not build on the server."
            );
            println!(
                "2. Choose the systemd or rootless Docker model in docs/remote-runtime-options.md."
            );
            println!(
                "3. Provide a dedicated service identity, protected database and independently recoverable secrets."
            );
            println!(
                "4. Configure a redacting HTTPS reverse proxy, DNS and firewall; register Discord OAuth callbacks and the MCP client."
            );
            println!(
                "5. Validate access isolation, authentication, monitoring and an isolated restore before real use."
            );
            println!(
                "Run `discord-mcp setup remote --check` in the intended service environment for a read-only configuration preflight."
            );
            println!(
                "Use `discord-mcp setup remote --check-https-origin` to check HTTPS origin syntax without a network connection."
            );
            println!(
                "For an existing database, `discord-mcp setup remote --check-storage` inspects its metadata without creating or opening it."
            );
            println!(
                "Review `discord-mcp setup remote systemd --preview` before optionally creating a local preparation bundle."
            );
            println!(
                "HTTP mode remains experimental and is not validated for production. This command does not install or deploy anything."
            );
        }
        "recovery" => {
            println!("Recovery planning only: no database or backup is read, created or changed.");
            println!(
                "1. Restore a snapshot to an isolated, private source; keep the original backup and key custody independent."
            );
            println!(
                "2. With the original DISCORD_MCP_KEY supplied securely, run `discord-mcp recovery-prepare SOURCE NEW_DESTINATION`. The destination must not exist."
            );
            println!(
                "3. Review the quarantined copy and invalidated permissions, tokens and codes. Keep the running service and source untouched."
            );
            println!(
                "4. Only after explicit review, point DISCORD_MCP_DB at the quarantined copy and run `discord-mcp recovery-activate --confirm` with the same key. Previous grants and tokens remain invalid."
            );
            println!(
                "5. Reissue access deliberately and verify the actual service, proxy and recovery process in an authorized environment."
            );
            println!(
                "Synthetic exercises are documented in docs/sqlite-recovery-drill.md; they do not establish production recovery, RPO or RTO."
            );
        }
        _ => return Err("use setup local, setup remote or setup recovery".into()),
    }
    Ok(())
}

pub fn check_https_origin() -> Result<(), Box<dyn std::error::Error>> {
    let origin = std::env::var("DISCORD_MCP_PUBLIC_ORIGIN")
        .map_err(|_| "missing or invalid DISCORD_MCP_PUBLIC_ORIGIN")?;
    if !discord_mcp::oauth_web::valid_origin(&origin) || !origin.starts_with("https://") {
        return Err("DISCORD_MCP_PUBLIC_ORIGIN must be a canonical HTTPS origin without credentials, path, query or fragment".into());
    }
    println!(
        "HTTPS origin syntax passed. DNS, reachability, TLS certificate, proxy configuration and public exposure were not verified."
    );
    Ok(())
}

fn caddy_site(host: &str) -> Result<String, Box<dyn std::error::Error>> {
    if host.len() > 253
        || !host.contains('.')
        || host.bytes().all(|b| b.is_ascii_digit() || b == b'.')
    {
        return Err("host must be a lowercase DNS name, not an IP address".into());
    }
    for label in host.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err("host must be a lowercase DNS name, not an IP address".into());
        }
    }
    Ok(format!(
        "# Review before installation. Caddy manages TLS when activated.\n# Do not enable access logs for sensitive OAuth and MCP requests.\n{host} {{\n\treverse_proxy 127.0.0.1:8000\n\thandle_errors {{\n\t\trespond \"Service unavailable\" 502\n\t}}\n}}\n"
    ))
}

pub fn preview_caddy(host: &str) -> Result<(), Box<dyn std::error::Error>> {
    let site = caddy_site(host)?;
    println!("PREVIEW ONLY: no files, proxy, DNS or certificates are changed.\n{site}");
    println!("After review: setup remote caddy HOST ABSOLUTE_DIRECTORY --confirm");
    Ok(())
}

pub fn install_caddy(
    host: &str,
    directory: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let site = caddy_site(host)?;
    install_systemd_file(directory, "discord-mcp.caddy", &site)?;
    println!(
        "Caddy site file prepared. No proxy was installed, reloaded or contacted; no certificate was obtained."
    );
    Ok(())
}

pub fn preview_remote_bundle(host: &str) -> Result<(), Box<dyn std::error::Error>> {
    let site = caddy_site(host)?;
    println!(
        "PREVIEW ONLY: a new local directory with six files; no server or configuration is changed."
    );
    println!(
        "--- discord-mcp.caddy ---\n{site}\n--- discord-mcp.service ---\n{SYSTEMD_UNIT}\n--- ciphera.conf ---\n{CIPHERA_DROPIN}\n--- service.env.example ---\n{SYSTEMD_ENV_TEMPLATE}\n--- SETUP.md ---\n{SYSTEMD_GUIDE}\n--- RECOVERY.md ---\n{RECOVERY_GUIDE}"
    );
    println!(
        "Review HTTPS origin, paths, identity, TLS, secrets and recovery independently. Ciphera is optional; nothing will be activated automatically."
    );
    Ok(())
}

pub fn prepare_remote_bundle(
    host: &str,
    directory: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let site = caddy_site(host)?;
    if !directory.is_absolute() {
        return Err("bundle directory must be an absolute new path".into());
    }
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)?;
    for (name, content) in [
        ("discord-mcp.caddy", site.as_str()),
        ("discord-mcp.service", SYSTEMD_UNIT),
        ("ciphera.conf", CIPHERA_DROPIN),
        ("service.env.example", SYSTEMD_ENV_TEMPLATE),
        ("SETUP.md", SYSTEMD_GUIDE),
        ("RECOVERY.md", RECOVERY_GUIDE),
    ] {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(directory.join(name))?;
        file.write_all(content.as_bytes())?;
    }
    println!(
        "Local bundle prepared without secrets. Review all files; no server was contacted or changed."
    );
    Ok(())
}

pub fn check_storage() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::var_os("DISCORD_MCP_DB").ok_or("missing DISCORD_MCP_DB")?;
    let path = std::path::Path::new(&path);
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let parent_metadata =
        std::fs::symlink_metadata(parent).map_err(|_| "unable to inspect database directory")?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        return Err("database directory must be a real directory, not a symlink".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if parent_metadata.mode() & 0o077 != 0 {
            return Err("database directory must be private to the service user".into());
        }
    }
    // Do not print the path or the OS error, which may disclose deployment details.
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "unable to inspect database; confirm it exists and is accessible")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("database must be a regular file, not a symlink".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 || metadata.nlink() != 1 {
            return Err("database must be private and have exactly one hard link".into());
        }
    }
    println!(
        "Database metadata check passed. Ownership, ancestor directories, SQLite contents, key validity and recovery were not verified."
    );
    Ok(())
}

const SYSTEMD_UNIT: &str = include_str!("../deploy/systemd/discord-mcp.service");
const SYSTEMD_GUIDE: &str = include_str!("../deploy/systemd/SETUP.md");
const SYSTEMD_ENV_TEMPLATE: &str = include_str!("../deploy/systemd/service.env.example");
const CIPHERA_DROPIN: &str = include_str!("../deploy/systemd/ciphera.conf");
const RECOVERY_GUIDE: &str = include_str!("../deploy/systemd/RECOVERY.md");

pub fn generate_key() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{IsTerminal, Write};
    let stdout = std::io::stdout();
    if stdout.is_terminal() {
        return Err("refusing to print a key to a terminal; use a trusted secret manager via a protected pipe".into());
    }
    let mut key = [0_u8; 32];
    getrandom::fill(&mut key).map_err(|_| "unable to generate a secure key")?;
    writeln!(stdout.lock(), "{}", hex::encode(key))?;
    Ok(())
}

pub fn preview_systemd() {
    println!(
        "PREVIEW ONLY: two local files in a new directory; no remote connection or installation.\n--- discord-mcp.service ---\n{SYSTEMD_UNIT}\n--- SETUP.md ---\n{SYSTEMD_GUIDE}"
    );
    println!("To create this bundle: discord-mcp setup remote systemd NEW_DIRECTORY --confirm");
    println!("--- service.env.example (optional separate template) ---\n{SYSTEMD_ENV_TEMPLATE}");
    println!(
        "To create the template without overwriting: discord-mcp setup remote systemd --install-env-template ABSOLUTE_DIRECTORY --confirm"
    );
    println!("--- ciphera.conf (optional drop-in) ---\n{CIPHERA_DROPIN}");
    println!(
        "After review, use setup remote systemd --install-ciphera-dropin ABSOLUTE_DIRECTORY --confirm. No credentials are installed and no service is restarted."
    );
    println!(
        "To install this unit on the current host after review: discord-mcp setup remote systemd --install-unit ABSOLUTE_DIRECTORY --confirm. The directory must be trusted and already exist; no reload or start is performed."
    );
}

pub fn install_systemd_unit(directory: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    install_systemd_file(directory, "discord-mcp.service", SYSTEMD_UNIT)?;
    println!(
        "Unit file installed on this host. No daemon reload, service start, proxy change or server connection was performed. Review prerequisites before enabling it."
    );
    Ok(())
}

pub fn install_systemd_env_template(
    directory: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    install_systemd_file(directory, "service.env.example", SYSTEMD_ENV_TEMPLATE)?;
    println!(
        "Example created without secrets. It is not the service environment and does not configure or start the server."
    );
    Ok(())
}

pub fn install_ciphera_dropin(
    directory: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    install_systemd_file(directory, "ciphera.conf", CIPHERA_DROPIN)?;
    println!(
        "Ciphera drop-in installed without credentials. No reload, restart or server connection was performed."
    );
    Ok(())
}

fn install_systemd_file(
    directory: &std::path::Path,
    name: &str,
    contents: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;

    if !directory.is_absolute() {
        return Err("unit directory must be an absolute trusted directory".into());
    }
    let metadata = std::fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("unit directory must be a real existing directory, not a symlink".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err("unit directory must not be writable by group or others".into());
        }
    }
    // Publish only a complete, synced file and never replace an existing unit.
    // The directory is operator-selected and must not be writable by others.
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(contents.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist_noclobber(directory.join(name))?;
    Ok(())
}

pub fn prepare_systemd(destination: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;

    // Creating the directory atomically prevents overwriting existing files or
    // following a pre-existing destination symlink. Never delete partial output.
    std::fs::create_dir(destination)?;
    for (name, contents) in [
        ("discord-mcp.service", SYSTEMD_UNIT),
        ("SETUP.md", SYSTEMD_GUIDE),
    ] {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination.join(name))?;
        file.write_all(contents.as_bytes())?;
    }
    println!("Local systemd bundle created. Review SETUP.md; no server was contacted or changed.");
    Ok(())
}

pub fn check_remote() -> Result<(), Box<dyn std::error::Error>> {
    let mut missing = false;
    for name in [
        "DISCORD_MCP_DB",
        "DISCORD_MCP_KEY",
        "DISCORD_MCP_PUBLIC_ORIGIN",
        "DISCORD_MCP_DISCORD_CLIENT_ID",
        "DISCORD_MCP_DISCORD_CLIENT_SECRET",
        "DISCORD_MCP_ADMIN_ID",
        "DISCORD_MCP_CLIENT_ID",
        "DISCORD_MCP_CLIENT_NAME",
        "DISCORD_MCP_CLIENT_REDIRECT",
    ] {
        let present = std::env::var_os(name).is_some_and(|v| !v.is_empty());
        println!("{name}: {}", if present { "present" } else { "missing" });
        missing |= !present;
    }
    let listen = std::env::var("DISCORD_MCP_LISTEN").unwrap_or_else(|_| "127.0.0.1:8000".into());
    let loopback = listen
        .parse::<std::net::SocketAddr>()
        .is_ok_and(|v| v.ip().is_loopback());
    if !loopback {
        return Err("DISCORD_MCP_LISTEN must be a valid loopback address".into());
    }
    if missing {
        return Err("missing required configuration; values were not displayed".into());
    }
    let key = std::env::var("DISCORD_MCP_KEY").map_err(|_| "invalid key encoding")?;
    if !hex::decode(key).is_ok_and(|bytes| bytes.len() == 32) {
        return Err("DISCORD_MCP_KEY must be 32 bytes encoded in hex".into());
    }
    println!(
        "Presence, key format and listener checks passed; TLS, Discord, permissions, storage access and recovery were not tested. HTTP mode remains experimental."
    );
    Ok(())
}
