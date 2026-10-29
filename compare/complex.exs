#!/usr/bin/env elixir
# Complex setups end to end, as a backend-less app would write them: the engines' published shapes
# (OpenFGA's sample stores, AuthZed's schema examples, Cerbos's demo apps), set up with
# authz.relate and authz.protect, checked through what each user sees and may write. See
# docs/proposals/setting-rules.md for where these come from and what they cannot yet express.
#
#   github  teams inside teams, repo access to a user or a team, the org owner sees every repo, a
#           ban (but not), push needed to update                          OpenFGA, SpiceDB
#   drive   nested groups, viewers inherited down a folder tree, a public link (user:*), set up by one
#           authz.setup document
#                                                                         OpenFGA, SpiceDB
#   expense derived roles (owner, finance), an amount limit on approval, a tenant's scoped deny, camelCase attributes
#                                                                         Cerbos
#
#   elixir compare/complex.exs                    # openfga, spicedb, cerbos
#   elixir compare/complex.exs --engine cerbos --no-build

Mix.install([{:req, "~> 0.7"}, {:postgrex, "~> 0.22"}], config: [postgrex: [json_library: JSON]])

dir = Path.join(__DIR__, "lib")
for f <- ~w(engine docker engines/openfga engines/spicedb engines/cerbos stack), do: Code.require_file(Path.join(dir, f <> ".exs"))

