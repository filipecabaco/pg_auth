#!/usr/bin/env elixir
# Relationship tables end to end: an application with no backend grants and revokes access by
# writing rows, through row level security, and the engine (OpenFGA, SpiceDB) follows. Each step
# is checked through what subjects see, and timed from the writing transaction's commit to the
# moment the subject sees the change. See compare/README.md ("Relationship tables").
#
#   elixir compare/relationships.exs                    # OpenFGA, then SpiceDB
#   elixir compare/relationships.exs --engine spicedb --no-build

Mix.install([{:req, "~> 0.7"}, {:postgrex, "~> 0.22"}], config: [postgrex: [json_library: JSON]])

dir = Path.join(__DIR__, "lib")
for f <- ~w(engine docker engines/openfga engines/spicedb engines/cerbos stack), do: Code.require_file(Path.join(dir, f <> ".exs"))

defmodule Relationships do
  alias Compare.{Docker, Stack}

  # What the application writes are plain relations (admin, member, shared_with), what it asks
  # are permissions computed from them (viewer): SpiceDB only takes writes to relations, and the
  # model reads the same in OpenFGA.
  @model """
  model
    schema 1.1
  type user
  type team
    relations
      define member: [user]
  type org
    relations
      define admin: [user]
      define member: [user, team#member]
      define can_manage: admin
      define can_read: member or admin
  type document
    relations
      define org: [org]
      define shared_with: [user, team#member]
      define viewer: shared_with or can_read from org
  """

  # how long a change may take to become visible before the check fails
  @deadline_ms 15_000

  def main(argv) do
    {opts, _} = OptionParser.parse!(argv, strict: [engine: :string, build: :boolean])
    root = Path.expand("..", __DIR__)
    if Keyword.get(opts, :build, true), do: Stack.build(root)
    engines = if e = opts[:engine], do: [e], else: ["openfga", "spicedb"]
    Process.flag(:trap_exit, true)
    results = for e <- engines, do: {e, run(e)}
    for {e, steps} <- results, {step, outcome} <- steps, do: IO.puts("#{String.pad_trailing(e, 8)} #{String.pad_trailing(step, 52)} #{outcome}")
    ok = Enum.all?(results, fn {_, steps} -> Enum.all?(steps, fn {_, o} -> not String.starts_with?(o, "FAILED") end) end)
    IO.puts(if ok, do: "All steps passed.", else: "Failures.")
    if not ok, do: System.halt(1)
  end

  defp run(engine) do
    stack = Stack.new("relationships", "pg_auth_compare=1")

    try do
      url = "postgres://postgres:relationships@#{stack.id}-postgres:5432/postgres"
      # production cache settings: answers cached a second, refreshed while active
      stack = Stack.start_engine(stack, @model, [default: 1000], engine: engine, sidecar_cache_ms: nil, sidecar_env: [{"PG_AUTH_DATABASE_URL", url}])
      conn = start_postgres(stack)
      setup(conn)
      steps(conn, stack)
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
        Stack.pg_docker_args(stack) ++ Docker.coverage_args("postgres") ++ ["-e", "POSTGRES_PASSWORD=relationships"])
    Stack.wait(fn -> match?({_, 0}, System.cmd("docker", ["exec", pg, "pg_isready", "-h", "127.0.0.1", "-U", "postgres"])) end, pg)
    {:ok, conn} =
      retry(fn -> Postgrex.start_link(hostname: "127.0.0.1", port: port, username: "postgres", password: "relationships", database: "postgres", pool_size: 1) end, 20)
    for sql <- ["create extension pg_auth", "alter system set pg_auth.sidecar_socket = '#{Stack.socket("default")}'", "select pg_reload_conf()"],
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

  # The application's schema, as its migrations would write it: documents belong to an org, orgs
  # have members with a role, documents can be shared with a user or a team. Each table's rows are
  # relationships; reads and writes go through pg_auth.
  defp setup(conn) do
    for sql <- [
          "create role app_user nologin",
          "create schema app",
          "grant usage on schema app to app_user",
          "create table app.documents (id int primary key, org_id int not null, title text not null)",
          "create table app.memberships (org_id int not null, user_id text not null, role text not null, primary key (org_id, user_id))",
          "create table app.team_members (team text not null, user_id text not null, primary key (team, user_id))",
          "create table app.shares (doc_id int not null, user_id text, team text)",
          "grant select, insert, delete on all tables in schema app to app_user",
          # a document belongs to its org; a membership makes a user an admin or a member
          "select authz.relate('app.documents', object_type => 'document', object_column => 'id', relation => 'org', subject_type => 'org', subject_column => 'org_id')",
          "select authz.relate('app.memberships', object_type => 'org', object_column => 'org_id', relation_column => 'role', subject_type => 'user', subject_column => 'user_id')",
          "select authz.relate('app.team_members', object_type => 'team', object_column => 'team', relation => 'member', subject_type => 'user', subject_column => 'user_id')",
          # a share names a user, or a team's members
          "select authz.relate('app.shares', name => 'with_user', object_type => 'document', object_column => 'doc_id', relation => 'shared_with', subject_type => 'user', subject_column => 'user_id')",
          "select authz.relate('app.shares', name => 'with_team', object_type => 'document', object_column => 'doc_id', relation => 'shared_with', subject_type => 'team', subject_column => 'team', subject_relation => 'member')",
          # who may read and write what: only an org's admins manage its members, anyone who can
          # see a document can share it
          "select authz.protect('app.documents', type => 'document', id => 'id', read => 'viewer', write => 'viewer')",
          "select authz.protect('app.memberships', type => 'org', id => 'org_id', read => 'can_read', write => 'can_manage')",
          "select authz.protect('app.shares', type => 'document', id => 'doc_id', read => 'viewer', write => 'viewer')",
          # bootstrap, as a migration would: two orgs' documents, and org 1's first admin
          "insert into app.documents select i, 1 + (i > 4)::int, 'doc ' || i from generate_series(1, 6) i",
          "insert into app.memberships values (1, 'anne', 'admin')"
        ],
        do: Postgrex.query!(conn, sql, [])
  end

  defp steps(conn, stack) do
    delivered!(conn)
    [
      check("anne, org 1's admin, sees its documents", fn -> visible(conn, "anne") == [1, 2, 3, 4] end),
      check("bob sees nothing yet", fn -> visible(conn, "bob") == [] end),
      timed(conn, "anne adds bob to org 1: bob sees its documents", fn -> as(conn, "anne", "insert into app.memberships values (1, 'bob', 'member')") end,
            fn -> visible(conn, "bob") == [1, 2, 3, 4] end),
      check("bob, a member, cannot make himself an admin of org 2", fn ->
        match?({:error, _}, as(conn, "bob", "insert into app.memberships values (2, 'bob', 'admin')"))
      end),
      check("bob cannot share a document of org 2", fn ->
        match?({:error, _}, as(conn, "bob", "insert into app.shares (doc_id, user_id) values (5, 'bob')"))
      end),
      timed(conn, "bob shares document 2 with carl: carl sees it alone", fn -> as(conn, "bob", "insert into app.shares (doc_id, user_id) values (2, 'carl')") end,
            fn -> visible(conn, "carl") == [2] end),
      timed(conn, "a team: dana joins eng, document 3 shared with eng", fn ->
        Postgrex.query!(conn, "insert into app.team_members values ('eng', 'dana')", [])
        as(conn, "anne", "insert into app.shares (doc_id, team) values (3, 'eng')")
      end, fn -> visible(conn, "dana") == [3] end),
      timed(conn, "anne removes bob: he sees nothing", fn -> as(conn, "anne", "delete from app.memberships where user_id = 'bob'") end,
            fn -> visible(conn, "bob") == [] end),
      outage(conn, stack),
      check("the outbox is empty", fn -> delivered?(conn) end)
    ]
  end

  # The engine down while a grant is written (by an administrator: an application's own write is
  # checked with the engine, and fails closed while it is down): the change waits in the outbox,
  # and reaches the engine once it is back. A change the engine refuses (no such relation) is set
  # aside, and does not hold up the ones after it.
  defp outage(conn, stack) do
    {_, 0} = System.cmd("docker", ["pause", stack.engine.container])
    Postgrex.query!(conn, "insert into app.memberships values (1, 'erin', 'member'), (1, 'fred', 'owner')", [])
    Process.sleep(2000)
    waiting = not delivered?(conn)
    {_, 0} = System.cmd("docker", ["unpause", stack.engine.container])
    arrived = eventually(fn -> visible(conn, "erin") == [1, 2, 3, 4] end)
    set_aside = eventually(fn -> Postgrex.query!(conn, "select count(*) from authz.outbox where error is not null", []).rows == [[1]] end)
    Postgrex.query!(conn, "delete from authz.outbox where error is not null", [])
    {"engine down: erin's grant waits, then arrives; an invalid one is set aside",
     outcome(waiting and arrived and set_aside, "waited in the outbox, delivered; fred's 'owner' set aside")}
  end

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

  # what `user` sees, as the application's role
  defp visible(conn, user) do
    {:ok, ids} =
      Postgrex.transaction(conn, fn c ->
        Postgrex.query!(c, "set local pg_auth.subject_id = '#{user}'", [])
        Postgrex.query!(c, "set local role app_user", [])
        Postgrex.query!(c, "select id from app.documents order by id", []).rows |> List.flatten()
      end)
    ids
  end

  # a write by `user`, as the application's role; {:error, _} when RLS refuses it
  defp as(conn, user, sql) do
    Postgrex.transaction(conn, fn c ->
      Postgrex.query!(c, "set local pg_auth.subject_id = '#{user}'", [])
      Postgrex.query!(c, "set local role app_user", [])
      case Postgrex.query(c, sql, []) do
        {:ok, r} -> r
        {:error, e} -> Postgrex.rollback(c, e)
      end
    end)
  end
end

Relationships.main(System.argv())
