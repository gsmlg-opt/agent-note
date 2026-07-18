use anyhow::Context as _;
use serde::Deserialize;
use std::io::Write as _;
use std::path::{Path, PathBuf};

const DEFAULT_CONFIG_PATH: &str = "dev-data/config.toml";
const DEFAULT_DATABASE_PATH: &str = "notes.db";
const DEFAULT_ATTACHMENTS_PATH: &str = "attachments";
const DEFAULT_EMBEDDING_MODEL: &str = "bge-m3";
const DEFAULT_DEV_CONFIG: &str = r#"[database]
engine = "embed"
path = "notes.db"

[embedding]
engine = "local"

[attachments]
engine = "filesystem"
path = "attachments"
"#;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    database: Option<FileDatabaseConfig>,
    embedding: Option<FileEmbeddingConfig>,
    attachments: Option<FileAttachmentConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDatabaseConfig {
    engine: Option<String>,
    path: Option<PathBuf>,
    url: Option<String>,
    max_connections: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEmbeddingConfig {
    engine: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    api_key_env: Option<String>,
    timeout_secs: Option<u64>,
    max_retries: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileAttachmentConfig {
    engine: Option<String>,
    path: Option<PathBuf>,
    bucket: Option<String>,
    prefix: Option<String>,
    region: Option<String>,
    endpoint: Option<String>,
    force_path_style: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseConfig {
    Embed { path: PathBuf },
    Pg { url: String, max_connections: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddingConfig {
    Local,
    OpenAi {
        base_url: String,
        model: String,
        api_key_env: Option<String>,
        timeout_secs: u64,
        max_retries: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentConfig {
    Filesystem {
        path: PathBuf,
    },
    S3 {
        bucket: String,
        prefix: String,
        region: Option<String>,
        endpoint: Option<String>,
        force_path_style: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub config_path: PathBuf,
    pub database: DatabaseConfig,
    pub embedding: EmbeddingConfig,
    pub attachments: AttachmentConfig,
}

#[derive(Debug, Default)]
struct EnvValues {
    config_path: Option<PathBuf>,
    database_engine: Option<String>,
    database_path: Option<PathBuf>,
    database_url: Option<String>,
    database_max_connections: Option<String>,
    embedding_engine: Option<String>,
    embedding_base_url: Option<String>,
    embedding_model: Option<String>,
    embedding_api_key_env: Option<String>,
    embedding_timeout_secs: Option<String>,
    embedding_max_retries: Option<String>,
    attachments_engine: Option<String>,
    attachments_dir: Option<PathBuf>,
    s3_bucket: Option<String>,
    s3_prefix: Option<String>,
    aws_region: Option<String>,
    s3_endpoint: Option<String>,
    s3_force_path_style: Option<String>,
}

pub fn load_runtime_config() -> anyhow::Result<RuntimeConfig> {
    let cwd = std::env::current_dir().context("resolve current working directory")?;
    let env = EnvValues {
        config_path: std::env::var_os("NOTE_CONFIG_PATH").map(PathBuf::from),
        database_engine: std::env::var("NOTE_DB_ENGINE").ok(),
        database_path: std::env::var_os("NOTE_DB_PATH").map(PathBuf::from),
        database_url: std::env::var("DATABASE_URL").ok(),
        database_max_connections: std::env::var("NOTE_DB_MAX_CONNECTIONS").ok(),
        embedding_engine: std::env::var("NOTE_EMBEDDING_ENGINE").ok(),
        embedding_base_url: std::env::var("NOTE_EMBEDDING_BASE_URL").ok(),
        embedding_model: std::env::var("NOTE_EMBEDDING_MODEL").ok(),
        embedding_api_key_env: std::env::var("NOTE_EMBEDDING_API_KEY_ENV").ok(),
        embedding_timeout_secs: std::env::var("NOTE_EMBEDDING_TIMEOUT_SECS").ok(),
        embedding_max_retries: std::env::var("NOTE_EMBEDDING_MAX_RETRIES").ok(),
        attachments_engine: std::env::var("NOTE_ATTACHMENTS_ENGINE").ok(),
        attachments_dir: std::env::var_os("NOTE_ATTACHMENTS_DIR").map(PathBuf::from),
        s3_bucket: std::env::var("NOTE_S3_BUCKET").ok(),
        s3_prefix: std::env::var("NOTE_S3_PREFIX").ok(),
        aws_region: std::env::var("AWS_REGION").ok(),
        s3_endpoint: std::env::var("NOTE_S3_ENDPOINT").ok(),
        s3_force_path_style: std::env::var("NOTE_S3_FORCE_PATH_STYLE").ok(),
    };
    resolve_runtime_config(&cwd, env, cfg!(debug_assertions))
}

fn resolve_runtime_config(
    cwd: &Path,
    env: EnvValues,
    generate_implicit_config: bool,
) -> anyhow::Result<RuntimeConfig> {
    let (file, config_path) =
        load_file_config(cwd, env.config_path.as_deref(), generate_implicit_config)?;
    let config_base = config_path.parent().unwrap_or(cwd);
    let file_database = file.database.unwrap_or_default();
    let file_embedding = file.embedding.unwrap_or_default();
    let file_attachments = file.attachments.unwrap_or_default();

    let database = resolve_database(file_database, &env, config_base)?;
    let embedding = resolve_embedding(file_embedding, &env)?;
    let attachments = resolve_attachments(file_attachments, &env, config_base)?;

    Ok(RuntimeConfig {
        config_path,
        database,
        embedding,
        attachments,
    })
}

fn load_file_config(
    cwd: &Path,
    selected_path: Option<&Path>,
    generate_implicit_config: bool,
) -> anyhow::Result<(FileConfig, PathBuf)> {
    let explicit = selected_path.is_some();
    let path = resolve_path(
        cwd,
        selected_path
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH)),
    );
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error)
            if !explicit
                && generate_implicit_config
                && error.kind() == std::io::ErrorKind::NotFound =>
        {
            generate_default_config(&path)?;
            std::fs::read_to_string(&path)
                .with_context(|| format!("read generated config file {}", path.display()))?
        }
        Err(error) => {
            return Err(error).with_context(|| format!("read config file {}", path.display()));
        }
    };
    let config = toml::from_str(&contents)
        .with_context(|| format!("parse config file {}", path.display()))?;
    Ok((config, path))
}

fn generate_default_config(path: &Path) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .context("implicit config path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create config directory {}", parent.display()))?;

    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary config file in {}", parent.display()))?;
    temporary
        .write_all(DEFAULT_DEV_CONFIG.as_bytes())
        .context("write default config file")?;
    temporary
        .as_file()
        .sync_all()
        .context("sync default config file")?;

    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => {
            Err(error.error).with_context(|| format!("persist config file {}", path.display()))
        }
    }
}

fn resolve_database(
    file: FileDatabaseConfig,
    env: &EnvValues,
    config_base: &Path,
) -> anyhow::Result<DatabaseConfig> {
    match file
        .engine
        .as_deref()
        .or(env.database_engine.as_deref())
        .unwrap_or("embed")
    {
        "embed" => {
            let path = file
                .path
                .or_else(|| env.database_path.clone())
                .unwrap_or_else(|| PathBuf::from(DEFAULT_DATABASE_PATH));
            require_path("database.path", &path)?;
            Ok(DatabaseConfig::Embed {
                path: resolve_path(config_base, path),
            })
        }
        "pg" => {
            let url = require_string(
                "database.url",
                file.url.or_else(|| env.database_url.clone()),
            )?;
            let max_connections = resolve_env_number(
                file.max_connections,
                env.database_max_connections.as_deref(),
                10,
                "NOTE_DB_MAX_CONNECTIONS",
            )?;
            Ok(DatabaseConfig::Pg {
                url,
                max_connections,
            })
        }
        other => anyhow::bail!("unknown database engine {other:?}"),
    }
}

fn resolve_embedding(
    file: FileEmbeddingConfig,
    env: &EnvValues,
) -> anyhow::Result<EmbeddingConfig> {
    match file
        .engine
        .as_deref()
        .or(env.embedding_engine.as_deref())
        .unwrap_or("local")
    {
        "local" => Ok(EmbeddingConfig::Local),
        "openai" => {
            let base_url = require_string(
                "embedding.base_url",
                file.base_url.or_else(|| env.embedding_base_url.clone()),
            )?;
            let model = require_string(
                "embedding.model",
                Some(
                    file.model
                        .or_else(|| env.embedding_model.clone())
                        .unwrap_or_else(|| DEFAULT_EMBEDDING_MODEL.into()),
                ),
            )?;
            let timeout_secs = resolve_env_number(
                file.timeout_secs,
                env.embedding_timeout_secs.as_deref(),
                30,
                "NOTE_EMBEDDING_TIMEOUT_SECS",
            )?;
            let max_retries = resolve_env_number(
                file.max_retries,
                env.embedding_max_retries.as_deref(),
                3,
                "NOTE_EMBEDDING_MAX_RETRIES",
            )?;
            Ok(EmbeddingConfig::OpenAi {
                base_url,
                model,
                api_key_env: file
                    .api_key_env
                    .or_else(|| env.embedding_api_key_env.clone()),
                timeout_secs,
                max_retries,
            })
        }
        other => anyhow::bail!("unknown embedding engine {other:?}"),
    }
}

fn resolve_attachments(
    file: FileAttachmentConfig,
    env: &EnvValues,
    config_base: &Path,
) -> anyhow::Result<AttachmentConfig> {
    match file
        .engine
        .as_deref()
        .or(env.attachments_engine.as_deref())
        .unwrap_or("filesystem")
    {
        "filesystem" => {
            let path = file
                .path
                .or_else(|| env.attachments_dir.clone())
                .unwrap_or_else(|| PathBuf::from(DEFAULT_ATTACHMENTS_PATH));
            require_path("attachments.path", &path)?;
            Ok(AttachmentConfig::Filesystem {
                path: resolve_path(config_base, path),
            })
        }
        "s3" => {
            let bucket = require_string(
                "attachments.bucket",
                file.bucket.or_else(|| env.s3_bucket.clone()),
            )?;
            let prefix = file
                .prefix
                .or_else(|| env.s3_prefix.clone())
                .unwrap_or_default();
            let force_path_style = resolve_env_bool(
                file.force_path_style,
                env.s3_force_path_style.as_deref(),
                false,
                "NOTE_S3_FORCE_PATH_STYLE",
            )?;
            Ok(AttachmentConfig::S3 {
                bucket,
                prefix,
                region: file.region.or_else(|| env.aws_region.clone()),
                endpoint: file.endpoint.or_else(|| env.s3_endpoint.clone()),
                force_path_style,
            })
        }
        other => anyhow::bail!("unknown attachments engine {other:?}"),
    }
}

fn require_string(field: &str, value: Option<String>) -> anyhow::Result<String> {
    match value {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => anyhow::bail!("{field} must not be blank"),
    }
}

fn require_path(field: &str, value: &Path) -> anyhow::Result<()> {
    if value.as_os_str().is_empty() {
        anyhow::bail!("{field} must not be blank");
    }
    Ok(())
}

fn resolve_env_number<T>(
    file: Option<T>,
    env: Option<&str>,
    default: T,
    variable: &str,
) -> anyhow::Result<T>
where
    T: std::str::FromStr,
{
    match (file, env) {
        (Some(value), _) => Ok(value),
        (None, Some(value)) => value
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid {variable}={value:?}")),
        (None, None) => Ok(default),
    }
}

fn resolve_env_bool(
    file: Option<bool>,
    env: Option<&str>,
    default: bool,
    variable: &str,
) -> anyhow::Result<bool> {
    resolve_env_number(file, env, default, variable)
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
    fn debug_policy_creates_the_implicit_config_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();

        let config = resolve_runtime_config(dir.path(), EnvValues::default(), true).unwrap();
        let path = dir.path().join("dev-data/config.toml");

        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_DEV_CONFIG);
        assert_eq!(config.config_path, path);
        assert_eq!(
            config.database,
            DatabaseConfig::Embed {
                path: dir.path().join("dev-data/notes.db"),
            }
        );
        assert_eq!(config.embedding, EmbeddingConfig::Local);
        assert_eq!(
            config.attachments,
            AttachmentConfig::Filesystem {
                path: dir.path().join("dev-data/attachments"),
            }
        );

        let replacement = r#"[database]
engine = "embed"
path = "winner.db"

[embedding]
engine = "local"

[attachments]
engine = "filesystem"
path = "winner-attachments"
"#;
        std::fs::write(&path, replacement).unwrap();

        let winner = resolve_runtime_config(dir.path(), EnvValues::default(), true).unwrap();

        assert_eq!(std::fs::read_to_string(path).unwrap(), replacement);
        assert_eq!(
            winner.database,
            DatabaseConfig::Embed {
                path: dir.path().join("dev-data/winner.db"),
            }
        );
    }

    #[test]
    fn release_policy_rejects_a_missing_implicit_config() {
        let dir = tempfile::tempdir().unwrap();

        let error = resolve_runtime_config(dir.path(), EnvValues::default(), false).unwrap_err();

        assert!(error.to_string().contains("dev-data/config.toml"));
        assert!(!dir.path().join("dev-data/config.toml").exists());
    }

    #[test]
    fn explicit_missing_config_is_never_generated() {
        let dir = tempfile::tempdir().unwrap();

        let error = resolve_runtime_config(
            dir.path(),
            EnvValues {
                config_path: Some("custom/config.toml".into()),
                ..EnvValues::default()
            },
            true,
        )
        .unwrap_err();

        assert!(error.to_string().contains("custom/config.toml"));
        assert!(!dir.path().join("custom/config.toml").exists());
    }

    #[test]
    fn file_values_win_then_environment_then_defaults_per_field() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "settings/config.toml",
            r#"[database]