defmodule Complex do
  alias Compare.{Docker, Stack}

  # One model for both relationship scenarios. The application writes plain relations; tables are
  # protected by permissions computed from them (SpiceDB only takes writes to relations).
  @model """
  model
    schema 1.1
  type user
  type team
    relations
      define member: [user, team#member]
  type organization
    relations
      define owner: [user]
      define member: [user]
      define can_read_all: owner
  type repo
    relations
      define organization: [organization]
      define reader: [user, team#member]
      define writer: [user, team#member]
      define admin: [user, team#member]
      define banned: [user]
      define can_read: (reader or writer or admin or can_read_all from organization) but not banned
      define can_push: (writer or admin or can_read_all from organization) but not banned
      define can_delete: (admin or can_read_all from organization) but not banned
  type group
    relations
      define member: [user, group#member]
  type folder
    relations
      define parent: [folder]
      define viewer_grant: [user, user:*, group#member]
      define viewer: viewer_grant or viewer from parent
  type doc
    relations
      define folder: [folder]
      define viewer_grant: [user, user:*, group#member]
      define viewer: viewer_grant or viewer from folder
  """

  # Drive's setup as one document, applied with authz.setup
  @drive_setup %{
    relationships: [
      %{table: "app.group_members", name: "user", object: %{type: "group", column: "group_id"}, relation: "member",
        subject: %{type: "user", column: "user_id"}},
      %{table: "app.group_members", name: "group", object: %{type: "group", column: "group_id"}, relation: "member",
        subject: %{type: "group", column: "child_group", relation: "member"}},
      %{table: "app.folders", object: %{type: "folder", column: "id"}, relation: "parent", subject: %{type: "folder", column: "parent_id"}},
      %{table: "app.docs", object: %{type: "doc", column: "id"}, relation: "folder", subject: %{type: "folder", column: "folder_id"}},
      %{table: "app.folder_shares", name: "user", object: %{type: "folder", column: "folder_id"}, relation: "viewer_grant",
        subject: %{type: "user", column: "user_id"}},
      %{table: "app.folder_shares", name: "group", object: %{type: "folder", column: "folder_id"}, relation: "viewer_grant",
        subject: %{type: "group", column: "group_id", relation: "member"}},
      # one table, a declaration per kind of grantee; the public link is everyone (user:*)
      %{table: "app.doc_shares", by: "grantee_kind", object: %{type: "doc", column: "doc_id"}, relation: "viewer_grant",
        subject: %{user: %{type: "user", column: "grantee_id"}, public: %{type: "user", id: "*"}}}
    ],
    protect: [
      %{table: "app.docs", type: "doc", column: "id", select: "viewer"},
      %{table: "app.folders", type: "folder", column: "id", select: "viewer"}
    ]
  }

  # how long a change may take to become visible before the check fails
  @deadline_ms 15_000

  def main(argv) do
    {opts, _} = OptionParser.parse!(argv, strict: [engine: :string, build: :boolean])
    root = Path.expand("..", __DIR__)
    if Keyword.get(opts, :build, true), do: Stack.build(root)
    engines = if e = opts[:engine], do: [e], else: ["openfga", "spicedb", "cerbos"]
    Process.flag(:trap_exit, true)
    results = for e <- engines, do: {e, run(e)}
    for {e, steps} <- results, {step, outcome} <- steps, do: IO.puts("#{String.pad_trailing(e, 8)} #{String.pad_trailing(step, 60)} #{outcome}")
    ok = Enum.all?(results, fn {_, steps} -> Enum.all?(steps, fn {_, o} -> not String.starts_with?(o, "FAILED") end) end)
    IO.puts(if ok, do: "All steps passed.", else: "Failures.")
    if not ok, do: System.halt(1)
  end

  defp run(engine) do
    stack = Stack.new("complex", "pg_auth_compare=1")

    try do
      if engine == "cerbos" do
        policies = Path.join(__DIR__, "scenarios/complex/cerbos")
        stack = Stack.start_engine(stack, "", [default: 1000], engine: "cerbos", policies: policies, sidecar_cache_ms: nil)
        conn = start_postgres(stack)
        expense(conn)
      else
        url = "postgres://postgres:complex@#{stack.id}-postgres:5432/postgres"
        stack = Stack.start_engine(stack, @model, [default: 1000], engine: engine, sidecar_cache_ms: nil, sidecar_env: [{"PG_AUTH_DATABASE_URL", url}])
        conn = start_postgres(stack)
        github(conn) ++ drive(conn)
      end
    catch
      kind, reason ->
        for {c, log} <- Stack.logs(stack), do: IO.puts(:stderr, "---- #{c}\n#{String.slice(log, -3000, 3000)}")
        [{"run", "FAILED: " <> Exception.format(kind, reason, __STACKTRACE__)}]
    after
      Stack.stop(stack)
    end
  end

  defp start_postgres(stack) do
    pg = Stack.track(stack, "#{stack.id}-postgres")
    {^pg, port} =
      Docker.run(pg, Stack.pg_image(), 5432, ["-c", "fsync=off"],
        Stack.pg_docker_args(stack) ++ Docker.coverage_args("postgres") ++ ["-e", "POSTGRES_PASSWORD=complex"])
    Stack.wait(fn -> match?({_, 0}, System.cmd("docker", ["exec", pg, "pg_isready", "-h", "127.0.0.1", "-U", "postgres"])) end, pg)
    {:ok, conn} =
      retry(fn -> Postgrex.start_link(hostname: "127.0.0.1", port: port, username: "postgres", password: "complex", database: "postgres", pool_size: 1) end, 20)
    for sql <- ["create extension pg_auth", "alter system set pg_auth.sidecar_socket = '#{Stack.socket("default")}'", "select pg_reload_conf()",
                "create role app_user nologin", "create schema app", "grant usage on schema app to app_user"],
        do: Postgrex.query!(conn, sql, [])
    conn
  end

  defp retry(f, 0), do: f.()

  defp retry(f, n) do
    case f.() do
      {:ok, c} -> {:ok, c}
      _ -> Process.sleep(500) && retry(f, n - 1)
    end
  end

  # ── GitHub: OpenFGA's github sample store and AuthZed's GitHub schema, cut to their shape ─────

  defp github(conn) do
    run_all(conn, [
      "select authz.configure(subject => 'jwt')",
      "create table app.org_members (org_id int, user_id text, role text, primary key (org_id, user_id))",
      "create table app.team_members (team text, user_id text, child_team text)",
      "create table app.repos (id int primary key, org_id int not null, name text not null)",
      # access goes to a user or to a team's members: one table, the kind in a column
      "create table app.repo_access (repo_id int not null, grantee_kind text not null, grantee_id text not null, role text not null)",
      "create table app.repo_bans (repo_id int not null, user_id text not null)",
      "grant select, insert, update, delete on all tables in schema app to app_user",
      "insert into app.org_members values (1, 'olivia', 'owner')",
      # core's members are alice; platform's are core's members (a team inside a team)
      "insert into app.team_members values ('core', 'alice', null), ('platform', null, 'core')",
      "insert into app.repos select i, case when i = 5 then 2 else 1 end, 'repo ' || i from generate_series(1, 5) i",
      "insert into app.repo_access values (1, 'team', 'platform', 'reader'), (2, 'user', 'bob', 'writer'), (3, 'team', 'core', 'admin'), (5, 'user', 'alice', 'reader')",
      "insert into app.repo_bans values (3, 'alice')",
      "select authz.relate('app.org_members', object_type => 'organization', object_column => 'org_id', relation_column => 'role', subject_type => 'user', subject_column => 'user_id')",
      "select authz.relate('app.team_members', name => 'user', object_type => 'team', object_column => 'team', relation => 'member', subject_type => 'user', subject_column => 'user_id')",
      "select authz.relate('app.team_members', name => 'team', object_type => 'team', object_column => 'team', relation => 'member', subject_type => 'team', subject_column => 'child_team', subject_relation => 'member')",
      "select authz.relate('app.repos', object_type => 'repo', object_column => 'id', relation => 'organization', subject_type => 'organization', subject_column => 'org_id')",
      # a declaration per kind of grantee, and only the roles the model has
      "select authz.relate('app.repo_access', name => 'user', filter => 'grantee_kind = ''user''', object_type => 'repo', object_column => 'repo_id', relation_column => 'role', subject_type => 'user', subject_column => 'grantee_id', relations => array['reader', 'writer', 'admin'])",
      "select authz.relate('app.repo_access', name => 'team', filter => 'grantee_kind = ''team''', object_type => 'repo', object_column => 'repo_id', relation_column => 'role', subject_type => 'team', subject_column => 'grantee_id', subject_relation => 'member', relations => array['reader', 'writer', 'admin'])",
      "select authz.relate('app.repo_bans', object_type => 'repo', object_column => 'repo_id', relation => 'banned', subject_type => 'user', subject_column => 'user_id')",
      # pushing updates a repo; deleting one takes more
      "select authz.protect('app.repos', type => 'repo', id => 'id', read => 'can_read', write => 'can_push', on_delete => 'can_delete')"
    ])
    delivered!(conn)
    repos = &visible(conn, %{sub: &1}, "select id from app.repos order by id")
    [
      check("github: the org owner sees every repo of the org", fn -> repos.("olivia") == [1, 2, 3, 4] end),
      check("github: alice, in core inside platform, is banned from 3", fn -> repos.("alice") == [1, 5] end),
      check("github: bob, a direct writer, sees his repo alone", fn -> repos.("bob") == [2] end),
      check("github: carol, in no team, sees nothing", fn -> repos.("carol") == [] end),
      check("github: bob may push to his repo", fn -> updated(conn, %{sub: "bob"}, "update app.repos set name = 'x' where id = 2") == 1 end),
      check("github: alice, a reader, may not push", fn -> updated(conn, %{sub: "alice"}, "update app.repos set name = 'x' where id = 1") == 0 end),
      check("github: the owner may push anywhere in the org", fn -> updated(conn, %{sub: "olivia"}, "update app.repos set name = 'x' where id = 4") == 1 end),
      check("github: bob, a writer, may not delete his repo", fn -> updated(conn, %{sub: "bob"}, "delete from app.repos where id = 2") == 0 end),
      check("github: the owner may delete a repo", fn -> updated(conn, %{sub: "olivia"}, "delete from app.repos where id = 4") == 1 end),
      check("github: a role the model lacks fails the write itself", fn ->
        match?({:error, %Postgrex.Error{postgres: %{code: :check_violation}}}, query(conn, "insert into app.repo_access values (4, 'user', 'carol', 'owner')"))
      end),
      timed(conn, "github: carol given read on repo 4 sees it", fn -> query(conn, "insert into app.repo_access values (4, 'user', 'carol', 'reader')") end,
            fn -> repos.("carol") == [4] end),
      timed(conn, "github: alice's ban lifted, she sees repo 3", fn -> query(conn, "delete from app.repo_bans where user_id = 'alice'") end,
            fn -> repos.("alice") == [1, 3, 5] end)
    ]
  end

  # ── Drive: OpenFGA's gdrive sample store and AuthZed's docs-style sharing ─────────────────────

  defp drive(conn) do
    run_all(conn, [
      "create table app.group_members (group_id text not null, user_id text, child_group text)",
      "create table app.folders (id int primary key, parent_id int)",
      "create table app.docs (id int primary key, folder_id int not null, title text not null)",
      "create table app.folder_shares (folder_id int not null, user_id text, group_id text)",
      # a public link is a share with everyone (user:*): its own kind, no grantee
      "create table app.doc_shares (doc_id int not null, grantee_kind text not null, grantee_id text)",
      "grant select, insert, update, delete on all tables in schema app to app_user",
      # staff's members (eve) are also engineers (a group inside a group)
      "insert into app.group_members values ('staff', 'eve', null), ('engineers', 'frank', null), ('engineers', null, 'staff')",
      "insert into app.folders values (1, null), (2, 1), (3, 2)",
      "insert into app.docs values (1, 1, 'handbook'), (2, 2, 'design'), (3, 3, 'secrets'), (4, 1, 'press kit')",
      "insert into app.folder_shares values (2, null, 'engineers'), (3, 'frank', null)",
      "insert into app.doc_shares values (4, 'public', null)",
      # the whole setup as one document (authz.setup), where github above uses the calls
      "select authz.setup('#{JSON.encode!(@drive_setup)}')"
    ])
    delivered!(conn)
    docs = &visible(conn, %{sub: &1}, "select id from app.docs order by id")
    [
      check("drive: eve, staff inside engineers, inherits eng's docs down the tree", fn -> docs.("eve") == [2, 3, 4] end),
      check("drive: frank, an engineer, also has secrets shared directly", fn -> docs.("frank") == [2, 3, 4] end),
      check("drive: zed, in no group, sees the public doc alone", fn -> docs.("zed") == [4] end),
      check("drive: folders follow the same tree", fn -> visible(conn, %{sub: "eve"}, "select id from app.folders order by id") == [2, 3] end),
      timed(conn, "drive: eve leaves staff, keeps the public doc", fn -> query(conn, "delete from app.group_members where user_id = 'eve'") end,
            fn -> docs.("eve") == [4] end),
      timed(conn, "drive: the public link removed, zed sees nothing", fn -> query(conn, "delete from app.doc_shares where grantee_kind = 'public'") end,
            fn -> docs.("zed") == [] end)
    ]
  end

  # ── Expense approval: Cerbos's expense demo, with a tenant's scoped policy ─────────────────────

  defp expense(conn) do
    run_all(conn, [
      # the principal: the token's sub and role, its claims as attributes, its tenant as the scope
      "select authz.configure(subject => 'jwt', roles_claim => 'role', scope_claim => 'tenant')",
      "create table app.expenses (id int primary key, owner text not null, amount numeric not null, status text not null)",
      "create index on app.expenses (owner)",
      "grant select, insert, update, delete on all tables in schema app to app_user",
      "insert into app.expenses values (1, 'ann', 500, 'open'), (2, 'ann', 5000, 'open'), (3, 'ben', 200, 'open'), (4, 'ann', 100, 'flagged')",
      # the policies say ownerId, as Cerbos's demos do; the column is owner
      "select authz.protect('app.expenses', type => 'expense', id => 'id', read => 'view', write => 'approve', attributes => array['amount', 'status'], attribute_map => '{\"ownerId\": \"owner\"}')"
    ])
    ann = %{sub: "ann", role: "user", department: "sales", tenant: "acme"}
    ben = %{sub: "ben", role: "user", department: "sales", tenant: "acme"}
    fin = %{sub: "fin", role: "user", department: "finance", tenant: "acme"}
    # finance outside any tenant: the base policy alone, the scoped deny does not apply
    fin_untenanted = %{sub: "fin2", role: "user", department: "finance"}
    expenses = &visible(conn, &1, "select id from app.expenses order by id")
    [
      check("expense: ann owns 1, 2 and 4; acme hides the flagged one", fn -> expenses.(ann) == [1, 2] end),
      check("expense: ben owns 3", fn -> expenses.(ben) == [3] end),
      check("expense: finance in acme sees every unflagged expense", fn -> expenses.(fin) == [1, 2, 3] end),
      check("expense: finance without a tenant sees the flagged one too", fn -> expenses.(fin_untenanted) == [1, 2, 3, 4] end),
      check("expense: finance approves the two small ones it did not file", fn ->
        updated(conn, fin, "update app.expenses set status = 'approved' where status = 'open'") == 2
      end),
      check("expense: ann may not approve her own", fn -> updated(conn, ann, "update app.expenses set status = 'approved' where id = 2") == 0 end),
      check("expense: the plan is a filter on the columns, not a check per row", fn ->
        plan = explain(conn, fin, "select id from app.expenses")
        not String.contains?(plan, "AuthzBatch") and not String.contains?(plan, "permits_resource")
      end)
    ]
  end

  # ── helpers ────────────────────────────────────────────────────────────────────────────────

  defp run_all(conn, statements), do: Enum.each(statements, &Postgrex.query!(conn, &1, []))
  defp query(conn, sql), do: Postgrex.query(conn, sql, [])

  defp check(what, f), do: {what, outcome(f.(), "ok")}

  defp timed(conn, what, write, visible?) do
    {:ok, _} = write.()
    started = System.monotonic_time(:millisecond)
    ok = eventually(visible?)
    ms = System.monotonic_time(:millisecond) - started
    delivered!(conn)
    {what, outcome(ok, "visible after #{ms} ms")}
  end

  defp outcome(true, detail), do: detail
  defp outcome(false, _), do: "FAILED"

  defp eventually(f, waited \\ 0) do
    cond do
      f.() -> true
      waited >= @deadline_ms -> false
      true -> Process.sleep(20) && eventually(f, waited + 20)
    end
  end

  defp delivered?(conn), do: Postgrex.query!(conn, "select count(*) from authz.outbox", []).rows == [[0]]
  defp delivered!(conn), do: eventually(fn -> delivered?(conn) end) || raise("the outbox was not delivered")

  # as the application's role, with these token claims (as PostgREST sets them)
  defp as(conn, claims, f) do
    Postgrex.transaction(conn, fn c ->
      Postgrex.query!(c, "select set_config('request.jwt.claims', $1, true)", [JSON.encode!(claims)])
      Postgrex.query!(c, "set local role app_user", [])
      f.(c)
    end)
  end

  defp visible(conn, claims, sql) do
    {:ok, ids} = as(conn, claims, fn c -> Postgrex.query!(c, sql, []).rows |> List.flatten() end)
    ids
  end

  # rows changed; the write is rolled back so the next check starts from the same data
  defp updated(conn, claims, sql) do
    {:error, {:changed, n}} = as(conn, claims, fn c -> Postgrex.rollback(c, {:changed, Postgrex.query!(c, sql, []).num_rows}) end)
    n
  end

  defp explain(conn, claims, sql) do
    {:ok, plan} = as(conn, claims, fn c -> Postgrex.query!(c, "explain " <> sql, []).rows |> List.flatten() |> Enum.join("\n") end)
    plan
  end
end

Complex.main(System.argv())
