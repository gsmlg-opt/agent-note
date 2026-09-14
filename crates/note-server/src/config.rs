use anyhow::Context as _;
use serde::Deserialize;
use std::io::Write as _;
use std::path::{Path, PathBuf};

const DEFAULT_CONFIG_PATH: &str = "dev-data/config.toml";
const DEFAULT_DATABASE_PATH: &str = "notes.db";
const DEFAULT_ATTACHMENTS_PATH: &str = "attachments";
const DEFAULT_EMBEDDING_MODEL: &str = "bge-m3";
const DEFAULT_BIND_ADDR: &str = "0.0.0.0:6222";
const DEFAULT_DEV_CONFIG: &str = r#"[server]
bind_addr = "0.0.0.0:6222"

[database]
engine = "embed"
path = "notes.db"

[embedding]
engine = "local"

[attachments]
engine = "filesystem"
path = "attachments"
"#;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    server: Option<FileServerConfig>,
    database: Option<FileDatabaseConfig>,
    embedding: Option<FileEmbeddingConfig>,
    attachments: Option<FileAttachmentConfig>,
    export: Option<FileExportConfig>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileExportConfig {
    pdf: Option<FilePdfExportConfig>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePdfExportConfig {
    enabled: Option<bool>,
    renderer_url: Option<String>,
    total_deadline_secs: Option<u64>,
    renderer_timeout_secs: Option<u64>,
    max_in_flight: Option<usize>,
    max_markdown_bytes: Option<u64>,
    max_asset_count: Option<usize>,
    max_asset_bytes: Option<u64>,
    max_combined_asset_bytes: Option<u64>,
    max_pixels_per_image: Option<u64>,
    max_combined_pixels: Option<u64>,
    max_pdf_bytes: Option<usize>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileServerConfig {
    bind_addr: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDatabaseConfig {
    engine: Option<String>,
    path: Option<PathBuf>,
    url: Option<String>,
    max_connections: Option<u32>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEmbeddingConfig {
    engine: Option<String>,
    model_path: Option<PathBuf>,
    base_url: Option<String>,
    model: Option<String>,
    api_key_env: Option<String>,
    timeout_secs: Option<u64>,
    max_retries: Option<u32>,
}

#[derive(Default, Deserialize)]
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

#[derive(Clone, PartialEq, Eq)]
pub enum DatabaseConfig {
    Embed { path: PathBuf },
    Pg { url: String, max_connections: u32 },
}

impl std::fmt::Debug for DatabaseConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Embed { path } => formatter.debug_struct("Embed").field("path", path).finish(),
            Self::Pg {
                max_connections, ..
            } => formatter
                .debug_struct("Pg")
                .field("url", &"<redacted>")
                .field("max_connections", max_connections)
                .finish(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum EmbeddingConfig {
    Local {
        model_path: Option<PathBuf>,
    },
    OpenAi {
        base_url: String,
        model: String,
        api_key_env: Option<String>,
        timeout_secs: u64,
        max_retries: u32,
    },
}

impl std::fmt::Debug for EmbeddingConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local { model_path } => formatter
                .debug_struct("Local")
                .field("model_path", model_path)
                .finish(),
            Self::OpenAi {
                model,
                timeout_secs,
                max_retries,
                ..
            } => formatter
                .debug_struct("OpenAi")
                .field("model", model)
                .field("timeout_secs", timeout_secs)
                .field("max_retries", max_retries)
                .finish(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
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

impl std::fmt::Debug for AttachmentConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Filesystem { path } => formatter
                .debug_struct("Filesystem")
                .field("path", path)
                .finish(),
            Self::S3 {
                bucket,
                prefix,
                region,
                endpoint,
                force_path_style,
            } => formatter
                .debug_struct("S3")
                .field("bucket", bucket)
                .field("prefix", prefix)
                .field("region", region)
                .field("endpoint", &endpoint.as_ref().map(|_| "<redacted>"))
                .field("force_path_style", force_path_style)
                .finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub config_path: PathBuf,
    pub bind_addr: String,
    pub database: DatabaseConfig,
    pub embedding: EmbeddingConfig,
    pub attachments: AttachmentConfig,
    pub pdf_export: PdfExportConfig,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PdfExportConfig {
    pub enabled: bool,
    pub renderer_url: Option<String>,
    pub total_deadline_secs: u64,
    pub renderer_timeout_secs: u64,
    pub max_in_flight: usize,
    pub max_markdown_bytes: u64,
    pub max_asset_count: usize,
    pub max_asset_bytes: u64,
    pub max_combined_asset_bytes: u64,
    pub max_pixels_per_image: u64,
    pub max_combined_pixels: u64,
    pub max_pdf_bytes: usize,
}

impl Default for PdfExportConfig {
    fn default() -> Self {
        resolve_pdf_export(FilePdfExportConfig::default())
            .expect("built-in PDF export defaults must be valid")
    }
}

impl std::fmt::Debug for PdfExportConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PdfExportConfig")
            .field("enabled", &self.enabled)
            .field(
                "renderer_url",
                &self.renderer_url.as_ref().map(|_| "<redacted>"),
            )
            .field("total_deadline_secs", &self.total_deadline_secs)
            .field("renderer_timeout_secs", &self.renderer_timeout_secs)
            .field("max_in_flight", &self.max_in_flight)
            .field("max_markdown_bytes", &self.max_markdown_bytes)
            .field("max_asset_count", &self.max_asset_count)
            .field("max_asset_bytes", &self.max_asset_bytes)
            .field("max_combined_asset_bytes", &self.max_combined_asset_bytes)
            .field("max_pixels_per_image", &self.max_pixels_per_image)
            .field("max_combined_pixels", &self.max_combined_pixels)
            .field("max_pdf_bytes", &self.max_pdf_bytes)
            .finish()
    }
}

#[derive(Default)]
struct EnvValues {
    config_path: Option<PathBuf>,
    bind_addr: Option<String>,
    database_engine: Option<String>,
    database_path: Option<PathBuf>,
    database_url: Option<String>,
    database_max_connections: Option<String>,
    embedding_engine: Option<String>,
    embedding_model_path: Option<PathBuf>,
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
        bind_addr: read_string_env("NOTE_BIND_ADDR")?,
        database_engine: read_string_env("NOTE_DB_ENGINE")?,
        database_path: std::env::var_os("NOTE_DB_PATH").map(PathBuf::from),
        database_url: read_string_env("DATABASE_URL")?,
        database_max_connections: read_string_env("NOTE_DB_MAX_CONNECTIONS")?,
        embedding_engine: read_string_env("NOTE_EMBEDDING_ENGINE")?,
        embedding_model_path: std::env::var_os("NOTE_MODEL_PATH").map(PathBuf::from),
        embedding_base_url: read_string_env("NOTE_EMBEDDING_BASE_URL")?,
        embedding_model: read_string_env("NOTE_EMBEDDING_MODEL")?,
        embedding_api_key_env: read_string_env("NOTE_EMBEDDING_API_KEY_ENV")?,
        embedding_timeout_secs: read_string_env("NOTE_EMBEDDING_TIMEOUT_SECS")?,
        embedding_max_retries: read_string_env("NOTE_EMBEDDING_MAX_RETRIES")?,
        attachments_engine: read_string_env("NOTE_ATTACHMENTS_ENGINE")?,
        attachments_dir: std::env::var_os("NOTE_ATTACHMENTS_DIR").map(PathBuf::from),
        s3_bucket: read_string_env("NOTE_S3_BUCKET")?,
        s3_prefix: read_string_env("NOTE_S3_PREFIX")?,
        aws_region: read_string_env("AWS_REGION")?,
        s3_endpoint: read_string_env("NOTE_S3_ENDPOINT")?,
        s3_force_path_style: read_string_env("NOTE_S3_FORCE_PATH_STYLE")?,
    };
    resolve_runtime_config(&cwd, env, cfg!(debug_assertions))
}

fn read_string_env(variable: &str) -> anyhow::Result<Option<String>> {
    decode_string_env(variable, std::env::var(variable))
}

fn decode_string_env(
    variable: &str,
    result: Result<String, std::env::VarError>,
) -> anyhow::Result<Option<String>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("environment variable {variable} is not valid Unicode")
        }
    }
}

