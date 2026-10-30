#!/usr/bin/env elixir
# The demo's rules files (docker/demo/rules), loaded as they are into a real OpenFGA, SpiceDB and
# Cerbos: authz.set_rules stores each file, its notes (`# pg_auth: ...`) protect and relate the
# tables, the sidecar pushes the file, and the demo's story holds through row level security.
# Nothing is protected or related by hand. Then that the engine is what decides: the policies hold
# nothing but the call to pg_auth, rows follow the engine when it is changed behind Postgres's back
# (and not Postgres's own tables), and with the engine or the sidecar stopped nothing gets through
# past the sidecar's 1 s answers. See compare/README.md.
#
#   elixir compare/notes.exs                       # openfga, spicedb, cerbos
#   elixir compare/notes.exs --engine spicedb --no-build

Mix.install([{:req, "~> 0.7"}, {:postgrex, "~> 0.22"}], config: [postgrex: [json_library: JSON]])

dir = Path.join(__DIR__, "lib")
for f <- ~w(engine docker engines/openfga engines/spicedb engines/cerbos stack), do: Code.require_file(Path.join(dir, f <> ".exs"))

defmodule Notes do
  alias Compare.{Docker, Stack}

  @rules Path.expand("../docker/demo/rules", __DIR__)
  @files %{"openfga" => {"model", "openfga.fga"}, "spicedb" => {"schema", "spicedb.zed"}, "cerbos" => {"resource/document", "cerbos.yaml"}}
  @bare "model\n  schema 1.1\ntype user\n"
  @deadline_ms 30_000

  def main(argv) do
    {opts, _} = OptionParser.parse!(argv, strict: [engine: :string, build: :boolean])
    if Keyword.get(opts, :build, true), do: Stack.build(Path.expand("..", __DIR__))
    engines = if e = opts[:engine], do: [e], else: ["openfga", "spicedb", "cerbos"]
    Process.flag(:trap_exit, true)
    results = for e <- engines, do: {e, run(e)}
    for {e, steps} <- results, {step, outcome} <- steps, do: IO.puts("#{String.pad_trailing(e, 8)} #{String.pad_trailing(step, 62)} #{outcome}")
    ok = Enum.all?(results, fn {_, steps} -> Enum.all?(steps, fn {_, o} -> not String.starts_with?(o, "FAILED") end) end)
    IO.puts(if ok, do: "All steps passed.", else: "Failures.")
    if not ok, do: System.halt(1)
  end

  defp run(engine) do
    stack = Stack.new("notes", "pg_auth_compare=1")

    try do
      # the sidecar as an ordinary login holding only the authz_sidecar role
      url = "postgres://sidecar:notes@#{stack.id}-postgres:5432/postgres"
      env = [{"PG_AUTH_DATABASE_URL", url}]

      stack =
        if engine == "cerbos" do
          args = ["server", "--set=storage.driver=sqlite3", "--set=storage.sqlite3.dsn=:memory:", "--set=server.adminAPI.enabled=true"]
          Stack.start_engine(stack, "", [default: 1000], engine: "cerbos", cerbos_args: args, sidecar_cache_ms: nil,
            sidecar_env: env ++ [{"CERBOS_ADMIN_USER", "cerbos"}, {"CERBOS_ADMIN_PASSWORD", "cerbosAdmin"}])
        else
          Stack.start_engine(stack, @bare, [default: 1000], engine: engine, sidecar_cache_ms: nil, sidecar_env: env)
        end

      conn = start_postgres(stack)
      setup(conn, engine)
      steps(conn, engine) ++ engine_decides(conn, engine, stack)
    catch
      kind, reason ->
        for {c, log} <- Stack.logs(stack), do: IO.puts(:stderr, "---- #{c}\n#{String.slice(log, -3000, 3000)}")
        [{"run", "FAILED: " <> Exception.format(kind, reason, __STACKTRACE__)}]
    after
      Stack.stop(stack)
    end
  end

  # the demo's tables and data (docker/demo/sql), then the engine's file as it is
  defp setup(conn, "cerbos") do
    run_all(conn, [
      "create table documents (id int primary key, title text not null, owner text not null, status text not null default 'draft')",
      "grant select, insert, update, delete on documents to authenticated",
      "insert into documents values (1, 'Roadmap', 'anne', 'published'), (2, 'Budget', 'anne', 'draft'),
         (3, 'Launch plan', 'anne', 'draft'), (4, 'Hiring', 'carl', 'published'), (5, 'Pricing', 'carl', 'draft')"
    ])
    store_rules(conn, "cerbos")
  end

  defp setup(conn, engine) do
    run_all(conn, [
      "create table documents (id int primary key, org_id int not null, title text not null)",
      "create table memberships (org_id int not null, user_id text not null, role text not null, primary key (org_id, user_id))",
      "create table shares (doc_id int not null, user_id text not null, expires_at timestamptz, primary key (doc_id, user_id))",
      "grant select, insert, update, delete on documents, memberships, shares to authenticated",
      "insert into documents values (1, 1, 'Roadmap'), (2, 1, 'Budget'), (3, 1, 'Launch plan'), (4, 2, 'Hiring'), (5, 2, 'Pricing')",
      "insert into memberships values (1, 'anne', 'admin'), (2, 'carl', 'admin')"
    ])
    store_rules(conn, engine)
  end

  defp store_rules(conn, engine) do
    {name, file} = @files[engine]
    Postgrex.query!(conn, "select authz.set_rules($1, $2, $3)", [engine, name, File.read!(Path.join(@rules, file))])
    eventually(fn -> Postgrex.query!(conn, "select count(*) from authz.rules() where pushed_at is null", []).rows == [[0]] end) ||
      raise("rules not pushed")
    eventually(fn -> Postgrex.query!(conn, "select count(*) from authz_internal.outbox where not_before is null", []).rows == [[0]] end) ||
      raise("relationships not delivered")
  end

  defp steps(conn, "cerbos") do
    [
      check("the note protected the table: pg_auth's policies on it", fn -> policies(conn) == ["documents pg_auth_delete", "documents pg_auth_insert", "documents pg_auth_select", "documents pg_auth_update"] end),
      check("anne sees her documents and the published ones", fn -> eventually(fn -> visible(conn, "anne") == [1, 2, 3, 4] end) end),
      check("bob sees only the published ones", fn -> visible(conn, "bob") == [1, 4] end),
      check("anne publishes a draft: bob sees it", fn ->
        as(conn, "anne", "update documents set status = 'published' where id = 2") == 1 and eventually(fn -> visible(conn, "bob") == [1, 2, 4] end)
      end),
      check("bob cannot edit anne's document: 0 rows", fn -> as(conn, "bob", "update documents set title = 'Mine' where id = 1") == 0 end)
    ]
  end

  defp steps(conn, _relationships) do
    [
      check("the notes protected the three tables", fn ->
        policies(conn) |> Enum.map(&hd(String.split(&1))) |> Enum.uniq() == ["documents", "memberships", "shares"]
      end),
      check("anne, org 1's admin, sees its documents; bob sees none", fn ->
        eventually(fn -> visible(conn, "anne") == [1, 2, 3] end) and visible(conn, "bob") == []
      end),
      check("anne adds bob to org 1: bob sees its documents", fn ->
        as(conn, "anne", "insert into memberships values (1, 'bob', 'member')") == 1 and eventually(fn -> visible(conn, "bob") == [1, 2, 3] end)
      end),
      check("bob, a member, cannot add members", fn -> refused?(conn, "bob", "insert into memberships values (1, 'dan', 'member')") end),
      check("a role the model has no relation for fails at once", fn ->
        match?({:error, %Postgrex.Error{postgres: %{code: :check_violation}}},
               as_result(conn, "anne", "update memberships set role = 'owner' where user_id = 'bob'"))
      end),
      check("carl shares document 4 with bob: bob sees it", fn ->
        as(conn, "carl", "insert into shares values (4, 'bob', null)") == 1 and eventually(fn -> visible(conn, "bob") == [1, 2, 3, 4] end)
      end),
      check("a share with an end shows until then and goes after", fn ->
        as(conn, "carl", "insert into shares values (5, 'bob', now() + interval '4 seconds')") == 1 and
          eventually(fn -> visible(conn, "bob") == [1, 2, 3, 4, 5] end) and
          eventually(fn -> visible(conn, "bob") == [1, 2, 3, 4] end)
      end)
    ]
  end

  # Each of these fails if Postgres decided on its own: the engine is changed behind Postgres's
  # back and the rows follow it, the policies hold nothing but the call to pg_auth, and with the
  # sidecar gone nothing gets through.
  defp engine_decides(conn, engine, stack) do
    [check("the policies hold no rule of their own, only the call to pg_auth", fn -> policies_only_ask(conn) end)] ++
      engine_changes(conn, engine, stack) ++
      [
        check("the engine stopped: past the sidecar's 1 s answers, every read fails", fn ->
          System.cmd("docker", ["stop", "-t", "0", stack.engine.container], stderr_to_stdout: true)
          Process.sleep(1100)
          results = for u <- ~w(anne bob carl dan), do: as_result(conn, u, "select id from documents")
          Enum.all?(results, &match?({:error, %Postgrex.Error{}}, &1)) or raise(inspect(Enum.map(results, &elem(&1, 0))))
        end),
        # an answer the sidecar gave is kept by the backend for the time the sidecar says it holds
        # (its own 1 s cache); past it every statement has to ask, and fails
        check("the sidecar stopped: past its 1 s answers, every read fails", fn ->
          System.cmd("docker", ["stop", "-t", "0", sidecar(stack)], stderr_to_stdout: true)
          Process.sleep(1100)
          Enum.all?(~w(anne bob carl dan), &match?({:error, %Postgrex.Error{postgres: %{code: :connection_failure}}}, as_result(conn, &1, "select id from documents")))
        end),
        check("the sidecar stopped: writing fails too", fn ->
          match?({:error, %Postgrex.Error{postgres: %{code: :connection_failure}}}, as_result(conn, "carl", "update documents set title = title where id = 4"))
        end)
      ]
  end

  defp engine_changes(conn, "cerbos", stack) do
    owner_only = %{
      apiVersion: "api.cerbos.dev/v1",
      resourcePolicy: %{version: "default", resource: "document", rules: [
        %{actions: ["view", "edit"], effect: "EFFECT_ALLOW", roles: ["authenticated"],
          condition: %{match: %{expr: "request.resource.attr.owner == request.principal.id"}}}
      ]}
    }

    [
      check("a policy put straight into Cerbos: bob loses the published ones", fn ->
        %{status: 200} = Req.put!(stack.engine.req, url: "/admin/policy", auth: {:basic, "cerbos:cerbosAdmin"}, json: %{policies: [owner_only]})
        # each subject's earlier answer holds for up to the sidecar's 1 s
        eventually(fn -> visible(conn, "bob") == [] and visible(conn, "anne") == [1, 2, 3] and visible(conn, "carl") == [4, 5] end)
      end)
    ]
  end

  defp engine_changes(conn, engine, stack) do
    dan = %{object: "document:5", relation: "shared_with", user: "user:dan", condition: nil}
    anne = %{object: "org:1", relation: "admin", user: "user:anne", condition: nil}

    [
      check("a relationship written straight into #{engine}: dan sees document 5", fn ->
        nothing_in_postgres = Postgrex.query!(conn, "select count(*) from shares where user_id = 'dan'", []).rows == [[0]]
        :ok = engine_write(stack, engine, [dan])
        nothing_in_postgres and eventually(fn -> visible(conn, "dan") == [5] end)
      end),
      check("a relationship deleted straight from #{engine}: anne sees none of org 1", fn ->
        still_in_postgres = Postgrex.query!(conn, "select role from memberships where org_id = 1 and user_id = 'anne'", []).rows == [["admin"]]
        :ok = stack.engine_mod.delete_tuples(stack.engine, [anne])
        still_in_postgres and eventually(fn -> visible(conn, "anne") == [] end)
      end)
    ]
  end

  # OpenFGA against its latest model (the one the rules pushed), not the harness's bare one
  defp engine_write(stack, "openfga", tuples) do
    body = %{writes: %{tuple_keys: Enum.map(tuples, &Map.take(&1, [:object, :relation, :user]))}}
    %{status: 200} = Req.post!(stack.engine.req, url: "/stores/#{stack.engine.store}/write", json: body)
    :ok
  end

  defp engine_write(stack, _spicedb, tuples) do
    {:ok, []} = stack.engine_mod.write_tuples(stack.engine, tuples)
    :ok
  end

  defp policies_only_ask(conn) do
    rows = Postgrex.query!(conn, "select coalesce(qual, '') || ' ' || coalesce(with_check, '') from pg_policies where policyname like 'pg_auth%'", []).rows
    rows != [] and
      Enum.all?(rows, fn [expr] -> expr =~ "authz." and not (expr =~ ~r/memberships|shares|select|\s(=|<>|in|and|or)\s/i) end)
  end

  defp sidecar(stack) do
    {out, 0} = System.cmd("docker", ["ps", "-q", "--filter", "name=#{stack.id}-sidecar"])
    String.trim(out)
  end

  defp policies(conn) do
    Postgrex.query!(conn, "select tablename || ' ' || policyname from pg_policies where policyname like 'pg_auth%' order by 1", []).rows
    |> List.flatten()
  end

  defp start_postgres(stack) do
    pg = Stack.track(stack, "#{stack.id}-postgres")
    {^pg, port} =
      Docker.run(pg, Stack.pg_image(), 5432, ["-c", "fsync=off"],
        Stack.pg_docker_args(stack) ++ Docker.coverage_args("postgres") ++ ["-e", "POSTGRES_PASSWORD=notes"])
    Stack.wait(fn -> match?({_, 0}, System.cmd("docker", ["exec", pg, "pg_isready", "-h", "127.0.0.1", "-U", "postgres"])) end, pg)
    {:ok, conn} =
      retry(fn -> Postgrex.start_link(hostname: "127.0.0.1", port: port, username: "postgres", password: "notes", database: "postgres", pool_size: 1) end, 20)
    run_all(conn, [
      "create extension pg_auth",
      "alter system set pg_auth.sidecar_socket = '#{Stack.socket("default")}'",
      "select pg_reload_conf()",
      "create role authenticated nologin",
      "create role sidecar login password 'notes' in role authz_sidecar",
      "select authz.configure(subject => 'jwt')"
    ])
    conn
  end

  defp retry(f, 0), do: f.()

  defp retry(f, n) do
    case f.() do
      {:ok, c} -> {:ok, c}
      _ -> Process.sleep(500) && retry(f, n - 1)
    end
  end

  defp run_all(conn, statements), do: Enum.each(statements, &Postgrex.query!(conn, &1, []))

  defp check(what, f) do
    {what, if(f.(), do: "ok", else: "FAILED")}
  rescue
    e -> {what, "FAILED: " <> Exception.message(e)}
  end

  defp eventually(f, waited \\ 0) do
    cond do
      f.() -> true
      waited >= @deadline_ms -> false
      true -> Process.sleep(50) && eventually(f, waited + 50)
    end
  end

  # a statement as `user`, the way PostgREST runs one: its claims, the authenticated role
  defp as_result(conn, user, sql) do
    Postgrex.transaction(conn, fn c ->
      Postgrex.query!(c, "select set_config('request.jwt.claims', $1, true)", [JSON.encode!(%{sub: user, role: "authenticated"})])
      Postgrex.query!(c, "set local role authenticated", [])
      Postgrex.query(c, sql, []) |> case do
        {:ok, r} -> r
        {:error, e} -> Postgrex.rollback(c, e)
      end
    end)
  end

  defp as(conn, user, sql) do
    {:ok, r} = as_result(conn, user, sql)
    r.num_rows
  end

  defp refused?(conn, user, sql), do: match?({:error, %Postgrex.Error{}}, as_result(conn, user, sql))

  defp visible(conn, user) do
    {:ok, r} = as_result(conn, user, "select id from documents order by id")
    List.flatten(r.rows)
  end
end

Notes.main(System.argv())
