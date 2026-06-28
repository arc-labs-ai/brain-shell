//! Startup credential resolution — fail-closed.
//!
//! The shell authenticates with a mandatory API-key token; there is no
//! anonymous connect. This module resolves the effective `(server,
//! token)` pair from the flags, env, and config, in a fixed precedence:
//!
//! 1. `--profile <name>` (or `BRAIN_PROFILE`) → that profile's server +
//!    token. An unknown name is an error listing the known profiles.
//! 2. `--token` / `BRAIN_TOKEN` → that token, with the server from
//!    `--server` (if explicitly set) else `settings.server` else the
//!    default endpoint.
//! 3. `settings.active_profile` → that profile's server + token.
//! 4. Otherwise → fail. No credential, no connection.
//!
//! The resolver is pure: it takes the loaded [`Config`] and the gathered
//! inputs and returns the resolved credential or a typed error. The
//! caller turns the error into a non-zero exit.

use crate::cli::config::Config;
use crate::parser::command::DEFAULT_SERVER;

/// Inputs gathered from the CLI invocation. Keeping them in a struct
/// makes the call site readable and the tests tidy.
#[derive(Debug, Default, Clone)]
pub struct CredentialInputs<'a> {
    /// `--profile <name>` / `BRAIN_PROFILE`.
    pub profile_flag: Option<&'a str>,
    /// `--token` / `BRAIN_TOKEN`, already filtered to non-empty.
    pub token: Option<&'a str>,
    /// `--server` only when explicitly set (i.e. differs from the
    /// built-in default). `None` lets the token path fall back to the
    /// persisted setting / default.
    pub server_override: Option<&'a str>,
}

/// Where the resolved credential came from — surfaced in diagnostics
/// and the banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource {
    /// `--profile <name>` / `BRAIN_PROFILE`.
    ProfileFlag(String),
    /// `--token` / `BRAIN_TOKEN`.
    Token,
    /// `settings.active_profile`.
    ActiveProfile(String),
}

impl CredentialSource {
    /// Name of the credential profile in play, if a profile was used.
    /// `None` for the bare-token path (no profile name exists).
    #[must_use]
    pub fn profile_name(&self) -> Option<&str> {
        match self {
            CredentialSource::ProfileFlag(n) | CredentialSource::ActiveProfile(n) => Some(n),
            CredentialSource::Token => None,
        }
    }
}

/// The resolved connection credential.
#[derive(Debug, Clone)]
pub struct ResolvedCredential {
    pub server: String,
    pub token: String,
    pub source: CredentialSource,
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("unknown profile '{name}'. known profiles: {}", fmt_known(known))]
    UnknownProfile { name: String, known: Vec<String> },

    #[error(
        "no credential: pass --token, set BRAIN_TOKEN, or select a profile \
         (\\profile use <name> / [profiles.*] in config)"
    )]
    NoCredential,
}

fn fmt_known(known: &[String]) -> String {
    if known.is_empty() {
        "(none configured)".to_string()
    } else {
        known.join(", ")
    }
}

