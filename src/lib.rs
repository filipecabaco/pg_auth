// Unsafe code is confined to the modules that must manipulate Postgres memory directly:
// `rewrite` (the planner hook) and `ffi` (catalog lookups). Everything else is safe Rust.
#![deny(unsafe_code)]

use pgrx::prelude::*;

mod access;
mod advice;
mod condition;
mod explain;
#[allow(unsafe_code)]
mod ffi;
mod jwt;
mod lint;
mod nodetree;
#[allow(unsafe_code)]
mod pg;
mod rewrite;
mod sidecar;
mod subject;

::pgrx::pg_module_magic!(name, version);

extension_sql_file!("../sql/authz.sql", name = "authz_schema", bootstrap);
extension_sql_file!(
    "../sql/api.sql",
    name = "authz_api",
    requires = [
        "authz_schema",
        "authz_hierarchy",
        "authz_relationships",
        access::permits,
        access::check,
        access::engine,
        access::answers_changed,
        subject::subject_id
    ]
);
extension_sql_file!("../sql/lint.sql", name = "authz_lint", requires = ["authz_schema", lint::lint_policies]);
extension_sql_file!("../sql/index.sql", name = "authz_index", requires = ["authz_schema", subject::subject_id, subject::subject_type]);
extension_sql_file!("../sql/relationships.sql", name = "authz_relationships", requires = ["authz_schema"]);
extension_sql_file!(
    "../sql/hierarchy.sql",
    name = "authz_hierarchy",
    requires = ["authz_schema", access::allowed_text, subject::invalidate_config]
);

#[pg_guard]
pub extern "C-unwind" fn _PG_init() {
    subject::init();
    sidecar::init();
    rewrite::init();
    advice::init();
}

#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "a test fails by panicking")]
mod tests {
    use pgrx::prelude::*;

    /// What the fake engine answers: (subject, type, permission) -> the full allowed set, and
    /// whether `list` reports it as too many to enumerate.
    const ANSWERS: &[(&str, &str, &str, &[&str], bool)] = &[
        ("user:alice", "org", "view", &["1"], false),
        ("user:dana", "org", "view", &["1"], false),
        ("user:dana", "org", "edit", &["1"], false),
        ("user:bob", "org", "view", &["2"], false),
        ("user:carol", "org", "view", &["2"], false),
        ("user:both", "org", "view", &["1", "2"], false),
        // storage-style folders: the first segment of an object's path
        ("user:alice", "folder", "view", &["alice"], false),
        ("user:alice", "folder", "edit", &["alice"], false),
        ("user:wide", "folder", "view", &["alice", "bob"], true),
        ("user:alice", "document", "view", &["100"], false),
        ("user:dana", "document", "view", &["100"], false),
        ("user:bob", "document", "view", &["200"], false),
        ("user:carol", "document", "view", &["200"], false),
        ("user:jon", "document", "viewer", &["2"], false),
        // a subject the engine cannot list for: rows are checked one by one
        ("user:wide", "org", "view", &["1", "2"], true),
        ("user:wide_alice", "org", "view", &["1"], true),
        // items are protected one by one (their own id): batches check one id per row
        ("user:wide", "item", "view", &["5", "50", "150"], true),
        ("user:wide2", "item", "view", &["5"], true),
        ("user:alice", "item", "view", &["50"], false),
        // ids of every type the listed filter supports, and ids written otherwise than Postgres
        // writes them (an upper-case uuid, a padded integer): other objects, on every path
        ("user:alice", "account", "view", &["00000000-0000-0000-0000-000000000001", "00000000-0000-0000-0000-00000000000B", "nope"], false),
        ("user:one", "account", "view", &["00000000-0000-0000-0000-000000000002"], false),
        ("user:wide", "account", "view", &["00000000-0000-0000-0000-000000000001", "00000000-0000-0000-0000-000000000003"], true),
        ("user:alice", "ledger", "view", &["5000000001", "05000000002"], false),
        ("user:one", "ledger", "view", &["5000000002"], false),
        ("user:wide", "ledger", "view", &["5000000001", "5000000003"], true),
        ("user:alice", "tag", "view", &["red", "Blue"], false),
        ("user:one", "tag", "view", &["blue"], false),
        ("user:wide", "tag", "view", &["red", "green"], true),
        // declared hierarchy: only direct grants; space#viewer itself is never answered here
        ("user:alice", "space", "viewer_direct", &["2"], false),
        ("user:bob", "space", "viewer_direct", &["3", "6"], false),
    ];

