#!/usr/bin/env elixir
# Relationships that end (authz_internal.relate expires_column), end to end on a real OpenFGA and SpiceDB:
# a share shows until its end and not after (and how long after its end it took to go), an end
# moved later keeps it, one moved earlier ends it, a renewal brings it back, and the expiries the
# outbox holds are not changes waiting. SpiceDB also gets the end as the relationship's
# expiration: the engine ends it itself with the sidecar stopped, within its revision window. The rules come from Postgres
# (authz.set_rules). See compare/README.md and docs/usage.md.
#
#   elixir compare/expiry.exs                       # openfga, then spicedb
#   elixir compare/expiry.exs --engine spicedb --no-build

Mix.install([{:req, "~> 0.7"}, {:postgrex, "~> 0.22"}], config: [postgrex: [json_library: JSON]])

dir = Path.join(__DIR__, "lib")
for f <- ~w(engine docker engines/openfga engines/spicedb engines/cerbos stack), do: Code.require_file(Path.join(dir, f <> ".exs"))

defmodule Expiry do
  alias Compare.{Docker, Stack}

  @bare "model\n  schema 1.1\ntype user\n"

  @rules %{
    "openfga" => {"model", "model\n  schema 1.1\ntype user\ntype doc\n  relations\n    define viewer: [user]\n"},
    "spicedb" => {"schema", "use expiration\n\ndefinition user {}\n\ndefinition doc {\n  relation viewer: user | user with expiration\n}\n"}
  }

  # how long a share lasts in these steps, and how long after its end it may still show
  @lasts_s 3
  @gone_within_ms 3_000
  # SpiceDB reads at a revision up to its quantization window old (5 s, plus 10%)
  @revision_window_ms 5_500
  @deadline_ms 30_000

  def main(argv) do
    {opts, _} = OptionParser.parse!(argv, strict: [engine: :string, build: :boolean])
    if Keyword.get(opts, :build, true), do: Stack.build(Path.expand("..", __DIR__))
    engines = if e = opts[:engine], do: [e], else: ["openfga", "spicedb"]
    Process.flag(:trap_exit, true)
    results = for e <- engines, do: {e, run(e)}
    for {e, steps} <- results, {step, outcome} <- steps, do: IO.puts("#{String.pad_trailing(e, 8)} #{String.pad_trailing(step, 60)} #{outcome}")
    ok = Enum.all?(results, fn {_, steps} -> Enum.all?(steps, fn {_, o} -> not String.starts_with?(o, "FAILED") end) end)
    IO.puts(if ok, do: "All steps passed.", else: "Failures.")
    if not ok, do: System.halt(1)
  end

  defp run(engine) do
    stack = Stack.new("expiry", "pg_auth_compare=1")

    try do
      url = "postgres://postgres:expiry@#{stack.id}-postgres:5432/postgres"
      stack = Stack.start_engine(stack, @bare, [default: 1000], engine: engine, sidecar_cache_ms: nil, sidecar_env: [{"PG_AUTH_DATABASE_URL", url}])
      conn = start_postgres(stack)
      setup(conn, engine)
      results = steps(conn, engine, stack)
      if Enum.any?(results, fn {_, o} -> String.starts_with?(o, "FAILED") end), do: diagnose(conn, stack)
      results
    catch
      kind, reason ->
        for {c, log} <- Stack.logs(stack), do: IO.puts(:stderr, "---- #{c}\n#{String.slice(log, -3000, 3000)}")
        [{"run", "FAILED: " <> Exception.format(kind, reason, __STACKTRACE__)}]
    after
      Stack.stop(stack)
    end
  end

  defp setup(conn, engine) do
    {name, rules} = @rules[engine]
    run_all(conn, [
      "select authz.configure(subject => 'jwt')",
      "create table app.docs (id int primary key)",
      "create table app.shares (doc_id int, user_id text, expires_at timestamptz, primary key (doc_id, user_id))",
      "grant select on app.docs to app_user",
      "insert into app.docs select generate_series(1, 6)"
    ])
    Postgrex.query!(conn, "select authz.set_rules($1, $2, $3)", [engine, name, rules])
    eventually(fn -> Postgrex.query!(conn, "select pushed_at is not null from authz.rules()", []).rows == [[true]] end) || raise("rules not pushed")
    run_all(conn, [
      "select authz_internal.relate('app.shares', object_type => 'doc', object_column => 'doc_id', relation => 'viewer',
                           subject_type => 'user', subject_column => 'user_id', expires_column => 'expires_at')",
      "select authz_internal.protect('app.docs', type => 'doc', id => 'id', read => 'viewer')"
    ])
  end

  defp steps(conn, engine, stack) do
    lasts = "now() + interval '#{@lasts_s} seconds'"

    [
      check("a share shows until its end; one already ended never does", fn ->
        share(conn, "(1, 'anne', #{lasts}), (2, 'anne', null), (3, 'anne', now() - interval '1 minute')")
        eventually(fn -> visible(conn) == [1, 2] end)
      end),
      timed("at its end it goes", @gone_within_ms, fn -> gone(conn, 1, [2]) end),
      check("an end moved later keeps it past the first", fn ->
        share(conn, "(4, 'anne', #{lasts})")
        eventually(fn -> visible(conn) == [2, 4] end)
        Postgrex.query!(conn, "update app.shares set expires_at = now() + interval '1 hour' where doc_id = 4", [])
        Process.sleep((@lasts_s + 1) * 1000 + @gone_within_ms)
        visible(conn) == [2, 4]
      end),
      check("an end moved earlier ends it now", fn ->
        Postgrex.query!(conn, "update app.shares set expires_at = now() where doc_id = 4", [])
        eventually(fn -> visible(conn) == [2] end)
      end),
      check("renewed after its end, it shows again", fn ->
        Postgrex.query!(conn, "update app.shares set expires_at = now() + interval '1 hour' where doc_id = 1", [])
        eventually(fn -> visible(conn) == [1, 2] end)
      end),
      check("expiries the outbox holds are not changes waiting", fn ->
        Postgrex.query!(conn, "select status || ': ' || detail from authz.doctor() where check_name = 'relationship tables'", []).rows ==
          [["ok: 1 declared, delivered, 1 expiries scheduled"]]
      end)
    ] ++ if(engine == "spicedb", do: [engine_ends_it(conn, stack)], else: [])
  end

  # with the sidecar paused, nothing deletes it: SpiceDB ends it at its expiration
  defp engine_ends_it(conn, stack) do
    timed("spicedb ends it itself, with the sidecar stopped", @revision_window_ms, fn ->
      share(conn, "(5, 'anne', #{"now() + interval '#{@lasts_s} seconds'"})")
      eventually(fn -> visible(conn) == [1, 2, 5] end)
      sidecars = stack |> Stack.logs() |> Map.keys() |> Enum.filter(&String.contains?(&1, "-sidecar-"))
      for s <- sidecars, do: {_, 0} = System.cmd("docker", ["pause", s])
      allowed = fn -> stack.engine_mod.check(stack.engine, %{object: "doc:5", relation: "viewer", subject: "user:anne"}) end
      granted = allowed.() == {:ok, true}
      [[ms]] = Postgrex.query!(conn, "select (extract(epoch from expires_at - clock_timestamp()) * 1000)::int from app.shares where doc_id = 5", []).rows
      Process.sleep(max(ms, 0))
      started = System.monotonic_time(:millisecond)
      ended = eventually(fn -> allowed.() == {:ok, false} end, 0, 20_000)
      after_ms = System.monotonic_time(:millisecond) - started
      for s <- sidecars, do: System.cmd("docker", ["unpause", s])
      granted and ended and after_ms
    end)
  end

  defp diagnose(conn, stack) do
    for q <- ["select engine, name, version, pushed_at, error from authz.rules()", "select * from authz_internal.outbox"],
        do: IO.puts(:stderr, "---- #{q}\n#{inspect(Postgrex.query!(conn, q, []).rows, limit: 20)}")
    for {c, log} <- Stack.logs(stack), String.contains?(c, "sidecar"), do: IO.puts(:stderr, "---- #{c}\n#{String.slice(log, -2500, 2500)}")
  end

  defp share(conn, values), do: Postgrex.query!(conn, "insert into app.shares values #{values}", [])

  # the moment its end came, then until it no longer shows: ms after its end
  defp gone(conn, doc, left) do
    [[ms]] = Postgrex.query!(conn, "select (extract(epoch from expires_at - clock_timestamp()) * 1000)::int from app.shares where doc_id = $1", [doc]).rows
    Process.sleep(max(ms, 0))
    started = System.monotonic_time(:millisecond)
    eventually(fn -> visible(conn) == left end) && System.monotonic_time(:millisecond) - started
  end

  defp start_postgres(stack) do
    pg = Stack.track(stack, "#{stack.id}-postgres")
    {^pg, port} =
      Docker.run(pg, Stack.pg_image(), 5432, ["-c", "fsync=off"],
        Stack.pg_docker_args(stack) ++ Docker.coverage_args("postgres") ++ ["-e", "POSTGRES_PASSWORD=expiry"])
    Stack.wait(fn -> match?({_, 0}, System.cmd("docker", ["exec", pg, "pg_isready", "-h", "127.0.0.1", "-U", "postgres"])) end, pg)
    {:ok, conn} =
      retry(fn -> Postgrex.start_link(hostname: "127.0.0.1", port: port, username: "postgres", password: "expiry", database: "postgres", pool_size: 1) end, 20)
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

  defp timed(what, within_ms, f) do
    case f.() do
      ms when is_integer(ms) and ms <= within_ms -> {what, "ok (#{ms} ms after its end)"}
      ms when is_integer(ms) -> {what, "FAILED (#{ms} ms after its end)"}
      _ -> {what, "FAILED"}
    end
  rescue
    e -> {what, "FAILED: " <> Exception.message(e)}
  end

  defp eventually(f, waited \\ 0, deadline \\ @deadline_ms) do
    cond do
      f.() -> true
      waited >= deadline -> false
      true -> Process.sleep(50) && eventually(f, waited + 50, deadline)
    end
  end

  defp visible(conn) do
    {:ok, ids} =
      Postgrex.transaction(conn, fn c ->
        Postgrex.query!(c, "select set_config('request.jwt.claims', $1, true)", [JSON.encode!(%{sub: "anne", role: "user"})])
        Postgrex.query!(c, "set local role app_user", [])
        Postgrex.query!(c, "select id from app.docs order by id", []).rows |> List.flatten()
      end)
    ids
  end
end

Expiry.main(System.argv())