fn resolve_runtime_config(
    cwd: &Path,
    env: EnvValues,
    generate_implicit_config: bool,
) -> anyhow::Result<RuntimeConfig> {
    let (file, config_path) =
        load_file_config(cwd, env.config_path.as_deref(), generate_implicit_config)?;
    let config_base = config_path.parent().unwrap_or(cwd);
    let file_server = file.server.unwrap_or_default();
    let file_database = file.database.unwrap_or_default();
    let file_embedding = file.embedding.unwrap_or_default();
    let file_attachments = file.attachments.unwrap_or_default();
    let file_pdf_export = file.export.unwrap_or_default().pdf.unwrap_or_default();

    let bind_addr = file_server
        .bind_addr
        .or_else(|| env.bind_addr.clone())
        .unwrap_or_else(|| DEFAULT_BIND_ADDR.to_string());
    let database = resolve_database(file_database, &env, config_base)?;
    let embedding = resolve_embedding(file_embedding, &env, config_base)?;
    let attachments = resolve_attachments(file_attachments, &env, config_base)?;
    let pdf_export = resolve_pdf_export(file_pdf_export)?;
    persist_missing_bind_addr(&config_path, &bind_addr)?;

    Ok(RuntimeConfig {
        config_path,
        bind_addr,
        database,
        embedding,
        attachments,
        pdf_export,
    })
}

