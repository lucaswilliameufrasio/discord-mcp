use std::process::Command;

#[test]
fn help_ignores_invalid_storage_environment_without_creating_database() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("private-path-marker");
    for command in ["help", "--help"] {
        for key in [None, Some("private-invalid-key-marker")] {
            let mut child = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
            child
                .env_clear()
                .env("DISCORD_MCP_DB", &database)
                .arg(command);
            if let Some(key) = key {
                child.env("DISCORD_MCP_KEY", key);
            }
            let output = child.output().unwrap();
            assert!(output.status.success());
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stdout.contains("setup local"));
            for text in [stdout, stderr] {
                assert!(!text.contains("private-path-marker"));
                assert!(!text.contains("private-invalid-key-marker"));
            }
            assert!(!database.exists());
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        }
    }
}

#[test]
fn malformed_commands_do_not_create_or_modify_database() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("must-not-be-created.db");
    for args in [
        vec!["setup"],
        vec!["setup", "local", "--init", "unexpected"],
        vec!["setup", "remote", "--check", "unexpected"],
        vec!["setup", "remote", "--check-storage", "unexpected"],
        vec!["setup", "recovery", "unexpected"],
        vec!["remote-set-permission", "https://example.test", "123"],
        vec!["remote-list-channels", "https://example.test", "unexpected"],
        vec!["recovery-prepare", "missing-argument"],
        vec!["recovery-activate", "unexpected"],
        vec!["serve", "unexpected"],
        vec!["set-permission", "101", "channel", "invalid"],
        vec!["unknown-command"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .env("DISCORD_MCP_DB", &database)
            .env("DISCORD_MCP_KEY", hex::encode([42u8; 32]))
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!database.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
    std::fs::write(&database, b"existing synthetic data").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .env("DISCORD_MCP_DB", &database)
        .env("DISCORD_MCP_KEY", hex::encode([42u8; 32]))
        .args(["setup", "remote", "--check", "unexpected"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        std::fs::read(&database).unwrap(),
        b"existing synthetic data"
    );
}

#[test]
fn remote_bundle_is_opt_in_consistent_and_preserves_existing_paths() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("bundle");
    let binary = env!("CARGO_BIN_EXE_discord-mcp");
    let run = |host: &str, confirmed: bool| {
        let mut cmd = Command::new(binary);
        cmd.args(["setup", "remote", "bundle", host])
            .arg(&destination)
            .env("DISCORD_MCP_KEY", "private-key-marker")
            .env("DISCORD_MCP_DISCORD_CLIENT_SECRET", "private-secret-marker");
        if confirmed {
            cmd.arg("--confirm");
        }
        cmd.output().unwrap()
    };
    let preview = Command::new(binary)
        .args(["setup", "remote", "bundle", "example.test", "--preview"])
        .output()
        .unwrap();
    assert!(preview.status.success());
    assert!(String::from_utf8_lossy(&preview.stdout).contains("example.test {"));
    assert!(!run("example.test", false).status.success());
    assert!(!run("example.test\ninvalid", true).status.success());
    assert!(!destination.exists());
    assert!(run("example.test", true).status.success());
    for name in [
        "discord-mcp.caddy",
        "discord-mcp.service",
        "ciphera.conf",
        "service.env.example",
        "SETUP.md",
        "RECOVERY.md",
    ] {
        let contents = std::fs::read_to_string(destination.join(name)).unwrap();
        assert!(!contents.contains("private-key-marker"));
        assert!(!contents.contains("private-secret-marker"));
    }
    let caddy = std::fs::read_to_string(destination.join("discord-mcp.caddy")).unwrap();
    assert!(caddy.contains("example.test {"));
    assert!(caddy.contains("reverse_proxy 127.0.0.1:8000"));
    assert_eq!(
        std::fs::read_to_string(destination.join("discord-mcp.service")).unwrap(),
        include_str!("../deploy/systemd/discord-mcp.service")
    );
    std::fs::write(destination.join("SETUP.md"), "operator data").unwrap();
    assert!(!run("example.test", true).status.success());
    assert_eq!(
        std::fs::read_to_string(destination.join("SETUP.md")).unwrap(),
        "operator data"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

#[cfg(unix)]
#[test]
fn ciphera_failure_or_missing_application_key_does_not_start_service() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let dropin = include_str!("../deploy/systemd/ciphera.conf");
    let line = dropin
        .lines()
        .find(|line| line.starts_with("ExecStart=/bin/sh -c '"))
        .unwrap();
    let shell = line
        .strip_prefix("ExecStart=/bin/sh -c '")
        .unwrap()
        .strip_suffix('\'')
        .unwrap();
    let fake = dir.path().join("fake-ciphera");
    let database = dir.path().join("must-not-exist.db");
    std::fs::write(dir.path().join("ciphera_token"), "synthetic-token").unwrap();
    let shell = shell
        .replace("$$", "$")
        .replace("/usr/local/bin/ciphera", fake.to_str().unwrap())
        .replace(
            "/usr/local/bin/discord-mcp",
            env!("CARGO_BIN_EXE_discord-mcp"),
        );
    let run = || {
        Command::new("/bin/sh")
            .env_clear()
            .args(["-c", &shell])
            .env("CREDENTIALS_DIRECTORY", dir.path())
            .env("CIPHERA_PROJECT", "synthetic-project")
            .env("CIPHERA_ENV", "test")
            .env("DISCORD_MCP_DB", &database)
            .output()
            .unwrap()
    };
    std::fs::write(&fake, "#!/bin/sh\nexit 23\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
    let failed = run();
    assert_eq!(failed.status.code(), Some(23));
    assert!(!database.exists());
    assert!(!String::from_utf8_lossy(&failed.stderr).contains("synthetic-token"));
    std::fs::write(&fake, "#!/bin/sh\nshift 6\nexec \"$@\"\n").unwrap();
    let missing_key = run();
    assert!(!missing_key.status.success());
    assert!(!database.exists());
    assert!(!String::from_utf8_lossy(&missing_key.stderr).contains("synthetic-token"));
    assert!(String::from_utf8_lossy(&missing_key.stderr).contains("configure both"));
}

#[test]
fn recovery_planning_is_read_only_and_explains_explicit_activation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("private-db-marker");
    let output = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .args(["setup", "recovery"])
        .env("DISCORD_MCP_DB", &db)
        .env("DISCORD_MCP_KEY", "private-key-marker")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("recovery-prepare SOURCE NEW_DESTINATION"));
    assert!(text.contains("recovery-activate --confirm"));
    assert!(text.contains("Previous grants and tokens remain invalid"));
    assert!(!text.contains("private-db-marker"));
    assert!(!text.contains("private-key-marker"));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn caddy_preparation_validates_host_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let binary = env!("CARGO_BIN_EXE_discord-mcp");
    let path = dir.path().join("discord-mcp.caddy");
    for host in [
        "example.test\nimport evil",
        "*.example.test",
        "example.test:443",
        "127.0.0.1",
        "EXAMPLE.test",
        "-a.test",
    ] {
        let output = Command::new(binary)
            .args(["setup", "remote", "caddy", host, "--preview"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!path.exists());
    }
    let preview = Command::new(binary)
        .args(["setup", "remote", "caddy", "example.test", "--preview"])
        .output()
        .unwrap();
    assert!(preview.status.success());
    assert!(String::from_utf8_lossy(&preview.stdout).contains("reverse_proxy 127.0.0.1:8000"));
    assert!(String::from_utf8_lossy(&preview.stdout).contains("handle_errors"));
    let run = |confirmed| {
        let mut command = Command::new(binary);
        command
            .args(["setup", "remote", "caddy", "example.test"])
            .arg(dir.path());
        if confirmed {
            command.arg("--confirm");
        }
        command.output().unwrap()
    };
    assert!(!run(false).status.success());
    assert!(!path.exists());
    assert!(run(true).status.success());
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("example.test {")
    );
    std::fs::write(&path, "operator configuration").unwrap();
    assert!(!run(true).status.success());
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "operator configuration"
    );
}

