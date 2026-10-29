//! AuthzBatch's decisions at execution: how a scan reads for the subject running its plan, and
//! the engine's answers for a batch of rows. pg::exec runs the scan (paths.rs plans it).

use super::paths::share_of;
use crate::sidecar::{self, IdSet, Listed, Loaded};
use pgrx::prelude::*;
use std::collections::HashMap;
use std::rc::Rc;

/// Rows per sidecar request: the first batch is small, so `LIMIT` queries check few ids; each
/// following batch doubles, up to the size of one sidecar check request.
pub const FIRST_BATCH: usize = 16;
pub const MAX_BATCH: usize = 1000;

/// How an execution reads, decided on its first row.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// no subject: nothing is allowed
    Nothing,
    /// listed subject over a listed-ids child: its rows pass as they are
    Pass,
    /// check the rows of this child in batches
    Batch(usize),
    /// check the rows of this child one by one against the subject's loaded permission index set
    Indexed(usize),
    /// this child's rows pass unchecked: the one id they all have is allowed
    Unchecked(usize),
}

/// How the id column is matched against an index set.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IdKind {
    Int2,
    Int4,
    Int8,
    /// rendered by the type's output function, as the policy's `col::text`
    Text,
}

/// An integer id column's value, from its datum's bits; None for ids matched as text.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)] // a by-value int2/int4/int8 datum holds the value in its low bits
pub fn int_id(kind: IdKind, datum: u64) -> Option<i64> {
    match kind {
        IdKind::Int2 => Some(i64::from(datum as i16)),
        IdKind::Int4 => Some(i64::from(datum as i32)),
        IdKind::Int8 => Some(datum as i64),
        IdKind::Text => None,
    }
}

/// The table's statistics recorded at planning, for choosing an adaptive AuthzBatch's child.
#[derive(Clone)]
pub struct Adaptive {
    /// rows the query's own conditions leave
    tuples: f64,
    distinct: f64,
    /// rows the query needs in order, or -1 for all
    limit: f64,
    /// the column's most common values (as ids are listed) and their shares of the rows
    common: Rc<HashMap<String, f64>>,
}

impl Adaptive {
    /// From what paths.rs recorded: "tuples,distinct,limit", and the common values as
    /// `value US share RS` pairs.
    pub fn parse(stats: &str, common: &str) -> Option<Self> {
        let v: Vec<f64> = stats.split(',').filter_map(|x| x.parse().ok()).collect();
        let common: HashMap<String, f64> = common
            .split('\u{1e}')
            .filter_map(|entry| entry.split_once('\u{1f}').and_then(|(value, share)| Some((value.to_string(), share.parse().ok()?))))
            .collect();
        (v.len() == 3 && v[1] > 0.0).then(|| Adaptive { tuples: v[0], distinct: v[1], limit: v[2], common: Rc::new(common) })
    }

    /// Whether fetching the subject's rows through its listed ids (and sorting them) reads fewer
    /// rows than walking the ordered index until the query has enough (half as many, since the
    /// fetch reads scattered rows and sorts them). The subject's share of the rows is its ids'
    /// shares: a common value's own, the rest an even part of what the common values leave.
    fn narrow(&self, ids: &IdSet) -> bool {
        let share = share_of(ids, self.distinct, &self.common);
        if share <= 0.0 {
            return true;
        }
        let fetched = share * self.tuples;
        let walked = if self.limit > 0.0 { (self.limit / share).min(self.tuples) } else { self.tuples };
        2.0 * fetched <= walked
    }
}

/// What an AuthzBatch scan checks, and what it decided for the subject running it.
pub struct Check {
    pub object_type: String,
    pub permission: String,
    /// whether child 0 has the listed-ids filter and child 1 is the same scan without it
    pub listed: bool,
    /// child 0's filter is `col = (SELECT one id)`: it answers only for subjects with at most one
    pub single: bool,
    /// child 0 is the listed rows sorted, child 1 an ordered walk: chosen by the subject's breadth
    pub adaptive: Option<Adaptive>,
    /// the one id every row has (the query pins the column to it): checked once, not per row
    pub pinned: Option<String>,
    /// decided on the first row, from the subject running the plan
    pub mode: Option<Mode>,
    /// the subject's loaded permission index set (Mode::Indexed), and the rows checked against it
    /// not counted yet
    pub set: Option<(String, Rc<Loaded>)>,
    pub lookups: usize,
}

