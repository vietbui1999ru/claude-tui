use std::path::PathBuf;

/// Resolve the Unix domain socket path for daemon IPC.
///
/// Search order:
/// 1. `CLAUDE_DAEMON_SOCKET` env var
/// 2. `$XDG_RUNTIME_DIR/claude-daemon.sock`
/// 3. `/tmp/claude-daemon-{uid}.sock`
pub fn socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("CLAUDE_DAEMON_SOCKET") {
        return PathBuf::from(path);
    }
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("claude-daemon.sock");
    }
    // Safety: getuid() is always safe to call and has no preconditions.
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/claude-daemon-{uid}.sock"))
}

/// Resolve the SQLite database path.
///
/// Search order:
/// 1. `$XDG_DATA_HOME/claude-daemon/usage.db`
/// 2. `~/.local/share/claude-daemon/usage.db`
pub fn db_path() -> PathBuf {
    if let Ok(data_home) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(data_home)
            .join("claude-daemon")
            .join("usage.db");
    }
    if let Some(home) = home_dir() {
        return home
            .join(".local")
            .join("share")
            .join("claude-daemon")
            .join("usage.db");
    }
    PathBuf::from("/tmp/claude-daemon/usage.db")
}

/// Resolve the config file path.
///
/// Returns `~/.config/claude-daemon/config.toml`.
pub fn config_path() -> PathBuf {
    if let Ok(config_home) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(config_home)
            .join("claude-daemon")
            .join("config.toml");
    }
    if let Some(home) = home_dir() {
        return home
            .join(".config")
            .join("claude-daemon")
            .join("config.toml");
    }
    PathBuf::from("/tmp/claude-daemon/config.toml")
}

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn test_socket_path_env_override() {
        let _guard = EnvGuard::set("CLAUDE_DAEMON_SOCKET", "/custom/path.sock");
        assert_eq!(socket_path(), PathBuf::from("/custom/path.sock"));
    }

    #[test]
    #[serial]
    fn test_db_path_returns_valid_path() {
        let path = db_path();
        assert!(path.to_str().unwrap().contains("usage.db"));
    }

    #[test]
    #[serial]
    fn test_config_path_returns_toml() {
        let path = config_path();
        assert!(path.to_str().unwrap().ends_with("config.toml"));
    }

    /// RAII guard for setting/restoring an env var in tests.
    struct EnvGuard {
        key: String,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            // Safety: only used in serial unit tests, not in multi-threaded contexts.
            unsafe { std::env::set_var(key, value) };
            Self {
                key: key.to_string(),
                prev,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // Safety: only used in serial unit tests.
            match &self.prev {
                Some(v) => unsafe { std::env::set_var(&self.key, v) },
                None => unsafe { std::env::remove_var(&self.key) },
            }
        }
    }
}