fn resolve_pdf_export(file: FilePdfExportConfig) -> anyhow::Result<PdfExportConfig> {
    const MIB: u64 = 1024 * 1024;
    let config = PdfExportConfig {
        enabled: file.enabled.unwrap_or(false),
        renderer_url: file.renderer_url.map(|value| value.trim().to_owned()),
        total_deadline_secs: file.total_deadline_secs.unwrap_or(35),
        renderer_timeout_secs: file.renderer_timeout_secs.unwrap_or(30),
        max_in_flight: file.max_in_flight.unwrap_or(2),
        max_markdown_bytes: file.max_markdown_bytes.unwrap_or(2 * MIB),
        max_asset_count: file.max_asset_count.unwrap_or(64),
        max_asset_bytes: file.max_asset_bytes.unwrap_or(8 * MIB),
        max_combined_asset_bytes: file.max_combined_asset_bytes.unwrap_or(32 * MIB),
        max_pixels_per_image: file.max_pixels_per_image.unwrap_or(20_000_000),
        max_combined_pixels: file.max_combined_pixels.unwrap_or(80_000_000),
        max_pdf_bytes: file.max_pdf_bytes.unwrap_or(32 * MIB as usize),
    };
    if !config.enabled {
        return Ok(config);
    }
    let renderer_url = config
        .renderer_url
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("config field export.pdf.renderer_url is required"))?;
    if crate::export::renderer::GotenbergRenderer::new(renderer_url, config.max_pdf_bytes).is_err()
    {
        anyhow::bail!("config field export.pdf.renderer_url is invalid");
    }
    let valid = config.total_deadline_secs > 0
        && config.renderer_timeout_secs > 0
        && config.renderer_timeout_secs <= 30
        && config.renderer_timeout_secs <= config.total_deadline_secs
        && config.max_in_flight > 0
        && config.max_in_flight <= 64
        && config.max_markdown_bytes > 0
        && config.max_markdown_bytes <= 2 * MIB
        && config.max_asset_count > 0
        && config.max_asset_count <= 64
        && config.max_asset_bytes > 0
        && config.max_asset_bytes <= 8 * MIB
        && config.max_combined_asset_bytes > 0
        && config.max_combined_asset_bytes <= 32 * MIB
        && config.max_pixels_per_image > 0
        && config.max_pixels_per_image <= 20_000_000
        && config.max_combined_pixels > 0
        && config.max_combined_pixels <= 80_000_000
        && config.max_pdf_bytes > 0
        && config.max_pdf_bytes <= 32 * MIB as usize;
    if !valid {
        anyhow::bail!("config fields under export.pdf contain invalid safety limits");
    }
    Ok(config)
}

fn load_file_config(
    cwd: &Path,
    selected_path: Option<&Path>,
    _generate_implicit_config: bool,
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
        Err(error) if !explicit && error.kind() == std::io::ErrorKind::NotFound => {
            generate_default_config(&path)?;
            std::fs::read_to_string(&path)
                .with_context(|| format!("read generated config file {}", path.display()))?
        }
        Err(error) => {
            return Err(error).with_context(|| format!("read config file {}", path.display()));
        }
    };
    let config = parse_file_config(&contents, &path)?;
    Ok((config, path))
}

