use sqlx::{ConnectOptions, PgPool};
use std::process::Command;

/// Schema setup can outlive the normal request deadline on NAS storage.
#[sqlx::test(migrations = false)]
async fn migration_cli_has_a_separate_bounded_ddl_budget(pool: PgPool) {
    sqlx::raw_sql(
        r#"
        CREATE FUNCTION public.delay_migration_setup() RETURNS event_trigger
        LANGUAGE plpgsql AS $$
        BEGIN
            IF EXISTS (
                SELECT 1 FROM pg_event_trigger_ddl_commands()
                WHERE object_identity = 'public._sqlx_migrations'
            ) THEN
                PERFORM pg_sleep(11);
            END IF;
        END;
        $$;
        CREATE EVENT TRIGGER synthetic_slow_schema ON ddl_command_end
        EXECUTE FUNCTION public.delay_migration_setup();
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let url = pool.connect_options().to_url_lossy().to_string();
    let result = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_monica-server"))
            .env_clear()
            .env("MONICA_DATABASE_URL", url)
            .arg("migrate")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(
        result.status.success(),
        "migration CLI failed: {} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM _sqlx_migrations WHERE success")
            .fetch_one(&pool)
            .await
            .unwrap()
            > 0
    );
    assert!(
        sqlx::query_scalar::<_, Option<String>>("SELECT to_regclass('crm.profiles')::text")
            .fetch_one(&pool)
            .await
            .unwrap()
            .is_some()
    );
}
