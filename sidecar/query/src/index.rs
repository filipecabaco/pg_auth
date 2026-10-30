//! The permission index (sql/index.sql; docs/proposals/permission-index.md, phase A): full
//! permission sets of subjects with more objects than the engine lists, kept in Postgres so the
//! extension answers their rows without asking the engine.
//!
//! The sidecar owns it: it fills a set when the extension asks (`fill`), by streaming the
//! engine's complete list, and it follows the engine's change feed (Engine::follow), marking
//! every set stale on any change (coarse: through inheritance a change can reach any set). The extension uses fresh
//! sets only; a set invalidated while it fills never becomes fresh (authz_sidecar.index_finish).
//! Optional: enabled by PG_AUTH_DATABASE_URL.

use crate::engine::{Engine, Feed, Key};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_postgres::{Client, NoTls};

/// Rows per insert while filling.
const CHUNK: usize = 5000;

pub struct Index {
    queue: mpsc::UnboundedSender<Key>,
    pending: Arc<Mutex<HashSet<Key>>>,
    fresh: Fresh,
}

/// Sizes of the sets this sidecar knows to be fresh: loaded at start, added when a fill
/// finishes, cleared by any change. Only a shortcut (see `Index::too_many`): the extension still
/// checks the set itself.
type Fresh = Arc<Mutex<HashMap<Key, usize>>>;

impl Index {
    pub fn start(url: String, engine: Arc<Engine>, retry: Duration) -> Index {
        let (queue, requests) = mpsc::unbounded_channel();
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let fresh: Fresh = Arc::new(Mutex::new(HashMap::new()));
        tokio::spawn(fill_loop(url.clone(), engine.clone(), requests, pending.clone(), fresh.clone()));
        tokio::spawn(feed_loop(url, engine, retry, fresh.clone()));
        Index { queue, pending, fresh }
    }

    /// Whether a fresh set shows the subject has at least `limit` objects, so the engine need
    /// not be asked to list (that alone costs a broad subject ~25 ms on OpenFGA).
    pub fn too_many(&self, key: &Key, limit: usize) -> bool {
        self.fresh.lock().ok().and_then(|f| f.get(key).copied()).is_some_and(|n| n >= limit)
    }

    /// Wait until no fill of `key` is queued or running, up to `limit`: whether its set is fresh.
    pub async fn wait(&self, key: &Key, limit: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + limit;
        while self.pending.lock().is_ok_and(|p| p.contains(key)) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.fresh.lock().is_ok_and(|f| f.contains_key(key))
    }

    /// Queue a fill; a request for a set already queued or filling is dropped.
    pub fn request(&self, key: Key) -> bool {
        let Ok(mut pending) = self.pending.lock() else { return false };
        if !pending.insert(key.clone()) {
            return true;
        }
        self.queue.send(key).is_ok()
    }
}

/// A connection to Postgres, reconnecting when the previous one closed.
struct Db {
    url: String,
    client: Option<Client>,
}

impl Db {
    async fn client(&mut self) -> Result<&Client, String> {
        match self.client.take() {
            Some(client) if !client.is_closed() => Ok(self.client.insert(client)),
            _ => {
                let (client, connection) = tokio_postgres::connect(&self.url, NoTls).await.map_err(|e| format!("postgres: {e}"))?;
                tokio::spawn(async move {
                    if let Err(e) = connection.await {
                        eprintln!("pg-auth-sidecar: index connection: {e}");
                    }
                });
                Ok(self.client.insert(client))
            }
        }
    }
}

async fn fill_loop(
    url: String,
    engine: Arc<Engine>,
    mut requests: mpsc::UnboundedReceiver<Key>,
    pending: Arc<Mutex<HashSet<Key>>>,
    fresh: Fresh,
) {
    let mut db = Db { url, client: None };
    while let Some(key) = requests.recv().await {
        match fill(&mut db, &engine, &key).await {
            Ok((n, true)) => {
                if let Ok(mut f) = fresh.lock() {
                    f.insert(key.clone(), n);
                }
                eprintln!("pg-auth-sidecar: indexed {} {}#{}: {n} objects", key.0, key.1, key.2);
            }
            Ok((_, false)) => eprintln!("pg-auth-sidecar: {} {}#{} changed while filling; left stale", key.0, key.1, key.2),
            Err(e) => eprintln!("pg-auth-sidecar: filling {} {}#{}: {e}", key.0, key.1, key.2),
        }
        if let Ok(mut p) = pending.lock() {
            p.remove(&key);
        }
    }
}

