//! Brain shell on-disk configuration.
//!
//! One TOML file at `${XDG_CONFIG_HOME:-~/.config}/brain/config.toml`
//! holding two concerns:
//!
//! - `[settings]` — persistent shell preferences (output format,
//!   timing, sticky context, default server, the sticky active profile).
//! - `[profiles.<name>]` — named credential profiles. Each profile pins
//!   a `server` endpoint and an API-key `token`. Profiles are how the
//!   shell authenticates: there is no anonymous connect.
//!
//! Profiles hold secrets, so the file is written `chmod 600` and the
//! `token` is never surfaced by `Debug` or by any `show` / `list`
//! command (those redact it).

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Default filename inside the config dir.
pub const FILE_NAME: &str = "config.toml";

/// Header comment written above an entirely-new file. Edits via
/// `set` / profile CRUD preserve the rest of the file but rewrite
/// this header on every save so the most current usage notes are
/// always at the top.
const HEADER: &str = "\
# Brain shell configuration.
#
# Created on first run. Edit by hand or via `brain config set`
# and `brain profile add`. Delete the file to reset everything.
#
# Settings are persistent shell preferences. Credential profiles under
# [profiles.<name>] each pin a server endpoint and an API-key token —
# the shell authenticates with one of them. Pass `--profile <name>`,
# set `settings.active_profile`, or pass `--token` / BRAIN_TOKEN. This
# file holds secrets and is written chmod 600.

";

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// Top-level config: persistent `[settings]` plus the credential
/// `[profiles.*]`. Unknown top-level sections are tolerated and dropped
/// — a config written by a different/older build (e.g. one carrying a
/// long-gone `[agents.*]` section) must not brick the tool. The stale
/// section is simply ignored on load and disappears on the next save.
/// Typos that matter are still caught: `[settings]`/`[profiles.*]` keep
/// `deny_unknown_fields`, and malformed TOML is still a hard error.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigFile {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileEntry>,
}

/// Closed set of persistent shell preferences. Adding a key takes
/// one line; unknown keys in the file are rejected so typos
/// surface as errors rather than ghost-persisting forever.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputPref>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sticky_context: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Name of the profile the shell defaults to when no `--profile`
    /// flag and no `--token` are supplied. Set by `profile use <name>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,
}

/// `output` preference. Mirror of the existing `OutputFormatArg` so
/// the wire schema is independent of clap's value-enum names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputPref {
    Table,
    Json,
}

/// A named credential profile: a server endpoint plus the API-key
/// token used to authenticate to it.
///
/// The `Debug` impl deliberately redacts the token — this struct is
/// reachable from diagnostics and a leaked secret in a log line is a
/// real hazard.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileEntry {
    /// Wire endpoint (`host:port`) the profile connects to.
    pub server: String,
    /// API-key token presented at handshake. Never rendered.
    pub token: String,
}

impl fmt::Debug for ProfileEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redact the token so it can't leak through any `{:?}` site.
        f.debug_struct("ProfileEntry")
            .field("server", &self.server)
            .field("token", &"<redacted>")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config dir unavailable (XDG_CONFIG_HOME and HOME both unset)")]
    NoConfigDir,

    #[error("config file at {path} is malformed: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("unknown setting key: {key}{hint}", hint = match suggestion { Some(s) => format!(". did you mean '{s}'?"), None => String::new() })]
    UnknownKey {
        key: String,
        suggestion: Option<String>,
    },

    #[error("invalid value for {key}: {detail}")]
    InvalidValue { key: String, detail: String },

    #[error("profile '{name}' already exists")]
    ProfileExists { name: String },

    #[error("unknown profile '{name}'{hint}", hint = match suggestion { Some(s) => format!(". did you mean '{s}'?"), None => String::new() })]
    ProfileUnknown {
        name: String,
        suggestion: Option<String>,
    },

    #[error("profile name '{name}' is invalid: {reason}")]
    ProfileBadName { name: String, reason: &'static str },

    #[error("profile '{name}' has an empty {field}")]
    ProfileEmptyField { name: String, field: &'static str },
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Resolve `<config_dir>/brain/config.toml` via `dirs::config_dir()`.
/// Returns `None` if no config dir is available.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("brain").join(FILE_NAME))
}

/// Same as [`default_path`] but takes the config-dir root explicitly
/// so tests can drive without touching process env.
#[must_use]
pub fn path_in(config_dir: &Path) -> PathBuf {
    config_dir.join("brain").join(FILE_NAME)
}

// ---------------------------------------------------------------------------
// Top-level handle
// ---------------------------------------------------------------------------

/// Loaded view of `config.toml`. Mutate via the typed setters, then
/// `save` to persist atomically.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub file: ConfigFile,
    pub path: PathBuf,
}