#[test]
fn https_origin_preflight_rejects_non_https_and_noncanonical_origins_without_disclosure() {
    let check = |origin: &str| {
        Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .args(["setup", "remote", "--check-https-origin"])
            .env("DISCORD_MCP_PUBLIC_ORIGIN", origin)
            .output()
            .unwrap()
    };
    let valid = check("https://example.test");
    assert!(valid.status.success());
    assert!(String::from_utf8_lossy(&valid.stdout).contains("were not verified"));
    for invalid in [
        "http://127.0.0.1:8000",
        "https://example.test/path",
        "https://example.test?private-marker=1",
        "https://name:private-marker@example.test",
        "https://EXAMPLE.test",
    ] {
        let output = check(invalid);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-marker"));
    }
}

#[cfg(unix)]
#[test]
fn ciphera_dropin_uses_scoped_bootstrap_and_fails_closed() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let dropin = include_str!("../deploy/systemd/ciphera.conf");
    let line = dropin
        .lines()
        .find(|line| line.starts_with("ExecStart=/bin/sh -c '"))
        .unwrap();
    let shell = line
        .strip_prefix("ExecStart=/bin/sh -c '")
        .unwrap()
        .strip_suffix('\'')
        .unwrap();
    let fake = dir.path().join("fake-ciphera");
    let marker = dir.path().join("invoked");
    std::fs::write(&fake, "#!/bin/sh\n[ \"$CIPHERA_TOKEN\" = synthetic-token ] || exit 9\n[ \"$1\" = run ] && [ \"$2\" = --project ] && [ \"$3\" = 'project literal;not-shell' ] && [ \"$4\" = --env ] && [ \"$5\" = test ] || exit 10\nprintf ok > \"$MARKER\"\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
    let shell = shell
        .replace("$$", "$")
        .replace("/usr/local/bin/ciphera", fake.to_str().unwrap());
    let run = || {
        Command::new("/bin/sh")
            .args(["-c", &shell])
            .env("CREDENTIALS_DIRECTORY", dir.path())
            .env("CIPHERA_PROJECT", "project literal;not-shell")
            .env("CIPHERA_ENV", "test")
            .env("MARKER", &marker)
            .output()
            .unwrap()
    };
    let missing = run();
    assert!(!missing.status.success());
    assert!(!marker.exists());
    std::fs::write(dir.path().join("ciphera_token"), "").unwrap();
    let empty = run();
    assert!(!empty.status.success());
    assert!(!marker.exists());
    std::fs::write(dir.path().join("ciphera_token"), "synthetic-token").unwrap();
    let no_project = Command::new("/bin/sh")
        .args(["-c", &shell])
        .env("CREDENTIALS_DIRECTORY", dir.path())
        .env_remove("CIPHERA_PROJECT")
        .env("CIPHERA_ENV", "test")
        .env("MARKER", &marker)
        .output()
        .unwrap();
    assert!(!no_project.status.success());
    assert!(!marker.exists());
    assert!(!String::from_utf8_lossy(&no_project.stderr).contains("synthetic-token"));
    for (name, value) in [
        ("DISCORD_MCP_KEY", "legacy-secret-marker"),
        ("DISCORD_MCP_DISCORD_CLIENT_SECRET", ""),
    ] {
        let legacy = Command::new("/bin/sh")
            .args(["-c", &shell])
            .env("CREDENTIALS_DIRECTORY", dir.path())
            .env("CIPHERA_PROJECT", "project literal;not-shell")
            .env("CIPHERA_ENV", "test")
            .env("MARKER", &marker)
            .env(name, value)
            .output()
            .unwrap();
        assert!(!legacy.status.success());
        assert!(!marker.exists());
        assert!(!String::from_utf8_lossy(&legacy.stderr).contains("legacy-secret-marker"));
    }
    let success = run();
    assert!(success.status.success());
    assert!(success.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&success.stderr).contains("synthetic-token"));
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "ok");
}

