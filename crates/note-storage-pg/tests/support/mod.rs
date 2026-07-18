use note_storage_pg::PgStorage;
use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, PgPool};
use url::Url;
use uuid::Uuid;

pub const TEST_DATABASE_URL_ENV: &str = "TEST_DATABASE_URL";

pub fn configured_url() -> Option<String> {
    std::env::var(TEST_DATABASE_URL_ENV).ok()
}

pub struct TestDatabase {
    pub url: String,
    admin_url: String,
    database_name: String,
}

impl TestDatabase {
    pub async fn create(admin_url: &str) -> Self {
        let mut parsed = Url::parse(admin_url).expect("TEST_DATABASE_URL must be a valid URL");
        let database_name = format!("agent_note_test_{}", Uuid::new_v4().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(admin_url)
            .await
            .expect("connect to PostgreSQL administrative database");
        sqlx::query(AssertSqlSafe(format!(
            r#"CREATE DATABASE "{database_name}""#
        )))
        .execute(&admin_pool)
        .await
        .expect("create isolated PostgreSQL test database");
        admin_pool.close().await;

        parsed.set_path(&format!("/{database_name}"));
        parsed.set_query(None);
        parsed.set_fragment(None);

        Self {
            url: parsed.to_string(),
            admin_url: admin_url.to_owned(),
            database_name,
        }
    }

    pub async fn provision_vector(&self) {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.url)
            .await
            .expect("connect to isolated PostgreSQL test database");
        sqlx::query("CREATE EXTENSION vector")
            .execute(&pool)
            .await
            .expect("provision vector extension");
        pool.close().await;
    }

    pub async fn inspect_pool(&self) -> PgPool {
        PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.url)
            .await
            .expect("connect to isolated PostgreSQL test database")
    }

    pub async fn cleanup(self, storage: Option<&PgStorage>) {
        if let Some(storage) = storage {
            storage.close().await;
        }

        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .expect("reconnect to PostgreSQL administrative database");
        sqlx::query(
            "SELECT pg_terminate_backend(pid)
             FROM pg_stat_activity
             WHERE datname = $1 AND pid <> pg_backend_pid()",
        )
        .bind(&self.database_name)
        .execute(&admin_pool)
        .await
        .expect("terminate isolated PostgreSQL test connections");
        sqlx::query(AssertSqlSafe(format!(
            r#"DROP DATABASE IF EXISTS "{}" WITH (FORCE)"#,
            self.database_name
        )))
        .execute(&admin_pool)
        .await
        .expect("drop isolated PostgreSQL test database");
        admin_pool.close().await;
    }
}