    /// Fill requests the fake sidecar received ("subject type#permission").
    static FILLS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    /// A stand-in for pg-auth-sidecar, served from a background thread of this backend.
    fn fake_sidecar() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        static PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let path = PATH.get_or_init(|| {
            let path = std::env::temp_dir().join(format!("pa-{}.sock", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let listener = UnixListener::bind(&path).expect("bind fake sidecar");
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    std::thread::spawn(move || {
                        let mut out = stream.try_clone().unwrap();
                        for line in BufReader::new(stream).lines().map_while(Result::ok) {
                            let req: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
                            let field = |k: &str| req.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let (subject, ty, perm) = (field("subject"), field("type"), field("permission"));
                            let entry = ANSWERS.iter().find(|a| a.0 == subject && a.1 == ty && a.2 == perm);
                            let all: Vec<String> = if (subject.as_str(), ty.as_str(), perm.as_str()) == ("user:most", "org", "view") {
                                // listed, but most of the 2000 orgs of the estimates test
                                (1..=1800).map(|i| i.to_string()).collect()
                            } else {
                                entry.map(|a| a.3.iter().map(ToString::to_string).collect()).unwrap_or_default()
                            };
                            // a sidecar that misbehaves: answers late, hangs up, garbles, or reports
                            // the engine's error
                            match subject.as_str() {
                                "user:hang" => std::thread::sleep(std::time::Duration::from_millis(600)),
                                "user:hangup" => break,
                                _ => {}
                            }
                            let resp = match (field("op").as_str(), subject.as_str()) {
                                (_, "user:garbled") => serde_json::json!({"surprise": true}),
                                (_, "user:refused") => serde_json::json!({"error": "engine down"}),
                                (op, _) => match op {
                                // an engine that decides on attributes: alice (role user) sees her
                                // own and published rows, admin all, auditor checked row by row
                                // (her own rows), anyone else none
                                "plan" => {
                                    let roles = req["principal"]["roles"].as_array().cloned().unwrap_or_default();
                                    match subject.as_str() {
                                        "user:alice" | "user:bob" if roles.contains(&"user".into()) => serde_json::json!({"plan": "condition", "condition": {"op": "or", "args": [
                                            {"op": "eq", "args": [{"column": "owner"}, {"value": subject.trim_start_matches("user:")}]},
                                            {"op": "eq", "args": [{"column": "status"}, {"value": "published"}]}]}}),
                                        "user:admin" => serde_json::json!({"plan": "always"}),
                                        "user:lister" => serde_json::json!({"plan": "condition", "condition": {"op": "in", "args": [{"column": "status"}, {"value": ["draft", "archived"]}]}}),
                                        "user:outsider" => serde_json::json!({"plan": "condition", "condition": {"op": "not", "args": [{"op": "in", "args": [{"column": "status"}, {"value": ["draft", "archived"]}]}]}}),
                                        "user:auditor" => serde_json::json!({"plan": "rows"}),
                                        // orderings, the value on either side, a kind that is not the column's
                                        "user:ranger" => serde_json::json!({"plan": "condition", "condition": {"op": "and", "args": [
                                            {"op": "ge", "args": [{"column": "size"}, {"value": 10}]},
                                            {"op": "gt", "args": [{"value": 40}, {"column": "size"}]}]}}),
                                        "user:edges" => serde_json::json!({"plan": "condition", "condition": {"op": "or", "args": [
                                            {"op": "le", "args": [{"column": "size"}, {"value": 1}]},
                                            {"op": "lt", "args": [{"value": 49}, {"column": "size"}]}]}}),
                                        "user:mixer" => serde_json::json!({"plan": "condition", "condition": {"op": "or", "args": [
                                            {"op": "eq", "args": [{"column": "size"}, {"value": "ten"}]},
                                            {"op": "lt", "args": [{"column": "owner"}, {"value": 3}]}]}}),
                                        "user:notdraft" => serde_json::json!({"plan": "condition", "condition": {"op": "and", "args": [
                                            {"op": "ne", "args": [{"column": "status"}, {"value": "draft"}]},
                                            {"op": "not", "args": [{"op": "eq", "args": [{"column": "owner"}, {"value": "bob"}]}]}]}}),
                                        "user:anyid" => serde_json::json!({"plan": "condition", "condition": {"op": "in", "args": [{"id": true}, {"value": ["2", "4"]}]}}),
                                        _ => serde_json::json!({"plan": "never"}),
                                    }
                                }
                                "check" if req.get("attrs").is_some() => {
                                    let me = subject.split_once(':').map_or("", |(_, id)| id);
                                    let attrs = req["attrs"].as_array().cloned().unwrap_or_default();
                                    serde_json::json!({"allowed": attrs.iter().map(|a| a["owner"].as_str() == Some(me)).collect::<Vec<_>>()})
                                }
                                "list" if entry.is_some_and(|a| a.4) => serde_json::json!({"too_many": true}),
                                "list" => serde_json::json!({"ids": all}),
                                "check" => {
                                    let ids = req["ids"].as_array().cloned().unwrap_or_default();
                                    serde_json::json!({"allowed": ids.iter().map(|i| all.iter().any(|a| a == i.as_str().unwrap_or(""))).collect::<Vec<_>>()})
                                }
                                // a fill this sidecar cannot do (it has no Postgres): a waiting
                                // request hears that the set is not fresh
                                "fill" if req.get("wait_ms").is_some() => {
                                    FILLS.lock().unwrap().push(format!("{subject} {ty}#{perm} waited"));
                                    serde_json::json!({"queued": true, "fresh": false})
                                }
                                "fill" => {
                                    FILLS.lock().unwrap().push(format!("{subject} {ty}#{perm}"));
                                    serde_json::json!({"queued": true})
                                }
                                _ => serde_json::json!({"engine": "fake"}),
                                },
                            };
                            if writeln!(out, "{resp}").is_err() {
                                break;
                            }
                        }
                    });
                }
            });
            path.to_string_lossy().into_owned()
        });
        Spi::run(&format!("set pg_auth.sidecar_socket = '{path}'")).unwrap();
    }

    fn fixture() {
        fake_sidecar();
        Spi::run(
            "
            create table public.docs (id int primary key, org_id int not null, title text not null);
            insert into public.docs select i, 1 + (i % 2), 'doc ' || i from generate_series(1, 10) i;

            create table public.pages (id int primary key, document_id int not null, body text not null);
            insert into public.pages values (1, 100, 'p1'), (2, 200, 'p2');

            select authz.bind('public.docs', 'org_id', 'org', 'view', 'edit');
            select authz.bind('public.pages', 'document_id', 'document', 'view');

            create role authz_test_user nologin;
            grant select, insert, update, delete on public.docs, public.pages to authz_test_user;
            ",
        )
        .unwrap();
    }

    fn as_subject(subject: &str) {
        Spi::run(&format!("reset role; set local pg_auth.subject_id = '{subject}'; set local role authz_test_user")).unwrap();
    }

    fn reset_role() {
        Spi::run("reset role").unwrap();
    }

    /// Does `sql` raise an error? Runs it in a subtransaction, so the test can carry on.
    fn fails(sql: &str) -> bool {
        Spi::run(
            "do $d$ begin if to_regprocedure('pg_temp.fails(text)') is null then
                 create function pg_temp.fails(q text) returns boolean language plpgsql as $f$
                     begin execute q; return false; exception when others then return true; end $f$;
             end if; end $d$",
        )
        .unwrap();
        Spi::get_one_with_args::<bool>("select pg_temp.fails($1)", &[sql.into()]).unwrap().unwrap()
    }

    fn count(sql: &str) -> i64 {
        Spi::get_one::<i64>(sql).unwrap().unwrap()
    }

    fn explain(sql: &str) -> String {
        normalize(&Spi::connect(|client| {
            client
                .select(&format!("explain (costs off) {sql}"), None, &[])
                .unwrap()
                .map(|row| row.get::<String>(1).unwrap().unwrap())
                .collect::<Vec<_>>()
                .join("\n")
        }))
    }

    /// A plan as Postgres 17+ prints it: before 17, an InitPlan's header reads `InitPlan 1
    /// (returns $0)` and its value `$0` where it is used, which 17 writes `(InitPlan 1).col1`.
    fn normalize(plan: &str) -> String {
        let mut params: Vec<(String, String)> = plan
            .lines()
            .filter_map(|line| {
                let rest = line.trim_start().strip_prefix("InitPlan ")?;
                let (number, returns) = rest.split_once(" (returns $")?;
                Some((format!("${}", returns.trim_end_matches(')')), format!("(InitPlan {number}).col1")))
            })
            .collect();
        if params.is_empty() {
            return plan.to_string();
        }
        // $10 before $1
        params.sort_by_key(|(param, _)| std::cmp::Reverse(param.len()));
        plan.lines()
            .map(|line| {
                if let Some(at) = line.find(" (returns $") {
                    return line[..at].to_string();
                }
                params.iter().fold(line.to_string(), |line, (param, value)| line.replace(param, value))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[pg_test]
    fn rls_only_shows_permitted_rows_in_both_modes() {
        fixture();
        for mode in ["off", "on"] {
            Spi::run(&format!("set pg_auth.plan_lookup = {mode}")).unwrap();
            as_subject("alice");
            assert_eq!(count("select count(*) from public.docs"), 5, "mode {mode}");
            assert_eq!(count("select count(*) from public.docs where org_id = 2"), 0, "mode {mode}");
            assert_eq!(count("select count(*) from public.pages"), 1, "mode {mode}");
            as_subject("carol");
            assert_eq!(count("select count(*) from public.docs where org_id = 2"), 5, "mode {mode}");
            assert_eq!(count("select count(*) from public.docs"), 5, "mode {mode}");
            as_subject("nobody");
            assert_eq!(count("select count(*) from public.docs"), 0, "mode {mode}");
            reset_role();
        }
    }

    #[pg_test]
    fn warm_prepares_a_backend_for_its_first_request() {
        fixture();
        as_subject("alice");
        let steps: Vec<String> = Spi::connect(|client| {
            client
                .select("select step || ': ' || detail from authz.warm()", None, &[])
                .unwrap()
                .map(|row| row.get::<String>(1).unwrap().unwrap())
                .collect()
        });
        assert!(steps.iter().any(|s| s.starts_with("queries: ") && s.contains("9 of 9")), "{steps:?}");
        assert!(steps.iter().any(|s| s.starts_with("sidecar: connected")), "{steps:?}");
        assert!(steps.iter().any(|s| s.starts_with("subject: user:alice") && s.contains("2 of 2")), "{steps:?}");
        // the prefetched lists answer the first query
        assert_eq!(count("select count(*) from public.docs"), 5);
        reset_role();
    }

    /// authz.why's rows for a row of public.docs, as `step: ok`.
    fn why(id: &str, as_subject: Option<&str>) -> Vec<String> {
        let subject = as_subject.map_or("null".into(), |s| format!("'{s}'"));
        Spi::connect(|client| {
            client
                .select(&format!("select step, ok, detail from authz.why('public.docs', '{id}', as_subject => {subject})"), None, &[])
                .unwrap()
                .map(|row| {
                    format!(
                        "{}: {:?} {}",
                        row.get::<String>(1).unwrap().unwrap(),
                        row.get::<bool>(2).unwrap(),
                        row.get::<String>(3).unwrap().unwrap()
                    )
                })
                .collect()
        })
    }

    #[pg_test]
    fn why_says_whether_and_why_a_row_is_visible() {
        fixture();
        // alice's org is 1: even ids
        let visible = why("2", Some("user:alice"));
        assert!(visible.iter().any(|s| s.starts_with("subject: Some(true) user:alice (pg_auth.as_subject")), "{visible:?}");
        assert!(visible.iter().any(|s| s.starts_with("policy pg_auth: Some(true) user:alice has org#view on org:1")), "{visible:?}");
        assert!(visible.iter().any(|s| s.starts_with("verdict: Some(true) visible to user:alice")), "{visible:?}");
        let hidden = why("3", Some("user:alice"));
        assert!(hidden.iter().any(|s| s.starts_with("policy pg_auth: Some(false) user:alice lacks org#view on org:2")), "{hidden:?}");
        assert!(hidden.iter().any(|s| s.starts_with("verdict: Some(false) hidden from user:alice")), "{hidden:?}");
        let missing = why("999", Some("user:alice"));
        assert!(missing.iter().any(|s| s.starts_with("verdict: Some(false) hidden: the row does not exist")), "{missing:?}");
        // the impersonation ends with the call
        assert_eq!(Spi::get_one::<String>("select current_setting('pg_auth.as_subject')").unwrap().unwrap_or_default(), "");
    }

    #[pg_test]
    fn the_principal_carries_the_requests_scope_and_policy_version() {
        fixture();
        // from the token's scope claim, with the configured policy version
        Spi::run(
            "select authz.configure(subject => 'jwt', scope_claim => 'tenant_id', policy_version => 'v2');
             select set_config('request.jwt.claims', '{\"sub\": \"anne\", \"role\": \"user\", \"tenant_id\": \"acme\"}', true)",
        )
        .unwrap();
        let principal = crate::subject::principal().unwrap();
        assert_eq!(principal["scope"], "acme", "{principal}");
        assert_eq!(principal["policy_version"], "v2", "{principal}");
        // a session subject: pg_auth.subject_scope, as for its roles
        Spi::run(
            "select authz.configure(subject => 'guc', scope_claim => '');
             set local pg_auth.subject_id = 'bob'; set local pg_auth.subject_roles = 'user';
             set local pg_auth.subject_scope = 'globex'",
        )
        .unwrap();
        assert_eq!(crate::subject::principal().unwrap()["scope"], "globex");
        // none configured: no scope is sent
        Spi::run("set local pg_auth.subject_scope = ''").unwrap();
        assert!(crate::subject::principal().unwrap().get("scope").is_none());
    }

    #[pg_test]
    fn why_explains_a_policy_the_engine_cannot_decide_instead_of_failing() {
        fixture();
        let refused = why("2", Some("user:refused"));
        assert!(
            refused
                .iter()
                .any(|s| s.starts_with("policy pg_auth: Some(false) view on org:1 could not be decided") && s.contains("engine down")),
            "{refused:?}"
        );
        assert!(refused.iter().any(|s| s.starts_with("verdict: Some(false) hidden from user:refused")), "{refused:?}");
    }

    #[pg_test]
    fn as_subject_overrides_the_subject_source() {
        fixture();
        Spi::run("select authz.configure(subject => 'jwt')").unwrap();
        // no claims: nobody; acting as carol, whose org is 2
        Spi::run("set local pg_auth.as_subject = 'user:carol'; set local role authz_test_user").unwrap();
        assert_eq!(count("select count(*) from public.docs where org_id = 2"), 5);
        assert_eq!(count("select count(*) from public.docs where org_id = 1"), 0);
        reset_role();
        Spi::run("set local pg_auth.as_subject = ''; set local role authz_test_user").unwrap();
        assert_eq!(count("select count(*) from public.docs"), 0);
        reset_role();
    }

    #[pg_test]
    fn doctor_names_what_to_fix() {
        fixture();
        Spi::run(
            "create role anon nologin;
             create table public.leaky (id int primary key);
             grant select on public.leaky, public.docs to anon;
             alter table public.docs no force row level security;",
        )
        .unwrap();
        let checks: Vec<String> = Spi::connect(|client| {
            client
                .select("select check_name || ': ' || status || ': ' || coalesce(fix, '') from authz.doctor()", None, &[])
                .unwrap()
                .map(|row| row.get::<String>(1).unwrap().unwrap())
                .collect()
        });
        assert!(checks.iter().any(|c| c.starts_with("sidecar: ok")), "{checks:?}");
        assert!(
            checks.iter().any(|c| c.starts_with("protected public.docs: fail: alter table public.docs force row level security")),
            "{checks:?}"
        );
        assert!(
            checks.iter().any(|c| c.starts_with("protected public.pages: warn: create index concurrently on public.pages (document_id)")),
            "{checks:?}"
        );
        assert!(checks.iter().any(|c| c.starts_with("exposed public.leaky: warn")), "{checks:?}");
        assert!(!checks.iter().any(|c| c.starts_with("exposed public.docs")), "{checks:?}");
    }

    /// The ids `subject` sees in `table` (ordered, comma separated).
    fn visible_ids(subject: &str, table: &str) -> String {
        as_subject(subject);
        let ids = Spi::get_one::<String>(&format!("select coalesce(string_agg(id::text, ',' order by id), '') from {table}"))
            .unwrap()
            .unwrap_or_default();
        reset_role();
        ids
    }

    #[pg_test]
    fn every_id_type_is_listed_checked_and_paged_alike() {
        fixture();
        Spi::run(
            "create table public.accounts (id uuid primary key, n int);
             insert into public.accounts select ('00000000-0000-0000-0000-00000000000' || to_hex(i))::uuid, i from generate_series(1, 11) i;
             create table public.ledgers (id bigint primary key, n int);
             insert into public.ledgers select 5000000000 + i, i from generate_series(1, 5) i;
             create table public.tags (id text primary key, n int);
             insert into public.tags values ('red', 1), ('blue', 2), ('green', 3), ('Blue', 4);
             select authz.bind('public.accounts', 'id', 'account', 'view');
             select authz.bind('public.ledgers', 'id', 'ledger', 'view');
             select authz.bind('public.tags', 'id', 'tag', 'view');
             grant select on public.accounts, public.ledgers, public.tags to authz_test_user",
        )
        .unwrap();
        // (table, alice's ids, one's id, wide's ids): an upper-case uuid, a padded integer and
        // a string that is no uuid name no row; 'Blue' is its own text id
        let cases = [
            (
                "public.accounts",
                "00000000-0000-0000-0000-000000000001",
                "00000000-0000-0000-0000-000000000002",
                "00000000-0000-0000-0000-000000000001,00000000-0000-0000-0000-000000000003",
            ),
            ("public.ledgers", "5000000001", "5000000002", "5000000001,5000000003"),
            ("public.tags", "Blue,red", "blue", "green,red"),
        ];
        for (table, alice, one, wide) in cases {
            for mode in ["on", "off"] {
                // the listed filter (on), and the per-row check it must agree with (off)
                Spi::run(&format!("set pg_auth.plan_lookup = {mode}")).unwrap();
                assert_eq!(visible_ids("alice", table), alice, "{table}, plan_lookup {mode}");
                assert_eq!(visible_ids("one", table), one, "{table}, plan_lookup {mode}");
                assert_eq!(visible_ids("wide", table), wide, "{table}, plan_lookup {mode}");
            }
            Spi::run("reset pg_auth.plan_lookup").unwrap();
            as_subject("alice");
            assert!(explain(&format!("select * from {table}")).contains("id = ANY ((InitPlan"), "{table}: the listed filter");
            reset_role();
            // one id, an ordered page: `id = (SELECT the one id)`
            as_subject("one");
            let page = format!("select id::text from {table} order by n limit 3");
            assert!(explain(&page).contains("id = (InitPlan"), "{table}: the one-id form\n{}", explain(&page));
            assert_eq!(Spi::get_one::<String>(&page).unwrap().as_deref(), Some(one), "{table}");
            reset_role();
            // too many to list: checked in batches, against the engine
            as_subject("wide");
            assert!(explain(&format!("select * from {table}")).contains("AuthzBatch"), "{table}: batched");
            reset_role();
        }
    }

    #[pg_test]
    fn a_one_id_plan_refuses_a_subject_with_more() {
        fixture();
        Spi::run(
            "create table public.tags (id text primary key, n int);
             insert into public.tags values ('red', 1), ('blue', 2), ('green', 3), ('Blue', 4);
             select authz.bind('public.tags', 'id', 'tag', 'view');
             grant select on public.tags to authz_test_user;
             set local pg_auth.replan_on_breadth = off;
             set local plan_cache_mode = force_generic_plan",
        )
        .unwrap();
        // fails() creates its helper on first use, which would invalidate the plan below
        assert!(!fails("select 1"));
        // a page reading a system column gets no AuthzBatch (its rows reach it as copies, which
        // carry none), so a one-id plan has no fallback: reused by a subject with more ids, it
        // fails rather than show a subset
        as_subject("one");
        Spi::run("prepare page as select ctid, id from public.tags order by n limit 2").unwrap();
        assert!(explain("execute page").contains("id = (InitPlan"), "{}", explain("execute page"));
        assert_eq!(
            Spi::get_one::<String>("select string_agg(id, ',') from (select id from public.tags order by n limit 2) s").unwrap().as_deref(),
            Some("blue")
        );
        reset_role();
        as_subject("alice");
        assert!(fails("execute page"), "a subject with two ids cannot be answered by a one-id plan");
        reset_role();
    }

    #[pg_test]
    fn compiled_orderings_agree_with_the_per_row_plan() {
        fixture();
        Spi::run(
            "create table public.files (id int primary key, owner text, status text, size int);
             insert into public.files select i, case when i % 3 = 0 then 'bob' when i % 7 = 0 then null else 'alice' end,
                    case when i % 4 = 0 then 'draft' when i % 5 = 0 then null else 'published' end,
                    case when i % 11 = 0 then null else i end
             from generate_series(1, 50) i;
             select authz.protect('public.files', type => 'file', id => 'id', read => 'read', attributes => array['owner', 'status', 'size']);
             grant select on public.files to authz_test_user;
             set local pg_auth.subject_roles = 'user'",
        )
        .unwrap();
        // each subject's rows through the compiled filter (on) and evaluated per row in Rust (off)
        let expected = [
            ("ranger", "select string_agg(id::text, ',' order by id) from public.files where size >= 10 and size < 40"),
            ("edges", "select string_agg(id::text, ',' order by id) from public.files where size <= 1 or size > 49"),
            // 'ten' is no number and 3 no string: neither comparison holds for any row
            ("mixer", "select ''"),
            // CEL: null differs from 'draft', and a null owner is not bob
            (
                "notdraft",
                "select string_agg(id::text, ',' order by id) from public.files where status is distinct from 'draft' and owner is distinct from 'bob'",
            ),
            ("anyid", "select '2,4'"),
        ];
        for (subject, want) in expected {
            let want = Spi::get_one::<String>(want).unwrap().unwrap_or_default();
            for mode in ["on", "off"] {
                Spi::run(&format!("set local pg_auth.plan_lookup = {mode}")).unwrap();
                assert_eq!(visible_ids(subject, "public.files"), want, "{subject}, plan_lookup {mode}");
            }
            Spi::run("set local pg_auth.plan_lookup = on").unwrap();
            as_subject(subject);
            let rows = explained("select * from public.files");
            assert!(rows[0].contains("compiled condition"), "{subject}: {rows:?}");
            reset_role();
        }
    }

    #[pg_test]
    fn check_answers_for_any_subject() {
        fixture();
        assert_eq!(Spi::get_one::<bool>("select authz.check('user:alice', 'view', 'org:1')").unwrap(), Some(true));
        assert_eq!(Spi::get_one::<bool>("select authz.check('user:alice', 'view', 'org:2')").unwrap(), Some(false));
        assert_eq!(Spi::get_one::<bool>("select authz.check('user:carol', 'view', 'org:2')").unwrap(), Some(true));
        assert!(fails("select authz.check('alice', 'view', 'org:1')"), "a subject is type:id");
        assert!(fails("select authz.check('user:alice', 'view', 'org')"), "an object is type:id");
        Spi::run("set local role authz_test_user").unwrap();
        assert!(fails("select authz.check('user:alice', 'view', 'org:1')"), "administrators only");
        reset_role();
    }

    /// The error `sql` raises (None when it runs), caught in a subtransaction.
    fn error_of(sql: &str) -> Option<String> {
        Spi::run(
            "do $d$ begin if to_regprocedure('pg_temp.error_of(text)') is null then
                 create function pg_temp.error_of(q text) returns text language plpgsql as $f$
                 begin execute q; return null; exception when others then return sqlerrm; end $f$;
             end if; end $d$",
        )
        .unwrap();
        Spi::get_one_with_args::<String>("select pg_temp.error_of($1)", &[sql.into()]).unwrap()
    }

    #[pg_test]
    fn a_misbehaving_sidecar_fails_the_statement_and_nothing_leaks_after() {
        fixture();
        // the helper first: creating it later would invalidate plans mid-test
        assert_eq!(error_of("select 1"), None);
        Spi::run("set local pg_auth.sidecar_timeout_ms = 200").unwrap();
        for (subject, error) in [
            ("hang", "did not answer within pg_auth.sidecar_timeout_ms (200 ms)"),
            ("hangup", "sidecar closed the connection"),
            ("garbled", "bad sidecar response"),
            ("refused", "engine down"),
        ] {
            as_subject(subject);
            let e = error_of("select count(*) from public.docs").unwrap_or_default();
            reset_role();
            assert!(e.contains(error), "{subject}: {e}");
            // the next request gets its own answer, not one the failed request left behind
            as_subject("alice");
            assert_eq!(count("select count(*) from public.docs"), 5, "after {subject}");
            reset_role();
        }
    }

    #[pg_test]
    fn allowed_reads_only_protected_columns() {
        fixture();
        assert_eq!(error_of("select 1"), None);
        Spi::run(
            "create table public.secrets (id int primary key, password text);
             insert into public.secrets values (1, 'hunter2');
             create table public.objects (id int primary key, name text not null);
             insert into public.objects values (1, 'alice/a'), (2, 'bob/b'), (3, 'carol/c');
             select authz.bind('public.objects', null, 'folder', 'view', null, true, 'split_part(name, ''/'', 1)');
             grant execute on all functions in schema authz to authz_test_user",
        )
        .unwrap();
        // a subject too broad to list: the column's distinct values are checked with the engine,
        // which these functions read as the extension owner, so only a protected column may be
        // named by the caller
        as_subject("wide");
        let e = error_of("select authz.allowed_text('org', 'view', 'public.secrets'::regclass, 'password')").unwrap_or_default();
        assert!(e.contains("public.secrets.password is not a protected column"), "{e}");
        let e = error_of("select authz.allowed_int4('org', 'view', 'public.secrets'::regclass, '')").unwrap_or_default();
        assert!(e.contains("has no key expression"), "{e}");
        // a protected id column, and a binding's key expression: its values, checked
        let orgs = Spi::get_one::<String>(
            "select string_agg(o::text, ',' order by o) from unnest(authz.allowed_int4('org', 'view', 'public.docs'::regclass, 'org_id')) o",
        );
        assert_eq!(orgs.unwrap().as_deref(), Some("1,2"));
        let folders =
            Spi::get_one::<Vec<String>>("select authz.allowed_text('folder', 'view', 'public.objects'::regclass, '')").unwrap().unwrap();
        let mut folders = folders;
        folders.sort();
        assert_eq!(folders, ["alice", "bob"], "carol's folder is not the subject's");
        reset_role();
    }

    #[pg_test]
    fn warn_ms_warns_about_engine_heavy_statements() {
        fixture();
        Spi::run("set local pg_auth.warn_ms = 0").unwrap();
        // batched checks with the engine: a warning (with the advice as its hint), not an error
        as_subject("wide");
        assert_eq!(count("select count(*) from public.docs where id > 1"), 9);
        reset_role();
    }

    /// The outbox's changes, in order, as `op object relation subject`.
    fn outbox() -> Vec<String> {
        Spi::connect(|client| {
            client
                .select("select op || ' ' || object || ' ' || relation || ' ' || subject from authz.outbox order by id", None, &[])
                .unwrap()
                .map(|row| row.get::<String>(1).unwrap().unwrap())
                .collect()
        })
    }

    #[pg_test]
    fn relationship_tables_send_their_changes_to_the_engine() {
        Spi::run(
            "create table public.memberships (org_id int not null, user_id text not null, role text, primary key (org_id, user_id));
             insert into public.memberships values (1, 'anne', 'admin'), (2, 'bob', null);",
        )
        .unwrap();
        // the rows already there are delivered too (bob's has no role: no relationship)
        let queued = Spi::get_one::<i64>(
            "select authz.relate('public.memberships', object_type => 'org', object_column => 'org_id',
                                 subject_type => 'user', subject_column => 'user_id', relation_column => 'role')",
        )
        .unwrap();
        assert_eq!(queued, Some(1));
        assert_eq!(outbox(), ["write org:1 admin user:anne"]);
        Spi::run("delete from authz.outbox").unwrap();

        Spi::run(
            "insert into public.memberships values (3, 'carl', 'member');
             update public.memberships set role = 'member' where user_id = 'anne';
             update public.memberships set role = 'member' where user_id = 'anne';  -- no change
             update public.memberships set role = 'viewer' where user_id = 'bob';   -- from nothing
             delete from public.memberships where user_id = 'carl';",
        )
        .unwrap();
        assert_eq!(
            outbox(),
            [
                "write org:3 member user:carl",
                "delete org:1 admin user:anne",
                "write org:1 member user:anne",
                "write org:2 viewer user:bob",
                "delete org:3 member user:carl",
            ]
        );
        Spi::run("delete from authz.outbox").unwrap();

        // a group as the subject, and a fixed relation; a write rolled back never reaches the engine
        Spi::run(
            "create table public.team_docs (doc_id int, team text);
             select authz.relate('public.team_docs', object_type => 'document', object_column => 'doc_id', relation => 'viewer',
                                 subject_type => 'team', subject_column => 'team', subject_relation => 'member');
             insert into public.team_docs values (7, 'eng'), (8, null);",
        )
        .unwrap();
        assert!(fails("do $$ begin insert into public.team_docs values (9, 'ops'); raise exception 'rolled back'; end $$"));
        assert_eq!(outbox(), ["write document:7 viewer team:eng#member"]);

        // no longer delivered once unrelated
        Spi::run("select authz.unrelate('public.team_docs'); insert into public.team_docs values (10, 'ops')").unwrap();
        assert_eq!(outbox().len(), 1);
        assert!(fails(
            "select authz.relate('public.team_docs', object_type => 'd', object_column => 'nope', subject_type => 't', subject_column => 'team', relation => 'v')"
        ));
        assert!(fails(
            "select authz.relate('public.team_docs', object_type => 'd', object_column => 'doc_id', subject_type => 't', subject_column => 'team')"
        ));
    }

    #[pg_test]
    fn one_shares_table_feeds_a_declaration_per_kind_with_constants_and_allowed_relations() {
        // one table for every kind of grantee, as apps keep it; rows already there are delivered
        Spi::run(
            "create table public.shares (doc_id int, grantee_kind text, grantee_id text, role text);
             insert into public.shares values (1, 'user', 'anne', 'viewer'), (1, 'team', 'eng', 'editor'), (2, 'public', null, 'viewer');
             select authz.relate('public.shares', name => 'user', filter => 'grantee_kind = ''user''',
                                 object_type => 'doc', object_column => 'doc_id', relation_column => 'role',
                                 subject_type => 'user', subject_column => 'grantee_id', relations => array['viewer', 'editor']);
             select authz.relate('public.shares', name => 'team', filter => 'grantee_kind = ''team''',
                                 object_type => 'doc', object_column => 'doc_id', relation_column => 'role',
                                 subject_type => 'team', subject_column => 'grantee_id', subject_relation => 'member',
                                 relations => array['viewer', 'editor']);
             select authz.relate('public.shares', name => 'public', filter => 'grantee_kind = ''public''',
                                 object_type => 'doc', object_column => 'doc_id', relation => 'viewer',
                                 subject_type => 'user', subject_id => '*');",
        )
        .unwrap();
        let mut sent = outbox();
        sent.sort();
        assert_eq!(sent, ["write doc:1 editor team:eng#member", "write doc:1 viewer user:anne", "write doc:2 viewer user:*"]);
        Spi::run("delete from authz.outbox").unwrap();

        // a relation outside the declared ones fails the write itself
        assert!(
            error_of("insert into public.shares values (3, 'user', 'bob', 'owner')")
                .unwrap_or_default()
                .contains("not one of viewer, editor")
        );
        // a row changing kind moves from one declaration to the other
        Spi::run("update public.shares set grantee_kind = 'public', grantee_id = null where doc_id = 1 and grantee_kind = 'user'").unwrap();
        // (each declaration has its own trigger: they fire in name order, and touch different tuples)
        let mut moved = outbox();
        moved.sort();
        assert_eq!(moved, ["delete doc:1 viewer user:anne", "write doc:1 viewer user:*"]);
        Spi::run("delete from authz.outbox").unwrap();

        // the type and the subject's relation read from columns, the object fixed (a singleton)
        Spi::run(
            "create table public.platform_admins (kind text, id text, via text);
             select authz.relate('public.platform_admins', object_type => 'platform', object_id => 'main', relation => 'admin',
                                 subject_type_column => 'kind', subject_column => 'id', subject_relation_column => 'via');
             insert into public.platform_admins values ('user', 'root', null), ('group', 'ops', 'member')",
        )
        .unwrap();
        assert_eq!(outbox(), ["write platform:main admin user:root", "write platform:main admin group:ops#member"]);

        // each part needs exactly one source, and a filter must parse
        assert!(
            error_of(
                "select authz.relate('public.shares', name => 'x', object_type => 'doc', object_id => '1', object_column => 'doc_id',
                                 relation => 'viewer', subject_type => 'user', subject_column => 'grantee_id')"
            )
            .unwrap_or_default()
            .contains("the object's id")
        );
        assert!(fails(
            "select authz.relate('public.shares', name => 'x', filter => 'no_such_column = 1', object_type => 'doc',
                                 object_column => 'doc_id', relation => 'viewer', subject_type => 'user', subject_column => 'grantee_id')"
        ));
    }

    #[pg_test]
    fn the_outbox_is_delivered_in_order_by_one_sidecar_and_removed_by_id() {
        Spi::run(
            "insert into authz.outbox (op, object, relation, subject) values
                 ('write', 'org:1', 'member', 'user:a'), ('delete', 'org:1', 'member', 'user:a'), ('write', 'org:2', 'member', 'user:b')",
        )
        .unwrap();
        let claimed = Spi::get_one::<String>("select string_agg(op || ' ' || object, ',' order by id) from authz.outbox_claim(2)").unwrap();
        assert_eq!(claimed.as_deref(), Some("write org:1,delete org:1"));
        // the claim's lock is this transaction's: another session would get nothing until it ends
        assert_eq!(Spi::get_one::<bool>("select pg_try_advisory_xact_lock(hashtext('pg_auth.outbox'))").unwrap(), Some(true));
        let first = Spi::get_one::<i64>("select min(id) from authz.outbox").unwrap().unwrap();
        assert_eq!(Spi::get_one::<i64>(&format!("select authz.outbox_done(array[{first}, {}])", first + 2)).unwrap(), Some(2));
        assert_eq!(outbox(), ["delete org:1 member user:a"]);
        // a change the engine refused for good is set aside: the next claim skips it
        let refused = Spi::get_one::<i64>("select min(id) from authz.outbox").unwrap().unwrap();
        Spi::run(&format!("select authz.outbox_failed({refused}, 'openfga write 400: no relation member')")).unwrap();
        Spi::run("insert into authz.outbox (op, object, relation, subject) values ('write', 'org:3', 'member', 'user:c')").unwrap();
        let claimed = Spi::get_one::<String>("select string_agg(object, ',') from authz.outbox_claim(10)").unwrap();
        assert_eq!(claimed.as_deref(), Some("org:3"));
        // ordinary roles cannot read or change it, nor tell backends to forget their answers
        Spi::run("create role authz_outsider nologin; set local role authz_outsider").unwrap();
        assert!(fails("select * from authz.outbox"));
        assert!(fails("select * from authz.outbox_claim(10)"));
        assert!(fails("select authz.answers_changed()"));
        Spi::run("reset role").unwrap();
        Spi::run("select authz.answers_changed()").unwrap();
    }

    #[pg_test]
    fn doctor_reports_refused_relationship_changes() {
        fixture();
        Spi::run(
            "create table public.members (org_id int, user_id text);
             select authz.relate('public.members', object_type => 'org', object_column => 'org_id', relation => 'member',
                                 subject_type => 'user', subject_column => 'user_id');
             insert into authz.outbox (op, object, relation, subject, error) values ('write', 'org:1', 'member', 'user:a', 'no relation member')",
        )
        .unwrap();
        let row =
            Spi::get_one::<String>("select status || ': ' || detail from authz.doctor() where check_name = 'relationship tables'").unwrap();
        assert_eq!(row.as_deref(), Some("fail: 1 changes the engine refused, e.g. no relation member"));
    }

    #[pg_test]
    fn no_subject_means_no_rows() {
        fixture();
        Spi::run("set local role authz_test_user").unwrap();
        assert_eq!(count("select count(*) from public.docs"), 0);
        reset_role();
    }

    #[pg_test]
    fn jwt_claims_identify_the_subject() {
        fixture();
        Spi::run("select authz.configure(subject => 'jwt')").unwrap();
        Spi::run(r#"set local role authz_test_user; set local request.jwt.claims = '{"sub": "bob"}'"#).unwrap();
        assert_eq!(count("select count(*) from public.docs where org_id = 2"), 5);
        assert_eq!(count("select count(*) from public.docs where org_id = 1"), 0);
        reset_role();
    }

    #[pg_test]
    fn listed_subjects_get_an_indexable_filter() {
        fixture();
        as_subject("alice");
        let plan =
            explain("select d.id, p.body from public.docs d join public.pages p on p.id = d.id where d.id > 1 order by d.id limit 3");
        // a join: the listed-ids form, which a cached plan reuses for any listed subject
        assert!(
            plan.contains("org_id = ANY ((InitPlan") && plan.contains("document_id = ANY ((InitPlan"),
            "expected the listed-ids filter:\n{plan}"
        );
        assert!(!plan.contains("permits"), "the per-row check should be gone:\n{plan}");
        assert_eq!(count("select count(*) from public.docs"), 5);
        // listed rows pass through AuthzBatch as they are; computed output comes from a node above
        let titles = Spi::get_one::<String>("select string_agg(upper(title), ',' order by id) from public.docs where id < 5").unwrap();
        assert_eq!(titles.as_deref(), Some("DOC 2,DOC 4"));
        assert_eq!(count("select count(*) from (select upper(title) t, id * 2 from public.docs) s"), 5);

        // rescanned with a new parameter per outer row, and instrumented by EXPLAIN ANALYZE
        let lateral = "select string_agg(o.n || ':' || d.id, ',' order by o.n, d.id) from generate_series(1, 3) o(n)
                       cross join lateral (select id from public.docs where id <= o.n * 4 offset 0) d";
        let want = Some("1:2,1:4,2:2,2:4,2:6,2:8,3:2,3:4,3:6,3:8,3:10");
        assert_eq!(Spi::get_one::<String>(lateral).unwrap().as_deref(), want);
        // the same through AuthzBatch's pass-through, which wraps a listed scan when cached plans
        // are reused across breadths (pg_auth.replan_on_breadth off)
        Spi::run("set local pg_auth.replan_on_breadth = off").unwrap();
        assert_eq!(Spi::get_one::<String>(lateral).unwrap().as_deref(), want);
        let analyzed = Spi::connect(|client| {
            client
                .select(&format!("explain (analyze, costs off, timing off, summary off) {lateral}"), None, &[])
                .unwrap()
                .map(|row| row.get::<String>(1).unwrap().unwrap())
                .collect::<Vec<_>>()
                .join("\n")
        });
        // 18 prints rows per loop with decimals (3.67), earlier versions rounded (4)
        assert!(analyzed.contains("Custom Scan (AuthzBatch) on docs (actual rows=") && analyzed.contains(" loops=3)"), "{analyzed}");
        assert_eq!(Spi::get_one::<String>(lateral).unwrap().as_deref(), want, "after EXPLAIN ANALYZE");
        reset_role();
    }

    #[pg_test]
    fn broad_subjects_are_checked_in_batches() {
        fixture();
        as_subject("wide");
        let sql = "select id from public.docs where id > 1";
        let plan = explain(sql);
        assert!(plan.contains("AuthzBatch") && plan.contains("Authz Check: org#view on org_id"), "{plan}");
        assert!(!plan.contains("permits") && !plan.contains("InitPlan"), "{plan}");
        assert_eq!(count("select count(*) from public.docs"), 10);

        // locking reads (and writes) need rechecks AuthzBatch does not do: rows are checked one by one
        let plan = explain("select id from public.docs for update");
        assert!(plan.contains("permits") && !plan.contains("AuthzBatch"), "{plan}");
        // and lock only rows the write permission allows, which wide does not have
        assert_eq!(count("select count(*) from (select id from public.docs for update) d"), 0);

        Spi::run("set local pg_auth.batch_check = off").unwrap();
        assert!(explain(sql).contains("permits"), "batching off: per-row check");
        assert_eq!(count("select count(*) from public.docs"), 10);
        reset_role();

        as_subject("alice");
        Spi::run("set local pg_auth.plan_lookup = off; set local pg_auth.batch_check = on").unwrap();
        assert!(explain(sql).contains("AuthzBatch"), "lookup off: rows are checked in batches");
        assert_eq!(count("select count(*) from public.docs"), 5);
        reset_role();
    }

    #[pg_test]
    fn batches_check_only_the_rows_a_scan_reaches() {
        fixture();
        Spi::run(
            "create table public.items (id int primary key);
             insert into public.items select generate_series(1, 200);
             select authz.bind('public.items', 'id', 'item', 'view');
             grant select on public.items to authz_test_user",
        )
        .unwrap();
        let ids_checked = || Spi::get_one::<i64>("select ids_checked from authz.stats()").unwrap().unwrap();
        let calls = || Spi::get_one::<i64>("select sidecar_calls from authz.stats()").unwrap().unwrap();
        let ids = |sql: &str| Spi::get_one::<String>(&format!("select string_agg(id::text, ',' order by id) from ({sql}) q")).unwrap();

        as_subject("wide");
        let before = ids_checked();
        // ordered by the primary key (an index-only scan): the batch keeps that order, so LIMIT
        // stops within the first batch
        let sql = "select id from public.items order by id limit 1";
        let plan = explain(sql);
        assert!(plan.contains("AuthzBatch") && plan.contains("Scan using items_pkey"), "{plan}");
        assert_eq!(ids(sql).as_deref(), Some("5"));
        assert_eq!(ids_checked() - before, 16, "LIMIT 1 stops after the first batch of 16");

        // batches of 16, 32, 64, 128: the first is answered from the transaction's cache
        let (before, before_calls) = (ids_checked(), calls());
        assert_eq!(ids("select id from public.items").as_deref(), Some("5,50,150"));
        assert_eq!(ids_checked() - before, 184, "only ids not checked yet are sent");
        assert_eq!(calls() - before_calls, 3, "one request per batch with unknown ids");
        reset_role();
    }

    /// Write a permission index set as the sidecar would; `finish` makes it usable.
    fn index_set(subject: &str, ids: &[&str], finish: bool) {
        let ids = format!("{{{}}}", ids.join(","));
        Spi::run(&format!(
            "select authz.index_add(authz.index_begin('{subject}', 'item', 'view', 'r1'), '{ids}');
             {}",
            if finish {
                format!("select authz.index_finish(set_id) from authz.permission_sets where subject = '{subject}'")
            } else {
                String::new()
            }
        ))
        .unwrap();
    }

    #[pg_test]
    fn rows_protected_by_their_attributes_follow_the_subjects_plan() {
        fixture();
        Spi::run(
            "create table public.notes (id int primary key, owner text, status text);
             insert into public.notes values (1, 'alice', 'draft'), (2, 'bob', 'draft'), (3, 'bob', 'published'), (4, null, 'draft'), (5, 'auditor', 'draft');
             create index on public.notes (owner);
             select authz.protect('public.notes', 'note', 'id', 'read', attributes => array['owner', 'status']);
             grant select on public.notes to authz_test_user",
        )
        .unwrap();
        let visible = |subject: &str, roles: &str| {
            Spi::run(&format!("reset role; set local pg_auth.subject_roles = '{roles}'")).unwrap();
            as_subject(subject);
            let ids = Spi::get_one::<String>("select coalesce(string_agg(id::text, ',' order by id), '') from public.notes")
                .unwrap()
                .unwrap_or_default();
            reset_role();
            ids
        };
        assert_eq!(visible("alice", "user"), "1,3", "her own rows and the published one");
        assert_eq!(visible("alice", "guest"), "", "the plan follows her roles");
        assert_eq!(visible("admin", "admin"), "1,2,3,4,5");
        assert_eq!(visible("auditor", "auditor"), "5", "checked row by row with its attributes");
        assert_eq!(visible("nobody", "user"), "");
        assert_eq!(visible("lister", "user"), "1,2,4,5", "in a list of values");
        assert_eq!(visible("outsider", "user"), "3", "not in a list: published only");

        // the plan compiled into a filter the planner sees, its values read at execution
        Spi::run("reset role; set local pg_auth.subject_roles = 'user'").unwrap();
        as_subject("alice");
        let rows = explained("select * from public.notes");
        assert!(rows[0].contains("compiled condition") && rows[0].contains("indexable filter"), "{rows:?}");
        let plan = explain("select * from public.notes");
        assert!(plan.contains("(owner = (InitPlan") && !plan.contains("permits_resource"), "{plan}");
        assert!(!plan.contains("'alice'"), "no subject's value is a constant of the plan:\n{plan}");
        // an index on an attribute serves its comparison, as it serves RLS
        Spi::run("set local enable_seqscan = off").unwrap();
        let plan = explain("select * from public.notes where owner = 'alice'");
        assert!(plan.contains("notes_owner_idx"), "{plan}");
        Spi::run("reset enable_seqscan").unwrap();
        reset_role();
        // a cached plan, built for alice, run by bob (same shape) and by an admin (another)
        Spi::run("set local plan_cache_mode = force_generic_plan; prepare mine as select string_agg(id::text, ',' order by id) from public.notes").unwrap();
        as_subject("alice");
        assert_eq!(Spi::get_one::<String>("execute mine").unwrap().as_deref(), Some("1,3"));
        reset_role();
        as_subject("bob");
        assert_eq!(Spi::get_one::<String>("execute mine").unwrap().as_deref(), Some("2,3"));
        reset_role();
        // a SET of another shape's principal rebuilds the cached plans first
        Spi::run("set local pg_auth.subject_roles = 'admin'").unwrap();
        as_subject("admin");
        assert_eq!(Spi::get_one::<String>("execute mine").unwrap().as_deref(), Some("1,2,3,4,5"));
        reset_role();
        // a principal changed without a SET (inside a statement) meets the plan's guard: refused
        // rather than filtered with another shape (fails' helper made first: creating it would
        // invalidate the cached plan)
        fails("select 1");
        Spi::run(
            "set local pg_auth.subject_roles = 'user'; prepare alices as select string_agg(id::text, ',' order by id) from public.notes",
        )
        .unwrap();
        as_subject("alice");
        assert_eq!(Spi::get_one::<String>("execute alices").unwrap().as_deref(), Some("1,3"));
        reset_role();
        Spi::run("select set_config('pg_auth.subject_roles', 'admin', true), set_config('pg_auth.subject_id', 'admin', true), set_config('role', 'authz_test_user', true)").unwrap();
        assert!(fails("execute alices"), "another shape of plan is refused rather than filtered with alice's");
        reset_role();
    }

    #[pg_test]
    fn short_lists_of_text_values_match_every_row() {
        fixture();
        // enough rows, with per-row work, for a list kept in one row's memory to be overwritten
        Spi::run(
            "create table public.notes (id int primary key, owner text, status text);
             insert into public.notes select i, md5(i::text), (array['draft', 'published', 'archived'])[1 + i % 3] from generate_series(1, 3000) i;
             select authz.protect('public.notes', 'note', 'id', 'read', attributes => array['owner', 'status']);
             grant select on public.notes to authz_test_user;
             set local pg_auth.subject_roles = 'user'",
        )
        .unwrap();
        as_subject("lister");
        assert_eq!(count("select count(*) from public.notes"), 2000, "draft and archived rows");
        reset_role();
        as_subject("outsider");
        assert_eq!(count("select count(*) from public.notes"), 1000, "published rows");
        reset_role();
    }

    #[pg_test]
    fn ids_match_integers_only_as_postgres_writes_them() {
        use crate::access::canonical_int;
        for (s, want) in [
            ("7", Some(7)),
            ("-7", Some(-7)),
            ("0", Some(0)),
            ("007", None),
            ("+7", None),
            ("-0", None),
            ("", None),
            ("-", None),
            ("7a", None),
            (" 7", None),
            ("9223372036854775808", None),
        ] {
            assert_eq!(canonical_int::<i64>(s), want, "{s:?}");
        }
        assert_eq!(canonical_int::<i32>("2147483648"), None);
    }

    #[pg_test]
    fn check_answers_for_the_subject_it_is_given() {
        fixture();
        items();
        // the current subject's index set has item 50; wide2 may see item 5 only
        index_set("user:wide", &["5", "50", "150"], true);
        Spi::run("set local pg_auth.subject_id = 'wide'").unwrap();
        assert_eq!(Spi::get_one::<bool>("select authz.check('user:wide2', 'view', 'item:50')").unwrap(), Some(false));
        assert_eq!(Spi::get_one::<bool>("select authz.check('user:wide2', 'view', 'item:5')").unwrap(), Some(true));
    }

    #[pg_test]
    fn allowed_ids_read_only_protected_columns() {
        fixture();
        // a subject the engine cannot list and with no index set: the column's values are read
        as_subject("wide");
        assert!(fails("select authz.allowed_text('org', 'view', 'pg_catalog.pg_authid'::regclass, 'rolname')"));
        reset_role();
    }

    #[pg_test]
    fn a_long_list_is_not_an_index_condition_of_a_nested_loop() {
        fixture();
        Spi::run(
            "create table public.purchases (id int primary key, created int not null);
             insert into public.purchases select i, i from generate_series(1, 20000) i;
             create table public.lines (id int primary key, purchase_id int not null, product int not null);
             insert into public.lines select i, 1 + (i * 7) % 20000, i % 50 from generate_series(1, 60000) i;
             create index on public.lines (product); analyze public.purchases; analyze public.lines;
             select authz.bind('public.purchases', 'id', 'org', 'view');
             grant select on public.purchases, public.lines to authz_test_user",
        )
        .unwrap();
        // 1800 listed ids; a few lines, each finding its purchase by key
        as_subject("most");
        let sql = "select p.id from public.lines l join public.purchases p on p.id = l.purchase_id where l.product = 3 and l.id < 600";
        let plan = explain(sql);
        assert!(!plan.contains("Index Cond: ((id = l.purchase_id) AND (id = ANY"), "the list re-sorted at every loop:\n{plan}");
        reset_role();
        let want = count(&format!("select count(*) from ({sql}) s where id <= 1800"));
        as_subject("most");
        assert_eq!(count(&format!("select count(*) from ({sql}) s")), want);
        reset_role();
    }

    #[pg_test]
    fn an_ordered_read_of_a_broad_listed_subject_keeps_its_paths() {
        fixture();
        Spi::run(
            "create table public.channels (id bigint primary key, org_id int not null, name text not null);
             insert into public.channels select i, 1 + (i % 250) * 8, 'channel ' || i from generate_series(1, 1000) i;
             create index on public.channels (org_id, id); analyze public.channels;
             select authz.bind('public.channels', 'org_id', 'org', 'view');
             grant select on public.channels to authz_test_user",
        )
        .unwrap();
        // 225 of the table's 250 orgs listed, every row in id order with no LIMIT: the adaptive
        // path's fetching child (a bitmap scan) came from the relation's paths, and add_path frees
        // a path an ordered one dominates once this subject's estimate raised its cost
        as_subject("most");
        let sql = "select id, name from public.channels order by id";
        let plan = explain(sql);
        // (16 costs a sequential scan and sort cheaper there: detached copies either)
        assert!((plan.contains("Bitmap Heap Scan") || plan.contains("Seq Scan")) && plan.contains("an ordered walk"), "{plan}");
        assert_eq!(count(&format!("select count(*) from ({sql}) s")), 900);
        let first: i64 = Spi::get_one(&format!("select id from ({sql}) s limit 1")).unwrap().unwrap();
        assert_eq!(first, 1);
        reset_role();
    }

    #[pg_test]
    fn bulk_writes_find_their_rows_through_the_index_set() {
        fixture();
        Spi::run(
            "create table public.stock (id int primary key, item_id int not null, x int not null default 0);
             insert into public.stock select i, i % 200, 0 from generate_series(1, 4000) i;
             create index on public.stock (item_id); analyze public.stock;
             select authz.bind('public.stock', 'item_id', 'item', 'view', 'view');
             grant select, update, delete on public.stock to authz_test_user",
        )
        .unwrap();
        let ids: Vec<String> = (0..200).filter(|i| i % 3 == 0).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        index_set("user:wide", &refs, true);
        as_subject("wide");
        // a write that may reach many rows: through the set, an indexable filter, no row checks
        let bulk = "update public.stock set x = 1 where item_id % 4 = 0";
        let plan = explain(bulk);
        assert!(plan.contains("item_id = ANY ((InitPlan") && !plan.contains("permits"), "{plan}");
        assert_eq!(
            count(&format!("with u as ({bulk} returning 1) select count(*) from u")),
            340,
            "items 0, 12, ..., 192: 17 of them, 20 rows each"
        );
        // one row by its key: the row's check, no list
        assert!(explain("update public.stock set x = 2 where id = 12").contains("permits"));
        assert_eq!(count("with u as (update public.stock set x = 2 where id = 12 returning 1) select count(*) from u"), 1);
        assert_eq!(
            count("with u as (update public.stock set x = 2 where id = 13 returning 1) select count(*) from u"),
            0,
            "item 13 is not the subject's"
        );
        reset_role();
        // nothing outside the set was written
        assert_eq!(count("select count(*) from public.stock where x <> 0 and item_id % 3 <> 0"), 0);
        assert_eq!(count("select count(*) from public.stock where x = 1"), 339);
    }

    #[pg_test]
    fn small_permission_index_sets_are_listed() {
        fixture();
        Spi::run("set pg_auth.index_list_limit = 10000").unwrap();
        items();
        index_set("user:wide", &["5", "007", "50", "150"], true);
        as_subject("wide");
        // the engine could not list them, Postgres can: an indexable filter, no row checks
        let plan = explain("select id from public.items where id < 100");
        assert!(plan.contains("id = ANY ((InitPlan") && !plan.contains("permits"), "{plan}");
        let checked = || Spi::get_one::<i64>("select ids_checked from authz.stats()").unwrap().unwrap();
        let before = checked();
        // written as Postgres writes the column: "007" is not 7
        assert_eq!(
            Spi::get_one::<String>("select string_agg(id::text, ',' order by id) from public.items").unwrap().as_deref(),
            Some("5,50,150")
        );
        assert_eq!(checked() - before, 0, "nothing asked of the engine");
        // above the limit: rows checked against the set, as before
        Spi::run("set local pg_auth.index_list_limit = 3").unwrap();
        crate::sidecar::forget_answers();
        assert!(explain("select id from public.items").contains("AuthzBatch"));
        assert_eq!(count("select count(*) from public.items"), 3);
        reset_role();
    }

    #[pg_test]
    fn broad_subjects_are_answered_from_a_fresh_permission_index() {
        fixture();
        // the row-by-row index path (sets this small are otherwise listed)
        Spi::run("set pg_auth.index_list_limit = 0").unwrap();
        items();
        // the index says 7 too, which the engine would deny: an answer with 7 came from the index
        index_set("user:wide", &["5", "7", "50", "150"], true);
        let stat = |c: &str| Spi::get_one::<i64>(&format!("select {c} from authz.stats()")).unwrap().unwrap();
        let (calls, checked, looked_up) = (stat("sidecar_calls"), stat("ids_checked"), stat("index_lookups"));
        as_subject("wide");
        let ids = Spi::get_one::<String>("select string_agg(id::text, ',' order by id) from public.items").unwrap();
        assert_eq!(ids.as_deref(), Some("5,7,50,150"));
        // per-row checks (writes, row locks) use it too
        assert!(Spi::get_one::<bool>("select authz.can('view', 'item:7')").unwrap().unwrap());
        reset_role();
        assert_eq!(stat("ids_checked") - checked, 0, "no engine checks");
        // one list, which a real sidecar answers from the fresh set it knows, without the engine
        assert_eq!(stat("sidecar_calls") - calls, 1, "one list call, nothing checked");
        assert!(stat("index_lookups") - looked_up >= 200, "every row answered from the index");

        // switched off: the engine answers again, so 7 is gone
        as_subject("wide");
        Spi::run("set local pg_auth.permission_index = off").unwrap();
        let ids = Spi::get_one::<String>("select string_agg(id::text, ',' order by id) from public.items").unwrap();
        assert_eq!(ids.as_deref(), Some("5,50,150"));
        reset_role();
    }

    #[pg_test]
    fn index_sets_match_ids_as_the_policy_renders_them() {
        fixture();
        // the row-by-row index path (sets this small are otherwise listed)
        Spi::run("set pg_auth.index_list_limit = 0").unwrap();
        items();
        Spi::run(
            "create table public.labels (id text primary key); insert into public.labels values ('5'), ('7'), ('007');
             select authz.bind('public.labels', 'id', 'item', 'view');
             grant select on public.labels to authz_test_user;
             create function public.over_ten(i int) returns bool language plpgsql as 'begin return i > 10; end'",
        )
        .unwrap();
        // no integer column renders 7 as "007": the policy compares col::text
        index_set("user:wide", &["5", "007", "50", "150"], true);
        as_subject("wide");
        let items = "select string_agg(id::text, ',' order by id) from public.items";
        // the first read probes, then loads the set mid-scan; the second checks every row against it
        for read in ["first", "loaded"] {
            assert_eq!(Spi::get_one::<String>(items).unwrap().as_deref(), Some("5,50,150"), "{read}");
        }
        // nothing to project: rows come from the child as they are (the direct path), counted
        let looked_up = || Spi::get_one::<i64>("select index_lookups from authz.stats()").unwrap().unwrap();
        let before = looked_up();
        assert_eq!(count("select count(*) from public.items"), 3);
        assert_eq!(looked_up() - before, 200, "every row checked against the set");
        assert_eq!(count("select count(*) from public.items where id > 1 and id < 100"), 2);
        let labels = Spi::get_one::<String>("select string_agg(id, ',' order by id) from public.labels").unwrap();
        assert_eq!(labels.as_deref(), Some("007,5"));
        assert_eq!(count("select count(*) from public.labels"), 2);
        // a condition that may leak runs after the check, on AuthzBatch itself
        let leaky = "select string_agg(id::text, ',' order by id) from public.items where public.over_ten(id)";
        assert_eq!(Spi::get_one::<String>(leaky).unwrap().as_deref(), Some("50,150"));
        let analyzed = Spi::connect(|client| {
            client
                .select(&format!("explain (analyze, costs off, timing off, summary off) {leaky}"), None, &[])
                .unwrap()
                .map(|row| row.get::<String>(1).unwrap().unwrap())
                .collect::<Vec<_>>()
                .join("\n")
        });
        assert!(
            analyzed.contains("Custom Scan (AuthzBatch)")
                && analyzed.contains("Filter: over_ten(id)")
                && analyzed.contains("Rows Removed by Filter: 1"),
            "{analyzed}"
        );
        reset_role();
    }

    #[pg_test]
    fn a_query_pinned_to_one_id_checks_it_once() {
        fixture();
        Spi::run("insert into public.docs select i, 1 + (i % 2), 'more ' || i from generate_series(11, 400) i; analyze public.docs")
            .unwrap();
        let checked = || Spi::get_one::<i64>("select ids_checked from authz.stats()").unwrap().unwrap_or(0);
        let counted = |sql: &str| {
            let before = checked();
            (count(sql), checked() - before)
        };
        // too many orgs to list: the rows of org 1 pass on one check of org 1
        as_subject("wide");
        Spi::run("set local pg_auth.permission_index = off").unwrap();
        assert!(explain("select id from public.docs where org_id = 1").contains("AuthzBatch"));
        assert_eq!(counted("select count(*) from public.docs where org_id = 1"), (200, 1));
        // allowed org 1 only: org 2's rows are not read, one check says no
        as_subject("wide_alice");
        Spi::run("set local pg_auth.permission_index = off").unwrap();
        assert_eq!(counted("select count(*) from public.docs where org_id = 2"), (0, 1));
        assert_eq!(counted("select count(*) from public.docs where org_id = 1 and id > 100"), (150, 1));
        // not pinned: rows are checked (one id per distinct org)
        assert_eq!(count("select count(*) from public.docs where org_id in (1, 2)"), 200);
        reset_role();

        // rows that pass unchecked still get AuthzBatch's projection and its conditions, the
        // leaky ones after authorization: the same rows and values as unprotected
        Spi::run("create function public.long_title(t text) returns bool language plpgsql as 'begin return length(t) > 7; end'").unwrap();
        let queries = [
            "select string_agg(id || ':' || title, ',' order by id) from public.docs where org_id = 1 and id < 30",
            "select string_agg(upper(title), ',' order by id) from public.docs where org_id = 1 and public.long_title(title) and id < 60",
            "select string_agg(d.id::text, ',' order by d.id) from generate_series(1, 400, 7) g(i) join public.docs d on d.id = g.i where d.org_id = 2",
        ];
        let want: Vec<Option<String>> = queries.iter().map(|q| Spi::get_one::<String>(q).unwrap()).collect();
        as_subject("wide");
        Spi::run("set local pg_auth.permission_index = off").unwrap();
        for (q, want) in queries.iter().zip(&want) {
            assert_eq!(&Spi::get_one::<String>(q).unwrap(), want, "{q}");
        }
        reset_role();
    }

    #[pg_test]
    fn adaptive_pages_weigh_skewed_ids_by_their_share() {
        fixture();
        Spi::run(
            "create table public.skewed (id int primary key, org_id int not null, kind text not null, created int not null);
             insert into public.skewed select i, case when i % 5 < 2 then 1 else 2 + i % 1000 end,
                 case when i % 7 = 0 then 'error' else 'info' end, i from generate_series(1, 20000) i;
             create index on public.skewed (org_id, created);
             create index on public.skewed (created) where kind = 'error';
             analyze public.skewed;
             select authz.bind('public.skewed', 'org_id', 'org', 'view');
             grant select on public.skewed to authz_test_user",
        )
        .unwrap();
        let page = "select id from public.skewed where kind = 'error' order by created desc limit 5";
        // which child ran: the listed fetch (first) or the ordered walk on the partial index
        let walked = || {
            let plan = Spi::connect(|client| {
                client
                    .select(&format!("explain (analyze, costs off, timing off, summary off) {page}"), None, &[])
                    .unwrap()
                    .map(|r| r.get::<String>(1).unwrap().unwrap())
                    .collect::<Vec<_>>()
            });
            let walk = plan.iter().find(|l| l.contains("Index Scan Backward using skewed_created_idx")).cloned().unwrap_or_default();
            assert!(plan.iter().any(|l| l.contains("AuthzBatch")), "{plan:#?}");
            (!walk.contains("never executed"), plan.join("\n"))
        };
        // alice's one org holds 40% of the rows: walking the recent errors finds 5 at once
        as_subject("alice");
        let estimate =
            Spi::get_one::<pgrx::Json>("explain (format json) select id from public.skewed").unwrap().unwrap().0[0]["Plan"]["Plan Rows"]
                .as_f64()
                .unwrap_or(0.0);
        assert!(estimate > 6000.0, "her org's share (40% of 20000), not an average org's: {estimate}");
        // and costed by it: a page of one kind walks her org's rows in order, not a bitmap of her
        // org and the kind sorted (which the average org's share would make look like 4 rows)
        reset_role();
        Spi::run("create table public.kinds (id int primary key, org_id int not null, kind int not null, created int not null);
                  insert into public.kinds select i, case when i % 5 < 2 then 1 else 2 + i % 1000 end, i % 50, i from generate_series(1, 50000) i;
                  create index on public.kinds (org_id, created); create index on public.kinds (kind); analyze public.kinds;
                  select authz.bind('public.kinds', 'org_id', 'org', 'view'); grant select on public.kinds to authz_test_user").unwrap();
        as_subject("alice");
        let plan = explain("select id from public.kinds where kind = 7 order by created desc limit 5");
        assert!(!plan.contains("Bitmap") && !plan.contains("\n  ->  Sort") && plan.contains("Index Cond: (org_id = (InitPlan"), "{plan}");
        let (walk, plan) = walked();
        assert!(walk, "a common id walks:\n{plan}");
        let mine = Spi::get_one::<String>(&format!("select string_agg(id::text, ',') from ({page}) p")).unwrap();
        reset_role();
        let want = Spi::get_one::<String>("select string_agg(id::text, ',') from (select id from public.skewed where kind = 'error' and org_id = 1 order by created desc limit 5) p").unwrap();
        assert_eq!(mine, want);
        // bob's one org is rare: its rows fetched and sorted
        as_subject("bob");
        let (walk, plan) = walked();
        assert!(!walk, "a rare id fetches:\n{plan}");
        reset_role();
    }

    #[pg_test]
    fn ordered_walks_keep_the_query_range() {
        fixture();
        Spi::run(
            "create table public.stored (id int primary key, org_id int not null, bucket text not null, name text not null);
             insert into public.stored select i, 1 + (i % 2000), 'b' || (i % 2), lpad(i::text, 6, '0') from generate_series(1, 20000) i;
             create index on public.stored (bucket, name);
             create index on public.stored (org_id);
             analyze public.stored;
             select authz.bind('public.stored', 'org_id', 'org', 'view');
             grant select on public.stored to authz_test_user",
        )
        .unwrap();
        let page = "select id from public.stored where bucket = 'b0' and name >= '010000' and name < '010100' order by name limit 20";
        let ids =
            |sql: &str| Spi::get_one::<String>(&format!("select string_agg(id::text, ',') from ({sql}) p")).unwrap().unwrap_or_default();
        // both children read the range: the listed one and the ordered walk for broad subjects
        as_subject("alice");
        let plan = explain(page);
        assert!(
            plan.contains("AuthzBatch")
                && plan.matches("Index Cond: ((bucket = 'b0'::text) AND (name >= '010000'::text) AND (name < '010100'::text))").count()
                    == 2,
            "{plan}"
        );
        assert_eq!(ids(page), "10000", "org 1: ids 2000 apart");
        as_subject("most");
        assert_eq!(ids(page), (10000..10040).step_by(2).map(|i| i.to_string()).collect::<Vec<_>>().join(","));
        reset_role();
    }

    #[pg_test]
    fn cached_plans_are_rebuilt_for_a_subject_of_another_breadth() {
        fixture();
        Spi::run(
            "create table public.breadth_pages (id int primary key, org_id int not null, created int not null);
             insert into public.breadth_pages select i, 1 + (i % 2), 1000 - i from generate_series(1, 400) i;
             create index on public.breadth_pages (org_id, created desc);
             analyze public.breadth_pages;
             select authz.bind('public.breadth_pages', 'org_id', 'org', 'view');
             grant select on public.breadth_pages to authz_test_user",
        )
        .unwrap();
        let page = "select string_agg(id::text, ',' order by created desc) from (select id, created from public.breadth_pages order by created desc limit 4) e";
        let generic = || Spi::run("set local plan_cache_mode = force_generic_plan").unwrap();
        // cached for a one-org subject: its org's ordered index range
        as_subject("alice");
        Spi::run(&format!("prepare q as {page}")).unwrap();
        generic();
        let narrow = explain("execute q");
        assert!(narrow.contains("Index Cond: (org_id = (InitPlan"), "{narrow}");
        // a subject too broad to list: rebuilt for it
        as_subject("wide");
        generic();
        let broad = explain("execute q");
        assert!(!broad.contains("Index Cond: (org_id = (InitPlan"), "{broad}");
        assert_eq!(Spi::get_one::<String>("execute q").unwrap().as_deref(), Some("1,2,3,4"));
        // as broad: kept
        as_subject("wide_alice");
        generic();
        assert_eq!(explain("execute q"), broad);
        // off: reused as built, whoever runs it
        Spi::run("set local pg_auth.replan_on_breadth = off").unwrap();
        as_subject("alice");
        generic();
        assert_eq!(explain("execute q"), broad);
        assert_eq!(Spi::get_one::<String>("execute q").unwrap().as_deref(), Some("2,4,6,8"));
        reset_role();
        Spi::run("deallocate q").unwrap();
    }

    #[pg_test]
    fn one_org_subjects_keep_the_index_order() {
        fixture();
        // reuse as built, whoever runs it (not rebuilt for another breadth)
        Spi::run("set pg_auth.replan_on_breadth = off").unwrap();
        Spi::run(
            "create table public.org_pages (id int primary key, org_id int not null, created int not null);
             insert into public.org_pages select i, 1 + (i % 2), 1000 - i from generate_series(1, 400) i;
             create index on public.org_pages (org_id, created desc);
             analyze public.org_pages;
             select authz.bind('public.org_pages', 'org_id', 'org', 'view');
             grant select on public.org_pages to authz_test_user",
        )
        .unwrap();
        let page = "select string_agg(id::text, ',' order by created desc) from (select id, created from public.org_pages order by created desc limit 4) e";
        // one org: an ordered scan of (org_id, created) that stops after the page, no sort
        as_subject("alice");
        let plan = explain("select id from public.org_pages order by created desc limit 4");
        assert!(
            plan.contains("Index Cond: (org_id = (InitPlan") && plan.contains("\n  ->  Custom Scan (AuthzBatch) on org_pages"),
            "no sort above the scan (the one under it is the fallback):\n{plan}"
        );
        assert_eq!(Spi::get_one::<String>(page).unwrap().as_deref(), Some("2,4,6,8"));

        // that plan, cached and reused by subjects with more orgs: the sorted fallback
        Spi::run(&format!("prepare q as {page}; set local plan_cache_mode = force_generic_plan")).unwrap();
        assert_eq!(Spi::get_one::<String>("execute q").unwrap().as_deref(), Some("2,4,6,8"));
        for (subject, want) in [("both", "1,2,3,4"), ("wide", "1,2,3,4"), ("bob", "1,3,5,7"), ("nobody", "")] {
            as_subject(subject);
            Spi::run("set local plan_cache_mode = force_generic_plan").unwrap();
            assert_eq!(Spi::get_one::<String>("execute q").unwrap().unwrap_or_default(), want, "{subject}");
        }
        reset_role();
        Spi::run("deallocate q").unwrap();

        // with an index on created alone, subjects with more orgs reuse it: an ordered walk
        // checked in batches, instead of sorting the table
        Spi::run("create index on public.org_pages (created desc); analyze public.org_pages").unwrap();
        as_subject("alice");
        Spi::run(&format!("prepare q as {page}; set local plan_cache_mode = force_generic_plan")).unwrap();
        let plan = explain("execute q");
        assert!(plan.contains("org_pages_created_idx") && !plan.contains("Sort Key"), "{plan}");
        assert_eq!(Spi::get_one::<String>("execute q").unwrap().as_deref(), Some("2,4,6,8"));
        for (subject, want) in [("both", "1,2,3,4"), ("wide", "1,2,3,4"), ("bob", "1,3,5,7")] {
            as_subject(subject);
            Spi::run("set local plan_cache_mode = force_generic_plan").unwrap();
            assert_eq!(Spi::get_one::<String>("execute q").unwrap().unwrap_or_default(), want, "{subject}");
        }
        reset_role();
        Spi::run("deallocate q").unwrap();
        Spi::run("drop index public.org_pages_created_idx").unwrap();

        // a join on the protected column: `org_id = $one` would spread to the other side (an
        // equivalence class), outside AuthzBatch, so such queries keep the listed-ids form
        Spi::run("create table public.page_orgs (id int primary key); insert into public.page_orgs values (1), (2); grant select on public.page_orgs to authz_test_user").unwrap();
        let joined = "select string_agg(e.id::text, ',' order by e.created desc) from (select e.id, e.created from public.org_pages e join public.page_orgs o on o.id = e.org_id order by e.created desc limit 4) e";
        as_subject("alice");
        assert!(
            explain("select e.id from public.org_pages e join public.page_orgs o on o.id = e.org_id").contains("org_id = ANY ((InitPlan")
        );
        Spi::run(&format!("prepare j as {joined}; set local plan_cache_mode = force_generic_plan")).unwrap();
        assert_eq!(Spi::get_one::<String>("execute j").unwrap().as_deref(), Some("2,4,6,8"));
        as_subject("both");
        Spi::run("set local plan_cache_mode = force_generic_plan").unwrap();
        assert_eq!(Spi::get_one::<String>("execute j").unwrap().as_deref(), Some("1,2,3,4"));
        reset_role();
        Spi::run("deallocate j").unwrap();
    }

    #[pg_test]
    fn plans_follow_the_subjects_object_count() {
        fixture();
        Spi::run(
            "create table public.tasks (id int primary key, org_id int not null, updated int not null);
             insert into public.tasks select i, 1 + (i % 2000), i from generate_series(1, 40000) i;
             create index on public.tasks (updated desc);
             create index on public.tasks (org_id);
             analyze public.tasks;
             select authz.bind('public.tasks', 'org_id', 'org', 'view');
             grant select on public.tasks to authz_test_user",
        )
        .unwrap();
        let rows = |sql: &str| {
            let plan = Spi::get_one::<pgrx::Json>(&format!("explain (format json) {sql}")).unwrap().unwrap().0;
            plan[0]["Plan"]["Plan Rows"].as_f64().unwrap()
        };
        // 1800 of 2000 orgs: the planner expects most rows, and walks the updated index, stopping
        // after the page, instead of fetching them by org and sorting
        as_subject("most");
        assert!((rows("select id from public.tasks") - 36_000.0).abs() < 2_000.0, "{}", rows("select id from public.tasks"));
        let plan = explain("select id from public.tasks order by updated desc limit 10");
        assert!(plan.contains("tasks_updated_idx") && !plan.contains("Sort Key"), "{plan}");
        let page = "select string_agg(id::text, ',' order by updated desc) from (select id, updated from public.tasks order by updated desc limit 3) t";
        assert_eq!(Spi::get_one::<String>(page).unwrap().as_deref(), Some("40000,39799,39798"), "rows of orgs 1801-2000 are not theirs");
        // 2 orgs: Postgres's own guess (about ten ids) is kept, never lowered, so a plan cached
        // for a narrow subject still serves broader ones
        as_subject("both");
        assert!((rows("select id from public.tasks") - 200.0).abs() < 50.0, "{}", rows("select id from public.tasks"));
        reset_role();
    }

    #[pg_test]
    fn one_cached_page_plan_serves_narrow_and_broad_subjects() {
        fixture();
        // reuse as built, whoever runs it (not rebuilt for another breadth)
        Spi::run("set pg_auth.replan_on_breadth = off").unwrap();
        Spi::run(
            "create table public.cached_tasks (id int primary key, org_id int not null, updated int not null);
             insert into public.cached_tasks select i, 1 + (i % 2000), i from generate_series(1, 40000) i;
             create index on public.cached_tasks (updated desc);
             create index on public.cached_tasks (org_id);
             analyze public.cached_tasks;
             select authz.bind('public.cached_tasks', 'org_id', 'org', 'view');
             grant select on public.cached_tasks to authz_test_user",
        )
        .unwrap();
        let page = "select string_agg(id::text, ',' order by updated desc) from (select id, updated from public.cached_tasks order by updated desc limit 3) t";
        // planned for a subject of one org: its rows, sorted; no sort above AuthzBatch
        as_subject("alice");
        let plan = explain("select id from public.cached_tasks order by updated desc limit 3");
        assert!(plan.contains("sorted for narrow subjects") && plan.contains("\n  ->  Custom Scan (AuthzBatch)"), "{plan}");
        Spi::run(&format!("prepare q as {page}; set local plan_cache_mode = force_generic_plan")).unwrap();
        assert_eq!(Spi::get_one::<String>("execute q").unwrap().as_deref(), Some("40000,38000,36000"));
        // the same cached plan for 1800 of 2000 orgs walks the updated index and stops early
        for (subject, want) in
            [("most", "40000,39799,39798"), ("both", "40000,38001,38000"), ("wide", "40000,38001,38000"), ("alice", "40000,38000,36000")]
        {
            as_subject(subject);
            Spi::run("set local plan_cache_mode = force_generic_plan").unwrap();
            assert_eq!(Spi::get_one::<String>("execute q").unwrap().unwrap_or_default(), want, "{subject}");
        }
        as_subject("most");
        Spi::run("set local plan_cache_mode = force_generic_plan").unwrap();
        let analyzed = Spi::connect(|client| {
            client
                .select("explain (analyze, costs off, timing off, summary off) execute q", None, &[])
                .unwrap()
                .map(|row| row.get::<String>(1).unwrap().unwrap())
                .collect::<Vec<_>>()
                .join("\n")
        });
        // the 200 newest rows are of orgs 1801-2000: batches of 16, 32, 64 and 128 reach the page
        assert!(
            analyzed.contains("Index Scan using cached_tasks_updated_idx on cached_tasks (actual rows=240")
                && analyzed.contains("Sort (never executed)"),
            "an early-stopping walk:\n{analyzed}"
        );
        reset_role();
        Spi::run("deallocate q").unwrap();
    }

    #[pg_test]
    fn writes_and_sub_selects_use_the_listed_ids() {
        fixture();
        // dana may view and edit org 1: the UPDATE's policy, the SELECT policy on its target, the
        // sub-select's and the check on the new row all use org 1's listed id, none checks per row
        as_subject("dana");
        let update = "update public.docs set title = 'x' where id in (select id from public.docs where id > 1)";
        let plan = explain(update);
        assert!(!plan.contains("permits") && plan.contains("org_id = ANY ((InitPlan"), "{plan}");
        assert_eq!(count(&format!("with u as ({update} returning 1) select count(*) from u")), 5);
        // the new row is still checked: moving it out of org 1 fails
        assert!(fails("update public.docs set org_id = 2 where id = 2"));
        assert!(fails("insert into public.docs values (100, 2, 'elsewhere')"));
        assert_eq!(count("with i as (insert into public.docs values (101, 1, 'here') returning 1) select count(*) from i"), 1);
        // alice may view org 1 but not edit it: nothing to update
        as_subject("alice");
        assert_eq!(count(&format!("with u as ({update} returning 1) select count(*) from u")), 0);
        assert!(fails("insert into public.docs values (102, 1, 'here')"));
        reset_role();
    }

    #[pg_test]
    fn tables_protected_by_a_key_expression() {
        fixture();
        Spi::run(
            "create table public.objects (id int primary key, name text not null);
             insert into public.objects select i, (array['alice', 'bob', 'carol'])[1 + i % 3] || '/file' || i from generate_series(1, 300) i;
             create index on public.objects (split_part(name, '/', 1));
             analyze public.objects;
             select authz.protect('public.objects', 'folder', null, 'view', 'edit', key => $$split_part(name, '/', 1)$$);
             grant select, insert on public.objects to authz_test_user",
        )
        .unwrap();
        // a listed subject: the key expression against its ids, through the index on it
        as_subject("alice");
        let plan = explain("select id from public.objects");
        assert!(
            plan.contains("split_part(name, '/'::text, 1) = ANY ((InitPlan") && !plan.contains("permits"),
            "the listed filter, no row checks:\n{plan}"
        );
        Spi::run("set local enable_seqscan = off").unwrap();
        assert!(explain("select id from public.objects").contains("objects_split_part_idx"), "an index on the expression serves it");
        Spi::run("set local enable_seqscan = on").unwrap();
        assert_eq!(count("select count(*) from public.objects"), 100);
        assert_eq!(
            Spi::get_one::<String>(
                "select string_agg(id::text, ',' order by id) from (select id from public.objects order by id limit 3) o"
            )
            .unwrap()
            .as_deref(),
            Some("3,6,9")
        );
        // writes: the new row's key is checked
        assert_eq!(count("with i as (insert into public.objects values (1000, 'alice/new') returning 1) select count(*) from i"), 1);
        assert!(fails("insert into public.objects values (1001, 'bob/new')"));
        // too broad to list: AuthzBatch evaluates the key on each row
        as_subject("wide");
        let plan = explain("select id from public.objects");
        assert!(plan.contains("AuthzBatch") && plan.contains("its key expression"), "{plan}");
        assert_eq!(count("select count(*) from public.objects"), 201, "alice and bob, with the row alice added");
        assert_eq!(count("select count(*) from (select id, length(name) from public.objects where id < 30) o"), 19);
        as_subject("nobody");
        assert_eq!(count("select count(*) from public.objects"), 0);
        reset_role();
        // a key whose value for a row could change is refused
        assert!(fails("select authz.protect('public.objects', 'folder', null, 'view', key => 'name || random()::text')"));
    }

    #[pg_test]
    fn several_protections_and_inserts_checked_on_the_parent() {
        fixture();
        Spi::run(
            "create table public.notes (id int primary key, org_id int not null, doc_id int not null);
             insert into public.notes values (1, 1, 5), (2, 2, 100), (3, 2, 7), (4, 1, 100);
             select authz.protect('public.notes', 'org', 'org_id', 'view');
             select authz.protect('public.notes', 'document', 'doc_id', 'view', name => 'shared');
             create table public.tickets (id int primary key, org_id int not null);
             select authz.protect('public.tickets', 'document', 'id', 'view', 'edit',
                                  insert_type => 'org', insert_id => 'org_id', insert_permission => 'edit');
             grant select, insert on public.notes, public.tickets to authz_test_user",
        )
        .unwrap();
        let notes =
            || Spi::get_one::<String>("select string_agg(id::text, ',' order by id) from public.notes").unwrap().unwrap_or_default();
        // visible through either protection: alice's org 1, and document 100 shared with her
        as_subject("alice");
        let plan = explain("select id from public.notes");
        assert!(
            plan.contains("org_id = ANY ((InitPlan") && plan.contains("doc_id = ANY ((InitPlan") && !plan.contains("permits"),
            "{plan}"
        );
        assert_eq!(notes(), "1,2,4");
        as_subject("bob");
        assert_eq!(notes(), "2,3");
        // too many orgs to list: that arm is checked per row, the other still listed
        as_subject("wide");
        assert_eq!(notes(), "1,2,3,4");
        // a ticket is its own object, which the engine cannot know before it exists: inserts are
        // checked on its org
        as_subject("dana");
        assert_eq!(count("with i as (insert into public.tickets values (5, 1) returning 1) select count(*) from i"), 1);
        assert!(fails("insert into public.tickets values (6, 2)"));
        as_subject("alice");
        assert!(fails("insert into public.tickets values (7, 1)"));
        reset_role();
        let status =
            Spi::get_one::<String>("select string_agg(value, ' | ' order by item) from authz.status() where item like 'protected %'")
                .unwrap()
                .unwrap_or_default();
        assert!(status.contains("document#view via doc_id") && status.contains("insert org#edit via org_id"), "{status}");
    }

    #[pg_test]
    fn declared_hierarchies_expand_direct_grants_in_postgres() {
        fixture();
        Spi::run(
            "create table public.spaces (id int primary key, parent_id int references public.spaces);
             insert into public.spaces values (1, null), (2, 1), (3, 1), (4, 2), (5, 4), (6, 3);
             create table public.files (id int primary key, space_id int not null);
             insert into public.files select i * 10, i from generate_series(1, 6) i;
             select authz.protect('public.files', 'space', 'space_id', 'viewer', 'viewer',
                                  hierarchy => 'public.spaces(id, parent_id)', direct => 'viewer_direct');
             grant select, insert on public.files to authz_test_user;
             grant select, insert, update, delete, truncate on public.spaces to authz_test_user",
        )
        .unwrap();
        // one level: a hierarchy expanding itself, or another declared one, would never end
        assert!(fails("select authz.declare_hierarchy('space', 'viewer_direct', 'viewer', 'public.spaces', 'id', 'parent_id')"));
        assert!(fails("select authz.declare_hierarchy('space', 'editor', 'editor', 'public.spaces', 'id', 'parent_id')"));
        let files =
            || Spi::get_one::<String>("select string_agg(id::text, ',' order by id) from public.files").unwrap().unwrap_or_default();
        let fresh = || {
            reset_role();
            crate::sidecar::forget_answers();
        };
        // alice was granted space 2: it and everything below it, though the engine never answers
        // space#viewer
        as_subject("alice");
        assert_eq!(files(), "20,40,50");
        assert!(explain("select id from public.files").contains("space_id = ANY ((InitPlan"));
        reset_role();
        let status = Spi::get_one::<String>("select value from authz.status() where item = 'hierarchy space#viewer'").unwrap();
        assert_eq!(status.as_deref(), Some("space#viewer_direct down spaces(id, parent_id), version 0"));
        as_subject("alice");
        assert_eq!(count("with i as (insert into public.files values (51, 5) returning 1) select count(*) from i"), 1);
        assert!(fails("insert into public.files values (61, 6)"));
        as_subject("bob");
        assert_eq!(files(), "30,60", "a grant below another grant is not counted twice");
        // the tree changes: a later transaction sees the move, the new node and the removal
        fresh();
        Spi::run("update public.spaces set parent_id = 4 where id = 3; insert into public.spaces values (7, 6); insert into public.files values (70, 7)").unwrap();
        as_subject("alice");
        assert_eq!(files(), "20,30,40,50,51,60,70");
        fresh();
        Spi::run("delete from public.files where space_id = 6").unwrap();
        // the tree's own writers need no rights on pg_auth's closure
        as_subject("alice");
        Spi::run("delete from public.spaces where id in (7, 6)").unwrap();
        assert!(fails("update public.spaces set parent_id = 5 where id = 2"), "a cycle is refused");
        fresh();
        as_subject("alice");
        assert_eq!(files(), "20,30,40,50,51");
        as_subject("bob");
        assert_eq!(files(), "30", "bob's grant on 6 reaches nothing once it is gone");
        // too many to list: the expansion answers AuthzBatch as the subject's index set, with the
        // permission index off
        fresh();
        Spi::run(
            "insert into public.spaces select i, 5 from generate_series(100, 1600) i;
             insert into public.files select i * 10, i from generate_series(100, 1600) i;
             analyze public.files",
        )
        .unwrap();
        as_subject("alice");
        Spi::run("set local pg_auth.permission_index = off").unwrap();
        assert!(explain("select id from public.files").contains("AuthzBatch"));
        assert_eq!(count("select count(*) from public.files"), 1506);
        assert_eq!(count("select count(*) from public.files where id < 100"), 5);
        assert!(fails("insert into public.files values (11, 1)"));
        // emptied, then rebuilt from the top
        fresh();
        Spi::run("delete from public.files; truncate public.spaces; insert into public.spaces values (2, null), (1, 2); insert into public.files values (10, 1)").unwrap();
        as_subject("alice");
        assert_eq!(files(), "10");
        // dropped: the engine answers space#viewer again (here: nothing)
        fresh();
        Spi::run("select authz.drop_hierarchy('space', 'viewer')").unwrap();
        as_subject("alice");
        assert_eq!(files(), "");
        reset_role();
        assert_eq!(count("select count(*) from pg_trigger where tgrelid = 'public.spaces'::regclass and tgname like 'authz%'"), 0);
        // the declaration is checked
        assert!(fails(
            "select authz.protect('public.files', 'space', 'space_id', 'viewer', hierarchy => 'public.spaces(id, nope)', direct => 'viewer_direct')"
        ));
        assert!(fails("select authz.protect('public.files', 'space', 'space_id', 'viewer', hierarchy => 'public.spaces(id, parent_id)')"));
    }

    #[pg_test]
    fn row_checks_follow_the_subject_permission_and_index() {
        fixture();
        // the row-by-row index path (sets this small are otherwise listed)
        Spi::run("set pg_auth.index_list_limit = 0").unwrap();
        items();
        index_set("user:wide", &["5", "7", "50", "150"], true);
        // authz.permits once per row (as writes and locking reads use it): its last answer is
        // reused for the next row only while subject, type and permission stay the same
        let permitted = |object_type: &str, permission: &str, ids: &str| {
            Spi::get_one::<String>(&format!(
                "select string_agg(i::text, ',' order by i) from unnest('{{{ids}}}'::int[]) i where authz.permits('{object_type}', '{permission}', i::text)"
            ))
            .unwrap()
        };
        for _ in 0..2 {
            as_subject("alice");
            assert_eq!(permitted("org", "view", "1,2").as_deref(), Some("1"));
            assert_eq!(permitted("org", "edit", "1,2"), None, "another permission");
            assert_eq!(permitted("document", "view", "100,200").as_deref(), Some("100"), "another type");
            as_subject("bob");
            assert_eq!(permitted("org", "view", "1,2").as_deref(), Some("2"), "another subject");
            // a subject answered from its loaded index set (7 is in the index, not the engine)
            as_subject("wide");
            assert_eq!(permitted("item", "view", "5,7,8,50,150,151").as_deref(), Some("5,7,50,150"));
            Spi::run("set local pg_auth.permission_index = off").unwrap();
            assert_eq!(permitted("item", "view", "5,7,8,50,150,151").as_deref(), Some("5,50,150"), "index off: the engine");
            Spi::run("set local pg_auth.permission_index = on").unwrap();
            reset_role();
        }
    }

    #[pg_test]
    fn heavy_statements_wait_for_the_fill_once() {
        fixture();
        items();
        FILLS.lock().unwrap().clear();
        as_subject("wide");
        // batches of 16, 32, 64: the fourth batch has passed 100 ids sent to the engine
        Spi::run("set local pg_auth.fill_wait_after = 100; set local pg_auth.fill_wait_ms = 50").unwrap();
        let rows = advice_for("select count(*) from public.items");
        let advice = rows[0].3.as_deref().unwrap_or_default();
        assert!(advice.contains("waited") && advice.contains("did not become fresh"), "{advice}");
        assert_eq!(*FILLS.lock().unwrap(), ["user:wide item#view", "user:wide item#view waited"]);
        // the engine answered the rest; a later heavy statement in the transaction does not wait again
        assert_eq!(count("select count(*) from public.items where id > 0"), 3);
        assert_eq!(FILLS.lock().unwrap().len(), 2, "one wait per transaction");
        reset_role();
    }

    #[pg_test]
    fn row_by_row_checks_wait_for_the_fill_too() {
        fixture();
        items();
        FILLS.lock().unwrap().clear();
        as_subject("wide");
        // per-row checks, as a large write makes: past 100 ids sent to the engine, it waits once
        Spi::run("set local pg_auth.fill_wait_after = 100; set local pg_auth.fill_wait_ms = 50").unwrap();
        assert_eq!(count("select count(*) from generate_series(1, 200) g where authz.permits('item', 'view', g::text)"), 3);
        assert_eq!(*FILLS.lock().unwrap(), ["user:wide item#view", "user:wide item#view waited"]);
        reset_role();
    }

    #[pg_test]
    fn sets_not_fresh_are_not_used_and_get_filled() {
        fixture();
        items();
        // still filling, then invalidated while filling: finishing must not make it usable
        index_set("user:wide", &["5", "7", "50", "150"], false);
        assert_eq!(Spi::get_one::<i64>("select authz.index_invalidate()").unwrap(), Some(1));
        let finished =
            Spi::get_one::<bool>("select authz.index_finish(set_id) from authz.permission_sets where subject = 'user:wide'").unwrap();
        assert_eq!(finished, None, "a set invalidated while filling stays unused");
        FILLS.lock().unwrap().clear();
        as_subject("wide");
        let ids = Spi::get_one::<String>("select string_agg(id::text, ',' order by id) from public.items").unwrap();
        assert_eq!(ids.as_deref(), Some("5,50,150"), "answered by the engine");
        reset_role();
        assert_eq!(*FILLS.lock().unwrap(), ["user:wide item#view"], "a fill was requested once");
    }

    /// Run `sql` as its own top-level statement would (the executor hooks frame real statements;
    /// inside a test everything is nested in the test's own call), then return its advice rows.
    fn advice_for(sql: &str) -> Vec<(String, i64, i64, Option<String>)> {
        crate::sidecar::statement_reset();
        Spi::run(sql).unwrap();
        crate::advice::statement_end();
        Spi::connect(|c| {
            c.select("select subject, ids_checked, index_lookups, advice from authz.last_statement_advice() order by subject", None, &[])
                .unwrap()
                .map(|r| {
                    (
                        r.get::<String>(1).unwrap().unwrap(),
                        r.get::<i64>(2).unwrap().unwrap(),
                        r.get::<i64>(3).unwrap().unwrap(),
                        r.get::<String>(4).unwrap(),
                    )
                })
                .collect()
        })
    }

    #[pg_test]
    fn heavy_statements_say_why_and_what_to_change() {
        fixture();
        // the row-by-row index path (sets this small are otherwise listed)
        Spi::run("set pg_auth.index_list_limit = 0").unwrap();
        items();
        as_subject("wide");
        // no index set yet: checked with the engine, and the set is being filled
        let rows = advice_for("select count(*) from public.items");
        let (subject, checked, _, advice) = &rows[0];
        assert_eq!((subject.as_str(), *checked), ("user:wide", 200));
        let advice = advice.as_deref().unwrap_or_default();
        assert!(advice.contains("200 ids were checked with the engine") && advice.contains("being filled"), "{advice}");
        let plan_advice =
            Spi::get_one_with_args::<String>("select advice from authz.explain($1)", &["select * from public.items".into()]).unwrap();
        assert!(plan_advice.unwrap_or_default().contains("until the sidecar fills it"));

        // asked again in the same transaction: cached, so nothing reached the engine
        assert_eq!(advice_for("select count(*) from public.items")[0].1, 0);
        reset_role();

        // index off: say so (another subject, whose rows are not cached yet)
        as_subject("wide2");
        Spi::run("set local pg_auth.permission_index = off").unwrap();
        let rows = advice_for("select count(*) from public.items");
        assert!(rows[0].3.as_deref().unwrap_or_default().contains("pg_auth.permission_index is off"), "{:?}", rows[0].3);
        reset_role();

        // a fresh set answers: nothing to advise (a subject whose set was not looked up yet this
        // transaction, since pg_auth reads the index once per transaction)
        index_set("user:wide2", &["5", "50", "150"], true);
        crate::sidecar::forget_answers();
        as_subject("wide2");
        Spi::run("set local pg_auth.permission_index = on").unwrap();
        let plan_advice =
            Spi::get_one_with_args::<String>("select advice from authz.explain($1)", &["select * from public.items".into()]).unwrap();
        let plan_advice = plan_advice.unwrap_or_default();
        assert!(plan_advice.contains("answered from the permission index (3 objects)"), "{plan_advice}");
        reset_role();
    }

    #[pg_test]
    fn the_permission_index_is_private() {
        fixture();
        items();
        index_set("user:wide", &["5", "7"], true);
        as_subject("alice");
        assert_eq!(
            Spi::get_one::<Vec<String>>("select authz.indexed_ids('item', 'view')").unwrap(),
            None,
            "only the current subject's set"
        );
        assert!(fails("select count(*) from authz.permitted"), "the index tables are not readable");
        assert!(fails("select authz.index_begin('user:alice', 'item', 'view', 'x')"), "nor writable");
        assert!(fails("select authz.index_invalidate()"));
        reset_role();
    }

    fn items() {
        Spi::run(
            "create table public.items (id int primary key);
             insert into public.items select generate_series(1, 200);
             select authz.bind('public.items', 'id', 'item', 'view');
             grant select on public.items to authz_test_user",
        )
        .unwrap();
    }

    #[pg_test]
    fn reused_listed_plans_check_only_the_rows_they_reach() {
        fixture();
        items();
        let ids_checked = || Spi::get_one::<i64>("select ids_checked from authz.stats()").unwrap().unwrap();
        as_subject("alice");
        Spi::run("prepare q as select id from public.items order by id limit 1; set local plan_cache_mode = force_generic_plan").unwrap();
        assert_eq!(Spi::get_one::<i32>("execute q").unwrap(), Some(50));
        let plan = explain("execute q");
        assert!(plan.contains("AuthzBatch") && plan.contains("InitPlan"), "a listed plan, with the unlisted scan beside it:\n{plan}");

        // a broad subject reuses it: the scan without the listed filter, checked in batches, so
        // LIMIT 1 checks the first 16 rows instead of every value of the column
        as_subject("wide");
        Spi::run("set local plan_cache_mode = force_generic_plan").unwrap();
        let before = ids_checked();
        assert_eq!(Spi::get_one::<i32>("execute q").unwrap(), Some(5));
        assert_eq!(ids_checked() - before, 16);

        Spi::run("reset role; set local pg_auth.subject_id to default; set local role authz_test_user; set local plan_cache_mode = force_generic_plan").unwrap();
        let rows = Spi::connect(|c| c.select("execute q", None, &[]).unwrap().len());
        assert_eq!(rows, 0, "no subject: nothing");
        reset_role();
        Spi::run("deallocate q").unwrap();
    }

    #[pg_test]
    fn batches_keep_dropped_and_added_columns_in_place() {
        fixture();
        // AuthzBatch for both subjects: listed ones get it when plans are reused across breadths
        Spi::run("set pg_auth.replan_on_breadth = off").unwrap();
        Spi::run(
            "create table public.reshaped (gone int, id int primary key, label text);
             insert into public.reshaped select i, i, 'item ' || i from generate_series(1, 60) i;
             alter table public.reshaped drop column gone;
             alter table public.reshaped add column added int default 7;
             select authz.bind('public.reshaped', 'id', 'item', 'view');
             grant select on public.reshaped to authz_test_user",
        )
        .unwrap();
        let rows = |sql: &str| Spi::get_one::<String>(&format!("select string_agg(r::text, ',' order by r::text) from ({sql}) r")).unwrap();
        for subject in ["wide", "alice"] {
            as_subject(subject);
            assert!(explain("select * from public.reshaped").contains("AuthzBatch"), "{subject}");
            let want = if subject == "wide" { "(5,\"item 5\",7),(50,\"item 50\",7)" } else { "(50,\"item 50\",7)" };
            assert_eq!(rows("select * from public.reshaped").as_deref(), Some(want), "{subject}");
            // a whole-row reference reads every column; a count none of them
            let json = if subject == "wide" {
                r#"{"id":5,"label":"item 5","added":7},{"id":50,"label":"item 50","added":7}"#
            } else {
                r#"{"id":50,"label":"item 50","added":7}"#
            };
            let got = Spi::get_one::<String>("select string_agg(row_to_json(t)::text, ',' order by t.id) from public.reshaped t").unwrap();
            assert_eq!(got.as_deref(), Some(json), "{subject}");
            assert_eq!(count("select count(*) from public.reshaped"), if subject == "wide" { 2 } else { 1 }, "{subject}");
            // output computed from columns (a projection planned with an empty target list):
            // every column it reads must reach it
            let projected = Spi::get_one::<String>("select string_agg(id || ':' || n, ',' order by id) from (select id, length(label) as n from public.reshaped order by id limit 2) r").unwrap();
            let want = if subject == "wide" { "5:6,50:7" } else { "50:7" };
            assert_eq!(projected.as_deref(), Some(want), "{subject}");
            let projected =
                Spi::get_one::<String>("select string_agg(id::text || upper(label), ',' order by id) from public.reshaped").unwrap();
            assert_eq!(projected.as_deref(), Some(if subject == "wide" { "5ITEM 5,50ITEM 50" } else { "50ITEM 50" }), "{subject}");
            // system columns are not carried through the batch: such reads keep the per-row check
            assert!(!explain("select ctid, id from public.reshaped").contains("AuthzBatch"), "{subject}");
            assert_eq!(count("select count(ctid) from public.reshaped"), if subject == "wide" { 2 } else { 1 }, "{subject}");
            reset_role();
        }
    }

    #[pg_test]
    fn cached_batch_plans_answer_for_whoever_runs_them() {
        fixture();
        // reuse as built, whoever runs it (not rebuilt for another breadth)
        Spi::run("set pg_auth.replan_on_breadth = off").unwrap();
        Spi::run(
            "create table public.items (id int primary key);
             insert into public.items select generate_series(1, 200);
             select authz.bind('public.items', 'id', 'item', 'view');
             grant select on public.items to authz_test_user",
        )
        .unwrap();
        as_subject("wide");
        Spi::run(
            "prepare q as select string_agg(id::text, ',' order by id) from public.items; set local plan_cache_mode = force_generic_plan",
        )
        .unwrap();
        assert_eq!(Spi::get_one::<String>("execute q").unwrap().as_deref(), Some("5,50,150"));
        assert!(explain("execute q").contains("AuthzBatch"), "planned for a broad subject");
        // reused by a listed subject: answered from its listed ids
        as_subject("alice");
        Spi::run("set local plan_cache_mode = force_generic_plan").unwrap();
        assert!(explain("execute q").contains("AuthzBatch"), "the cached plan is reused");
        assert_eq!(Spi::get_one::<String>("execute q").unwrap().as_deref(), Some("50"));
        // and without a subject: nothing
        Spi::run("reset role; set local pg_auth.subject_id to default; set local role authz_test_user; set local plan_cache_mode = force_generic_plan").unwrap();
        assert_eq!(Spi::get_one::<String>("execute q").unwrap(), None);
        reset_role();
        Spi::run("deallocate q").unwrap();
    }

    #[pg_test]
    fn cached_plans_fetch_ids_for_whoever_runs_them() {
        fixture();
        // reuse as built, whoever runs it (not rebuilt for another breadth)
        Spi::run("set pg_auth.replan_on_breadth = off").unwrap();
        as_subject("alice");
        Spi::run(
            "prepare q as select string_agg(id::text, ',' order by id) from public.docs; set local plan_cache_mode = force_generic_plan",
        )
        .unwrap();
        assert_eq!(Spi::get_one::<String>("execute q").unwrap(), Some("2,4,6,8,10".into()));
        as_subject("bob");
        Spi::run("set local plan_cache_mode = force_generic_plan").unwrap();
        assert_eq!(Spi::get_one::<String>("execute q").unwrap(), Some("1,3,5,7,9".into()));
        // the plan was built for a listed subject; broad subjects batch-check the column's values
        as_subject("wide");
        Spi::run("set local plan_cache_mode = force_generic_plan").unwrap();
        assert!(explain("execute q").contains("InitPlan"), "the cached listed-ids plan is reused");
        assert_eq!(Spi::get_one::<String>("execute q").unwrap(), Some("1,2,3,4,5,6,7,8,9,10".into()));
        as_subject("wide_alice");
        Spi::run("set local plan_cache_mode = force_generic_plan").unwrap();
        assert_eq!(Spi::get_one::<String>("execute q").unwrap(), Some("2,4,6,8,10".into()));
        reset_role();
        Spi::run("deallocate q").unwrap();
    }

    #[pg_test]
    fn nullable_side_of_outer_join_keeps_plain_rls_and_correct_results() {
        fixture();
        Spi::run(
            "create table public.orgs (id int primary key); insert into public.orgs values (1), (2);
             grant select on public.orgs to authz_test_user",
        )
        .unwrap();
        as_subject("alice");
        let sql = "select count(*) from public.orgs o left join public.docs d on d.org_id = o.id";
        // org 1 matches 5 docs, org 2 is null-extended: 6 rows
        assert_eq!(count(sql), 6);
        reset_role();
    }

    #[pg_test]
    fn leaky_functions_never_see_unauthorized_rows() {
        fixture();
        Spi::run(
            "create table public.leaked (org_id int);
             grant insert on public.leaked to authz_test_user;
             create function public.peek(o int) returns boolean language plpgsql cost 0.0001 as $$
                 begin insert into public.leaked values (o); return true; end $$;",
        )
        .unwrap();
        // both enforcement forms stay security quals, which run before any user function
        for subject in ["alice", "wide_alice"] {
            as_subject(subject);
            for sql in [
                "select count(*) from public.docs where public.peek(org_id)",
                "select count(*) from public.docs d join public.pages p on public.peek(d.org_id)",
                "select count(*) from (select org_id, public.peek(org_id) p from public.docs) s where s.p",
                "select count(*) from (select id from public.docs where public.peek(org_id)) s",
                "select public.peek(org_id) from public.docs",
                "select id from public.docs order by public.peek(org_id), id",
                "select * from (select public.peek(org_id) p, count(*) from public.docs group by 1) s where s.p",
            ] {
                Spi::run(&format!("select count(*) from ({sql}) q")).unwrap();
            }
            reset_role();
        }
        assert_eq!(count("select count(*) from public.leaked"), count("select count(*) from public.leaked where org_id = 1"));
        assert!(count("select count(*) from public.leaked") > 0, "peek must have run");
    }

    #[pg_test]
    fn writes_need_the_write_permission() {
        fixture();
        as_subject("dana");
        Spi::run("insert into public.docs values (100, 1, 'mine')").unwrap();
        assert_eq!(count("select count(*) from public.docs where id = 100"), 1);
        reset_role();

        // alice can view org 1 but not edit it
        as_subject("alice");
        Spi::run(
            "do $$ begin
                 insert into public.docs values (101, 1, 'not allowed');
                 raise exception 'insert should have been denied';
             exception when insufficient_privilege then null;
             end $$",
        )
        .unwrap();
        reset_role();
    }

    #[pg_test]
    fn attributes_can_carry_another_name_than_their_column() {
        fake_sidecar();
        Spi::run(
            "create table public.claims (id int primary key, owner_id text, status text);
             select authz.protect('public.claims', type => 'claim', id => 'id', read => 'view',
                                  attributes => array['status'], attribute_map => '{\"ownerId\": \"owner_id\"}')",
        )
        .unwrap();
        let policy = Spi::get_one::<String>("select qual from pg_policies where tablename = 'claims' and cmd = 'SELECT'")
            .unwrap()
            .unwrap_or_default();
        assert!(policy.contains("'ownerId', owner_id") && policy.contains("'status', status"), "{policy}");
        // a map naming a column the table lacks fails at once
        assert!(fails(
            "select authz.protect('public.claims', type => 'claim', id => 'id', read => 'view', attribute_map => '{\"x\": \"nope\"}')"
        ));
    }

    #[pg_test]
    fn update_and_delete_can_need_their_own_permission() {
        fixture();
        // dana may edit org 1 but has no 'remove' on it: she updates, she does not delete
        Spi::run(
            "select authz.protect('public.docs', type => 'org', id => 'org_id', read => 'view', write => 'edit', on_delete => 'remove')",
        )
        .unwrap();
        let status =
            Spi::get_one::<String>("select value from authz.status() where item like 'protected %docs'").unwrap().unwrap_or_default();
        assert!(status.contains("/ write edit / delete remove"), "{status}");
        assert_eq!(error_of("select 1"), None);
        as_subject("dana");
        assert_eq!(count("with u as (update public.docs set title = 'x' where org_id = 1 returning 1) select count(*) from u"), 5);
        assert_eq!(count("with d as (delete from public.docs where org_id = 1 returning 1) select count(*) from d"), 0);
        reset_role();
        // alice may only view: neither
        as_subject("alice");
        assert_eq!(count("with u as (update public.docs set title = 'y' where org_id = 1 returning 1) select count(*) from u"), 0);
        reset_role();
    }

    fn lint_findings(object_like: &str) -> Vec<String> {
        Spi::connect(|client| {
            client
                .select("select code || ' ' || object from authz.lint() where object like $1 order by 1", None, &[object_like.into()])
                .unwrap()
                .map(|row| row.get::<String>(1).unwrap().unwrap())
                .collect()
        })
    }

    #[pg_test]
    fn lint_flags_common_rls_problems() {
        Spi::run(
            "create schema lint;
             create table lint.members (user_id text, org_id int);
             alter table lint.members enable row level security;
             create policy own on lint.members using (user_id = current_user);
             create function lint.uid() returns text language sql stable as $$ select current_setting('app.uid', true) $$;
             create function lint.is_member(o int) returns boolean language plpgsql stable as $$
                 begin return exists (select 1 from lint.members where org_id = o and user_id = lint.uid()); end $$;

             create table lint.per_row_fn (id int, org_id int);
             alter table lint.per_row_fn enable row level security;
             create policy p on lint.per_row_fn using (lint.is_member(org_id));

             create table lint.unwrapped (id int, owner text);
             alter table lint.unwrapped enable row level security;
             create policy p on lint.unwrapped using (owner = lint.uid());

             create table lint.correlated (id int, org_id int);
             alter table lint.correlated enable row level security;
             create policy p on lint.correlated using (exists (
                 select 1 from lint.members m where m.org_id = correlated.org_id and m.user_id = (select lint.uid())));

             create table lint.good (id int, owner text);
             create index on lint.good (owner);
             alter table lint.good enable row level security;
             create policy p on lint.good using (owner = (select lint.uid()));

             create table lint.no_policies (id int);
             alter table lint.no_policies enable row level security;

             create table lint.parts (id int, org_id int) partition by list (org_id);
             create table lint.parts_1 partition of lint.parts for values in (1);
             alter table lint.parts enable row level security;
             create policy p on lint.parts using (org_id = 1);

             create view lint.owner_view as select * from lint.good;

             create table lint.update_only (id int);
             alter table lint.update_only enable row level security;
             create policy u on lint.update_only for update using (true);",
        )
        .unwrap();

        let all = lint_findings("lint.%");
        let has = |code: &str, object: &str| all.iter().any(|f| f.starts_with(code) && f.contains(object));
        assert!(has("P001", "lint.per_row_fn.p"), "plpgsql per row: {all:?}");
        assert!(has("P002", "lint.unwrapped.p"), "unwrapped uid(): {all:?}");
        assert!(has("P005", "lint.unwrapped.p"), "owner not indexed: {all:?}");
        assert!(has("P003", "lint.correlated.p"), "correlated EXISTS: {all:?}");
        assert!(has("P004", "lint.correlated.p"), "reads lint.members which has RLS: {all:?}");
        assert!(has("R001", "lint.no_policies"), "{all:?}");
        assert!(has("R002", "lint.parts_1"), "{all:?}");
        assert!(has("R003", "lint.owner_view"), "{all:?}");
        assert!(has("R004", "lint.update_only"), "{all:?}");
        assert!(!all.iter().any(|f| f.contains("lint.good.p")), "well-formed policy is clean: {all:?}");
    }

    #[pg_test]
    fn lint_accepts_pg_auth_policies() {
        fixture();
        Spi::run("create index on public.docs (org_id); create index on public.pages (document_id)").unwrap();
        assert_eq!(lint_findings("public.%"), Vec::<String>::new());
    }

    fn explained(sql: &str) -> Vec<String> {
        Spi::connect(|client| {
            client
                .select("select table_name || ' | ' || mode || ' | ' || reason from authz.explain($1) order by 1", None, &[sql.into()])
                .unwrap()
                .map(|row| row.get::<String>(1).unwrap().unwrap())
                .collect()
        })
    }

    #[pg_test]
    fn explain_reports_mode_and_reason_per_table() {
        fixture();
        as_subject("alice");
        let rows = explained("select * from public.docs d join public.pages p on p.id = d.id");
        assert_eq!(
            rows,
            [
                "public.docs | listed ids | user:alice may access 1 org objects: indexable filter",
                "public.pages | listed ids | user:alice may access 1 document objects: indexable filter",
            ],
        );

        as_subject("nobody");
        let rows = explained("select * from public.docs");
        assert!(rows[0].contains("listed ids") && rows[0].contains("no rows read"), "{rows:?}");
        assert_eq!(count("select count(*) from public.docs"), 0);

        as_subject("wide");
        let rows = explained("select * from public.docs");
        assert!(rows[0].contains("batched check") && rows[0].contains("more org objects than the engine lists"), "{rows:?}");

        Spi::run("set local pg_auth.plan_lookup = off").unwrap();
        let rows = explained("select * from public.docs");
        assert!(rows[0].contains("plan_lookup is off"), "{rows:?}");
        reset_role();
    }

    #[pg_test]
    fn ordinary_roles_cannot_choose_their_subject() {
        fixture();
        Spi::run("set local role authz_test_user").unwrap();
        Spi::run(
            "do $$ begin
                 set pg_auth.subject_id = 'carol';
                 raise exception 'setting the subject should have been denied';
             exception when insufficient_privilege then null;
             end $$",
        )
        .unwrap();
        assert_eq!(count("select count(*) from public.docs"), 0);
        reset_role();

        // the API's connection role is allowed explicitly
        Spi::run(
            "create role authz_api nologin;
             grant set on parameter pg_auth.subject_id to authz_api;
             grant select on public.docs to authz_api;
             set local role authz_api;
             set local pg_auth.subject_id = 'carol'",
        )
        .unwrap();
        assert_eq!(count("select count(*) from public.docs where org_id = 2"), 5);
        reset_role();
    }

    #[pg_test]
    fn jwt_claims_do_not_override_the_configured_source() {
        fixture();
        // default source is the privileged setting: forged claims are ignored
        Spi::run(r#"set local role authz_test_user; set local request.jwt.claims = '{"sub": "carol"}'"#).unwrap();
        assert_eq!(count("select count(*) from public.docs"), 0);
        let detail = Spi::get_one::<String>("select detail from authz.current_subject()").unwrap().unwrap();
        assert!(detail.contains("not set"), "{detail}");
        reset_role();
    }

    #[pg_test]
    fn signed_tokens_identify_the_subject_and_forgeries_do_not() {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use hmac::{KeyInit, Mac};
        let b64 = |b: &[u8]| URL_SAFE_NO_PAD.encode(b);
        let token = |secret: &[u8], claims: &str| {
            let signed = format!("{}.{}", b64(br#"{"alg":"HS256","kid":"k1"}"#), b64(claims.as_bytes()));
            let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret).unwrap();
            mac.update(signed.as_bytes());
            format!("{signed}.{}", b64(&mac.finalize().into_bytes()))
        };
        let as_bearer = |t: &str| {
            Spi::run(&format!(
                r#"reset role; set local role authz_test_user;
                   set local request.headers = '{{"Authorization": "Bearer {t}"}}'"#
            ))
            .unwrap();
        };

        fixture();
        Spi::run(&format!(
            r#"select authz.configure(subject => 'jwt_signed', jwks => '{{"keys": [{{"kty": "oct", "kid": "k1", "k": "{}"}}]}}')"#,
            b64(b"top-secret")
        ))
        .unwrap();

        as_bearer(&token(b"top-secret", r#"{"sub":"bob","exp":4102444800}"#));
        assert_eq!(count("select count(*) from public.docs where org_id = 2"), 5);

        as_bearer(&token(b"guessed", r#"{"sub":"bob","exp":4102444800}"#));
        assert_eq!(count("select count(*) from public.docs"), 0, "wrong key");
        let detail = Spi::get_one::<String>("select detail from authz.current_subject()").unwrap().unwrap();
        assert!(detail.contains("signature"), "{detail}");

        as_bearer(&token(b"top-secret", r#"{"sub":"bob","exp":1000}"#));
        assert_eq!(count("select count(*) from public.docs"), 0, "expired");
        reset_role();
    }

    #[pg_test]
    fn bound_tables_apply_policies_to_their_owner() {
        fixture();
        Spi::run(
            "create role authz_owner nologin;
             create table public.owned (id int, org_id int);
             insert into public.owned values (1, 1), (2, 2);
             alter table public.owned owner to authz_owner;
             select authz.bind('public.owned', 'org_id', 'org', 'view');
             set local pg_auth.subject_id = 'alice';
             set local role authz_owner",
        )
        .unwrap();
        assert_eq!(count("select count(*) from public.owned"), 1);
        reset_role();
    }

    #[pg_test]
    fn partitions_created_after_bind_are_protected() {
        fixture();
        Spi::run(
            "create table public.late_events (id int, org_id int not null) partition by list (org_id);
             create table public.late_events_1 partition of public.late_events for values in (1);
             select authz.bind('public.late_events', 'org_id', 'org', 'view');
             create table public.late_events_2 partition of public.late_events for values in (2);
             create table public.late_events_3 (id int, org_id int not null);
             alter table public.late_events attach partition public.late_events_3 for values in (3);
             insert into public.late_events values (1, 1), (2, 2), (3, 3);
             grant select on public.late_events, public.late_events_1, public.late_events_2, public.late_events_3 to authz_test_user",
        )
        .unwrap();
        as_subject("alice");
        assert_eq!(count("select count(*) from public.late_events"), 1);
        assert_eq!(count("select count(*) from public.late_events_2"), 0, "created after bind");
        assert_eq!(count("select count(*) from public.late_events_3"), 0, "attached after bind");
        reset_role();
    }

    #[pg_test]
    fn stats_count_round_trips_and_checked_ids() {
        fixture();
        let stats = || {
            Spi::get_two::<i64, i64>("select sidecar_calls, ids_checked from authz.stats()").map(|(c, i)| (c.unwrap(), i.unwrap())).unwrap()
        };
        let (calls, ids) = stats();
        as_subject("alice");
        assert_eq!(count("select count(*) from public.docs"), 5);
        reset_role();
        let (calls_listed, ids_listed) = stats();
        assert_eq!((calls_listed - calls, ids_listed - ids), (1, 0), "one list, nothing checked");

        // broad subject: the scan checks its rows in a batch
        as_subject("wide");
        assert_eq!(count("select count(*) from public.docs"), 10);
        reset_role();
        let (calls_wide, ids_wide) = stats();
        assert_eq!(ids_wide - ids_listed, 2, "two orgs checked");
        assert_eq!(calls_wide - calls_listed, 3, "one list, a fill request (no permission index set), one batch check for both orgs");
    }

    #[pg_test]
    fn check_and_status_ask_the_engine() {
        fixture();
        fails("select 1");
        let check = |s: &str, p: &str, o: &str| {
            Spi::get_one_with_args::<bool>("select authz.check($1, $2, $3)", &[s.into(), p.into(), o.into()]).unwrap().unwrap()
        };
        assert!(check("user:alice", "view", "org:1"));
        assert!(!check("user:alice", "view", "org:2"));
        assert!(check("user:wide", "view", "org:2"), "per-object check for broad subjects");
        assert!(fails("select authz.check('alice', 'view', 'org:1')"), "subjects are type:id");

        as_subject("bob");
        assert_eq!(Spi::get_one::<bool>("select authz.can('view', 'org:2')").unwrap(), Some(true));
        assert_eq!(Spi::get_one::<bool>("select authz.can('view', 'org:1')").unwrap(), Some(false));
        assert!(fails("select authz.check('user:alice', 'view', 'org:1')"), "check is for administrators");
        reset_role();

        let engine = Spi::get_one::<String>("select value from authz.status() where item = 'engine'").unwrap();
        assert_eq!(engine.as_deref(), Some("fake"));
        Spi::run("set pg_auth.sidecar_socket = '/nonexistent.sock'").unwrap();
        let engine = Spi::get_one::<String>("select value from authz.status() where item = 'engine'").unwrap().unwrap();
        assert!(engine.starts_with("unreachable"), "{engine}");
        as_subject("carol"); // not asked about yet in this transaction
        assert!(fails("select count(*) from public.docs"), "fails closed");
        reset_role();
    }

    #[pg_test]
    fn partitions_are_protected_when_queried_directly() {
        Spi::run(
            "create table public.events (id int, org_id int not null) partition by list (org_id);
             create table public.events_1 partition of public.events for values in (1);
             create table public.events_2 partition of public.events for values in (2);
             insert into public.events values (1, 1), (2, 2);",
        )
        .unwrap();
        fixture();
        Spi::run(
            "select authz.bind('public.events', 'org_id', 'org', 'view');
             grant select on public.events, public.events_1, public.events_2 to authz_test_user",
        )
        .unwrap();
        as_subject("alice");
        assert_eq!(count("select count(*) from public.events"), 1);
        assert_eq!(count("select count(*) from public.events_2"), 0);
        reset_role();
    }
}

/// This module is required by `cargo pgrx test` invocations.
/// It must be visible at the root of your extension crate.
#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {}

    #[must_use]
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        vec!["shared_preload_libraries = 'pg_auth'"]
    }
}