impl Config {
    /// Load from `dirs::config_dir()/brain/config.toml`. Returns a
    /// default Config (empty file) when the path doesn't exist yet.
    pub fn load_or_default() -> Result<Self, ConfigError> {
        let path = default_path().ok_or(ConfigError::NoConfigDir)?;
        Self::load_or_default_at(&path)
    }

    /// As above but takes the path explicitly — testing entry point.
    pub fn load_or_default_at(path: &Path) -> Result<Self, ConfigError> {
        match fs::read_to_string(path) {
            Ok(contents) => {
                let file = toml::from_str::<ConfigFile>(&contents).map_err(|source| {
                    ConfigError::Parse {
                        path: path.to_path_buf(),
                        source,
                    }
                })?;
                Ok(Self {
                    file,
                    path: path.to_path_buf(),
                })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self {
                file: ConfigFile::default(),
                path: path.to_path_buf(),
            }),
            Err(e) => Err(ConfigError::Read {
                path: path.to_path_buf(),
                source: e,
            }),
        }
    }

    /// Atomically render and rewrite the file. Creates the parent
    /// directory if missing, chmod 600 on Unix (profiles hold secrets).
    pub fn save(&self) -> Result<(), ConfigError> {
        save_atomic(&self.path, &self.file)
    }

    pub fn settings(&self) -> &Settings {
        &self.file.settings
    }

    pub fn profiles(&self) -> &BTreeMap<String, ProfileEntry> {
        &self.file.profiles
    }

    // ----- typed setting CRUD ------------------------------------

