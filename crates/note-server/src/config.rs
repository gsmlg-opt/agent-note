use anyhow::Context as _;
use serde::Deserialize;
use std::path::{Path, PathBuf};

const DEFAULT_CONFIG_PATH: &str = "config.toml";
const DEFAULT_DATABASE_PATH: &str = "dev-data/notes.db";
const DEFAULT_ATTACHMENTS_DIR: &str = "dev-data/attachments";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseEngine {
    Embed,
    Pg,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    attachments_dir: Option<PathBuf>,
    database: Option<FileDatabaseConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDatabaseConfig {
    engine: Option<DatabaseEngine>,
    path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub database_engine: DatabaseEngine,
    pub database_path: PathBuf,
    pub attachments_dir: PathBuf,
}

impl RuntimeConfig {
    pub fn validate_supported(&self) -> anyhow::Result<()> {
        match self.database_engine {
            DatabaseEngine::Embed => Ok(()),
            DatabaseEngine::Pg => {
                anyhow::bail!("PostgreSQL storage is not supported in this release")
            }
        }
    }
}

#[derive(Debug, Default)]
struct EnvValues {
    config_path: Option<PathBuf>,
    database_engine: Option<String>,
    database_path: Option<PathBuf>,
    attachments_dir: Option<PathBuf>,
}

pub fn load_runtime_config() -> anyhow::Result<RuntimeConfig> {
    let cwd = std::env::current_dir().context("resolve current working directory")?;
    let env = EnvValues {
        config_path: std::env::var_os("NOTE_CONFIG_PATH").map(PathBuf::from),
        database_engine: std::env::var("NOTE_DB_ENGINE").ok(),
        database_path: std::env::var_os("NOTE_DB_PATH").map(PathBuf::from),
        attachments_dir: std::env::var_os("NOTE_ATTACHMENTS_DIR").map(PathBuf::from),
    };
    resolve_runtime_config(&cwd, env)
}

fn resolve_runtime_config(cwd: &Path, env: EnvValues) -> anyhow::Result<RuntimeConfig> {
    let (file, file_base) = load_file_config(cwd, env.config_path.as_deref())?;
    let file_database = file.database.unwrap_or_default();

    let database_engine = match file_database.engine {
        Some(engine) => engine,
        None => match env.database_engine {
            Some(engine) => parse_database_engine(&engine)?,
            None => DatabaseEngine::Embed,
        },
    };
    let database_path = match file_database.path {
        Some(path) => resolve_path(
            file_base
                .as_deref()
                .expect("file values require a selected config file"),
            path,
        ),
        None => resolve_path(
            cwd,
            env.database_path
                .unwrap_or_else(|| PathBuf::from(DEFAULT_DATABASE_PATH)),
        ),
    };
    let attachments_dir = match file.attachments_dir {
        Some(path) => resolve_path(
            file_base
                .as_deref()
                .expect("file values require a selected config file"),
            path,
        ),
        None => resolve_path(
            cwd,
            env.attachments_dir
                .unwrap_or_else(|| PathBuf::from(DEFAULT_ATTACHMENTS_DIR)),
        ),
    };

    Ok(RuntimeConfig {
        database_engine,
        database_path,
        attachments_dir,
    })
}

fn load_file_config(
    cwd: &Path,
    selected_path: Option<&Path>,
) -> anyhow::Result<(FileConfig, Option<PathBuf>)> {
    let explicit = selected_path.is_some();
    let path = resolve_path(
        cwd,
        selected_path
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH)),
    );
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if !explicit && error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((FileConfig::default(), None));
        }
        Err(error) => {
            return Err(error).with_context(|| format!("read config file {}", path.display()));
        }
    };
    let config = toml::from_str(&contents)
        .with_context(|| format!("parse config file {}", path.display()))?;
    let base = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(cwd)
        .to_path_buf();
    Ok((config, Some(base)))
}

fn parse_database_engine(value: &str) -> anyhow::Result<DatabaseEngine> {
    match value {
        "embed" => Ok(DatabaseEngine::Embed),
        "pg" => Ok(DatabaseEngine::Pg),
        other => anyhow::bail!("unknown NOTE_DB_ENGINE={other}"),
    }
}

fn resolve_path(base: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn defaults_when_optional_config_is_absent() {
        let dir = tempfile::tempdir().unwrap();

        let config = resolve_runtime_config(dir.path(), EnvValues::default()).unwrap();

        assert_eq!(config.database_engine, DatabaseEngine::Embed);
        assert_eq!(config.database_path, dir.path().join("dev-data/notes.db"));
        assert_eq!(
            config.attachments_dir,
            dir.path().join("dev-data/attachments")
        );
    }

    #[test]
    fn explicit_missing_config_path_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = PathBuf::from("missing/config.toml");

        let error = resolve_runtime_config(
            dir.path(),
            EnvValues {
                config_path: Some(missing.clone()),
                ..EnvValues::default()
            },
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains(&dir.path().join(missing).display().to_string()));
    }

    #[test]
    fn relative_config_selector_and_toml_paths_use_the_config_parent() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "settings/agent-note.toml",
            r#"
