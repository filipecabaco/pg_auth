//! The engine's rules kept in Postgres (sql/rules.sql, authz.set_rules): the latest version of
//! each is pushed to the engine, and whether it took it recorded beside it.
//!
//! One sidecar pushes at a time (authz.rules_pending takes an advisory lock for its transaction).
//! Rules the engine refuses (an invalid model, a policy that does not parse) are recorded with the
//! error, and the engine keeps the version before; a failure it may get over (unreachable) is
//! retried. After a push every backend forgets the answers it kept (authz.answers_changed), since
//! new rules may change any of them. It wakes on authz.set_rules's NOTIFY, and every `poll` in case
//! one was missed. Optional: enabled by PG_AUTH_DATABASE_URL.

use crate::engine::{Engine, Rule};
use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_postgres::{AsyncMessage, Client, NoTls};

/// Push the rules for as long as the sidecar runs; `pushed` is called after rules the engine took
/// (the sidecar then forgets the answers it cached).
pub fn start(url: String, engine: Arc<Engine>, poll: Duration, pushed: impl Fn() + Send + Sync + 'static) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = push(&url, &engine, poll, &pushed).await {
                eprintln!("pg-auth-sidecar: rules: {e}");
            }
            tokio::time::sleep(poll).await;
        }
    });
}

async fn push(url: &str, engine: &Engine, poll: Duration, pushed: &(impl Fn() + Send + Sync)) -> Result<(), String> {
    let (mut client, mut connection) = tokio_postgres::connect(url, NoTls).await.map_err(|e| format!("postgres: {e}"))?;
    let (wake, mut woken) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut messages = futures_util::stream::poll_fn(move |cx| connection.poll_message(cx));
        while let Some(message) = messages.next().await {
            match message {
                Ok(AsyncMessage::Notification(_)) => {
                    let _ = wake.send(());
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("pg-auth-sidecar: rules connection: {e}");
                    break;
                }
            }
        }
    });
    client.batch_execute("listen pg_auth_rules").await.map_err(|e| format!("postgres: {e}"))?;
    loop {
        match once(&mut client, engine).await {
            Ok(false) => {
                if client.is_closed() {
                    return Err("postgres connection closed".into());
                }
                let _ = tokio::time::timeout(poll, woken.recv()).await;
                while woken.try_recv().is_ok() {}
            }
            Ok(true) => pushed(),
            Err(e) => {
                eprintln!("pg-auth-sidecar: rules: {e}; retrying");
                if client.is_closed() {
                    return Err("postgres connection closed".into());
                }
                tokio::time::sleep(poll).await;
            }
        }
    }
}

/// Push what is pending: whether the engine took new rules.
async fn once(client: &mut Client, engine: &Engine) -> Result<bool, String> {
    let pg = |e: tokio_postgres::Error| format!("postgres: {e}");
    let tx = client.transaction().await.map_err(pg)?;
    let rows = tx.query("select name, version, body, previous from authz.rules_pending($1)", &[&engine.name()]).await.map_err(pg)?;
    if rows.is_empty() {
        tx.commit().await.map_err(pg)?;
        return Ok(false);
    }
    let rules: Vec<Rule> = rows.iter().map(|r| Rule { name: r.get(0), body: r.get(2), previous: r.get(3) }).collect();
    let versions: Vec<(String, i32)> = rows.iter().map(|r| (r.get(0), r.get(1))).collect();
    let error = match engine.push_rules(&rules).await {
        Ok(()) => None,
        // the engine unreachable: the rules stay pending, pushed again later
        Err(e) if !e.permanent => return Err(e.message),
        Err(e) => Some(e.message),
    };
    for (name, version) in &versions {
        tx.execute("select authz.rules_pushed($1, $2, $3, $4)", &[&engine.name(), name, version, &error]).await.map_err(pg)?;
    }
    if error.is_none() {
        tx.execute("select authz.answers_changed()", &[]).await.map_err(pg)?;
    }
    tx.commit().await.map_err(pg)?;
    match &error {
        None => eprintln!(
            "pg-auth-sidecar: rules pushed: {}",
            versions.iter().map(|(n, v)| format!("{n} version {v}")).collect::<Vec<_>>().join(", ")
        ),
        Some(e) => eprintln!("pg-auth-sidecar: rules refused by the engine, kept the version before: {e}"),
    }
    Ok(error.is_none())
}
