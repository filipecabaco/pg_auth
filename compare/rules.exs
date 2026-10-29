#!/usr/bin/env elixir
# The engine's rules kept in Postgres (authz.set_rules), end to end: each engine starts without
# them, and the rules' notes (`# pg_auth: protects ...`) are the whole setup: no table is protected
# or related by hand. The sidecar pushes what Postgres holds, rows show as those rules say, a new
# version changes what they show, and rules the engine refuses are reported while it keeps the
# version before. In each engine's own language: OpenFGA's DSL, a SpiceDB schema, Cerbos's YAML. See
# compare/README.md and docs/proposals/setting-rules.md.
#
#   elixir compare/rules.exs                       # openfga, spicedb, cerbos
#   elixir compare/rules.exs --engine spicedb --no-build

Mix.install([{:req, "~> 0.7"}, {:postgrex, "~> 0.22"}], config: [postgrex: [json_library: JSON]])

dir = Path.join(__DIR__, "lib")
for f <- ~w(engine docker engines/openfga engines/spicedb engines/cerbos stack), do: Code.require_file(Path.join(dir, f <> ".exs"))

defmodule Rules do
  alias Compare.{Docker, Stack}

  # the engine starts with this alone: the rules come from Postgres
  @bare "model\n  schema 1.1\ntype user\n"

  # members of an org read its documents (version 1), then only its admins (version 2), then rules
  # the engine refuses (version 3: what pg_auth checks is there, what only the engine checks is not)
  @rules %{
    "openfga" => [
      """
      model
        schema 1.1
      type user
      type org
        # pg_auth: rows of app.memberships (org_id, role, user_id)
        relations
          define admin: [user]
          define member: [user]
          define can_read: member or admin
      type document
        # pg_auth: protects app.documents (id): read viewer
        # pg_auth: org from app.documents.org_id
        relations
          define org: [org]
          define viewer: can_read from org
      """,
      """
      model
        schema 1.1
      type user
      type org
        # pg_auth: rows of app.memberships (org_id, role, user_id)
        relations
          define admin: [user]
          define member: [user]
          define can_read: admin
      type document
        # pg_auth: protects app.documents (id): read viewer
        # pg_auth: org from app.documents.org_id
        relations
          define org: [org]
          define viewer: can_read from org
      """,
      # names what the setup uses, but computes from a relation the type does not have
      """
      model
        schema 1.1
      type user
      type org
        # pg_auth: rows of app.memberships (org_id, role, user_id)
        relations
          define admin: [user]
          define member: [user]
          define can_read: admin or ghost
      type document
        # pg_auth: protects app.documents (id): read viewer
        # pg_auth: org from app.documents.org_id
        relations
          define org: [org]
          define viewer: can_read from org
      """
    ],
    "spicedb" => [
      """
      definition user {}
      definition org {
        // pg_auth: rows of app.memberships (org_id, role, user_id)
        relation admin: user
        relation member: user
        permission can_read = member + admin
      }
      definition document {
        // pg_auth: protects app.documents (id): read viewer
        // pg_auth: org from app.documents.org_id
        relation org: org
        permission viewer = org->can_read
      }
      """,
      """
      definition user {}
      definition org {
        // pg_auth: rows of app.memberships (org_id, role, user_id)
        relation admin: user
        relation member: user
        permission can_read = admin
      }
      definition document {
        // pg_auth: protects app.documents (id): read viewer
        // pg_auth: org from app.documents.org_id
        relation org: org
        permission viewer = org->can_read
      }
      """,
      """
      definition user {}
      definition org {
        // pg_auth: rows of app.memberships (org_id, role, user_id)
        relation admin: user
        relation member: user
        permission can_read = admin + ghost
      }
      definition document {
        // pg_auth: protects app.documents (id): read viewer
        // pg_auth: org from app.documents.org_id
        relation org: org
        permission viewer = org->can_read
      }
      """
    ],
    "cerbos" => [
      """
      # pg_auth: protects app.documents (id): read view, attributes (owner, status)
      apiVersion: api.cerbos.dev/v1
      resourcePolicy:
        version: default
        resource: document
        rules:
          - actions: ["view"]
            effect: EFFECT_ALLOW
            roles: ["user"]
            condition:
              match:
                expr: request.resource.attr.owner == request.principal.id
      """,
      """
      # pg_auth: protects app.documents (id): read view, attributes (owner, status)
      apiVersion: api.cerbos.dev/v1
      resourcePolicy:
        version: default
        resource: document
        rules:
          - actions: ["view"]
            effect: EFFECT_ALLOW
            roles: ["user"]
            condition:
              match:
                any:
                  of:
                    - expr: request.resource.attr.owner == request.principal.id
                    - expr: request.resource.attr.status == "published"
      """,
      # a condition that does not compile
      """
      # pg_auth: protects app.documents (id): read view, attributes (owner, status)
      apiVersion: api.cerbos.dev/v1
      resourcePolicy:
        version: default
        resource: document
        rules:
          - actions: ["view"]
            effect: EFFECT_ALLOW
            roles: ["user"]
            condition:
              match:
                expr: request.resource.attr.owner ==
      """
    ]
  }

  @deadline_ms 30_000

  def main(argv) do
    {opts, _} = OptionParser.parse!(argv, strict: [engine: :string, build: :boolean])
    root = Path.expand("..", __DIR__)
    if Keyword.get(opts, :build, true), do: Stack.build(root)
    engines = if e = opts[:engine], do: [e], else: ["openfga", "spicedb", "cerbos"]
    Process.flag(:trap_exit, true)
    results = for e <- engines, do: {e, run(e)}
    for {e, steps} <- results, {step, outcome} <- steps, do: IO.puts("#{String.pad_trailing(e, 8)} #{String.pad_trailing(step, 64)} #{outcome}")
    ok = Enum.all?(results, fn {_, steps} -> Enum.all?(steps, fn {_, o} -> not String.starts_with?(o, "FAILED") end) end)
    IO.puts(if ok, do: "All steps passed.", else: "Failures.")
    if not ok, do: System.halt(1)
  end

  defp run(engine) do
    stack = Stack.new("rules", "pg_auth_compare=1")

    try do
      url = "postgres://postgres:rules@#{stack.id}-postgres:5432/postgres"
      env = [{"PG_AUTH_DATABASE_URL", url}]

      stack =
        if engine == "cerbos" do
          # policies stored through the Admin API, in a database: Cerbos's default admin credentials
          args = ["server", "--set=storage.driver=sqlite3", "--set=storage.sqlite3.dsn=:memory:", "--set=server.adminAPI.enabled=true"]
          Stack.start_engine(stack, "", [default: 1000], engine: "cerbos", cerbos_args: args, sidecar_cache_ms: nil,
            sidecar_env: env ++ [{"CERBOS_ADMIN_USER", "cerbos"}, {"CERBOS_ADMIN_PASSWORD", "cerbosAdmin"}])
        else
          Stack.start_engine(stack, @bare, [default: 1000], engine: engine, sidecar_cache_ms: nil, sidecar_env: env)
        end

      conn = start_postgres(stack)
      steps(conn, engine)
    catch
      kind, reason ->
        for {c, log} <- Stack.logs(stack), do: IO.puts(:stderr, "---- #{c}\n#{String.slice(log, -3000, 3000)}")
        [{"run", "FAILED: " <> Exception.format(kind, reason, __STACKTRACE__)}]
    after
      Stack.stop(stack)
    end
  end

  defp steps(conn, engine) do
    [v1, v2, refused] = @rules[engine]
    name = %{"openfga" => "model", "spicedb" => "schema", "cerbos" => "resource/document"}[engine]
    setup(conn, engine)
    # the notes protect and relate the tables; their rows reach the engine once it has the rules
    first = set_rules(conn, engine, name, v1)
    pushed?(conn, engine, 1) || raise("version 1 was not pushed")
    eventually(fn -> Postgrex.query!(conn, "select count(*) from authz.outbox", []).rows == [[0]] end) || raise("relationships not delivered")
    seen = if engine == "cerbos", do: [1, 2], else: [1, 2, 3]

    [
      check("version 1 stored and pushed to the engine", fn -> first == 1 and pushed?(conn, engine, 1) end),
      check("anne sees what version 1 allows", fn -> eventually(fn -> visible(conn, "anne") == seen end) end),
      check("the same text again is not a new version", fn -> set_rules(conn, engine, name, v1) == 1 end),
      check("version 2 pushed, and what anne sees follows it", fn ->
        2 = set_rules(conn, engine, name, v2)
        after_v2 = if engine == "cerbos", do: [1, 2, 3], else: []
        pushed?(conn, engine, 2) and eventually(fn -> visible(conn, "anne") == after_v2 end)
      end),
      check("rules the engine refuses are reported, and it keeps version 2", fn ->
        3 = set_rules(conn, engine, name, refused)
        after_v2 = if engine == "cerbos", do: [1, 2, 3], else: []
        eventually(fn -> refused?(conn, engine) end) and doctor_fails?(conn) and visible(conn, "anne") == after_v2
      end),
      check("a note naming what the rules lack fails at commit", fn ->
        typo = String.replace(v2, ~r/read (viewer|view)\b/, "read vewer")
        sql = "select authz.set_rules($1, $2, $3)"
        match?({:error, %Postgrex.Error{postgres: %{code: :check_violation, message: "the setup names what the rules" <> _}}},
               Postgrex.query(conn, sql, [engine, name, typo]))
      end),
      check("every version is kept, the refused one with the engine's reason", fn ->
        Postgrex.query!(conn, "select count(*), count(error) from authz.rules where engine = $1", [engine]).rows == [[3, 1]]
      end)
    ]
  end

  # members of org 1 (anne) and its documents (version 1: members read them); Cerbos: owners
  defp setup(conn, "cerbos") do
    run_all(conn, [
      "select authz.configure(subject => 'jwt')",
      "create table app.documents (id int primary key, owner text not null, status text not null)",
      "grant select on app.documents to app_user",
      "insert into app.documents values (1, 'anne', 'draft'), (2, 'anne', 'draft'), (3, 'bob', 'published'), (4, 'bob', 'draft')"
    ])
  end

  defp setup(conn, _relationships) do
    run_all(conn, [
      "select authz.configure(subject => 'jwt')",
      "create table app.documents (id int primary key, org_id int not null)",
      "create table app.memberships (org_id int, user_id text, role text, primary key (org_id, user_id))",
      "grant select on app.documents to app_user",
      "insert into app.documents values (1, 1), (2, 1), (3, 1), (4, 2)",
      "insert into app.memberships values (1, 'anne', 'member'), (2, 'bob', 'admin')"
    ])
  end

  defp set_rules(conn, engine, name, body) do
    [[version]] = Postgrex.query!(conn, "select authz.set_rules($1, $2, $3)", [engine, name, body]).rows
    version
  end

  defp pushed?(conn, engine, version) do
    eventually(fn ->
      Postgrex.query!(conn, "select pushed_at is not null from authz.rules where engine = $1 and version = $2", [engine, version]).rows == [[true]]
    end)
  end

  defp refused?(conn, engine),
    do: Postgrex.query!(conn, "select error is not null from authz.rules($1)", [engine]).rows == [[true]]

  defp doctor_fails?(conn),
    do: Postgrex.query!(conn, "select status from authz.doctor() where check_name = 'rules'", []).rows == [["fail"]]

  defp start_postgres(stack) do
    pg = Stack.track(stack, "#{stack.id}-postgres")
    {^pg, port} =
      Docker.run(pg, Stack.pg_image(), 5432, ["-c", "fsync=off"],
        Stack.pg_docker_args(stack) ++ Docker.coverage_args("postgres") ++ ["-e", "POSTGRES_PASSWORD=rules"])
    Stack.wait(fn -> match?({_, 0}, System.cmd("docker", ["exec", pg, "pg_isready", "-h", "127.0.0.1", "-U", "postgres"])) end, pg)
    {:ok, conn} =
      retry(fn -> Postgrex.start_link(hostname: "127.0.0.1", port: port, username: "postgres", password: "rules", database: "postgres", pool_size: 1) end, 20)
    run_all(conn, ["create extension pg_auth", "alter system set pg_auth.sidecar_socket = '#{Stack.socket("default")}'", "select pg_reload_conf()",
                   "create role app_user nologin", "create schema app", "grant usage on schema app to app_user"])
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

  defp visible(conn, user) do
    {:ok, ids} =
      Postgrex.transaction(conn, fn c ->
        Postgrex.query!(c, "select set_config('request.jwt.claims', $1, true)", [JSON.encode!(%{sub: user, role: "user"})])
        Postgrex.query!(c, "set local role app_user", [])
        Postgrex.query!(c, "select id from app.documents order by id", []).rows |> List.flatten()
      end)
    ids
  end
end

Rules.main(System.argv())