/// Fill one set. The set is marked filling (unused) before the engine is asked, so a change the
/// feed sees from then on keeps it from becoming fresh; a fill that fails is aborted, so the set
/// is not left filling (the extension would wait for it). -> (objects, became fresh)
async fn fill(db: &mut Db, engine: &Engine, key: &Key) -> Result<(usize, bool), String> {
    let client = db.client().await?;
    let pg = |e: tokio_postgres::Error| format!("postgres: {e}");
    let (subject, object_type, permission) = key;
    let set: i64 = client
        .query_one("select authz_sidecar.index_begin($1, $2, $3, null)", &[subject, object_type, permission])
        .await
        .map_err(pg)?
        .get(0);
    let filled = async {
        let ids = engine.list_all(key).await?;
        for chunk in ids.chunks(CHUNK) {
            client.execute("select authz_sidecar.index_add($1, $2)", &[&set, &chunk.to_vec()]).await.map_err(pg)?;
        }
        Ok::<_, String>(ids.len())
    }
    .await;
    let n = match filled {
        Ok(n) => n,
        Err(e) => {
            let _ = client.execute("select authz_sidecar.index_abort($1)", &[&set]).await;
            return Err(e);
        }
    };
    let fresh: Option<bool> = client.query_one("select authz_sidecar.index_finish($1)", &[&set]).await.map_err(pg)?.get(0);
    Ok((n, fresh.unwrap_or(false)))
}

/// Follow the engine's change feed and invalidate the index on any change, or when changes may
/// have been missed. A failed invalidation is retried, never dropped: until it succeeds the sets it
/// covers may be stale. The position the feed reached is kept in Postgres once everything before
/// it was handled (on every change, else at most every POSITION_EVERY), for the next feed to start
/// from: a sidecar restarting, or another starting, then keeps the sets if nothing changed.
async fn feed_loop(url: String, engine: Arc<Engine>, retry: Duration, fresh: Fresh) {
    let mut db = Db { url, client: None };
    let (start, resume) = loop {
        match begin(&mut db, &fresh, engine.name()).await {
            Ok(begun) => break begun,
            Err(e) => eprintln!("pg-auth-sidecar: change feed: {e}"),
        }
        tokio::time::sleep(retry).await;
    };
    let (feed, mut reports) = mpsc::unbounded_channel();
    let following = engine.clone();
    tokio::spawn(async move { following.follow(start, resume, retry, feed).await });
    let mut kept: Option<tokio::time::Instant> = None;
    while let Some(first) = reports.recv().await {
        // one invalidation covers every change reported so far
        let mut batch = vec![first];
        while let Ok(more) = reports.try_recv() {
            batch.push(more);
        }
        let stale = batch.iter().any(|r| !matches!(r, Feed::At(_)));
        let position = batch.into_iter().rev().find_map(|r| match r {
            Feed::Changed(at) => at,
            Feed::At(at) => Some(at),
            Feed::Gap => None,
        });
        if stale {
            if let Ok(mut f) = fresh.lock() {
                f.clear();
            }
            loop {
                match invalidate(&mut db).await {
                    Ok(()) => break,
                    Err(e) => eprintln!("pg-auth-sidecar: change feed: {e}"),
                }
                tokio::time::sleep(retry).await;
            }
        }
        if let Some(at) = position
            && (stale || kept.is_none_or(|k| k.elapsed() >= POSITION_EVERY))
        {
            // not kept: the next feed starts from an earlier position, and replays a little more
            match reached(&mut db, engine.name(), &at).await {
                Ok(()) => kept = Some(tokio::time::Instant::now()),
                Err(e) => eprintln!("pg-auth-sidecar: change feed position: {e}"),
            }
        }
    }
}

/// How often the feed's position is kept when nothing changes.
const POSITION_EVERY: Duration = Duration::from_secs(1);

/// Load the sets already fresh (filled before a restart), the moment the feed starts, and the
/// position a feed reached before, if any, to resume from.
async fn begin(db: &mut Db, fresh: &Fresh, engine: &str) -> Result<(String, Option<String>), String> {
    let client = db.client().await?;
    let pg = |e: tokio_postgres::Error| format!("postgres: {e}");
    let rows = client.query("select subject, object_type, permission, size from authz_sidecar.index_fresh()", &[]).await.map_err(pg)?;
    if let Ok(mut f) = fresh.lock() {
        for r in rows {
            f.insert((r.get(0), r.get(1), r.get(2)), usize::try_from(r.get::<_, Option<i64>>(3).unwrap_or(0)).unwrap_or(0));
        }
    }
    let resume: Option<String> = client.query_one("select authz_sidecar.feed_position($1)", &[&engine]).await.map_err(pg)?.get(0);
    let start: String =
        client.query_one(r#"select to_char(now() at time zone 'utc', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"')"#, &[]).await.map_err(pg)?.get(0);
    Ok((start, resume))
}

async fn reached(db: &mut Db, engine: &str, position: &str) -> Result<(), String> {
    let client = db.client().await?;
    client.execute("select authz_sidecar.feed_reached($1, $2)", &[&engine, &position]).await.map_err(|e| format!("postgres: {e}"))?;
    Ok(())
}

async fn invalidate(db: &mut Db) -> Result<(), String> {
    let client = db.client().await?;
    client.execute("select authz_sidecar.index_invalidate()", &[]).await.map_err(|e| format!("postgres: {e}"))?;
    Ok(())
}