#[test]
fn ciphera_dropin_install_requires_confirmation_and_preserves_existing() {
    let dir = tempfile::tempdir().unwrap();
    let run = |confirmed| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
        command
            .args(["setup", "remote", "systemd", "--install-ciphera-dropin"])
            .arg(dir.path());
        if confirmed {
            command.arg("--confirm");
        }
        command.output().unwrap()
    };
    let path = dir.path().join("ciphera.conf");
    assert!(!run(false).status.success());
    assert!(!path.exists());
    assert!(run(true).status.success());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        include_str!("../deploy/systemd/ciphera.conf")
    );
    std::fs::write(&path, "operator drop-in").unwrap();
    assert!(!run(true).status.success());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "operator drop-in");
}

#[test]
fn remote_env_template_is_opt_in_nonsecret_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let run = |confirm: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
        command
            .args(["setup", "remote", "systemd", "--install-env-template"])
            .arg(dir.path())
            .env("DISCORD_MCP_KEY", "private-key-marker")
            .env("DISCORD_MCP_DISCORD_CLIENT_SECRET", "private-secret-marker");
        if confirm {
            command.arg("--confirm");
        }
        command.output().unwrap()
    };
    let path = dir.path().join("service.env.example");
    assert!(!run(false).status.success());
    assert!(!path.exists());
    assert!(run(true).status.success());
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(contents.lines().all(|line| line.starts_with('#')));
    assert!(!contents.contains("private-key-marker"));
    assert!(!contents.contains("private-secret-marker"));
    std::fs::write(&path, "operator content").unwrap();
    assert!(!run(true).status.success());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "operator content");
}

