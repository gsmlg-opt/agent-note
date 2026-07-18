use note_storage::{StorageError, StorageErrorKind, StorageResult, TransactionMode};
use sqlx::pool::PoolConnection;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use std::ops::{Deref, DerefMut};
use tokio::sync::{Mutex, MutexGuard};

const IMMEDIATE_ADVISORY_LOCK_KEY: i64 = 0x4147_4E54_4E4F_5445;

#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
enum PgConnectionState {
    Session(PoolConnection<Postgres>),
    Transaction(Transaction<'static, Postgres>),
}

#[derive(Debug)]
pub struct PgSession {
    state: Mutex<Option<PgConnectionState>>,
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct PgConnectionGuard<'a> {
    state: MutexGuard<'a, Option<PgConnectionState>>,
}

impl PgConnectionState {
    #[cfg_attr(not(test), allow(dead_code))]
    fn connection(&mut self) -> &mut PgConnection {
        match self {
            Self::Session(connection) => connection.as_mut(),
            Self::Transaction(transaction) => &mut *transaction,
        }
    }
}

impl Deref for PgConnectionGuard<'_> {
    type Target = PgConnection;

    fn deref(&self) -> &Self::Target {
        match self.state.as_ref() {
            Some(PgConnectionState::Session(connection)) => connection.as_ref(),
            Some(PgConnectionState::Transaction(transaction)) => transaction,
            None => unreachable!("connection guards are only created for open sessions"),
        }
    }
}

impl DerefMut for PgConnectionGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.state
            .as_mut()
            .expect("connection guards are only created for open sessions")
            .connection()
    }
}

impl PgSession {
    pub(crate) async fn connect(pool: &PgPool) -> StorageResult<Self> {
        let connection = pool
            .acquire()
            .await
            .map_err(|error| map_sqlx_error("acquire PostgreSQL session", error))?;
        Ok(Self {
            state: Mutex::new(Some(PgConnectionState::Session(connection))),
        })
    }

