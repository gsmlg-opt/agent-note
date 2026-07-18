use note_storage_pg::PgStorage;
use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, PgPool};
use url::Url;
use uuid::Uuid;

pub const TEST_DATABASE_URL_ENV: &str = "TEST_DATABASE_URL";

pub fn configured_url_or_skip(test_name: &str) -> Option<String> {
    match std::env::var(TEST_DATABASE_URL_ENV) {
        Ok(url) => Some(url),
        Err(_) => {
            eprintln!("skipping {test_name}: TEST_DATABASE_URL is not set");
            None
        }
    }
}

pub struct TestDatabase {
    pub url: String,
    admin_url: String,
    database_name: String,
    armed: bool,
}

impl TestDatabase {
    pub async fn create(admin_url: &str) -> Self {
        let database_name = format!("agent_note_test_{}", Uuid::new_v4().simple());
        let database = Self {
            url: derived_database_url(admin_url, &database_name),
            admin_url: admin_url.to_owned(),
            database_name,
            armed: true,
        };

        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(admin_url)
            .await
            .unwrap_or_else(|_| panic!("connect to PostgreSQL administrative database"));
        sqlx::query(AssertSqlSafe(format!(
            r#"CREATE DATABASE "{}""#,
            database.database_name
        )))
        .execute(&admin_pool)
        .await
        .unwrap_or_else(|_| panic!("create isolated PostgreSQL test database"));
        admin_pool.close().await;

        database
    }

    pub async fn provision_vector(&self) {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.url)
            .await
            .unwrap_or_else(|_| panic!("connect to isolated PostgreSQL test database"));
        sqlx::query("CREATE EXTENSION vector")
            .execute(&pool)
            .await
            .unwrap_or_else(|_| panic!("provision vector extension"));
        pool.close().await;
    }

    pub async fn inspect_pool(&self) -> PgPool {
        PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.url)
            .await
            .unwrap_or_else(|_| panic!("connect to isolated PostgreSQL test database"))
    }

    pub fn database_name(&self) -> &str {
        &self.database_name
    }

    pub async fn cleanup(mut self, storage: Option<&PgStorage>) {
        if let Some(storage) = storage {
            storage.close().await;
        }

        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .unwrap_or_else(|_| panic!("reconnect to PostgreSQL administrative database"));
        let connection_count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint
             FROM pg_stat_activity
             WHERE datname = $1",
        )
        .bind(&self.database_name)
        .fetch_one(&admin_pool)
        .await
        .unwrap_or_else(|_| panic!("count isolated PostgreSQL test connections"));
        assert_eq!(
            connection_count, 0,
            "isolated PostgreSQL test database still has live connections"
        );

        sqlx::query(AssertSqlSafe(format!(
            r#"DROP DATABASE IF EXISTS "{}" WITH (FORCE)"#,
            self.database_name
        )))
        .execute(&admin_pool)
        .await
        .unwrap_or_else(|_| panic!("drop isolated PostgreSQL test database"));
        admin_pool.close().await;
        self.armed = false;
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        let admin_url = self.admin_url.clone();
        let database_name = self.database_name.clone();
        let fallback = std::thread::Builder::new()
            .name("agent-note-pg-test-cleanup".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                runtime.block_on(force_drop_database(&admin_url, &database_name));
            });
        if let Ok(fallback) = fallback {
            let _ = fallback.join();
        }
    }
}

fn derived_database_url(admin_url: &str, database_name: &str) -> String {
    let mut parsed =
        Url::parse(admin_url).unwrap_or_else(|_| panic!("TEST_DATABASE_URL must be a valid URL"));
    let mut query_pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(key, value)| {
            let value = if key == "dbname" {
                database_name.to_owned()
            } else {
                value.into_owned()
            };
            (key.into_owned(), value)
        })
        .collect();

    parsed.set_path(&format!("/{database_name}"));
    parsed.set_fragment(None);
    if query_pairs.is_empty() {
        parsed.set_query(None);
    } else {
        parsed
            .query_pairs_mut()
            .clear()
            .extend_pairs(query_pairs.drain(..));
    }
    parsed.to_string()
}

async fn force_drop_database(admin_url: &str, database_name: &str) {
    let Ok(admin_pool) = PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_url)
        .await
    else {
        return;
    };
    let _ = sqlx::query(
        "SELECT pg_terminate_backend(pid)
         FROM pg_stat_activity
         WHERE datname = $1 AND pid <> pg_backend_pid()",
    )
    .bind(database_name)
    .execute(&admin_pool)
    .await;
    let _ = sqlx::query(AssertSqlSafe(format!(
        r#"DROP DATABASE IF EXISTS "{database_name}" WITH (FORCE)"#
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
}

#[cfg(test)]
mod tests {
    use super::derived_database_url;
    use url::Url;

    #[test]
    fn derived_database_url_preserves_options_and_rewrites_dbname() {
        let derived = Url::parse(&derived_database_url(
            "postgresql://user:password@[::1]:5432/postgres?hostaddr=%3A%3A1&port=6543&user=query-user&password=query-password&sslmode=require&channel_binding=require&options=-c%20search_path%3Dpublic&application_name=agent-note&dbname=wrong&dbname=also-wrong#fragment",
            "agent_note_test_0123",
        ))
        .unwrap();

        assert_eq!(derived.host_str(), Some("[::1]"));
        assert_eq!(derived.port(), Some(5432));
        assert_eq!(derived.username(), "user");
        assert_eq!(derived.password(), Some("password"));
        assert_eq!(derived.path(), "/agent_note_test_0123");
        assert_eq!(derived.fragment(), None);

        let pairs: Vec<_> = derived.query_pairs().collect();
        assert!(pairs.contains(&("hostaddr".into(), "::1".into())));
        assert!(pairs.contains(&("port".into(), "6543".into())));
        assert!(pairs.contains(&("user".into(), "query-user".into())));
        assert!(pairs.contains(&("password".into(), "query-password".into())));
        assert!(pairs.contains(&("sslmode".into(), "require".into())));
        assert!(pairs.contains(&("channel_binding".into(), "require".into())));
        assert!(pairs.contains(&("options".into(), "-c search_path=public".into())));
        assert!(pairs.contains(&("application_name".into(), "agent-note".into())));
        assert_eq!(
            pairs
                .iter()
                .filter(|(key, _)| key == "dbname")
                .map(|(_, value)| value.as_ref())
                .collect::<Vec<_>>(),
            vec!["agent_note_test_0123", "agent_note_test_0123"]
        );
    }
}