#[cfg(unix)]
#[test]
fn storage_preflight_is_read_only_and_rejects_unsafe_metadata() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().join("private-path-marker");
    let check = |path: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .args(["setup", "remote", "--check-storage"])
            .env("DISCORD_MCP_DB", path)
            .output()
            .unwrap()
    };
    let missing = check(&path);
    assert!(!missing.status.success());
    assert!(!path.exists());
    assert!(!String::from_utf8_lossy(&missing.stderr).contains("private-path-marker"));
    std::fs::write(&path, "unchanged").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!check(&path).status.success());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let private = check(&path);
    assert!(
        private.status.success(),
        "{}",
        String::from_utf8_lossy(&private.stderr)
    );
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!check(&path).status.success());
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let directory_link = dir.path().join("directory-link");
    symlink(dir.path(), &directory_link).unwrap();
    assert!(
        !check(&directory_link.join("private-path-marker"))
            .status
            .success()
    );
    let link = dir.path().join("link");
    symlink(&path, &link).unwrap();
    assert!(!check(&link).status.success());
    let hardlink = dir.path().join("hardlink");
    std::fs::hard_link(&path, &hardlink).unwrap();
    assert!(!check(&path).status.success());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "unchanged");
}

#[cfg(unix)]
#[test]
fn unit_install_rejects_group_or_world_writable_directories() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    for mode in [0o770, 0o707] {
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .args(["setup", "remote", "systemd", "--install-unit"])
            .arg(dir.path())
            .arg("--confirm")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("writable by group or others"));
        assert!(!dir.path().join("discord-mcp.service").exists());
    }
}

#[cfg(unix)]
#[test]
fn key_generation_refuses_terminal_stdout() {
    let script = r#"
import os, pty, subprocess, sys
master, slave = pty.openpty()
try:
    child = subprocess.run([sys.argv[1], 'setup', 'local', '--generate-key', '--confirm'],
                           stdout=slave, stderr=subprocess.PIPE, timeout=10)
    os.close(slave)
    slave = -1
    try:
        output = os.read(master, 4096)
    except OSError:
        output = b''
    if child.returncode == 0 or output or b'refusing to print a key' not in child.stderr:
        sys.exit(1)
finally:
    if slave >= 0:
        os.close(slave)
    os.close(master)
"#;
    let output = Command::new("python3")
        .args(["-c", script, env!("CARGO_BIN_EXE_discord-mcp")])
        .output()
        .unwrap();
    assert!(output.status.success(), "terminal safety test failed");
}

