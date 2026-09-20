//! Application-wide error type and its mapping to process exit codes.

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("docker is not available: {0}")]
    DockerUnavailable(String),
    #[error("docker command failed: `{command}`: {stderr}")]
    DockerCommandFailed { command: String, stderr: String },
    #[error("required tool not found: {0}")]
    ToolMissing(String),
    #[error("tool command failed: `{command}`: {stderr}")]
    ToolFailed { command: String, stderr: String },
    #[error("invalid manifest: {0}")]
    ManifestInvalid(String),
    #[error("unsupported manifest schema version {0}")]
    ManifestUnsupportedVersion(u32),
    #[error("backup verification failed: {missing} missing, {corrupt} corrupt")]
    VerificationFailed { missing: usize, corrupt: usize },
    #[error("{0}")]
    Conflict(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    /// Process exit code for this error (see spec section 5).
    pub fn exit_code(&self) -> i32 {
        match self {
            AppError::DockerUnavailable(_) | AppError::ToolMissing(_) => 3,
            AppError::Conflict(_) => 2,
            _ => 1,
        }
    }

    /// Stable machine-readable identifier used in JSON error output.
    pub fn kind(&self) -> &'static str {
        match self {
            AppError::DockerUnavailable(_) => "docker_unavailable",
            AppError::DockerCommandFailed { .. } => "docker_command_failed",
            AppError::ToolMissing(_) => "tool_missing",
            AppError::ToolFailed { .. } => "tool_failed",
            AppError::ManifestInvalid(_) => "manifest_invalid",
            AppError::ManifestUnsupportedVersion(_) => "manifest_unsupported_version",
            AppError::VerificationFailed { .. } => "verification_failed",
            AppError::Conflict(_) => "conflict",
            AppError::Io(_) => "io",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_the_spec() {
        assert_eq!(AppError::DockerUnavailable("x".into()).exit_code(), 3);
        assert_eq!(AppError::ToolMissing("bzip2".into()).exit_code(), 3);
        assert_eq!(AppError::Conflict("bad".into()).exit_code(), 2);
        assert_eq!(AppError::ManifestInvalid("bad".into()).exit_code(), 1);
        assert_eq!(
            AppError::VerificationFailed {
                missing: 1,
                corrupt: 0
            }
            .exit_code(),
            1
        );
    }

    #[test]
    fn verification_failed_message_mentions_counts() {
        let message = AppError::VerificationFailed {
            missing: 1,
            corrupt: 2,
        }
        .to_string();
        assert!(message.contains("1 missing"));
        assert!(message.contains("2 corrupt"));
    }

    #[test]
    fn kind_is_stable_snake_case() {
        assert_eq!(
            AppError::DockerUnavailable("x".into()).kind(),
            "docker_unavailable"
        );
        assert_eq!(AppError::Io(std::io::Error::other("x")).kind(), "io");
    }
}