attachments_dir = "attachments"

[database]
engine = "embed"
path = "data/notes.db"
"#,
        );

        let config = resolve_runtime_config(
            dir.path(),
            EnvValues {
                config_path: Some(PathBuf::from("settings/agent-note.toml")),
                ..EnvValues::default()
            },
        )
        .unwrap();

        assert_eq!(config.database_engine, DatabaseEngine::Embed);
        assert_eq!(
            config.database_path,
            dir.path().join("settings/data/notes.db")
        );
        assert_eq!(
            config.attachments_dir,
            dir.path().join("settings/attachments")
        );
    }

    #[test]
    fn partial_file_uses_environment_for_omitted_fields() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "config.toml",
            r#"
[database]
engine = "embed"
"#,
        );

        let config = resolve_runtime_config(
            dir.path(),
            EnvValues {
                database_engine: Some("pg".into()),
                database_path: Some(PathBuf::from("env/notes.db")),
                ..EnvValues::default()
            },
        )
        .unwrap();

        assert_eq!(config.database_engine, DatabaseEngine::Embed);
        assert_eq!(config.database_path, dir.path().join("env/notes.db"));
        assert_eq!(
            config.attachments_dir,
            dir.path().join("dev-data/attachments")
        );
    }

    #[test]
    fn file_values_override_environment_values_per_field() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "config.toml",
            r#"
attachments_dir = "file-attachments"

[database]
engine = "embed"
path = "file-notes.db"
"#,
        );

        let config = resolve_runtime_config(
            dir.path(),
            EnvValues {
                database_engine: Some("pg".into()),
                database_path: Some(PathBuf::from("env-notes.db")),
                attachments_dir: Some(PathBuf::from("env-attachments")),
                ..EnvValues::default()
            },
        )
        .unwrap();

        assert_eq!(config.database_engine, DatabaseEngine::Embed);
        assert_eq!(config.database_path, dir.path().join("file-notes.db"));
        assert_eq!(config.attachments_dir, dir.path().join("file-attachments"));
    }

    #[test]
    fn environment_values_override_defaults_and_paths_use_the_working_directory() {
        let dir = tempfile::tempdir().unwrap();

        let config = resolve_runtime_config(
            dir.path(),
            EnvValues {
                database_engine: Some("embed".into()),
                database_path: Some(PathBuf::from("state/notes.db")),
                attachments_dir: Some(PathBuf::from("state/attachments")),
                ..EnvValues::default()
            },
        )
        .unwrap();

        assert_eq!(config.database_engine, DatabaseEngine::Embed);
        assert_eq!(config.database_path, dir.path().join("state/notes.db"));
        assert_eq!(config.attachments_dir, dir.path().join("state/attachments"));
    }

    #[test]
    fn absolute_paths_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let database_path = external.path().join("notes.db");
        let attachments_dir = external.path().join("attachments");

        let config = resolve_runtime_config(
            dir.path(),
            EnvValues {
                database_path: Some(database_path.clone()),
                attachments_dir: Some(attachments_dir.clone()),
                ..EnvValues::default()
            },
        )
        .unwrap();

        assert_eq!(config.database_path, database_path);
        assert_eq!(config.attachments_dir, attachments_dir);
    }

    #[test]
    fn unknown_top_level_and_database_fields_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "config.toml", "mystery = true");
        let top_level = resolve_runtime_config(dir.path(), EnvValues::default()).unwrap_err();
        assert!(format!("{top_level:#}").contains("unknown field"));

        write_config(
            dir.path(),
            "config.toml",
            r#"
[database]
mystery = true
"#,
        );
        let nested = resolve_runtime_config(dir.path(), EnvValues::default()).unwrap_err();
        assert!(format!("{nested:#}").contains("unknown field"));
    }

    #[test]
    fn unknown_environment_and_file_engines_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let environment = resolve_runtime_config(
            dir.path(),
            EnvValues {
                database_engine: Some("sqlite".into()),
                ..EnvValues::default()
            },
        )
        .unwrap_err();
        assert!(environment.to_string().contains("sqlite"));

        write_config(
            dir.path(),
            "config.toml",
            r#"
[database]
engine = "sqlite"
"#,
        );
        let file = resolve_runtime_config(dir.path(), EnvValues::default()).unwrap_err();
        assert!(format!("{file:#}").contains("unknown variant"));
    }

    #[test]
    fn malformed_toml_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "config.toml", "[database");

        let error = resolve_runtime_config(dir.path(), EnvValues::default()).unwrap_err();

        assert!(error.to_string().contains("parse"));
    }

    #[test]
    fn pg_parses_but_is_rejected_by_runtime_validation() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "config.toml",
            r#"
[database]
engine = "pg"
"#,
        );

        let config = resolve_runtime_config(dir.path(), EnvValues::default()).unwrap();

        assert_eq!(config.database_engine, DatabaseEngine::Pg);
        assert_eq!(
            config.validate_supported().unwrap_err().to_string(),
            "PostgreSQL storage is not supported in this release"
        );
    }

    fn write_config(root: &Path, relative_path: &str, contents: &str) {
        let path = root.join(relative_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
}