#[test]
fn key_generation_requires_confirmation_and_does_not_change_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("existing");
    std::fs::write(&existing, "keep").unwrap();
    let run = |arg: &str| {
        Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .args(["setup", "local", "--generate-key", arg])
            .current_dir(dir.path())
            .output()
            .unwrap()
    };
    let preview = run("--preview");
    assert!(preview.status.success());
    assert!(preview.stdout.is_empty());
    assert!(!run("--invalid").status.success());
    let first = run("--confirm");
    let second = run("--confirm");
    assert!(first.status.success() && second.status.success());
    let first_key = String::from_utf8(first.stdout).unwrap();
    let second_key = String::from_utf8(second.stdout).unwrap();
    assert_eq!(hex::decode(first_key.trim()).unwrap().len(), 32);
    assert_ne!(first_key, second_key);
    assert_eq!(std::fs::read_to_string(existing).unwrap(), "keep");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn unit_install_rejects_symlinks_without_modifying_their_targets() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let real_dir = root.path().join("real");
    std::fs::create_dir(&real_dir).unwrap();
    let linked_dir = root.path().join("linked");
    symlink(&real_dir, &linked_dir).unwrap();
    let run = |path: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .args(["setup", "remote", "systemd", "--install-unit"])
            .arg(path)
            .arg("--confirm")
            .output()
            .unwrap()
    };
    assert!(!run(&linked_dir).status.success());
    assert!(!real_dir.join("discord-mcp.service").exists());

    let protected = root.path().join("existing");
    std::fs::write(&protected, "unchanged").unwrap();
    let unit = real_dir.join("discord-mcp.service");
    symlink(&protected, &unit).unwrap();
    assert!(!run(&real_dir).status.success());
    assert_eq!(std::fs::read_to_string(&protected).unwrap(), "unchanged");
    assert!(
        std::fs::symlink_metadata(unit)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn concurrent_unit_installations_have_one_winner_and_complete_unit() {
    let dir = tempfile::tempdir().unwrap();
    let spawn = || {
        Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .args(["setup", "remote", "systemd", "--install-unit"])
            .arg(dir.path())
            .arg("--confirm")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut first = spawn();
    let mut second = spawn();
    let winners = usize::from(first.wait().unwrap().success())
        + usize::from(second.wait().unwrap().success());
    assert_eq!(winners, 1);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("discord-mcp.service")).unwrap(),
        include_str!("../deploy/systemd/discord-mcp.service")
    );
}

#[test]
fn unit_install_requires_confirmation_and_preserves_existing_unit() {
    let dir = tempfile::tempdir().unwrap();
    let binary = env!("CARGO_BIN_EXE_discord-mcp");
    let run = |confirmed| {
        let mut cmd = Command::new(binary);
        cmd.args(["setup", "remote", "systemd", "--install-unit"])
            .arg(dir.path());
        if confirmed {
            cmd.arg("--confirm");
        }
        cmd.output().unwrap()
    };
    assert!(!run(false).status.success());
    let unit = dir.path().join("discord-mcp.service");
    assert!(!unit.exists());
    assert!(run(true).status.success());
    assert_eq!(
        std::fs::read_to_string(&unit).unwrap(),
        include_str!("../deploy/systemd/discord-mcp.service")
    );
    std::fs::write(&unit, "existing operator unit").unwrap();
    assert!(!run(true).status.success());
    assert_eq!(
        std::fs::read_to_string(&unit).unwrap(),
        "existing operator unit"
    );
}