fn persist_missing_bind_addr(path: &Path, bind_addr: &str) -> anyhow::Result<()> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("read config file {}", path.display()))?;
    let mut document = contents
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parse config file {} for update", path.display()))?;
    if document
        .get("server")
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|server| server.get("bind_addr"))
        .is_some()
    {
        return Ok(());
    }

    if !document.contains_key("server") {
        document["server"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    document["server"]["bind_addr"] = toml_edit::value(bind_addr);
    let parent = path
        .parent()
        .context("config path has no parent directory")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary config file in {}", parent.display()))?;
    temporary
        .write_all(document.to_string().as_bytes())
        .with_context(|| format!("write updated config file {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("sync updated config file {}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("persist updated config file {}", path.display()))?;
    Ok(())
}

fn parse_file_config(contents: &str, path: &Path) -> anyhow::Result<FileConfig> {
    toml::from_str(contents).map_err(|error: toml::de::Error| {
        let location = error
            .span()
            .map(|span| line_and_column(contents, span.start))
            .map(|(line, column)| format!(" at line {line}, column {column}"))
            .unwrap_or_default();
        anyhow::anyhow!(
            "parse config file {}{location}: {}",
            path.display(),
            error.message()
        )
    })
}

fn line_and_column(contents: &str, offset: usize) -> (usize, usize) {
    let prefix = &contents[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit_once('\n')
        .map(|(_, line)| line)
        .unwrap_or(prefix)
        .chars()
        .count()
        + 1;
    (line, column)
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
    let (engine, source) = select_engine(
        file.engine.as_deref(),
        env.database_engine.as_deref(),
        "embed",
        "config field database.engine",
        "environment variable NOTE_DB_ENGINE",
    );
    match engine {
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
            if max_connections == 0 {
                anyhow::bail!("database.max_connections must be greater than zero");
            }
            Ok(DatabaseConfig::Pg {
                url,
                max_connections,
            })
        }
        _ => anyhow::bail!("unknown database engine selected by {source}"),
    }
}

fn resolve_embedding(
    file: FileEmbeddingConfig,
    env: &EnvValues,
    config_base: &Path,
) -> anyhow::Result<EmbeddingConfig> {
    let (engine, source) = select_engine(
        file.engine.as_deref(),
        env.embedding_engine.as_deref(),
        "local",
        "config field embedding.engine",
        "environment variable NOTE_EMBEDDING_ENGINE",
    );
    match engine {
        "local" => {
            let model_path = file.model_path.or_else(|| env.embedding_model_path.clone());
            if let Some(path) = model_path.as_deref() {
                require_path("embedding.model_path", path)?;
            }
            Ok(EmbeddingConfig::Local {
                model_path: model_path.map(|path| resolve_path(config_base, path)),
            })
        }
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
            )?
            .trim()
            .to_owned();
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
            if timeout_secs == 0 {
                anyhow::bail!("embedding.timeout_secs must be greater than zero");
            }
            if max_retries > 10 {
                anyhow::bail!("embedding.max_retries must be at most 10");
            }
            let api_key_env = match file
                .api_key_env
                .or_else(|| env.embedding_api_key_env.clone())
            {
                Some(name) => Some(
                    require_string("embedding.api_key_env", Some(name))?
                        .trim()
                        .to_owned(),
                ),
                None => None,
            };
            Ok(EmbeddingConfig::OpenAi {
                base_url,
                model,
                api_key_env,
                timeout_secs,
                max_retries,
            })
        }
        _ => anyhow::bail!("unknown embedding engine selected by {source}"),
    }
}

fn resolve_attachments(
    file: FileAttachmentConfig,
    env: &EnvValues,
    config_base: &Path,
) -> anyhow::Result<AttachmentConfig> {
    let (engine, source) = select_engine(
        file.engine.as_deref(),
        env.attachments_engine.as_deref(),
        "filesystem",
        "config field attachments.engine",
        "environment variable NOTE_ATTACHMENTS_ENGINE",
    );
    match engine {
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
        _ => anyhow::bail!("unknown attachments engine selected by {source}"),
    }
}