    pub(crate) async fn begin(pool: PgPool, mode: TransactionMode) -> StorageResult<Self> {
        let mut transaction = pool
            .begin()
            .await
            .map_err(|error| map_transaction_error("begin PostgreSQL transaction", error))?;
        if mode == TransactionMode::Immediate {
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(IMMEDIATE_ADVISORY_LOCK_KEY)
                .execute(&mut *transaction)
                .await
                .map_err(|error| {
                    map_transaction_error("acquire immediate PostgreSQL advisory lock", error)
                })?;
        }
        Ok(Self {
            state: Mutex::new(Some(PgConnectionState::Transaction(transaction))),
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn connection(&self) -> StorageResult<PgConnectionGuard<'_>> {
        let state = self.state.lock().await;
        if state.is_none() {
            return Err(StorageError::new(
                StorageErrorKind::Transaction,
                "use PostgreSQL storage session after finalization",
            ));
        }
        Ok(PgConnectionGuard { state })
    }

    pub async fn commit(self) -> StorageResult<()> {
        Box::new(self).finish(true).await
    }

    pub async fn rollback(self) -> StorageResult<()> {
        Box::new(self).finish(false).await
    }

    pub(crate) async fn finish(self: Box<Self>, commit: bool) -> StorageResult<()> {
        let state = self.state.lock().await.take();
        let Some(PgConnectionState::Transaction(transaction)) = state else {
            return Err(StorageError::new(
                StorageErrorKind::Transaction,
                "finalize PostgreSQL storage session without an open transaction",
            ));
        };
        if commit {
            transaction
                .commit()
                .await
                .map_err(|error| map_transaction_error("commit PostgreSQL transaction", error))
        } else {
            transaction
                .rollback()
                .await
                .map_err(|error| map_transaction_error("rollback PostgreSQL transaction", error))
        }
    }
}

pub(crate) fn map_connect_error(error: sqlx::Error) -> StorageError {
    StorageError::new(classify_sqlx_error(&error), "connect to PostgreSQL")
}

pub(crate) fn map_sqlx_error(context: &str, error: sqlx::Error) -> StorageError {
    let kind = classify_sqlx_error(&error);
    StorageError::with_source(kind, context, error)
}

fn classify_sqlx_error(error: &sqlx::Error) -> StorageErrorKind {
    match error {
        sqlx::Error::Database(database) => match database.code().as_deref() {
            Some("23502" | "23503" | "23505" | "23514" | "23P01") => StorageErrorKind::Constraint,
            Some("40001" | "40P01" | "55P03") => StorageErrorKind::Conflict,
            Some(code) if code.starts_with("08") => StorageErrorKind::Unavailable,
            Some("57P01" | "57P02" | "57P03") => StorageErrorKind::Unavailable,
            _ => StorageErrorKind::Operation,
        },
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::Protocol(_)
        | sqlx::Error::PoolClosed
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::WorkerCrashed => StorageErrorKind::Unavailable,
        sqlx::Error::BeginFailed => StorageErrorKind::Transaction,
        _ => StorageErrorKind::Operation,
    }
}

fn map_transaction_error(context: &str, error: sqlx::Error) -> StorageError {
    let kind = match classify_sqlx_error(&error) {
        StorageErrorKind::Operation => StorageErrorKind::Transaction,
        specific => specific,
    };
    StorageError::with_source(kind, context, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use note_storage::TransactionMode;
    use sqlx::postgres::PgPoolOptions;
    use sqlx::{AssertSqlSafe, Executor, PgPool};
    use std::error::Error as _;
    use std::io;
    use std::time::Duration;
    use tokio::time::timeout;

    async fn test_pool(test_name: &str, max_connections: u32) -> Option<PgPool> {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!(
                "skipping {test_name}: TEST_DATABASE_URL is not set; no external database was accessed"
            );
            return None;
        };

        Some(
            PgPoolOptions::new()
                .max_connections(max_connections)
                .connect(&url)
                .await
                .expect("connect to TEST_DATABASE_URL"),
        )
    }

    async fn assert_real_error_kind(pool: &PgPool, sql: &str, expected: StorageErrorKind) {
        let error = sqlx::query(AssertSqlSafe(sql.to_owned()))
            .execute(pool)
            .await
            .expect_err("query must produce the expected PostgreSQL error");
        assert_eq!(
            map_sqlx_error("test PostgreSQL error", error).kind(),
            expected
        );
    }

    async fn assert_real_transaction_error_kind(
        pool: &PgPool,
        sql: &str,
        expected: StorageErrorKind,
    ) {
        let error = sqlx::query(AssertSqlSafe(sql.to_owned()))
            .execute(pool)
            .await
            .expect_err("query must produce the expected PostgreSQL transaction error");
        assert_eq!(
            map_transaction_error("test PostgreSQL transaction error", error).kind(),
            expected
        );
    }

    #[tokio::test]
    async fn unfinished_owned_transaction_rolls_back_before_pool_reuse() {
        let Some(pool) = test_pool(
            "unfinished_owned_transaction_rolls_back_before_pool_reuse",
            1,
        )
        .await
        else {
            return;
        };

        pool.execute("CREATE TEMP TABLE drop_rollback_test (value integer NOT NULL)")
            .await
            .unwrap();
        let session = PgSession::begin(pool.clone(), TransactionMode::Deferred)
            .await
            .unwrap();
        {
            let mut connection = session.connection().await.unwrap();
            sqlx::query("INSERT INTO drop_rollback_test (value) VALUES (1)")
                .execute(&mut *connection)
                .await
                .unwrap();
        }
        drop(session);

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM drop_rollback_test")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        pool.close().await;
    }

    #[tokio::test]
    async fn deferred_transactions_may_begin_concurrently() {
        let Some(pool) = test_pool("deferred_transactions_may_begin_concurrently", 2).await else {
            return;
        };

        let (first, second) = timeout(Duration::from_secs(2), async {
            tokio::join!(
                PgSession::begin(pool.clone(), TransactionMode::Deferred),
                PgSession::begin(pool.clone(), TransactionMode::Deferred)
            )
        })
        .await
        .expect("deferred transactions must not serialize");
        let first = first.unwrap();
        let second = second.unwrap();
        first.rollback().await.unwrap();
        second.rollback().await.unwrap();
        pool.close().await;
    }

    async fn assert_immediate_serialization(pool: &PgPool, commit_holder: bool) {
        let holder = PgSession::begin(pool.clone(), TransactionMode::Immediate)
            .await
            .unwrap();
        {
            let mut connection = holder.connection().await.unwrap();
            let reentrant: bool =
                sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(4703814459809813573)")
                    .fetch_one(&mut *connection)
                    .await
                    .unwrap();
            assert!(
                reentrant,
                "the immediate lock holder must own the plan lock key"
            );
        }

        let pool_for_waiter = pool.clone();
        let mut waiter = tokio::spawn(async move {
            PgSession::begin(pool_for_waiter, TransactionMode::Immediate).await
        });
        assert!(
            timeout(Duration::from_millis(100), &mut waiter)
                .await
                .is_err(),
            "a second immediate transaction must remain blocked"
        );

        if commit_holder {
            holder.commit().await.unwrap();
        } else {
            holder.rollback().await.unwrap();
        }
        let waiter = timeout(Duration::from_secs(2), &mut waiter)
            .await
            .expect("the waiter must acquire the lock after holder finalization")
            .expect("waiter task must not panic")
            .expect("waiter transaction must begin");
        waiter.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn immediate_transactions_serialize_after_commit_and_rollback() {
        let Some(pool) = test_pool(
            "immediate_transactions_serialize_after_commit_and_rollback",
            2,
        )
        .await
        else {
            return;
        };

        assert_immediate_serialization(&pool, true).await;
        assert_immediate_serialization(&pool, false).await;
        pool.close().await;
    }

    #[tokio::test]
    async fn finalizing_nontransaction_sessions_returns_transaction_errors() {
        let Some(pool) = test_pool(
            "finalizing_nontransaction_sessions_returns_transaction_errors",
            1,
        )
        .await
        else {
            return;
        };

        let commit_error = PgSession::connect(&pool)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap_err();
        assert_eq!(commit_error.kind(), StorageErrorKind::Transaction);

        let rollback_error = PgSession::connect(&pool)
            .await
            .unwrap()
            .rollback()
            .await
            .unwrap_err();
        assert_eq!(rollback_error.kind(), StorageErrorKind::Transaction);
        pool.close().await;
    }

    #[tokio::test]
    async fn finalizing_an_empty_session_state_returns_a_transaction_error() {
        let session = PgSession {
            state: Mutex::new(None),
        };
        let error = Box::new(session).finish(true).await.unwrap_err();
        assert_eq!(error.kind(), StorageErrorKind::Transaction);
    }

    #[tokio::test]
    async fn real_postgresql_constraint_sqlstates_map_to_constraint() {
        let Some(pool) =
            test_pool("real_postgresql_constraint_sqlstates_map_to_constraint", 1).await
        else {
            return;
        };

        pool.execute(
            "CREATE TEMP TABLE constraint_parent (
                id integer PRIMARY KEY
            );
            CREATE TEMP TABLE constraint_child (
                id integer UNIQUE,
                parent_id integer REFERENCES constraint_parent(id),
                required integer NOT NULL,
                positive integer CHECK (positive > 0)
            );
            CREATE TEMP TABLE exclusion_test (
                period int4range,
                EXCLUDE USING gist (period WITH &&)
            );
            INSERT INTO constraint_parent (id) VALUES (1);
            INSERT INTO constraint_child (id, parent_id, required, positive)
            VALUES (1, 1, 1, 1);
            INSERT INTO exclusion_test (period) VALUES ('[1,3)');",
        )
        .await
        .unwrap();

        for sql in [
            "INSERT INTO constraint_child (id, parent_id, required, positive) VALUES (1, 1, 1, 1)",
            "INSERT INTO constraint_child (id, parent_id, required, positive) VALUES (2, 999, 1, 1)",
            "INSERT INTO constraint_child (id, parent_id, required, positive) VALUES (3, 1, 1, 0)",
            "INSERT INTO constraint_child (id, parent_id, required, positive) VALUES (4, 1, NULL, 1)",
            "INSERT INTO exclusion_test (period) VALUES ('[2,4)')",
        ] {
            assert_real_error_kind(&pool, sql, StorageErrorKind::Constraint).await;
        }
        pool.close().await;
    }

    #[tokio::test]
    async fn raised_conflict_and_unavailable_sqlstates_map_to_stable_kinds() {
        let Some(pool) = test_pool(
            "raised_conflict_and_unavailable_sqlstates_map_to_stable_kinds",
            1,
        )
        .await
        else {
            return;
        };

        for code in ["40001", "40P01", "55P03"] {
            let sql = format!("DO 'BEGIN RAISE SQLSTATE ''{code}''; END'");
            assert_real_transaction_error_kind(&pool, &sql, StorageErrorKind::Conflict).await;
        }
        assert_real_error_kind(
            &pool,
            "DO 'BEGIN RAISE SQLSTATE ''08006''; END'",
            StorageErrorKind::Unavailable,
        )
        .await;
        for code in ["57P01", "57P02", "57P03"] {
            let sql = format!("DO 'BEGIN RAISE SQLSTATE ''{code}''; END'");
            assert_real_error_kind(&pool, &sql, StorageErrorKind::Unavailable).await;
        }
        pool.close().await;
    }

    #[test]
    fn transport_pool_and_begin_errors_map_to_stable_kinds() {
        let unavailable = [
            sqlx::Error::Io(io::Error::new(io::ErrorKind::ConnectionReset, "test")),
            sqlx::Error::Tls(Box::new(io::Error::other("test"))),
            sqlx::Error::Protocol("test".into()),
            sqlx::Error::PoolClosed,
            sqlx::Error::PoolTimedOut,
            sqlx::Error::WorkerCrashed,
        ];
        for error in unavailable {
            let mapped = map_sqlx_error("test PostgreSQL transport", error);
            assert_eq!(mapped.kind(), StorageErrorKind::Unavailable);
            assert!(mapped.source().is_some());
        }

        assert_eq!(
            map_sqlx_error("test PostgreSQL begin", sqlx::Error::BeginFailed).kind(),
            StorageErrorKind::Transaction
        );
        assert_eq!(
            map_transaction_error("test PostgreSQL transaction", sqlx::Error::RowNotFound).kind(),
            StorageErrorKind::Transaction
        );
        assert_eq!(
            map_transaction_error("test PostgreSQL transaction", sqlx::Error::PoolClosed).kind(),
            StorageErrorKind::Unavailable
        );
    }

    #[test]
    fn connect_mapping_classifies_without_retaining_a_source() {
        let mapped = map_connect_error(sqlx::Error::PoolTimedOut);
        assert_eq!(mapped.kind(), StorageErrorKind::Unavailable);
        assert!(mapped.source().is_none());
    }
}
