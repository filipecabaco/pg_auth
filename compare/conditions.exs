#!/usr/bin/env elixir
# OpenFGA conditions and SpiceDB caveats, end to end: relationships written with a condition and
# the row's values for it (authz.relate condition, condition_context), and each request giving it
# the rest (authz.configure context: a token's claim, the time asked). A share in some regions
# shows to a token from one of them; a share until a deadline shows until then; a row's new
# values change what shows; a request without a value a condition needs is not let in. Each step
# runs on a sidecar that lists and on one that checks rows (LIST_LIMIT 2). See compare/README.md.
#
#   elixir compare/conditions.exs                       # openfga, then spicedb
#   elixir compare/conditions.exs --engine spicedb --no-build

Mix.install([{:req, "~> 0.7"}, {:postgrex, "~> 0.22"}], config: [postgrex: [json_library: JSON]])

dir = Path.join(__DIR__, "lib")
for f <- ~w(engine docker engines/openfga engines/spicedb engines/cerbos stack), do: Code.require_file(Path.join(dir, f <> ".exs"))

defmodule Conditions do
  alias Compare.{Docker, Stack}

  @bare "model\n  schema 1.1\ntype user\n"

  @rules %{
    "openfga" => {"model", """
    model
      schema 1.1
    type user
    type doc
      relations
        define viewer: [user, user with in_region, user with before_deadline]
    condition in_region(region: string, allowed: list<string>) {
      region in allowed
    }
    condition before_deadline(current_time: timestamp, deadline: timestamp) {
      current_time < deadline
    }
    """},
    "spicedb" => {"schema", """
    caveat in_region(region string, allowed list<string>) {
      region in allowed
    }

    caveat before_deadline(current_time timestamp, deadline timestamp) {
      current_time < deadline
    }

    definition user {}

    definition doc {
      relation viewer: user | user with in_region | user with before_deadline
    }
    """}
  }

  # the sidecars: one lists, one has too many to list and checks rows
  @sidecars [list: 1000, check: 2]
  @lasts_s 6
  @deadline_ms 30_000

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
    stack = Stack.new("conditions", "pg_auth_compare=1")

    try do
      url = "postgres://postgres:conditions@#{stack.id}-postgres:5432/postgres"
      stack = Stack.start_engine(stack, @bare, @sidecars, engine: engine, sidecar_cache_ms: nil, sidecar_env: [{"PG_AUTH_DATABASE_URL", url}, {"REQUEST_LOG", System.get_env("REQUEST_LOG", "0")}])
      conn = start_postgres(stack)
      setup(conn, engine)
      results = steps(conn)
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
      ~s|select authz.configure(subject => 'jwt', context => '{"current_time": "now", "region": {"claim": "region"}}')|,
      "create table app.docs (id int primary key)",
      "create table app.region_shares (doc_id int, user_id text, regions text[], primary key (doc_id, user_id))",
      "create table app.deadline_shares (doc_id int, user_id text, deadline timestamptz, primary key (doc_id, user_id))",
      "create table app.shares (doc_id int, user_id text, primary key (doc_id, user_id))",
      "grant select on app.docs to app_user",
      "insert into app.docs select generate_series(1, 8)",
      "insert into app.region_shares values (1, 'anne', '{eu}'), (2, 'anne', '{us}')",
      "insert into app.deadline_shares values (3, 'anne', now() + interval '1 hour'), (4, 'anne', now() - interval '1 hour')",
      "insert into app.shares values (5, 'anne')"
    ])
    Postgrex.query!(conn, "select authz.set_rules($1, $2, $3)", [engine, name, rules])
    eventually(fn -> Postgrex.query!(conn, "select pushed_at is not null from authz.rules()", []).rows == [[true]] end) || raise("rules not pushed")
    run_all(conn, [
      ~s|select authz.relate('app.region_shares', object_type => 'doc', object_column => 'doc_id', relation => 'viewer',
                             subject_type => 'user', subject_column => 'user_id',
                             condition => 'in_region', condition_context => '{"allowed": "regions"}')|,
      ~s|select authz.relate('app.deadline_shares', object_type => 'doc', object_column => 'doc_id', relation => 'viewer',
                             subject_type => 'user', subject_column => 'user_id',
                             condition => 'before_deadline', condition_context => '{"deadline": "deadline"}')|,
      "select authz.relate('app.shares', object_type => 'doc', object_column => 'doc_id', relation => 'viewer',
                           subject_type => 'user', subject_column => 'user_id')",
      "select authz.protect('app.docs', type => 'doc', id => 'id', read => 'viewer')"
    ])
    eventually(fn -> Postgrex.query!(conn, "select count(*) from authz.outbox", []).rows == [[0]] end) || raise("relationships not delivered")
  end

  defp steps(conn) do
    eu = %{sub: "anne", role: "user", region: "eu"}
    us = %{eu | region: "us"}
    nowhere = Map.delete(eu, :region)

    [
      both("a token from eu sees the eu share, the share before its deadline, the plain one", fn s ->
        eventually(fn -> visible(conn, eu, s) == [1, 3, 5] end)
      end),
      both("a token from us sees the us share instead", fn s -> visible(conn, us, s) == [2, 3, 5] end),
      both("a token with no region is not let in by a share needing one", fn s -> visible(conn, nowhere, s) == [3, 5] end),
      both("the row's regions change: what eu sees follows", fn s ->
        if s == :list, do: Postgrex.query!(conn, "update app.region_shares set regions = '{eu,us}' where doc_id = 2", [])
        eventually(fn -> visible(conn, eu, s) == [1, 2, 3, 5] end)
      end),
      timed("a deadline passes: the share goes", fn ->
        Postgrex.query!(conn, "insert into app.deadline_shares values (6, 'anne', now() + interval '#{@lasts_s} seconds')", [])
        eventually(fn -> visible(conn, eu, :list) == [1, 2, 3, 5, 6] end) || raise("the share never showed")
        [[ms]] = Postgrex.query!(conn, "select (extract(epoch from deadline - clock_timestamp()) * 1000)::int from app.deadline_shares where doc_id = 6", []).rows
        Process.sleep(max(ms, 0))
        started = System.monotonic_time(:millisecond)
        eventually(fn -> visible(conn, eu, :list) == [1, 2, 3, 5] and visible(conn, eu, :check) == [1, 2, 3, 5] end) &&
          System.monotonic_time(:millisecond) - started
      end)
    ]
  end

  # the same step through the sidecar that lists and the one that checks rows
  defp both(what, f) do
    outcomes = for s <- [:list, :check], do: {s, safely(fn -> f.(s) end)}
    case for {s, o} <- outcomes, o != true, do: "#{s}: #{inspect(o)}" do
      [] -> {what, "ok (listed and checked)"}
      failed -> {what, "FAILED " <> Enum.join(failed, "; ")}
    end
  end

  defp safely(f) do
    f.()
  rescue
    e -> Exception.message(e)
  end

  defp timed(what, f) do
    case safely(f) do
      ms when is_integer(ms) -> {what, "ok (#{ms} ms after it)"}
      other -> {what, "FAILED #{inspect(other)}"}
    end
  end

  defp diagnose(conn, stack) do
    for q <- ["select engine, name, version, pushed_at, error from authz.rules()", "select * from authz.outbox"],
        do: IO.puts(:stderr, "---- #{q}\n#{inspect(Postgrex.query!(conn, q, []).rows, limit: 20)}")
    for {c, log} <- Stack.logs(stack), String.contains?(c, "sidecar"), do: IO.puts(:stderr, "---- #{c}\n#{log |> String.split("\n") |> Enum.reject(&String.contains?(&1, "error connecting")) |> Enum.join("\n")}")
  end

  defp start_postgres(stack) do
    pg = Stack.track(stack, "#{stack.id}-postgres")
    {^pg, port} =
      Docker.run(pg, Stack.pg_image(), 5432, ["-c", "fsync=off"],
        Stack.pg_docker_args(stack) ++ Docker.coverage_args("postgres") ++ ["-e", "POSTGRES_PASSWORD=conditions"])
    Stack.wait(fn -> match?({_, 0}, System.cmd("docker", ["exec", pg, "pg_isready", "-h", "127.0.0.1", "-U", "postgres"])) end, pg)
    {:ok, conn} =
      retry(fn -> Postgrex.start_link(hostname: "127.0.0.1", port: port, username: "postgres", password: "conditions", database: "postgres", pool_size: 1) end, 20)
    run_all(conn, ["create extension pg_auth", "create role app_user nologin", "create schema app", "grant usage on schema app to app_user"])
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

  defp eventually(f, waited \\ 0) do
    cond do
      f.() -> true
      waited >= @deadline_ms -> false
      true -> Process.sleep(50) && eventually(f, waited + 50)
    end
  end

  # what anne sees with these claims, through the sidecar `via`
  defp visible(conn, claims, via) do
    {:ok, ids} =
      Postgrex.transaction(conn, fn c ->
        Postgrex.query!(c, "select set_config('pg_auth.sidecar_socket', $1, true)", [Stack.socket(Atom.to_string(via))])
        Postgrex.query!(c, "select set_config('request.jwt.claims', $1, true)", [JSON.encode!(claims)])
        Postgrex.query!(c, "set local role app_user", [])
        Postgrex.query!(c, "select id from app.docs order by id", []).rows |> List.flatten()
      end)
    ids
  end
end

Conditions.main(System.argv())