engine = "pg"
url = "postgresql://file"

[embedding]
engine = "openai"
base_url = "https://file.example"
timeout_secs = 45

[attachments]
engine = "s3"
bucket = "file-bucket"
region = "file-region"
force_path_style = true
"#,
        );

        let config = resolve_runtime_config(
            dir.path(),
            EnvValues {
                config_path: Some("settings/config.toml".into()),
                database_engine: Some("embed".into()),
                database_url: Some("postgresql://env".into()),
                database_max_connections: Some("17".into()),
                embedding_engine: Some("local".into()),
                embedding_base_url: Some("https://env.example".into()),
                embedding_model: Some("env-model".into()),
                embedding_timeout_secs: Some("90".into()),
                attachments_engine: Some("filesystem".into()),
                s3_bucket: Some("env-bucket".into()),
                s3_prefix: Some("env-prefix".into()),
                aws_region: Some("env-region".into()),
                s3_force_path_style: Some("false".into()),
                ..EnvValues::default()
            },
            false,
        )
        .unwrap();

        assert_eq!(
            config.database,
            DatabaseConfig::Pg {
                url: "postgresql://file".into(),
                max_connections: 17,
            }
        );
        assert_eq!(
            config.embedding,
            EmbeddingConfig::OpenAi {
                base_url: "https://file.example".into(),
                model: "env-model".into(),
                api_key_env: None,
                timeout_secs: 45,
                max_retries: 3,
            }
        );
        assert_eq!(
            config.attachments,
            AttachmentConfig::S3 {
                bucket: "file-bucket".into(),
                prefix: "env-prefix".into(),
                region: Some("file-region".into()),
                endpoint: None,
                force_path_style: true,
            }
        );
    }

    #[test]
    fn relative_paths_resolve_from_the_config_parent() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "settings/agent-note.toml",
            r#"[database]