    /// Validate and apply a `<key> <value>` pair. Caller persists via
    /// [`Self::save`] afterwards.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), ConfigError> {
        match key {
            "output" => {
                let v = match value {
                    "table" => OutputPref::Table,
                    "json" => OutputPref::Json,
                    other => {
                        return Err(ConfigError::InvalidValue {
                            key: key.into(),
                            detail: format!("'{other}' is not one of: table, json"),
                        });
                    }
                };
                self.file.settings.output = Some(v);
            }
            "timing" => {
                let v = match value {
                    "true" | "on" | "1" => true,
                    "false" | "off" | "0" => false,
                    other => {
                        return Err(ConfigError::InvalidValue {
                            key: key.into(),
                            detail: format!("'{other}' is not one of: true|on|1, false|off|0"),
                        });
                    }
                };
                self.file.settings.timing = Some(v);
            }
            "sticky_context" => {
                let v: u64 = value.parse().map_err(|e| ConfigError::InvalidValue {
                    key: key.into(),
                    detail: format!("'{value}' is not a non-negative integer: {e}"),
                })?;
                self.file.settings.sticky_context = Some(v);
            }
            "server" => {
                // Light validation: must contain ':' so it looks like
                // host:port. The full `parse_server` validator lives
                // higher up and depends on the connect path; we keep
                // this loose to avoid duplicating that logic.
                if !value.contains(':') {
                    return Err(ConfigError::InvalidValue {
                        key: key.into(),
                        detail: format!("'{value}' must be host:port"),
                    });
                }
                self.file.settings.server = Some(value.to_owned());
            }
            "active_profile" => {
                // Pointing at a profile that doesn't exist would silently
                // fail-closed at connect; reject it here so the mistake
                // surfaces at the point of the edit.
                if !self.file.profiles.contains_key(value) {
                    return Err(ConfigError::ProfileUnknown {
                        name: value.to_owned(),
                        suggestion: closest_profile(value, &self.file.profiles),
                    });
                }
                self.file.settings.active_profile = Some(value.to_owned());
            }
            other => {
                return Err(ConfigError::UnknownKey {
                    key: other.to_owned(),
                    suggestion: closest_key(other, &KNOWN_KEYS),
                });
            }
        }
        Ok(())
    }

    /// Read back a single value as the string we'd accept in `set`.
    /// Returns the literal `"(unset)"` when missing — chosen over
    /// `Option` so the CLI rendering is uniform without a wrapper.
    pub fn get(&self, key: &str) -> Result<String, ConfigError> {
        match key {
            "output" => Ok(self
                .file
                .settings
                .output
                .map(|o| match o {
                    OutputPref::Table => "table",
                    OutputPref::Json => "json",
                })
                .unwrap_or("(unset)")
                .to_owned()),
            "timing" => Ok(self
                .file
                .settings
                .timing
                .map(|b| if b { "true" } else { "false" })
                .unwrap_or("(unset)")
                .to_owned()),
            "sticky_context" => Ok(self
                .file
                .settings
                .sticky_context
                .map(|n| n.to_string())
                .unwrap_or_else(|| "(unset)".to_owned())),
            "server" => Ok(self
                .file
                .settings
                .server
                .clone()
                .unwrap_or_else(|| "(unset)".to_owned())),
            "active_profile" => Ok(self
                .file
                .settings
                .active_profile
                .clone()
                .unwrap_or_else(|| "(unset)".to_owned())),
            other => Err(ConfigError::UnknownKey {
                key: other.to_owned(),
                suggestion: closest_key(other, &KNOWN_KEYS),
            }),
        }
    }

    /// All known keys with their current values — for `config list`.
    /// Stable ordering matches the module-private `KNOWN_KEYS` array.
    pub fn list(&self) -> Vec<(&'static str, String)> {
        KNOWN_KEYS
            .iter()
            .map(|k| (*k, self.get(k).unwrap_or_else(|_| "(unset)".to_owned())))
            .collect()
    }

    // ----- profile CRUD ------------------------------------------

    /// Look up a profile by name.
    pub fn get_profile(&self, name: &str) -> Result<&ProfileEntry, ConfigError> {
        self.file
            .profiles
            .get(name)
            .ok_or_else(|| ConfigError::ProfileUnknown {
                name: name.to_owned(),
                suggestion: closest_profile(name, &self.file.profiles),
            })
    }

    /// Insert (or replace) a profile. Errors on an invalid name or an
    /// empty server / token — a profile with a blank credential is a
    /// fail-closed trap waiting to happen.
    pub fn set_profile(
        &mut self,
        name: &str,
        server: &str,
        token: &str,
    ) -> Result<(), ConfigError> {
        validate_profile_name(name)?;
        if server.trim().is_empty() {
            return Err(ConfigError::ProfileEmptyField {
                name: name.to_owned(),
                field: "server",
            });
        }
        if token.trim().is_empty() {
            return Err(ConfigError::ProfileEmptyField {
                name: name.to_owned(),
                field: "token",
            });
        }
        self.file.profiles.insert(
            name.to_owned(),
            ProfileEntry {
                server: server.to_owned(),
                token: token.to_owned(),
            },
        );
        Ok(())
    }

    /// Add a profile, refusing to clobber an existing one. `set_profile`
    /// overwrites; `add_profile` is the create-only guard the CLI uses.
    pub fn add_profile(
        &mut self,
        name: &str,
        server: &str,
        token: &str,
    ) -> Result<(), ConfigError> {
        if self.file.profiles.contains_key(name) {
            return Err(ConfigError::ProfileExists {
                name: name.to_owned(),
            });
        }
        self.set_profile(name, server, token)
    }

    /// Remove a profile. Clears `active_profile` if it pointed here so
    /// the file never references a profile that no longer exists.
    pub fn remove_profile(&mut self, name: &str) -> Result<ProfileEntry, ConfigError> {
        let removed =
            self.file
                .profiles
                .remove(name)
                .ok_or_else(|| ConfigError::ProfileUnknown {
                    name: name.to_owned(),
                    suggestion: closest_profile(name, &self.file.profiles),
                })?;
        if self.file.settings.active_profile.as_deref() == Some(name) {
            self.file.settings.active_profile = None;
        }
        Ok(removed)
    }

    /// The currently-selected sticky profile name, if any.
    #[must_use]
    pub fn active_profile(&self) -> Option<&str> {
        self.file.settings.active_profile.as_deref()
    }

    /// Mark `name` as the sticky active profile. Errors (without
    /// mutating) if `name` is unknown.
    pub fn set_active_profile(&mut self, name: &str) -> Result<(), ConfigError> {
        if !self.file.profiles.contains_key(name) {
            return Err(ConfigError::ProfileUnknown {
                name: name.to_owned(),
                suggestion: closest_profile(name, &self.file.profiles),
            });
        }
        self.file.settings.active_profile = Some(name.to_owned());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Atomic write
// ---------------------------------------------------------------------------

fn save_atomic(path: &Path, file: &ConfigFile) -> Result<(), ConfigError> {
    let parent = path
        .parent()
        .expect("config path always has a parent (config_dir/brain/...)");
    fs::create_dir_all(parent).map_err(|e| ConfigError::Write {
        path: path.to_path_buf(),
        source: e,
    })?;

    let body = toml::to_string_pretty(file).map_err(|e| ConfigError::Write {
        path: path.to_path_buf(),
        source: io::Error::other(e.to_string()),
    })?;
    let mut full = String::with_capacity(HEADER.len() + body.len());
    full.push_str(HEADER);
    full.push_str(&body);

    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| ConfigError::Write {
        path: path.to_path_buf(),
        source: e,
    })?;
    tmp.write_all(full.as_bytes())
        .map_err(|e| ConfigError::Write {
            path: path.to_path_buf(),
            source: e,
        })?;
    tmp.as_file().sync_all().map_err(|e| ConfigError::Write {
        path: path.to_path_buf(),
        source: e,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o600)).map_err(|e| {
            ConfigError::Write {
                path: path.to_path_buf(),
                source: e,
            }
        })?;
    }
    tmp.persist(path).map_err(|e| ConfigError::Write {
        path: path.to_path_buf(),
        source: e.error,
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const KNOWN_KEYS: [&str; 5] = [
    "output",
    "timing",
    "sticky_context",
    "server",
    "active_profile",
];

fn validate_profile_name(name: &str) -> Result<(), ConfigError> {
    if name.is_empty() {
        return Err(ConfigError::ProfileBadName {
            name: name.to_owned(),
            reason: "name must not be empty",
        });
    }
    if name.len() > 64 {
        return Err(ConfigError::ProfileBadName {
            name: name.to_owned(),
            reason: "name must be <= 64 chars",
        });
    }
    let ok = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        return Err(ConfigError::ProfileBadName {
            name: name.to_owned(),
            reason: "name must match [A-Za-z0-9_-]",
        });
    }
    Ok(())
}

/// Cheap Levenshtein-1 plus prefix heuristic for "did you mean".
fn closest_key(needle: &str, haystack: &[&str]) -> Option<String> {
    haystack
        .iter()
        .map(|h| (*h, levenshtein(needle, h)))
        .min_by_key(|(_, d)| *d)
        .and_then(|(h, d)| if d <= 2 { Some(h.to_owned()) } else { None })
}

fn closest_profile(needle: &str, profiles: &BTreeMap<String, ProfileEntry>) -> Option<String> {
    profiles
        .keys()
        .map(|h| (h.clone(), levenshtein(needle, h)))
        .min_by_key(|(_, d)| *d)
        .and_then(|(h, d)| if d <= 2 { Some(h) } else { None })
}

/// Standard textbook Levenshtein. ~10 LOC; not worth a crate.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr: Vec<usize> = vec![0; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn cfg_path(t: &TempDir) -> PathBuf {
        path_in(t.path())
    }

    // ----- file IO ------------------------------------------------

    #[test]
    fn load_missing_file_returns_default_config() {
        let t = TempDir::new().unwrap();
        let c = Config::load_or_default_at(&cfg_path(&t)).unwrap();
        assert!(c.file.profiles.is_empty());
        assert_eq!(c.file.settings, Settings::default());
    }

    #[test]
    fn save_then_load_round_trips() {
        let t = TempDir::new().unwrap();
        let path = cfg_path(&t);
        let mut c = Config::load_or_default_at(&path).unwrap();
        c.set("output", "json").unwrap();
        c.set("timing", "true").unwrap();
        c.set("sticky_context", "7").unwrap();
        c.add_profile("work", "127.0.0.1:9090", "sk-secret-token")
            .unwrap();
        c.set_active_profile("work").unwrap();
        c.save().unwrap();

        let re = Config::load_or_default_at(&path).unwrap();
        assert_eq!(re.file.settings.output, Some(OutputPref::Json));
        assert_eq!(re.file.settings.timing, Some(true));
        assert_eq!(re.file.settings.sticky_context, Some(7));
        assert_eq!(re.file.settings.active_profile.as_deref(), Some("work"));
        assert_eq!(re.file.profiles.len(), 1);
        let p = re.get_profile("work").unwrap();
        assert_eq!(p.server, "127.0.0.1:9090");
        assert_eq!(p.token, "sk-secret-token");
    }

    #[test]
    fn save_sets_chmod_600_on_unix() {
        let t = TempDir::new().unwrap();
        let path = cfg_path(&t);
        let mut c = Config::load_or_default_at(&path).unwrap();
        c.add_profile("a", "127.0.0.1:9090", "tok").unwrap();
        c.save().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn malformed_toml_errors_without_overwriting() {
        let t = TempDir::new().unwrap();
        let path = cfg_path(&t);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "garbage = ===").unwrap();
        let err = Config::load_or_default_at(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }), "got {err:?}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "garbage = ===");
    }

    #[test]
    fn unknown_top_level_section_is_ignored_not_an_error() {
        // A config carrying a long-gone section (the old `[agents.*]`) is
        // valid TOML but unknown to the current schema. It must NOT brick the
        // tool — the unknown section is dropped, the known sections load, and
        // the stale section vanishes on the next save. (No code reads the old
        // format; serde just ignores the extra table.)
        let t = TempDir::new().unwrap();
        let path = cfg_path(&t);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "[settings]\nserver = \"10.0.0.1:9090\"\n\n\
             [agents.work]\nid = \"019e44c6-0000-0000-0000-000000000000\"\nactive = true\n",
        )
        .unwrap();
        let cfg = Config::load_or_default_at(&path).expect("stale section must not error");
        assert_eq!(cfg.settings().server.as_deref(), Some("10.0.0.1:9090"));
        assert!(cfg.profiles().is_empty());
    }

    // ----- settings -----------------------------------------------

    #[test]
    fn set_then_get_returns_serialised_value() {
        let t = TempDir::new().unwrap();
        let mut c = Config::load_or_default_at(&cfg_path(&t)).unwrap();
        c.set("output", "json").unwrap();
        c.set("timing", "off").unwrap();
        c.set("sticky_context", "42").unwrap();
        c.set("server", "10.0.0.1:9090").unwrap();
        assert_eq!(c.get("output").unwrap(), "json");
        assert_eq!(c.get("timing").unwrap(), "false");
        assert_eq!(c.get("sticky_context").unwrap(), "42");
        assert_eq!(c.get("server").unwrap(), "10.0.0.1:9090");
    }

    #[test]
    fn set_active_profile_key_requires_existing_profile() {
        let t = TempDir::new().unwrap();
        let mut c = Config::load_or_default_at(&cfg_path(&t)).unwrap();
        let err = c.set("active_profile", "ghost").unwrap_err();
        assert!(
            matches!(err, ConfigError::ProfileUnknown { .. }),
            "got {err:?}"
        );
        c.add_profile("work", "127.0.0.1:9090", "tok").unwrap();
        c.set("active_profile", "work").unwrap();
        assert_eq!(c.get("active_profile").unwrap(), "work");
    }

    #[test]
    fn set_unknown_key_errors_with_hint() {
        let t = TempDir::new().unwrap();
        let mut c = Config::load_or_default_at(&cfg_path(&t)).unwrap();
        let err = c.set("ouput", "json").unwrap_err();
        match err {
            ConfigError::UnknownKey { key, suggestion } => {
                assert_eq!(key, "ouput");
                assert_eq!(suggestion.as_deref(), Some("output"));
            }
            other => panic!("expected UnknownKey, got {other:?}"),
        }
    }

    #[test]
    fn set_invalid_value_errors() {
        let t = TempDir::new().unwrap();
        let mut c = Config::load_or_default_at(&cfg_path(&t)).unwrap();
        assert!(matches!(
            c.set("output", "yaml").unwrap_err(),
            ConfigError::InvalidValue { .. }
        ));
        assert!(matches!(
            c.set("sticky_context", "not-a-number").unwrap_err(),
            ConfigError::InvalidValue { .. }
        ));
        assert!(matches!(
            c.set("server", "no-colon-here").unwrap_err(),
            ConfigError::InvalidValue { .. }
        ));
    }

    #[test]
    fn list_includes_unset_marker_for_missing_keys() {
        let t = TempDir::new().unwrap();
        let c = Config::load_or_default_at(&cfg_path(&t)).unwrap();
        let rows: Vec<_> = c.list();
        assert_eq!(rows.len(), KNOWN_KEYS.len());
        for (_, v) in &rows {
            assert_eq!(v, "(unset)");
        }
    }

    // ----- profiles ----------------------------------------------

    #[test]
    fn add_profile_round_trips_server_and_token() {
        let t = TempDir::new().unwrap();
        let mut c = Config::load_or_default_at(&cfg_path(&t)).unwrap();
        c.add_profile("work", "10.0.0.5:9090", "sk-abc").unwrap();
        let e = c.get_profile("work").unwrap();
        assert_eq!(e.server, "10.0.0.5:9090");
        assert_eq!(e.token, "sk-abc");
    }

    #[test]
    fn add_profile_duplicate_errors() {
        let t = TempDir::new().unwrap();
        let mut c = Config::load_or_default_at(&cfg_path(&t)).unwrap();
        c.add_profile("work", "h:1", "t").unwrap();
        let err = c.add_profile("work", "h:2", "t2").unwrap_err();
        assert!(
            matches!(err, ConfigError::ProfileExists { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn set_profile_overwrites() {
        let t = TempDir::new().unwrap();
        let mut c = Config::load_or_default_at(&cfg_path(&t)).unwrap();
        c.set_profile("work", "h:1", "t1").unwrap();
        c.set_profile("work", "h:2", "t2").unwrap();
        let e = c.get_profile("work").unwrap();
        assert_eq!(e.server, "h:2");
        assert_eq!(e.token, "t2");
    }

    #[test]
    fn add_profile_rejects_empty_server_or_token() {
        let t = TempDir::new().unwrap();
        let mut c = Config::load_or_default_at(&cfg_path(&t)).unwrap();
        assert!(matches!(
            c.add_profile("a", "", "t").unwrap_err(),
            ConfigError::ProfileEmptyField {
                field: "server",
                ..
            }
        ));
        assert!(matches!(
            c.add_profile("a", "h:1", "  ").unwrap_err(),
            ConfigError::ProfileEmptyField { field: "token", .. }
        ));
    }

    #[test]
    fn invalid_profile_name_errors() {
        let t = TempDir::new().unwrap();
        let mut c = Config::load_or_default_at(&cfg_path(&t)).unwrap();
        for bad in ["", "has space", "has/slash", &"x".repeat(100)] {
            let err = c.add_profile(bad, "h:1", "t").unwrap_err();
            assert!(
                matches!(err, ConfigError::ProfileBadName { .. }),
                "got {err:?}"
            );
        }
    }

    #[test]
    fn remove_profile_returns_entry_and_clears_active() {
        let t = TempDir::new().unwrap();
        let mut c = Config::load_or_default_at(&cfg_path(&t)).unwrap();
        c.add_profile("work", "h:1", "secret").unwrap();
        c.set_active_profile("work").unwrap();
        let removed = c.remove_profile("work").unwrap();
        assert_eq!(removed.token, "secret");
        assert!(c.get_profile("work").is_err());
        // active_profile must no longer dangle.
        assert!(c.active_profile().is_none());
    }

    #[test]
    fn remove_unknown_errors_with_hint() {
        let t = TempDir::new().unwrap();
        let mut c = Config::load_or_default_at(&cfg_path(&t)).unwrap();
        c.add_profile("work", "h:1", "t").unwrap();
        let err = c.remove_profile("wokr").unwrap_err();
        match err {
            ConfigError::ProfileUnknown { name, suggestion } => {
                assert_eq!(name, "wokr");
                assert_eq!(suggestion.as_deref(), Some("work"));
            }
            other => panic!("expected ProfileUnknown, got {other:?}"),
        }
    }

    #[test]
    fn set_active_profile_unknown_errors_no_mutation() {
        let t = TempDir::new().unwrap();
        let mut c = Config::load_or_default_at(&cfg_path(&t)).unwrap();
        c.add_profile("work", "h:1", "t").unwrap();
        c.set_active_profile("work").unwrap();
        let err = c.set_active_profile("nope").unwrap_err();
        assert!(
            matches!(err, ConfigError::ProfileUnknown { .. }),
            "got {err:?}"
        );
        assert_eq!(c.active_profile(), Some("work"));
    }

    // ----- secret hygiene ----------------------------------------

    #[test]
    fn profile_debug_redacts_token() {
        let e = ProfileEntry {
            server: "127.0.0.1:9090".into(),
            token: "super-secret-Bearer".into(),
        };
        let dbg = format!("{e:?}");
        assert!(!dbg.contains("super-secret"), "token leaked: {dbg}");
        assert!(!dbg.contains("Bearer"), "token leaked: {dbg}");
        assert!(dbg.contains("<redacted>"));
        assert!(dbg.contains("127.0.0.1:9090"), "server should still show");
    }

    #[test]
    fn config_debug_does_not_leak_token() {
        let t = TempDir::new().unwrap();
        let mut c = Config::load_or_default_at(&cfg_path(&t)).unwrap();
        c.add_profile("work", "127.0.0.1:9090", "tok-secret-xyz")
            .unwrap();
        let dbg = format!("{c:?}");
        assert!(
            !dbg.contains("tok-secret-xyz"),
            "token leaked via Config Debug: {dbg}"
        );
    }
}