#[test]
fn systemd_preparation_requires_confirmation_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("bundle");
    let binary = env!("CARGO_BIN_EXE_discord-mcp");
    let preview = Command::new(binary)
        .args(["setup", "remote", "systemd", "--preview"])
        .output()
        .unwrap();
    assert!(preview.status.success());
    let preview = String::from_utf8(preview.stdout).unwrap();
    assert!(preview.contains("no remote connection or installation"));
    assert!(preview.contains("ExecStart=/usr/local/bin/discord-mcp serve"));
    let missing = Command::new(binary)
        .args(["setup", "remote", "systemd"])
        .arg(&dest)
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(!dest.exists());
    let prepare = || {
        Command::new(binary)
            .args(["setup", "remote", "systemd"])
            .arg(&dest)
            .arg("--confirm")
            .output()
            .unwrap()
    };
    assert!(prepare().status.success());
    assert_eq!(
        std::fs::read_to_string(dest.join("discord-mcp.service")).unwrap(),
        include_str!("../deploy/systemd/discord-mcp.service")
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("SETUP.md")).unwrap(),
        include_str!("../deploy/systemd/SETUP.md")
    );
    assert!(!prepare().status.success());
    assert!(dest.join("SETUP.md").is_file());
}

#[test]
fn local_init_creates_storage_and_preserves_existing_channels() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("channels.db");
    let key = [42; 32];
    let init = || {
        Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
            .args(["setup", "local", "--init"])
            .env("DISCORD_MCP_DB", &db)
            .env("DISCORD_MCP_KEY", hex::encode(key))
            .output()
            .unwrap()
    };
    assert!(init().status.success());
    let store = discord_mcp::channels::ChannelStore::open(&db, &key).unwrap();
    store.put_bot("example", "123", "synthetic-token").unwrap();
    drop(store);
    assert!(init().status.success());
    let store = discord_mcp::channels::ChannelStore::open(&db, &key).unwrap();
    assert_eq!(store.list().unwrap(), vec!["example"]);
}

#[test]
fn setup_local_reports_presence_without_disclosing_values() {
    let key = "ab".repeat(32);
    let output = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .args(["setup", "local"])
        .env("DISCORD_MCP_DB", "private-path-marker")
        .env("DISCORD_MCP_KEY", &key)
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("present (not validated)"));
    assert!(!text.contains("private-path-marker"));
    assert!(!text.contains(&key));
    assert!(text.contains("format: valid"));
}

#[test]
fn setup_local_rejects_invalid_key_without_disclosing_it() {
    let output = Command::new(env!("CARGO_BIN_EXE_discord-mcp"))
        .args(["setup", "local"])
        .env("DISCORD_MCP_KEY", "private-invalid-marker")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("private-invalid-marker")
    );
}

#[test]
fn remote_setup_is_read_only_and_unknown_mode_fails() {
    let binary = env!("CARGO_BIN_EXE_discord-mcp");
    let output = Command::new(binary)
        .args(["setup", "remote"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("no connection or server changes")
    );
    let output = Command::new(binary)
        .args(["setup", "unknown"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn remote_preflight_checks_presence_and_rejects_public_listener_without_leaks() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_discord-mcp"));
    command.args(["setup", "remote", "--check"]);
    for name in [
        "DISCORD_MCP_DB",
        "DISCORD_MCP_PUBLIC_ORIGIN",
        "DISCORD_MCP_DISCORD_CLIENT_ID",
        "DISCORD_MCP_DISCORD_CLIENT_SECRET",
        "DISCORD_MCP_ADMIN_ID",
        "DISCORD_MCP_CLIENT_ID",
        "DISCORD_MCP_CLIENT_NAME",
        "DISCORD_MCP_CLIENT_REDIRECT",
    ] {
        command.env(name, "private-marker");
    }
    command.env("DISCORD_MCP_KEY", "ab".repeat(32));
    command.env("DISCORD_MCP_LISTEN", "0.0.0.0:8000");
    let output = command.output().unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-marker"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("loopback"));
    command.env("DISCORD_MCP_LISTEN", "127.0.0.1:8000");
    let output = command.output().unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-marker"));
    command.env("DISCORD_MCP_PUBLIC_ORIGIN", "http://127.0.0.1:8000");
    command.env("DISCORD_MCP_DISCORD_CLIENT_ID", "123");
    command.env("DISCORD_MCP_ADMIN_ID", "456");
    command.env(
        "DISCORD_MCP_CLIENT_REDIRECT",
        "http://127.0.0.1:8765/callback",
    );
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("were not tested"));
}
