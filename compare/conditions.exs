#!/usr/bin/env elixir
# OpenFGA conditions and SpiceDB caveats, end to end: relationships written with a condition and
# the row's values for it (a relation's `condition` in the mapping), and each request giving it
# the rest (authz.configure context: a token's claim, the time asked). A share in some regions
# shows to a token from one of them; a share until a deadline shows until then; a row's new
# values change what shows; a request without a value a condition needs is not let in. A
# protected table's rows give a condition values too (attributes): reports at or below a reader's
# clearance, with how many rows the engine was asked about (SpiceDB: only those its grant is
# conditional for; OpenFGA: every row). Each step runs on a sidecar that lists and on one that
# checks rows (LIST_LIMIT 2). See compare/README.md.
#
#   elixir compare/conditions.exs                       # openfga, then spicedb
#   elixir compare/conditions.exs --engine spicedb --no-build

Mix.install([{:req, "~> 0.7"}, {:postgrex, "~> 0.22"}], config: [postgrex: [json_library: JSON]])

dir = Path.join(__DIR__, "lib")
for f <- ~w(engine docker engines/openfga engines/spicedb engines/cerbos engines/cedar stack), do: Code.require_file(Path.join(dir, f <> ".exs"))

defmodule Conditions do
  alias Compare.{Docker, Stack}

  @bare "model\n  schema 1.1\ntype user\n"

  # each share a viewer, under the condition its table gives values to; each report gives its level
  @mapping """
  app.docs: {read: viewer}
  app.region_shares:
    type: doc
    relations:
      viewer: {object: doc_id, subject: user_id, condition: {name: in_region, with: {allowed: regions}}}
  app.deadline_shares:
    type: doc
    relations:
      viewer: {object: doc_id, subject: user_id, condition: {name: before_deadline, with: {deadline: deadline}}}
  app.shares:
    type: doc
    relations:
      viewer: {object: doc_id, subject: user_id}
  app.reports: {read: viewer, attributes: [level]}
  app.clearances:
    type: report
    relations:
      viewer: {object: report_id, subject: user_id, condition: {name: cleared, with: {clearance: clearance}}}
  app.report_shares:
    type: report
    relations:
      viewer: {object: report_id, subject: user_id}
  """

  @rules %{
    "openfga" => {"model", """
    model
      schema 1.1
    type user
    type doc
      relations
        define viewer: [user, user with in_region, user with before_deadline]
    type report
      relations
        define viewer: [user, user with cleared]
    condition cleared(clearance: int, level: int) {
      level <= clearance
    }
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

    caveat cleared(clearance int, level int) {
      level <= clearance
    }

    definition report {
      relation viewer: user | user with cleared
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
      Process.put(:conn, conn)
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
      "insert into app.shares values (5, 'anne')",
      # reports with a level each; anne cleared to 3 for 1-4, and given 6 outright
      "create table app.reports (id int primary key, level int not null)",
      "create table app.clearances (report_id int, user_id text, clearance int, primary key (report_id, user_id))",
      "create table app.report_shares (report_id int, user_id text, primary key (report_id, user_id))",
      "grant select on app.reports to app_user",
      "insert into app.reports values (1, 1), (2, 2), (3, 3), (4, 5), (5, 1), (6, 9)",
      "insert into app.clearances select r, 'anne', 3 from generate_series(1, 4) r",
      "insert into app.report_shares values (6, 'anne')"
    ])
    Postgrex.query!(conn, "select authz.set_rules($1, $2, $3, mapping => $4)", [engine, name, rules, @mapping])
    eventually(fn -> Postgrex.query!(conn, "select pushed_at is not null from authz.rules()", []).rows == [[true]] end) || raise("rules not pushed")
    eventually(fn -> Postgrex.query!(conn, "select count(*) from authz_internal.outbox", []).rows == [[0]] end) || raise("relationships not delivered")
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
      counted("a row's own value meets the grant's: reports at or below anne's clearance", fn s ->
        eventually(fn -> visible(conn, eu, s, "app.reports") == [1, 2, 3, 6] end)
      end),
      both("the row's value changes: report 4 lowered to her clearance shows", fn s ->
        if s == :list, do: Postgrex.query!(conn, "update app.reports set level = 2 where id = 4", [])
        eventually(fn -> visible(conn, eu, s, "app.reports") == [1, 2, 3, 4, 6] end)
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

  # the same, with how many rows each sidecar asked the engine about (authz.stats)
  defp counted(what, f) do
    {what, outcome} = both(what, fn s -> f.(s) end)
    asked = for s <- [:list, :check], do: "#{s} #{rows_asked(fn -> f.(s) end)}"
    {what, if(String.starts_with?(outcome, "FAILED"), do: outcome, else: "ok (rows asked: #{Enum.join(asked, ", ")})")}
  end

  defp rows_asked(f) do
    stat = fn -> hd(hd(Postgrex.query!(conn_of(), "select ids_checked from authz.stats()", []).rows)) end
    before = stat.()
    f.()
    stat.() - before
  end

  defp conn_of, do: Process.get(:conn)

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
    for q <- ["select engine, name, version, pushed_at, error from authz.rules()", "select * from authz_internal.outbox"],
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
  defp visible(conn, claims, via, table \\ "app.docs") do
    {:ok, ids} =
      Postgrex.transaction(conn, fn c ->
        Postgrex.query!(c, "select set_config('pg_auth.sidecar_socket', $1, true)", [Stack.socket(Atom.to_string(via))])
        Postgrex.query!(c, "select set_config('request.jwt.claims', $1, true)", [JSON.encode!(claims)])
        Postgrex.query!(c, "set local role app_user", [])
        Postgrex.query!(c, "select id from #{table} order by id", []).rows |> List.flatten()
      end)
    ids
  end
end

Conditions.main(System.argv())
