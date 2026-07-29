//! Live proof that background prefetch caches inbox bodies BEFORE any open.
//!
//! Runs the real sync path (`Core::sync_now` -> header sync -> body backfill
//! drain) against a COPY of the local Comail data dir, then reports
//! `body_state` and on-disk `.eml` presence per message. It never calls
//! `get_body`, so a `cached` result can only have come from prefetch.
//!
//! Usage:
//!   COMAIL_PROBE_DIR=/tmp/comail-prefetch-probe \
//!   COMAIL_PROBE_ACCOUNT=4 \
//!   COMAIL_PROBE_MESSAGES=7439,7260,6681 \
//!   cargo run -p comail-core --example prefetch_probe
//!
//! The caller is responsible for preparing the copy (see
//! docs/research/inbox-body-prefetch.md); the probe refuses to touch the real
//! data dir so a dogfood profile can never be mutated by a measurement.

use comail_core::config::Paths;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "comail_core::sync=info".into()),
        )
        .init();

    let dir = PathBuf::from(std::env::var("COMAIL_PROBE_DIR")?);
    let real = Paths::default_dirs().data_dir;
    if dir == real {
        return Err("refusing to run against the live data dir".into());
    }
    let account_id: i64 = std::env::var("COMAIL_PROBE_ACCOUNT")?.parse()?;
    let messages: Vec<i64> = std::env::var("COMAIL_PROBE_MESSAGES")?
        .split(',')
        .map(|part| part.trim().parse::<i64>())
        .collect::<Result<_, _>>()?;
    let budget = Duration::from_secs(
        std::env::var("COMAIL_PROBE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(180),
    );

    let core = comail_core::Core::start(Paths::for_tests(&dir)).await?;
    core.notify_ui_ready();

    report(&core, account_id, &messages, "BEFORE").await?;
    core.sync_now(Some(account_id)).await?;

    // Poll rather than assume: the backfill drain is nudged at the end of the
    // sync cycle, so the bodies land after `sync_now` has already returned.
    let deadline = Instant::now() + budget;
    loop {
        let states = states(&core, &messages).await?;
        let pending = states
            .iter()
            .filter(|(_, state, _)| state != "cached")
            .count();
        if pending == 0 || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    report(&core, account_id, &messages, "AFTER").await?;
    Ok(())
}

async fn states(
    core: &comail_core::Core,
    messages: &[i64],
) -> Result<Vec<(i64, String, Option<String>)>, Box<dyn std::error::Error>> {
    let ids = messages.to_vec();
    Ok(core
        .db
        .read(move |conn| {
            let mut out = Vec::new();
            for id in ids {
                let row = conn.query_row(
                    "SELECT body_state, raw_path FROM messages WHERE id = ?1",
                    rusqlite::params![id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )?;
                out.push((id, row.0, row.1));
            }
            Ok(out)
        })
        .await?)
}

async fn report(
    core: &comail_core::Core,
    account_id: i64,
    messages: &[i64],
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (done, total) = core
        .db
        .read(move |conn| comail_core::db::repo::messages::body_progress(conn, account_id))
        .await?;
    println!("\n=== {label}: bodies {done}/{total} cached for account {account_id} ===");
    for (id, state, raw_path) in states(core, messages).await? {
        let on_disk = raw_path
            .as_deref()
            .map(|path| {
                let path = std::path::Path::new(path);
                match std::fs::metadata(path) {
                    Ok(meta) => format!("{} ({} bytes)", path.display(), meta.len()),
                    Err(_) => format!("{} (MISSING)", path.display()),
                }
            })
            .unwrap_or_else(|| "-".into());
        println!("  message {id}: body_state={state} raw={on_disk}");
    }
    Ok(())
}