engine = "embed"
path = "data/notes.db"

[embedding]
engine = "local"

[attachments]
engine = "filesystem"
"#,
        );

        let config = resolve_runtime_config(
            dir.path(),
            EnvValues {
                config_path: Some(PathBuf::from("settings/agent-note.toml")),
                attachments_dir: Some(PathBuf::from("env-attachments")),
                ..EnvValues::default()
            },
            false,
        )
        .unwrap();

        assert_eq!(
            config.database,
            DatabaseConfig::Embed {
                path: dir.path().join("settings/data/notes.db"),
            }
        );
        assert_eq!(
            config.attachments,
            AttachmentConfig::Filesystem {
                path: dir.path().join("settings/env-attachments"),
            }
        );
    }

    #[test]
    fn inactive_adapter_fields_are_not_required() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "config.toml",
            r#"[database]
engine = "embed"
path = "notes.db"

[embedding]
engine = "local"

[attachments]
engine = "filesystem"
path = "attachments"
"#,
        );

        let config = resolve_runtime_config(
            dir.path(),
            EnvValues {
                config_path: Some("config.toml".into()),
                ..EnvValues::default()
            },
            false,
        )
        .unwrap();

        assert!(matches!(config.database, DatabaseConfig::Embed { .. }));
        assert_eq!(config.embedding, EmbeddingConfig::Local);
        assert!(matches!(
            config.attachments,
            AttachmentConfig::Filesystem { .. }
        ));
    }

    #[test]
    fn active_adapters_reject_blank_required_fields() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            (
                "pg.toml",
                r#"[database]
engine = "pg"
url = " "
"#,
                "database.url",
            ),
            (
                "openai.toml",
                r#"[embedding]
engine = "openai"
base_url = ""
"#,
                "embedding.base_url",
            ),
            (
                "s3.toml",
                r#"[attachments]
engine = "s3"
bucket = " "
"#,
                "attachments.bucket",
            ),
            (
                "embed.toml",
                r#"[database]
engine = "embed"
path = ""
"#,
                "database.path",
            ),
            (
                "filesystem.toml",
                r#"[attachments]
engine = "filesystem"
path = ""
"#,
                "attachments.path",
            ),
        ];

        for (name, contents, expected) in cases {
            write_config(dir.path(), name, contents);
            let error = resolve_runtime_config(
                dir.path(),
                EnvValues {
                    config_path: Some(name.into()),
                    ..EnvValues::default()
                },
                false,
            )
            .unwrap_err();
            assert!(error.to_string().contains(expected), "{name}: {error:#}");
        }
    }

    #[test]
    fn unknown_toml_fields_are_rejected_at_every_level() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "config.toml", "mystery = true");
        let top_level = resolve_explicit(dir.path(), "config.toml").unwrap_err();
        assert!(format!("{top_level:#}").contains("unknown field"));

        for (name, table) in [
            ("database.toml", "database"),
            ("embedding.toml", "embedding"),
            ("attachments.toml", "attachments"),
        ] {
            write_config(dir.path(), name, &format!("[{table}]\nmystery = true\n"));
            let nested = resolve_explicit(dir.path(), name).unwrap_err();
            assert!(format!("{nested:#}").contains("unknown field"));
        }
    }

    #[test]
    fn api_key_env_is_kept_as_a_name_without_reading_its_value() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "config.toml",
            r#"[embedding]