fn select_engine<'a>(
    file: Option<&'a str>,
    env: Option<&'a str>,
    default: &'a str,
    file_source: &'static str,
    env_source: &'static str,
) -> (&'a str, &'static str) {
    match (file, env) {
        (Some(engine), _) => (engine, file_source),
        (None, Some(engine)) => (engine, env_source),
        (None, None) => (default, "built-in default"),
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
    fn debug_policy_creates_and_migrates_the_implicit_config() {
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
        assert_eq!(
            config.embedding,
            EmbeddingConfig::Local { model_path: None }
        );
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

        let migrated = std::fs::read_to_string(path).unwrap();
        assert!(migrated.contains("path = \"winner.db\""));
        assert!(migrated.contains("path = \"winner-attachments\""));
        assert!(migrated.contains("[server]\nbind_addr = \"0.0.0.0:6222\""));
        assert_eq!(
            winner.database,
            DatabaseConfig::Embed {
                path: dir.path().join("dev-data/winner.db"),
            }
        );
    }

    #[test]
    fn server_bind_addr_uses_file_then_environment_then_default() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "file.toml",
            "[server]\nbind_addr = \"127.0.0.1:7000\"\n",
        );
        write_config(dir.path(), "env.toml", "");
        write_config(dir.path(), "default.toml", "");

        let file = resolve_runtime_config(
            dir.path(),
            EnvValues {
                config_path: Some("file.toml".into()),
                bind_addr: Some("127.0.0.1:7001".into()),
                ..EnvValues::default()
            },
            false,
        )
        .unwrap();
        let env = resolve_runtime_config(
            dir.path(),
            EnvValues {
                config_path: Some("env.toml".into()),
                bind_addr: Some("127.0.0.1:7001".into()),
                ..EnvValues::default()
            },
            false,
        )
        .unwrap();
        let default = resolve_runtime_config(
            dir.path(),
            EnvValues {
                config_path: Some("default.toml".into()),
                ..EnvValues::default()
            },
            false,
        )
        .unwrap();

        assert_eq!(file.bind_addr, "127.0.0.1:7000");
        assert_eq!(env.bind_addr, "127.0.0.1:7001");
        assert_eq!(default.bind_addr, "0.0.0.0:6222");
    }

    #[test]
    fn missing_implicit_config_is_generated_in_all_build_profiles() {
        let dir = tempfile::tempdir().unwrap();

        let config = resolve_runtime_config(dir.path(), EnvValues::default(), false).unwrap();
        let contents = std::fs::read_to_string(dir.path().join("dev-data/config.toml")).unwrap();

        assert_eq!(config.bind_addr, "0.0.0.0:6222");
        assert!(contents.contains("[server]\nbind_addr = \"0.0.0.0:6222\""));
    }

    #[test]
    fn missing_bind_addr_is_persisted_without_losing_existing_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let existing = "# keep this comment\n[database]\npath = \"custom.db\"\n";
        std::fs::write(&path, existing).unwrap();

        let config = resolve_runtime_config(
            dir.path(),
            EnvValues {
                config_path: Some("config.toml".into()),
                bind_addr: Some("0.0.0.0:7000".into()),
                ..EnvValues::default()
            },
            false,
        )
        .unwrap();
        let migrated = std::fs::read_to_string(&path).unwrap();

        assert_eq!(config.bind_addr, "0.0.0.0:7000");
        assert!(migrated.contains("# keep this comment"));
        assert!(migrated.contains("path = \"custom.db\""));
        assert!(migrated.contains("[server]\nbind_addr = \"0.0.0.0:7000\""));
    }

    #[test]
    fn existing_bind_addr_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let existing = "[server]\nbind_addr = \"127.0.0.1:9000\"\n";
        std::fs::write(&path, existing).unwrap();

        let config = resolve_runtime_config(
            dir.path(),
            EnvValues {
                config_path: Some("config.toml".into()),
                bind_addr: Some("0.0.0.0:7000".into()),
                ..EnvValues::default()
            },
            false,
        )
        .unwrap();

        assert_eq!(config.bind_addr, "127.0.0.1:9000");
        assert_eq!(std::fs::read_to_string(path).unwrap(), existing);
    }

    #[test]
    fn config_example_is_valid_and_documents_every_file_option() {
        let contents = include_str!("../../../config.example.toml");
        parse_file_config(contents, Path::new("config.example.toml")).unwrap();

        for option in [
            "bind_addr",
            "engine",
            "path",
            "url",
            "max_connections",
            "model_path",
            "base_url",
            "model",
            "api_key_env",
            "timeout_secs",
            "max_retries",
            "bucket",
            "prefix",
            "region",
            "endpoint",
            "force_path_style",
            "enabled",
            "renderer_url",
            "total_deadline_secs",
            "renderer_timeout_secs",
            "max_in_flight",
            "max_markdown_bytes",
            "max_asset_count",
            "max_asset_bytes",
            "max_combined_asset_bytes",
            "max_pixels_per_image",
            "max_combined_pixels",
            "max_pdf_bytes",
        ] {
            assert!(
                contents.lines().any(|line| {
                    line.trim_start_matches([' ', '#'])
                        .trim_start()
                        .starts_with(&format!("{option} ="))
                }),
                "config.example.toml does not document {option}"
            );
        }
    }

    #[test]
    fn concurrent_generation_accepts_one_atomic_winner() {
        const THREADS: usize = 16;
        let dir = tempfile::tempdir().unwrap();
        let path = std::sync::Arc::new(dir.path().join(DEFAULT_CONFIG_PATH));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(THREADS));

        let handles = (0..THREADS)
            .map(|_| {
                let path = std::sync::Arc::clone(&path);
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    generate_default_config(&path)
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle.join().unwrap().unwrap();
        }

        assert_eq!(std::fs::read_to_string(&*path).unwrap(), DEFAULT_DEV_CONFIG);
        let config = resolve_runtime_config(dir.path(), EnvValues::default(), false).unwrap();
        assert_eq!(config.config_path, *path);
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
    fn active_pg_config_rejects_zero_max_connections() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "config.toml",
            r#"[database]
engine = "pg"
url = "postgresql://localhost/notes"
max_connections = 0
"#,
        );

        let error = resolve_explicit(dir.path(), "config.toml").unwrap_err();

        assert!(error.to_string().contains("database.max_connections"));
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
    fn local_model_path_uses_file_before_environment_and_resolves_from_config_parent() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "settings/config.toml",
            r#"[embedding]