/// Resolve the effective credential. See the module docs for the
/// precedence. Fail-closed: returns `NoCredential` rather than ever
/// connecting without a token.
pub fn resolve(
    config: &Config,
    inputs: &CredentialInputs<'_>,
) -> Result<ResolvedCredential, CredentialError> {
    // 1. --profile <name> (or BRAIN_PROFILE).
    if let Some(name) = inputs.profile_flag.filter(|s| !s.is_empty()) {
        let entry = config
            .get_profile(name)
            .map_err(|_| CredentialError::UnknownProfile {
                name: name.to_owned(),
                known: config.profiles().keys().cloned().collect(),
            })?;
        return Ok(ResolvedCredential {
            server: entry.server.clone(),
            token: entry.token.clone(),
            source: CredentialSource::ProfileFlag(name.to_owned()),
        });
    }

    // 2. --token / BRAIN_TOKEN.
    if let Some(token) = inputs.token {
        let server = inputs
            .server_override
            .map(str::to_owned)
            .or_else(|| config.settings().server.clone())
            .unwrap_or_else(|| DEFAULT_SERVER.to_owned());
        return Ok(ResolvedCredential {
            server,
            token: token.to_owned(),
            source: CredentialSource::Token,
        });
    }

    // 3. settings.active_profile.
    if let Some(name) = config.active_profile() {
        // active_profile is validated on write, but a hand-edited file
        // could dangle; surface that as an unknown-profile error.
        let entry = config
            .get_profile(name)
            .map_err(|_| CredentialError::UnknownProfile {
                name: name.to_owned(),
                known: config.profiles().keys().cloned().collect(),
            })?;
        return Ok(ResolvedCredential {
            server: entry.server.clone(),
            token: entry.token.clone(),
            source: CredentialSource::ActiveProfile(name.to_owned()),
        });
    }

    // 4. fail-closed.
    Err(CredentialError::NoCredential)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::config::{path_in, Config};
    use tempfile::TempDir;

    fn config_with(profiles: &[(&str, &str, &str)], active: Option<&str>) -> Config {
        let t = TempDir::new().unwrap();
        let mut c = Config::load_or_default_at(&path_in(t.path())).unwrap();
        for (name, server, token) in profiles {
            c.add_profile(name, server, token).unwrap();
        }
        if let Some(a) = active {
            c.set_active_profile(a).unwrap();
        }
        c
    }

    #[test]
    fn profile_flag_wins() {
        let c = config_with(&[("work", "10.0.0.1:9090", "tok-work")], Some("work"));
        let r = resolve(
            &c,
            &CredentialInputs {
                profile_flag: Some("work"),
                token: Some("ignored"),
                server_override: Some("9.9.9.9:1"),
            },
        )
        .unwrap();
        assert_eq!(r.server, "10.0.0.1:9090");
        assert_eq!(r.token, "tok-work");
        assert_eq!(r.source, CredentialSource::ProfileFlag("work".into()));
    }

    #[test]
    fn unknown_profile_flag_errors_with_known_list() {
        let c = config_with(&[("work", "h:1", "t")], None);
        let err = resolve(
            &c,
            &CredentialInputs {
                profile_flag: Some("prod"),
                ..Default::default()
            },
        )
        .unwrap_err();
        match err {
            CredentialError::UnknownProfile { name, known } => {
                assert_eq!(name, "prod");
                assert_eq!(known, vec!["work".to_string()]);
            }
            other => panic!("expected UnknownProfile, got {other:?}"),
        }
    }

    #[test]
    fn token_path_uses_server_override_then_setting_then_default() {
        // override wins
        let c = config_with(&[], None);
        let r = resolve(
            &c,
            &CredentialInputs {
                token: Some("tok"),
                server_override: Some("1.2.3.4:5"),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(r.server, "1.2.3.4:5");
        assert_eq!(r.token, "tok");
        assert_eq!(r.source, CredentialSource::Token);

        // setting wins when no override
        let mut c2 = config_with(&[], None);
        c2.set("server", "5.6.7.8:9").unwrap();
        let r2 = resolve(
            &c2,
            &CredentialInputs {
                token: Some("tok"),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(r2.server, "5.6.7.8:9");

        // default when neither
        let c3 = config_with(&[], None);
        let r3 = resolve(
            &c3,
            &CredentialInputs {
                token: Some("tok"),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(r3.server, DEFAULT_SERVER);
    }

    #[test]
    fn active_profile_used_when_no_flag_or_token() {
        let c = config_with(&[("work", "10.0.0.1:9090", "tok-work")], Some("work"));
        let r = resolve(&c, &CredentialInputs::default()).unwrap();
        assert_eq!(r.server, "10.0.0.1:9090");
        assert_eq!(r.token, "tok-work");
        assert_eq!(r.source, CredentialSource::ActiveProfile("work".into()));
    }

    #[test]
    fn token_beats_active_profile() {
        let c = config_with(&[("work", "10.0.0.1:9090", "tok-work")], Some("work"));
        let r = resolve(
            &c,
            &CredentialInputs {
                token: Some("override-tok"),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(r.token, "override-tok");
        assert_eq!(r.source, CredentialSource::Token);
    }

    #[test]
    fn no_credential_fails_closed() {
        let c = config_with(&[], None);
        let err = resolve(&c, &CredentialInputs::default()).unwrap_err();
        assert!(matches!(err, CredentialError::NoCredential), "got {err:?}");
    }
}