engine = "openai"
base_url = "https://embedding.example"
api_key_env = "SECRET_THAT_IS_NOT_READ"
"#,
        );

        let config = resolve_explicit(dir.path(), "config.toml").unwrap();

        assert_eq!(
            config.embedding,
            EmbeddingConfig::OpenAi {
                base_url: "https://embedding.example".into(),
                model: "bge-m3".into(),
                api_key_env: Some("SECRET_THAT_IS_NOT_READ".into()),
                timeout_secs: 30,
                max_retries: 3,
            }
        );
    }

    #[test]
    fn unknown_file_and_environment_engines_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "config.toml", "");

        for env in [
            EnvValues {
                database_engine: Some("sqlite".into()),
                ..EnvValues::default()
            },
            EnvValues {
                embedding_engine: Some("ollama".into()),
                ..EnvValues::default()
            },
            EnvValues {
                attachments_engine: Some("gcs".into()),
                ..EnvValues::default()
            },
        ] {
            let error = resolve_runtime_config(
                dir.path(),
                EnvValues {
                    config_path: Some("config.toml".into()),
                    ..env
                },
                false,
            )
            .unwrap_err();
            assert!(error.to_string().contains("unknown"));
        }

        write_config(
            dir.path(),
            "config.toml",
            r#"[database]
engine = "sqlite"
"#,
        );
        let file = resolve_explicit(dir.path(), "config.toml").unwrap_err();
        assert!(file.to_string().contains("sqlite"));
    }

    #[test]
    fn invalid_numeric_and_boolean_environment_values_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "config.toml", "");

        let cases = [
            EnvValues {
                database_engine: Some("pg".into()),
                database_url: Some("postgresql://db".into()),
                database_max_connections: Some("many".into()),
                ..EnvValues::default()
            },
            EnvValues {
                embedding_engine: Some("openai".into()),
                embedding_base_url: Some("https://embedding.example".into()),
                embedding_timeout_secs: Some("soon".into()),
                ..EnvValues::default()
            },
            EnvValues {
                embedding_engine: Some("openai".into()),
                embedding_base_url: Some("https://embedding.example".into()),
                embedding_max_retries: Some("-1".into()),
                ..EnvValues::default()
            },
            EnvValues {
                attachments_engine: Some("s3".into()),
                s3_bucket: Some("bucket".into()),
                s3_force_path_style: Some("sometimes".into()),
                ..EnvValues::default()
            },
        ];

        for env in cases {
            let error = resolve_runtime_config(
                dir.path(),
                EnvValues {
                    config_path: Some("config.toml".into()),
                    ..env
                },
                false,
            )
            .unwrap_err();

            assert!(error.to_string().contains("invalid"), "{error:#}");
        }
    }

    fn write_config(root: &Path, relative_path: &str, contents: &str) {
        let path = root.join(relative_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn resolve_explicit(root: &Path, relative_path: &str) -> anyhow::Result<RuntimeConfig> {
        resolve_runtime_config(
            root,
            EnvValues {
                config_path: Some(relative_path.into()),
                ..EnvValues::default()
            },
            false,
        )
    }
}