engine = "local"
model_path = "models/file.onnx"
"#,
        );

        let config = resolve_runtime_config(
            dir.path(),
            EnvValues {
                config_path: Some("settings/config.toml".into()),
                embedding_model_path: Some("models/environment.onnx".into()),
                ..EnvValues::default()
            },
            false,
        )
        .unwrap();

        assert_eq!(
            config.embedding,
            EmbeddingConfig::Local {
                model_path: Some(dir.path().join("settings/models/file.onnx")),
            }
        );
    }

    #[test]
    fn local_model_path_falls_back_to_environment_and_resolves_from_config_parent() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "settings/config.toml",
            r#"[embedding]
engine = "local"
"#,
        );

        let config = resolve_runtime_config(
            dir.path(),
            EnvValues {
                config_path: Some("settings/config.toml".into()),
                embedding_model_path: Some("models/environment.onnx".into()),
                ..EnvValues::default()
            },
            false,
        )
        .unwrap();

        assert_eq!(
            config.embedding,
            EmbeddingConfig::Local {
                model_path: Some(dir.path().join("settings/models/environment.onnx")),
            }
        );
    }

    #[test]
    fn local_model_path_rejects_an_explicit_blank_value() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "config.toml",
            r#"[embedding]
engine = "local"
model_path = ""
"#,
        );

        let error = resolve_runtime_config(
            dir.path(),
            EnvValues {
                config_path: Some("config.toml".into()),
                ..EnvValues::default()
            },
            false,
        )
        .unwrap_err();

        assert!(error.to_string().contains("embedding.model_path"));
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
        assert_eq!(
            config.embedding,
            EmbeddingConfig::Local { model_path: None }
        );
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
    fn malformed_toml_errors_do_not_expose_source_credentials() {
        const PASSWORD: &str = "known-password-must-not-leak";
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "config.toml",
            &format!(
                r#"[database]
engine = "pg"
url = "postgresql://agent:{PASSWORD}@database/notes
"#
            ),
        );

        let error = resolve_explicit(dir.path(), "config.toml").unwrap_err();
        let rendered = format!("{error:#}");

        assert!(!rendered.contains(PASSWORD), "{rendered}");
        assert!(rendered.contains("config.toml"), "{rendered}");
        assert!(rendered.contains("line 3, column"), "{rendered}");
    }

    #[test]
    fn debug_output_redacts_database_credentials() {
        const PASSWORD: &str = "known-debug-password";
        let config = RuntimeConfig {
            config_path: "/tmp/config.toml".into(),
            bind_addr: DEFAULT_BIND_ADDR.into(),
            database: DatabaseConfig::Pg {
                url: format!("postgresql://agent:{PASSWORD}@database/notes"),
                max_connections: 10,
            },
            embedding: EmbeddingConfig::Local { model_path: None },
            attachments: AttachmentConfig::Filesystem {
                path: "/tmp/attachments".into(),
            },
            pdf_export: PdfExportConfig::default(),
        };

        for rendered in [format!("{:?}", config.database), format!("{config:?}")] {
            assert!(!rendered.contains(PASSWORD), "{rendered}");
            assert!(rendered.contains("<redacted>"), "{rendered}");
        }
    }

    #[test]
    fn runtime_debug_output_redacts_openai_connection_and_credential_name() {
        const URL_SENTINEL: &str = "openai-user:openai-password@embedding.example";
        const API_KEY_ENV_SENTINEL: &str = "EMBEDDING_API_KEY_SENTINEL";
        let config = RuntimeConfig {
            config_path: "/tmp/config.toml".into(),
            bind_addr: DEFAULT_BIND_ADDR.into(),
            database: DatabaseConfig::Embed {
                path: "/tmp/notes.db".into(),
            },
            embedding: EmbeddingConfig::OpenAi {
                base_url: format!("https://{URL_SENTINEL}"),
                model: "bge-m3".into(),
                api_key_env: Some(API_KEY_ENV_SENTINEL.into()),
                timeout_secs: 30,
                max_retries: 3,
            },
            attachments: AttachmentConfig::Filesystem {
                path: "/tmp/attachments".into(),
            },
            pdf_export: PdfExportConfig::default(),
        };

        let rendered = format!("{config:?}");

        assert!(!rendered.contains(URL_SENTINEL), "{rendered}");
        assert!(!rendered.contains("openai-password"), "{rendered}");
        assert!(!rendered.contains(API_KEY_ENV_SENTINEL), "{rendered}");
        assert!(rendered.contains("OpenAi"), "{rendered}");
        assert!(rendered.contains("bge-m3"), "{rendered}");
        assert!(rendered.contains("timeout_secs"), "{rendered}");
        assert!(rendered.contains("max_retries"), "{rendered}");
    }

    #[test]
    fn runtime_debug_output_redacts_s3_endpoint() {
        const ENDPOINT_SENTINEL: &str = "s3-user:s3-password@minio.example:9000";
        let config = RuntimeConfig {
            config_path: "/tmp/config.toml".into(),
            bind_addr: DEFAULT_BIND_ADDR.into(),
            database: DatabaseConfig::Embed {
                path: "/tmp/notes.db".into(),
            },
            embedding: EmbeddingConfig::Local { model_path: None },
            attachments: AttachmentConfig::S3 {
                bucket: "agent-note".into(),
                prefix: "attachments".into(),
                region: Some("us-east-1".into()),
                endpoint: Some(format!("https://{ENDPOINT_SENTINEL}")),
                force_path_style: true,
            },
            pdf_export: PdfExportConfig::default(),
        };

        for rendered in [format!("{:?}", config.attachments), format!("{config:?}")] {
            assert!(!rendered.contains(ENDPOINT_SENTINEL), "{rendered}");
            assert!(!rendered.contains("s3-password"), "{rendered}");
            assert!(rendered.contains("<redacted>"), "{rendered}");
            assert!(rendered.contains("agent-note"), "{rendered}");
            assert!(rendered.contains("attachments"), "{rendered}");
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
    fn api_key_env_name_is_trimmed_and_blank_names_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "trimmed.toml",
            r#"[embedding]
engine = "openai"
base_url = "https://embedding.example"
api_key_env = "  EMBEDDING_API_KEY  "
"#,
        );

        let config = resolve_explicit(dir.path(), "trimmed.toml").unwrap();
        assert!(matches!(
            config.embedding,
            EmbeddingConfig::OpenAi {
                api_key_env: Some(ref name),
                ..
            } if name == "EMBEDDING_API_KEY"
        ));

        write_config(
            dir.path(),
            "blank.toml",
            r#"[embedding]
engine = "openai"
base_url = "https://embedding.example"
api_key_env = " \t "
"#,
        );
        let error = resolve_explicit(dir.path(), "blank.toml").unwrap_err();
        assert!(
            error.to_string().contains("embedding.api_key_env"),
            "{error:#}"
        );
    }

    #[test]
    fn openai_defaults_are_applied_and_model_is_normalized() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "config.toml",
            r#"[embedding]