impl Check {
    /// How this execution reads: decided once, for the subject running the plan.
    pub fn mode(&mut self) -> Mode {
        if let Some(m) = self.mode {
            return m;
        }
        let m = match crate::subject::current_as_caller() {
            None => Mode::Nothing,
            Some(s) if !self.listed => self.indexed_or_batch(s, 0),
            Some(s) => match sidecar::list(&s, &self.object_type, &self.permission).as_deref() {
                // an adaptive plan: the walk for a subject broad enough that it reads less
                Ok(Listed::Ids(ids)) if self.adaptive.as_ref().is_some_and(|a| !a.narrow(ids)) => self.indexed_or_batch(s, 1),
                Ok(Listed::Ids(ids)) if !self.single || ids.len() <= 1 => Mode::Pass,
                // a single-id plan run by a subject with more ids: the ordered fallback, checked
                // against its listed set
                Ok(_) => self.indexed_or_batch(s, 1),
                Err(e) => fail(e),
            },
        };
        // every row has the one id the query pins the column to: one check for all of them
        let m = match (m, self.pinned.clone(), crate::subject::current_as_caller()) {
            (Mode::Batch(i) | Mode::Indexed(i), Some(id), Some(s)) => {
                let allowed = sidecar::permits_many(&s, &self.object_type, &self.permission, &[Some(id)]).unwrap_or_else(|e| fail(&e));
                if allowed.first().copied().unwrap_or(false) { Mode::Unchecked(i) } else { Mode::Nothing }
            }
            (m, _, _) => m,
        };
        self.mode = Some(m);
        m
    }

    /// Rows of `child` checked against the subject's index set when it is loaded already (this
    /// backend kept it from an earlier transaction), otherwise in batches until it is.
    fn indexed_or_batch(&mut self, subject: String, child: usize) -> Mode {
        match sidecar::loaded(&subject, &self.object_type, &self.permission) {
            Some(set) => {
                self.set = Some((subject, set));
                Mode::Indexed(child)
            }
            None => Mode::Batch(child),
        }
    }

    /// The engine's answer for a batch of `child`'s ids (None: a NULL id, denied). Answering may
    /// load the subject's index set: later rows are then checked against it.
    pub fn answer(&mut self, ids: &[Option<String>], child: usize) -> Vec<bool> {
        // a subject exists: mode() decided on batches
        let subject = crate::subject::current_as_caller().unwrap_or_default();
        let allowed = sidecar::permits_many(&subject, &self.object_type, &self.permission, ids).unwrap_or_else(|e| fail(&e));
        if let Some(set) = sidecar::loaded(&subject, &self.object_type, &self.permission) {
            self.set = Some((subject, set));
            self.mode = Some(Mode::Indexed(child));
        }
        allowed
    }

    /// Count the rows checked against the index set since the last count (authz.stats(), advice).
    pub fn count_lookups(&mut self) {
        if let Some((subject, _)) = &self.set
            && self.lookups > 0
        {
            sidecar::note_index_lookups(subject, &self.object_type, &self.permission, self.lookups);
        }
        self.lookups = 0;
    }

    /// EXPLAIN's line for the scan, checking `column`.
    pub fn explain(&self, column: &str) -> String {
        let how = match (self.listed, self.adaptive.is_some()) {
            (true, true) => "listed ids sorted for narrow subjects, or an ordered walk in batches of up to",
            (true, false) => "listed ids, or batches of up to",
            _ => "in batches of up to",
        };
        format!("{}#{} on {column}, {how} {MAX_BATCH}", self.object_type, self.permission)
    }
}

/// The engine could not answer: fail the statement (fail closed).
pub fn fail(e: &str) -> ! {
    ereport!(ERROR, PgSqlErrorCode::ERRCODE_CONNECTION_FAILURE, format!("pg_auth: {e}"));
}
