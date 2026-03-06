use thiserror::Error;

use crate::protocol::RpcError;

/// Top-level application error.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("collector error: {0}")]
    Collector(#[from] CollectorError),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("ipc error: {0}")]
    Ipc(#[from] IpcError),

    #[error("config error: {0}")]
    Config(String),
}

/// Errors from the data collector subsystem.
#[derive(Debug, Error)]
pub enum CollectorError {
    #[error("failed to parse log line: {0}")]
    LogParse(String),

    #[error("log file watch error: {0}")]
    LogWatch(String),

    #[error("log file not found: {path}")]
    LogNotFound { path: String },
}

/// Errors from SQLite storage.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(String),

    #[error("migration failed at version {version}: {reason}")]
    Migration { version: u32, reason: String },

    #[error("database not found at {path}")]
    NotFound { path: String },

    #[error("query error: {0}")]
    Query(String),
}

/// Errors from the IPC layer.
#[derive(Debug, Error)]
pub enum IpcError {
    #[error("connection failed: {0}")]
    Connection(String),

    #[error("socket bind failed at {path}: {reason}")]
    SocketBind { path: String, reason: String },

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("deserialization error: {0}")]
    Deserialization(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("request timeout after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },

    #[error("daemon not running (socket not found at {path})")]
    DaemonNotRunning { path: String },
}

impl IpcError {
    pub fn to_rpc_error(&self) -> RpcError {
        let code = match self {
            IpcError::Serialization(_) | IpcError::Deserialization(_) => -32700,
            IpcError::Protocol(_) => -32600,
            _ => -32603,
        };
        RpcError {
            code,
            message: self.to_string(),
            data: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ipc_error_to_rpc_error_serialization() {
        let err = IpcError::Serialization("bad json".to_string());
        let rpc = err.to_rpc_error();
        assert_eq!(rpc.code, -32700);
    }

    #[test]
    fn test_ipc_error_to_rpc_error_deserialization() {
        let err = IpcError::Deserialization("parse failed".to_string());
        let rpc = err.to_rpc_error();
        assert_eq!(rpc.code, -32700);
    }

    #[test]
    fn test_ipc_error_to_rpc_error_protocol() {
        let err = IpcError::Protocol("bad frame".to_string());
        let rpc = err.to_rpc_error();
        assert_eq!(rpc.code, -32600);
    }

    #[test]
    fn test_ipc_error_to_rpc_error_connection() {
        let err = IpcError::Connection("refused".to_string());
        let rpc = err.to_rpc_error();
        assert_eq!(rpc.code, -32603);
    }

    #[test]
    fn test_ipc_error_to_rpc_error_timeout() {
        let err = IpcError::Timeout { timeout_ms: 5000 };
        let rpc = err.to_rpc_error();
        assert_eq!(rpc.code, -32603);
    }

    #[test]
    fn test_ipc_error_to_rpc_error_daemon_not_running() {
        let err = IpcError::DaemonNotRunning {
            path: "/tmp/test.sock".to_string(),
        };
        let rpc = err.to_rpc_error();
        assert_eq!(rpc.code, -32603);
        assert!(rpc.message.contains("/tmp/test.sock"));
    }

    #[test]
    fn test_app_error_from_collector() {
        let collector_err = CollectorError::LogParse("bad line".to_string());
        let app_err: AppError = collector_err.into();
        assert!(matches!(app_err, AppError::Collector(_)));
        assert!(app_err.to_string().contains("bad line"));
    }

    #[test]
    fn test_app_error_from_storage() {
        let storage_err = StorageError::Query("bad SQL".to_string());
        let app_err: AppError = storage_err.into();
        assert!(matches!(app_err, AppError::Storage(_)));
    }

    #[test]
    fn test_app_error_from_ipc() {
        let ipc_err = IpcError::Connection("refused".to_string());
        let app_err: AppError = ipc_err.into();
        assert!(matches!(app_err, AppError::Ipc(_)));
    }

    #[test]
    fn test_collector_error_display() {
        let err = CollectorError::LogWatch("inotify failed".to_string());
        assert_eq!(err.to_string(), "log file watch error: inotify failed");
    }

    #[test]
    fn test_storage_error_display() {
        let err = StorageError::Migration {
            version: 2,
            reason: "column missing".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "migration failed at version 2: column missing"
        );
    }
}