engine = "openai"
base_url = "http://127.0.0.1:8000"
model = " bge-m3 "
"#,
        );

        let config = resolve_explicit(dir.path(), "config.toml").unwrap();

        assert_eq!(
            config.embedding,
            EmbeddingConfig::OpenAi {
                base_url: "http://127.0.0.1:8000".into(),
                model: "bge-m3".into(),
                api_key_env: None,
                timeout_secs: 30,
                max_retries: 3,
            }
        );
    }

    #[test]
    fn openai_file_values_override_embedding_environment_values() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "config.toml",
            r#"[embedding]
engine = "openai"
base_url = "http://127.0.0.1:8000"
model = "file-model"
timeout_secs = 12
max_retries = 5
"#,
        );

        let config = resolve_runtime_config(
            dir.path(),
            EnvValues {
                config_path: Some("config.toml".into()),
                embedding_model: Some("environment-model".into()),
                embedding_timeout_secs: Some("90".into()),
                embedding_max_retries: Some("8".into()),
                ..EnvValues::default()
            },
            false,
        )
        .unwrap();

        assert_eq!(
            config.embedding,
            EmbeddingConfig::OpenAi {
                base_url: "http://127.0.0.1:8000".into(),
                model: "file-model".into(),
                api_key_env: None,
                timeout_secs: 12,
                max_retries: 5,
            }
        );
    }

    #[test]
    fn active_openai_rejects_invalid_timeout_and_retry_limits() {
        let dir = tempfile::tempdir().unwrap();
        for (name, setting, expected) in [
            ("timeout.toml", "timeout_secs = 0", "embedding.timeout_secs"),
            ("retries.toml", "max_retries = 11", "embedding.max_retries"),
        ] {
            write_config(
                dir.path(),
                name,
                &format!(
                    "[embedding]\nengine = \"openai\"\nbase_url = \"http://127.0.0.1:8000\"\n{setting}\n"
                ),
            );

            let error = resolve_explicit(dir.path(), name).unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
        }
    }

    #[test]
    fn unknown_file_and_environment_engines_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "config.toml", "");

        for (env, expected) in [
            (
                EnvValues {
                    database_engine: Some("sqlite".into()),
                    ..EnvValues::default()
                },
                "environment variable NOTE_DB_ENGINE",
            ),
            (
                EnvValues {
                    embedding_engine: Some("ollama".into()),
                    ..EnvValues::default()
                },
                "environment variable NOTE_EMBEDDING_ENGINE",
            ),
            (
                EnvValues {
                    attachments_engine: Some("gcs".into()),
                    ..EnvValues::default()
                },
                "environment variable NOTE_ATTACHMENTS_ENGINE",
            ),
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
            assert!(error.to_string().contains(expected), "{error:#}");
        }

        for (name, contents, expected) in [
            (
                "database.toml",
                "[database]\nengine = \"sqlite\"\n",
                "config field database.engine",
            ),
            (
                "embedding.toml",
                "[embedding]\nengine = \"ollama\"\n",
                "config field embedding.engine",
            ),
            (
                "attachments.toml",
                "[attachments]\nengine = \"gcs\"\n",
                "config field attachments.engine",
            ),
        ] {
            write_config(dir.path(), name, contents);
            let error = resolve_explicit(dir.path(), name).unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
        }
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

    #[test]
    fn missing_string_environment_values_are_omitted() {
        let value =
            decode_string_env("NOTE_DB_ENGINE", Err(std::env::VarError::NotPresent)).unwrap();

        assert_eq!(value, None);
    }

    #[test]
    fn missing_pdf_export_configuration_is_backward_compatible_and_disabled() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "config.toml", "");

        let config = resolve_explicit(dir.path(), "config.toml").unwrap();

        assert!(!config.pdf_export.enabled);
        assert_eq!(config.pdf_export.renderer_url, None);
        assert_eq!(config.pdf_export.total_deadline_secs, 35);
        assert_eq!(config.pdf_export.renderer_timeout_secs, 30);
        assert_eq!(config.pdf_export.max_in_flight, 2);
        assert_eq!(config.pdf_export.max_pdf_bytes, 32 * 1024 * 1024);
    }

    #[test]
    fn enabled_pdf_export_configuration_loads_typed_limits() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "config.toml",
            r#"[export.pdf]
