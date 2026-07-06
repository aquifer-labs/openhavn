// SPDX-License-Identifier: Apache-2.0

//! The MCP admission gate: deterministic, typed checks run on a server registration *before* any
//! config is written — never prompt-based, so every decision (admit or one specific
//! [`Rejection`]) is mechanically loggable to the equipment log. Mirrors
//! `commands::skill::gate`.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

/// A server registration that cleared every gate check. Carries non-blocking warnings (e.g. an
/// env value that looks like an inline secret) for the caller to print.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdmittedServer {
    pub warnings: Vec<String>,
}

/// Every way the admission gate can refuse an `mcp add` — typed, never a free-form message only,
/// so callers (and the equipment log) can match on *why* deterministically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    UnsafeName { name: String },
    CommandNotFound { command: String },
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rejection::UnsafeName { name } => write!(
                f,
                "server name {name:?} is not a safe slug (expected [a-z0-9-_]+)"
            ),
            Rejection::CommandNotFound { command } => write!(
                f,
                "command {command:?} does not resolve (not an absolute executable path, and not \
                 found on PATH)"
            ),
        }
    }
}

impl std::error::Error for Rejection {}

pub enum GateOutcome {
    Admitted(AdmittedServer),
    Rejected(Rejection),
}

/// Run the deterministic admission checks, in order: the name is a safe slug, then the command
/// resolves (an absolute executable path, or a bare name found on `PATH`). Env values are then
/// scanned for inline-secret-shaped strings — those only ever produce a warning, never a
/// rejection.
pub fn evaluate(
    name: &str,
    command: &str,
    env: &BTreeMap<String, String>,
    path_env: Option<&OsStr>,
) -> GateOutcome {
    if !is_safe_slug(name) {
        return GateOutcome::Rejected(Rejection::UnsafeName {
            name: name.to_string(),
        });
    }
    if resolve_command(command, path_env).is_none() {
        return GateOutcome::Rejected(Rejection::CommandNotFound {
            command: command.to_string(),
        });
    }
    let warnings = env
        .iter()
        .filter(|(_, value)| looks_like_secret(value))
        .map(|(key, _)| {
            format!(
                "env {key} looks like it may carry an inline secret; consider an env-file \
                 indirection instead of passing it on the command line"
            )
        })
        .collect();
    GateOutcome::Admitted(AdmittedServer { warnings })
}

fn is_safe_slug(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// An absolute path resolves only if it exists and is executable; a bare command name resolves
/// only by being found (and executable) on one of `path_env`'s directories.
pub fn resolve_command(command: &str, path_env: Option<&OsStr>) -> Option<PathBuf> {
    let candidate = Path::new(command);
    if candidate.is_absolute() {
        return is_executable(candidate).then(|| candidate.to_path_buf());
    }
    let paths = path_env?;
    std::env::split_paths(paths)
        .map(|dir| dir.join(command))
        .find(|c| is_executable(c))
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// An env VALUE looks like it carries an inline secret when it embeds a `key=`/`token=`/
/// `secret=`/`password=`-shaped assignment (case-insensitive), or is a long (>32 char) run of
/// base64-ish characters.
fn looks_like_secret(value: &str) -> bool {
    let lower = value.to_lowercase();
    let has_inline_kv = ["key=", "token=", "secret=", "password="]
        .iter()
        .any(|needle| lower.contains(needle));
    let is_long_base64ish = value.len() > 32
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-'));
    has_inline_kv || is_long_base64ish
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_name_is_rejected() {
        let outcome = evaluate("Not A Slug!", "/bin/echo", &BTreeMap::new(), None);
        assert!(matches!(
            outcome,
            GateOutcome::Rejected(Rejection::UnsafeName { .. })
        ));
    }

    #[test]
    fn missing_command_is_rejected() {
        let outcome = evaluate(
            "demo",
            "/definitely/does/not/exist/binary",
            &BTreeMap::new(),
            None,
        );
        assert!(matches!(
            outcome,
            GateOutcome::Rejected(Rejection::CommandNotFound { .. })
        ));

        // A bare (non-absolute) name with PATH search disabled must also be rejected.
        let outcome = evaluate("demo", "totally-bogus-command-name", &BTreeMap::new(), None);
        assert!(matches!(
            outcome,
            GateOutcome::Rejected(Rejection::CommandNotFound { .. })
        ));
    }

    #[test]
    fn absolute_executable_command_admits() {
        let outcome = evaluate("demo", "/bin/echo", &BTreeMap::new(), None);
        assert!(matches!(outcome, GateOutcome::Admitted(_)));
    }

    #[test]
    fn secret_looking_env_value_warns_but_still_admits() {
        let mut env = BTreeMap::new();
        env.insert("EXTRA".to_string(), "token=abc123def456".to_string());
        let GateOutcome::Admitted(admitted) = evaluate("demo", "/bin/echo", &env, None) else {
            panic!("a secret-looking env value must warn, not reject");
        };
        assert_eq!(admitted.warnings.len(), 1);
        assert!(admitted.warnings[0].contains("EXTRA"));
    }

    #[test]
    fn long_base64ish_env_value_warns() {
        let mut env = BTreeMap::new();
        env.insert(
            "API_KEY".to_string(),
            "QWxhZGRpbjpvcGVuIHNlc2FtZQ==QWxhZGRpbjpvcGVu".to_string(),
        );
        let GateOutcome::Admitted(admitted) = evaluate("demo", "/bin/echo", &env, None) else {
            panic!("expected admission");
        };
        assert_eq!(admitted.warnings.len(), 1);
    }

    #[test]
    fn ordinary_env_value_has_no_warning() {
        let mut env = BTreeMap::new();
        env.insert("LOG_LEVEL".to_string(), "debug".to_string());
        let GateOutcome::Admitted(admitted) = evaluate("demo", "/bin/echo", &env, None) else {
            panic!("expected admission");
        };
        assert!(admitted.warnings.is_empty());
    }

    #[test]
    fn bare_command_resolves_via_injected_path() {
        let dir =
            std::env::temp_dir().join(format!("openhavn-mcp-gate-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("my-tool");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path_env = std::ffi::OsString::from(dir.display().to_string());
        assert!(resolve_command("my-tool", Some(&path_env)).is_some());
        assert!(resolve_command("my-tool", None).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
