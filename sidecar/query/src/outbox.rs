//! Relationship tables (sql/relationships.sql): the rows applications write that are
//! relationships in the engine, recorded in authz_internal.outbox by a trigger, delivered here in order.
//!
//! One sidecar delivers at a time (authz_sidecar.outbox_claim takes an advisory lock for its
//! transaction): it claims the oldest changes, applies them in the engine, and removes them by id
//! in the same transaction, so a change is removed only once the engine has it. A batch that fails
//! is rolled back and delivered again later; the engines' writes and deletes are idempotent. It
//! wakes on the trigger's NOTIFY, and every `poll` in case one was missed. Optional: enabled by
//! PG_AUTH_DATABASE_URL, with the permission index.

use crate::engine::{Change, Engine};
use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_postgres::{AsyncMessage, Client};

/// Changes claimed per transaction.
const CLAIM: i32 = 500;

/// Deliver the outbox for as long as the sidecar runs; `delivered` is called after each batch the
/// engine took (the sidecar then forgets the answers it cached).
pub fn start(url: String, engine: Arc<Engine>, poll: Duration, delivered: impl Fn() + Send + Sync + 'static) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = deliver(&url, &engine, poll, &delivered).await {
                eprintln!("pg-auth-sidecar: relationship tables: {e}");
            }
            tokio::time::sleep(poll).await;
        }
    });
}

/// Connect, listen for changes, and deliver until the connection fails.
async fn deliver(url: &str, engine: &Engine, poll: Duration, delivered: &(impl Fn() + Send + Sync)) -> Result<(), String> {
    let (mut client, mut connection) = crate::pg::connect(url).await?;
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
                    eprintln!("pg-auth-sidecar: relationship tables connection: {e}");
                    break;
                }
            }
        }
    });
    client.batch_execute("listen pg_auth_outbox").await.map_err(|e| format!("postgres: {e}"))?;
    loop {
        match once(&mut client, engine).await {
            Ok(0) => {
                if client.is_closed() {
                    return Err("postgres connection closed".into());
                }
                // nothing to deliver: wait for a change (or a missed one, at the next poll)
                let _ = tokio::time::timeout(poll, woken.recv()).await;
                while woken.try_recv().is_ok() {}
            }
            Ok(_) => delivered(),
            Err(e) => {
                eprintln!("pg-auth-sidecar: relationship tables: {e}; retrying");
                if client.is_closed() {
                    return Err("postgres connection closed".into());
                }
                tokio::time::sleep(poll).await;
            }
        }
    }
}

/// Deliver one batch: how many changes it settled (0: nothing to deliver, or another sidecar is
/// delivering). A batch the engine refuses for good is delivered change by change: the changes it
/// refuses are set aside with its reason (authz_sidecar.outbox_failed), the others go on. After a
/// delivery every backend forgets the answers it kept (authz_sidecar.answers_changed, at commit).
async fn once(client: &mut Client, engine: &Engine) -> Result<usize, String> {
    let pg = |e: tokio_postgres::Error| format!("postgres: {e}");
    let tx = client.transaction().await.map_err(pg)?;
    let rows = tx
        .query(
            "select id, op, object, relation, subject, expires_at, condition, condition_context from authz_sidecar.outbox_claim($1, $2)",
            &[&CLAIM, &engine.name()],
        )
        .await
        .map_err(pg)?;
    if rows.is_empty() {
        tx.commit().await.map_err(pg)?;
        return Ok(0);
    }
    let changes: Vec<(i64, Change)> = rows
        .iter()
        .map(|r| {
            let write = r.get::<_, String>(1) == "write";
            let context =
                r.get::<_, Option<String>>(7).and_then(|c| serde_json::from_str(&c).ok()).unwrap_or_else(|| serde_json::json!({}));
            let condition = r.get::<_, Option<String>>(6).map(|name| (name, context));
            (r.get(0), Change { write, object: r.get(2), relation: r.get(3), subject: r.get(4), expires_at: r.get(5), condition })
        })
        .collect();
    let all: Vec<Change> = changes.iter().map(|(_, c)| c.clone()).collect();
    // a failure the engine may get over (it is unreachable) leaves the transaction to roll back:
    // the changes stay for the next attempt
    let (done, refused) = match engine.write(&all).await {
        Ok(()) => (changes.iter().map(|(id, _)| *id).collect::<Vec<_>>(), Vec::new()),
        Err(e) if !e.permanent => return Err(e.message),
        Err(_) => {
            let mut done = Vec::new();
            let mut refused = Vec::new();
            for (id, change) in &changes {
                match engine.write(std::slice::from_ref(change)).await {
                    Ok(()) => done.push(*id),
                    Err(e) if e.permanent => refused.push((*id, e.message)),
                    // settle what is settled; the rest waits for the next attempt
                    Err(_) => break,
                }
            }
            (done, refused)
        }
    };
    tx.execute("select authz_sidecar.outbox_done($1)", &[&done]).await.map_err(pg)?;
    for (id, reason) in &refused {
        eprintln!("pg-auth-sidecar: relationship change {id} refused by the engine, set aside: {reason}");
        tx.execute("select authz_sidecar.outbox_failed($1, $2)", &[id, reason]).await.map_err(pg)?;
    }
    if !done.is_empty() {
        tx.execute("select authz_sidecar.answers_changed()", &[]).await.map_err(pg)?;
    }
    tx.commit().await.map_err(pg)?;
    let shown: Vec<String> = changes
        .iter()
        .filter(|(id, _)| done.contains(id))
        .take(3)
        .map(|(_, c)| format!("{} {}#{}@{}", if c.write { "write" } else { "delete" }, c.object, c.relation, c.subject))
        .collect();
    let more = if done.len() > shown.len() { format!(", and {} more", done.len() - shown.len()) } else { String::new() };
    let changes_word = if done.len() == 1 { "change" } else { "changes" };
    eprintln!("pg-auth-sidecar: delivered {} relationship {changes_word}: {}{more}", done.len(), shown.join(", "));
    Ok(done.len() + refused.len())
}