enabled = true
renderer_url = "http://renderer.internal:3000"
total_deadline_secs = 20
renderer_timeout_secs = 15
max_in_flight = 1
max_markdown_bytes = 1024
max_asset_count = 4
max_asset_bytes = 2048
max_combined_asset_bytes = 4096
max_pixels_per_image = 1000000
max_combined_pixels = 2000000
max_pdf_bytes = 8192
"#,
        );

        let config = resolve_explicit(dir.path(), "config.toml").unwrap();

        assert!(config.pdf_export.enabled);
        assert_eq!(
            config.pdf_export.renderer_url.as_deref(),
            Some("http://renderer.internal:3000")
        );
        assert_eq!(config.pdf_export.total_deadline_secs, 20);
        assert_eq!(config.pdf_export.renderer_timeout_secs, 15);
        assert_eq!(config.pdf_export.max_in_flight, 1);
        assert_eq!(config.pdf_export.max_markdown_bytes, 1024);
    }

    #[test]
    fn enabled_pdf_export_rejects_missing_or_unsafe_renderer_and_invalid_limits() {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in [
            ("missing.toml", "[export.pdf]\nenabled = true\n"),
            (
                "credentials.toml",
                "[export.pdf]\nenabled = true\nrenderer_url = \"http://user:secret@renderer:3000\"\n",
            ),
            (
                "deadline.toml",
                "[export.pdf]\nenabled = true\nrenderer_url = \"http://renderer:3000\"\ntotal_deadline_secs = 10\nrenderer_timeout_secs = 11\n",
            ),
            (
                "zero.toml",
                "[export.pdf]\nenabled = true\nrenderer_url = \"http://renderer:3000\"\nmax_in_flight = 0\n",
            ),
            (
                "long-timeout.toml",
                "[export.pdf]\nenabled = true\nrenderer_url = \"http://renderer:3000\"\ntotal_deadline_secs = 35\nrenderer_timeout_secs = 31\n",
            ),
        ] {
            write_config(dir.path(), name, body);
            let rendered = format!("{:#}", resolve_explicit(dir.path(), name).unwrap_err());
            assert!(rendered.contains("export.pdf"), "{name}: {rendered}");
            assert!(!rendered.contains("secret"), "{name}: {rendered}");
        }

        write_config(
            dir.path(),
            "unknown.toml",
            "[export.pdf]\nenabled = true\nrenderer_url = \"http://renderer:3000\"\nbrowser_option = true\n",
        );
        let rendered = format!(
            "{:#}",
            resolve_explicit(dir.path(), "unknown.toml").unwrap_err()
        );
        assert!(
            rendered.contains("unknown field `browser_option`"),
            "{rendered}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_string_environment_values_name_only_the_variable() {
        use std::os::unix::ffi::OsStringExt as _;

        let invalid = std::ffi::OsString::from_vec(b"known-env-secret-\xff".to_vec());
        let error = decode_string_env("DATABASE_URL", Err(std::env::VarError::NotUnicode(invalid)))
            .unwrap_err();
        let rendered = format!("{error:#}");

        assert_eq!(
            rendered,
            "environment variable DATABASE_URL is not valid Unicode"
        );
        assert!(!rendered.contains("known-env-secret"), "{rendered}");
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
