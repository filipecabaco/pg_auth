#!/usr/bin/env elixir
# The permission index across sidecar restarts, on real OpenFGA and SpiceDB. Every sidecar's change
# feed keeps the position it read the engine's changes to (authz_internal.feed_positions), and a
# feed starting resumes from it: a restart, or a second sidecar starting, keeps the sets when
# nothing changed, and a change made while no sidecar ran is caught on start. Broad subjects are
# made with a list limit of 1. See compare/README.md.
#
#   elixir compare/restarts.exs                     # openfga, spicedb
#   elixir compare/restarts.exs --engine spicedb --no-build

Mix.install([{:req, "~> 0.7"}, {:postgrex, "~> 0.22"}], config: [postgrex: [json_library: JSON]])

dir = Path.join(__DIR__, "lib")
for f <- ~w(engine docker engines/openfga engines/spicedb engines/cerbos stack), do: Code.require_file(Path.join(dir, f <> ".exs"))

defmodule Restarts do
  alias Compare.{Docker, Stack}

  @rules Path.expand("../docker/demo/rules", __DIR__)
  @files %{"openfga" => {"model", "openfga.fga"}, "spicedb" => {"schema", "spicedb.zed"}}
  @bare "model\n  schema 1.1\ntype user\n"
  @deadline_ms 30_000
  # long enough for a starting sidecar's feed to begin and, before positions were kept, to wipe
  @settle_ms 3_000

  def main(argv) do
    {opts, _} = OptionParser.parse!(argv, strict: [engine: :string, build: :boolean])
    if Keyword.get(opts, :build, true), do: Stack.build(Path.expand("..", __DIR__))
    engines = if e = opts[:engine], do: [e], else: ["openfga", "spicedb"]
    Process.flag(:trap_exit, true)
    results = for e <- engines, do: {e, run(e)}
    for {e, steps} <- results, {step, outcome} <- steps, do: IO.puts("#{String.pad_trailing(e, 8)} #{String.pad_trailing(step, 66)} #{outcome}")
    ok = Enum.all?(results, fn {_, steps} -> Enum.all?(steps, fn {_, o} -> not String.starts_with?(o, "FAILED") end) end)
    IO.puts(if ok, do: "All steps passed.", else: "Failures.")
    if not ok, do: System.halt(1)
  end

  defp run(engine) do
    stack = Stack.new("restarts", "pg_auth_compare=1")

    try do
      env = [{"PG_AUTH_DATABASE_URL", "postgres://sidecar:restarts@#{stack.id}-postgres:5432/postgres"}]
      stack = Stack.start_engine(stack, @bare, [default: 1], engine: engine, sidecar_cache_ms: nil, sidecar_env: env)
      conn = start_postgres(stack)
      setup(conn, engine)
      steps(conn, engine, stack)
    catch
      kind, reason ->
        for {c, log} <- Stack.logs(stack), do: IO.puts(:stderr, "---- #{c}\n#{String.slice(log, -3000, 3000)}")
        [{"run", "FAILED: " <> Exception.format(kind, reason, __STACKTRACE__)}]
    after
      Stack.stop(stack)
    end
  end

  # carl, org 2's admin, sees documents 4 and 5: more than the list limit, so his set is filled
  defp steps(conn, engine, stack) do
    [
      check("carl's set is filled", fn -> fill(conn) end),
      check("a second sidecar starting keeps it", fn ->
        Stack.start_sidecars(stack, [second: 1])
        Process.sleep(@settle_ms)
        fresh?(conn) and visible(conn, "carl") == [4, 5]
      end),
      check("a sidecar restarted keeps it when nothing changed", fn ->
        restart(stack, fn -> :ok end)
        Process.sleep(@settle_ms)
        fresh?(conn) and visible(conn, "carl") == [4, 5]
      end),
      check("a change made while no sidecar ran is caught on start", fn ->
        restart(stack, fn ->
          :ok = stack.engine_mod.delete_tuples(stack.engine, [%{object: "org:2", relation: "admin", user: "user:carl", condition: nil}])
        end)
        # the set kept from before would still show 4 and 5 (carl's next read may fill it again,
        # with nothing in it)
        eventually(fn -> visible(conn, "carl") == [] end)
      end),
      check("a position #{engine} does not serve wipes the sets on start", fn ->
        :ok = engine_write(stack, engine, %{object: "org:2", relation: "admin", user: "user:carl"})
        fill(conn) and
          (restart(stack, fn ->
             Postgrex.query!(conn, "update authz_internal.feed_positions set position = $1", [garbage(engine)])
           end) && eventually(fn -> not fresh?(conn) end))
      end)
    ]
  end

  defp garbage("openfga"), do: "token:nope"
  defp garbage("spicedb"), do: ~s({"token":"nope"})

  # carl's set fresh, and the feed's position kept after it was filled: the feed has read every
  # change before the fill (the setup's own writes included, which mark it stale when the feed
  # reads them; carl's next read fills it again)
  defp fill(conn) do
    eventually(fn ->
      visible(conn, "carl") == [4, 5] and fresh?(conn) and
        Postgrex.query!(conn, """
          select exists (select 1 from authz_internal.feed_positions f, authz_internal.permission_sets s
                         where s.subject = 'user:carl' and s.status = 'fresh' and f.at > s.filled_at + interval '1 second')
        """, []).rows == [[true]]
    end)
  end

  defp fresh?(conn) do
    Postgrex.query!(conn, "select count(*) from authz_internal.permission_sets where subject = 'user:carl' and status = 'fresh'", []).rows ==
      [[1]]
  end

  # stop every sidecar, do `meanwhile`, and start one on the socket Postgres asks
  defp restart(stack, meanwhile) do
    {out, 0} = System.cmd("docker", ["ps", "-q", "--filter", "name=#{stack.id}-sidecar"])
    for c <- String.split(out), do: System.cmd("docker", ["stop", "-t", "0", c], stderr_to_stdout: true)
    meanwhile.()
    Stack.start_sidecars(stack, [default: 1])
  end

  # OpenFGA against its latest model (the one the rules pushed), not the harness's bare one
  defp engine_write(stack, "openfga", t) do
    %{status: 200} = Req.post!(stack.engine.req, url: "/stores/#{stack.engine.store}/write", json: %{writes: %{tuple_keys: [t]}})
    :ok
  end

  defp engine_write(stack, _spicedb, t) do
    {:ok, []} = stack.engine_mod.write_tuples(stack.engine, [Map.put(t, :condition, nil)])
    :ok
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

    {name, file} = @files[engine]
    Postgrex.query!(conn, "select authz.set_rules($1, $2, $3)", [engine, name, File.read!(Path.join(@rules, file))])
    eventually(fn -> Postgrex.query!(conn, "select count(*) from authz.rules() where pushed_at is null", []).rows == [[0]] end) ||
      raise("rules not pushed")
    eventually(fn -> Postgrex.query!(conn, "select count(*) from authz_internal.outbox where not_before is null", []).rows == [[0]] end) ||
      raise("relationships not delivered")
  end

  defp start_postgres(stack) do
    pg = Stack.track(stack, "#{stack.id}-postgres")
    {^pg, port} =
      Docker.run(pg, Stack.pg_image(), 5432, ["-c", "fsync=off"],
        Stack.pg_docker_args(stack) ++ Docker.coverage_args("postgres") ++ ["-e", "POSTGRES_PASSWORD=restarts"])
    Stack.wait(fn -> match?({_, 0}, System.cmd("docker", ["exec", pg, "pg_isready", "-h", "127.0.0.1", "-U", "postgres"])) end, pg)
    {:ok, conn} =
      retry(fn -> Postgrex.start_link(hostname: "127.0.0.1", port: port, username: "postgres", password: "restarts", database: "postgres", pool_size: 1) end, 20)
    run_all(conn, [
      "create extension pg_auth",
      "alter system set pg_auth.sidecar_socket = '#{Stack.socket("default")}'",
      "select pg_reload_conf()",
      "create role authenticated nologin",
      "create role sidecar login password 'restarts' in role authz_sidecar",
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
      true -> Process.sleep(100) && eventually(f, waited + 100)
    end
  end

  # what `user` sees, the way PostgREST asks: its claims, the authenticated role
  defp visible(conn, user) do
    {:ok, rows} =
      Postgrex.transaction(conn, fn c ->
        Postgrex.query!(c, "select set_config('request.jwt.claims', $1, true)", [JSON.encode!(%{sub: user, role: "authenticated"})])
        Postgrex.query!(c, "set local role authenticated", [])
        case Postgrex.query(c, "select id from documents order by id", []) do
          {:ok, r} -> List.flatten(r.rows)
          {:error, e} -> Postgrex.rollback(c, e)
        end
      end)

    rows
  rescue
    _ -> :error
  end
end

Restarts.main(System.argv())
