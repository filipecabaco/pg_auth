#!/usr/bin/env elixir
# RLS benchmark runner: one isolated Docker container per scenario, N variants per scenario.
# See bench/docker/README.md.
#
#   elixir bench/docker/run.exs --scenario org-membership --scale 50000 --reps 5
#   elixir bench/docker/run.exs --scenario all --parallel 2
#   elixir bench/docker/run.exs --list | --clean

Mix.install([{:postgrex, "~> 0.22"}, {:table_rex, "~> 4.1"}, {:req, "~> 0.7"}], config: [postgrex: [json_library: JSON]])

# pg_auth variants run against OpenFGA and the sidecar, started with the compare harness's helpers
compare = Path.expand("../../compare/lib", __DIR__)
for f <- ~w(engine docker engines/openfga engines/spicedb engines/cerbos stack), do: Code.require_file(Path.join(compare, f <> ".exs"))

defmodule Bench.Docker do
  @label "pg_auth_bench=1"

  def label, do: @label

  def cmd(args, opts \\ []) do
    {out, status} = System.cmd("docker", args, [stderr_to_stdout: true] ++ opts)
    {String.trim(out), status}
  end

  def cmd!(args) do
    case cmd(args) do
      {out, 0} -> out
      {out, s} -> raise "docker #{Enum.join(args, " ")} failed (#{s}): #{out}"
    end
  end

  def image_exists?(image), do: match?({_, 0}, cmd(["image", "inspect", image]))

  # Everything a run started: its containers, and those on its networks (OpenFGA and its
  # datastore carry the compare harness's label), then the networks and volumes.
  def clean do
    nets = cmd!(["network", "ls", "-q", "--filter", "label=#{@label}"]) |> String.split()
    on_nets = Enum.flat_map(nets, &String.split(cmd!(["ps", "-aq", "--filter", "network=#{&1}"])))
    ids = (String.split(cmd!(["ps", "-aq", "--filter", "label=#{@label}"])) ++ on_nets) |> Enum.uniq()
    if ids != [], do: cmd(["rm", "-f", "-v" | ids])
    # networks and socket volumes of pg_auth variants' OpenFGA + sidecars
    for kind <- ["network", "volume"], n <- String.split(cmd!([kind, "ls", "-q", "--filter", "label=#{@label}"])), do: cmd([kind, "rm", n])
    length(ids)
  end
end

defmodule Bench.Scenario do
  @scenarios Path.join(__DIR__, "scenarios")
  @canonical ~w(no_rls_baseline naive_rls best_practice_rls)

  def root, do: @scenarios

  def list do
    @scenarios |> File.ls!() |> Enum.filter(&File.exists?(Path.join([@scenarios, &1, "scenario.json"]))) |> Enum.sort()
  end

  def load(id) do
    dir = Path.join(@scenarios, id)
    meta = dir |> Path.join("scenario.json") |> File.read!() |> JSON.decode!()
    queries = dir |> Path.join("queries.sql") |> File.read!() |> parse_queries()

    variants =
      dir
      |> Path.join("variants")
      |> File.ls!()
      |> Enum.filter(&(String.ends_with?(&1, ".sql") and not String.ends_with?(&1, ".queries.sql")))
      |> Enum.map(&String.replace_suffix(&1, ".sql", ""))
      |> Enum.sort_by(&{Enum.find_index(@canonical, fn c -> c == &1 end) || 99, &1})
      |> Enum.map(&load_variant(dir, &1, queries))

    # pg_auth/model.fga (OpenFGA DSL) and pg_auth/tuples.sql (object, relation, user rows from the
    # loaded schema) back the pg_auth_* variants
    authz = Path.join(dir, "pg_auth")

    pg_auth =
      if File.dir?(authz),
        do: %{model: File.read!(Path.join(authz, "model.fga")), tuples: File.read!(Path.join(authz, "tuples.sql"))}

    %{id: id, dir: dir, meta: meta, variants: variants, pg_auth: pg_auth}
  end

  defp load_variant(dir, name, base_queries) do
    sql = File.read!(Path.join([dir, "variants", name <> ".sql"]))

    directives =
      Regex.scan(~r/^--\s*@(\w+):\s*(.*?)\s*$/m, sql, capture: :all_but_first) |> Enum.map(fn [k, v] -> {k, v} end)

    override_file = Path.join([dir, "variants", name <> ".queries.sql"])

    overrides = if File.exists?(override_file), do: parse_queries(File.read!(override_file)), else: []
    names = Enum.map(base_queries, &elem(&1, 0))
    extra = Enum.reject(overrides, fn {n, _} -> n in names end)

    queries =
      Enum.map(base_queries, fn {n, q} -> {n, List.keyfind(overrides, n, 0, {n, q}) |> elem(1)} end) ++ extra

    %{
      name: name,
      file: "/scenario/variants/#{name}.sql",
      superuser: List.keyfind(directives, "role", 0) == {"role", "superuser"},
      pg_auth: String.starts_with?(name, "pg_auth"),
      # pg_auth_cerbos*: rules on the rows' attributes, from the scenario's pg_auth/cerbos policies;
      # the other pg_auth variants use the run's engine (--engine)
      engine: if(String.starts_with?(name, "pg_auth_cerbos"), do: "cerbos"),
      prepare_as: with({"prepare_as", s} <- List.keyfind(directives, "prepare_as", 0), do: s, else: (_ -> nil)),
      settings:
        for({"set", kv} <- directives, [k, v] = String.split(kv, "=", parts: 2), do: {String.trim(k), String.trim(v)}),
      queries: queries
    }
  end

  # "-- name: foo" header followed by one SQL statement.
  def parse_queries(text) do
    text
    |> then(&Regex.split(~r/^--\s*name:\s*/m, &1))
    |> tl()
    |> Enum.map(fn chunk ->
      [name, sql] = String.split(chunk, "\n", parts: 2) |> then(&(&1 ++ [""]) |> Enum.take(2))
      {String.trim(name), sql |> String.trim() |> String.trim_trailing(";") |> String.trim()}
    end)
  end
end

defmodule Bench.Plan do
  @moduledoc "Extracts metrics from EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) output."

  def walk(p), do: [p | Enum.flat_map(p["Plans"] || [], &walk/1)]

  def summarize(%{"Plan" => root} = top) do
    nodes = walk(root)

    subplans =
      Enum.filter(nodes, &(&1["Parent Relationship"] == "SubPlan" and (&1["Actual Loops"] || 0) > 1))

    appends = Enum.filter(nodes, &(&1["Node Type"] in ["Append", "Merge Append"]))

    {scanned, total} =
      Enum.reduce(appends, {0, 0}, fn a, {s, t} ->
        kids = a["Plans"] || []
        {s + Enum.count(kids, &((&1["Actual Loops"] || 0) > 0)), t + length(kids) + (a["Subplans Removed"] || 0)}
      end)

    est = root["Plan Rows"] || 0
    act = (root["Actual Rows"] || 0) * max(root["Actual Loops"] || 1, 1)

    %{
      exec_ms: top["Execution Time"],
      plan_ms: top["Planning Time"],
      shared_hit: root["Shared Hit Blocks"],
      shared_read: root["Shared Read Blocks"],
      top_nodes: top_nodes(root),
      per_row_subplan: subplans != [],
      subplan_loops: subplans |> Enum.map(& &1["Actual Loops"]) |> Enum.max(fn -> 0 end),
      parts_scanned: if(appends == [], do: nil, else: scanned),
      parts_total: if(appends == [], do: nil, else: total),
      rows_est: est,
      rows_actual: act,
      q_error_root: qerr(est, root["Actual Rows"] || 0),
      q_error_max:
        nodes
        |> Enum.filter(&((&1["Actual Loops"] || 0) > 0))
        |> Enum.map(&qerr(&1["Plan Rows"] || 0, &1["Actual Rows"] || 0))
        |> Enum.max(fn -> 1.0 end)
    }
  end

  defp qerr(est, act), do: Float.round(max((est + 1) / (act + 1), (act + 1) / (est + 1)), 1)

  # Top 3 nodes by exclusive time (total time * loops minus children).
  defp top_nodes(root) do
    root
    |> walk()
    |> Enum.map(fn n ->
      t = (n["Actual Total Time"] || 0) * (n["Actual Loops"] || 0)
      kids = Enum.sum(for c <- n["Plans"] || [], do: (c["Actual Total Time"] || 0) * (c["Actual Loops"] || 0))
      {n, max(t - kids, 0)}
    end)
    |> Enum.sort_by(&elem(&1, 1), :desc)
    |> Enum.take(3)
    |> Enum.map(fn {n, ms} ->
      rel = if n["Relation Name"], do: "(#{n["Relation Name"]})", else: ""
      "#{n["Node Type"]}#{rel} #{fmt(ms)}ms"
    end)
  end

  def fmt(x) when is_number(x) and x >= 100, do: Float.round(x * 1.0, 0) |> trunc() |> to_string()
  def fmt(x) when is_number(x) and x >= 1, do: :erlang.float_to_binary(x * 1.0, decimals: 1)
  def fmt(x) when is_number(x), do: :erlang.float_to_binary(x * 1.0, decimals: 3)
  def fmt(x), do: to_string(x)
end

defmodule Bench.Runner do
  alias Bench.{Docker, Plan}
  alias Compare.Stack

  # pg_auth_* variants pick one by socket (-- @set: pg_auth.sidecar_socket=/run/pg_auth/<name>.sock):
  # default lists up to 1000 objects (the sidecar's default), per_row never lists
  @sidecars [default: 1000, per_row: 1]

  # the variant's Postgres on its stack's network, for the sidecars: a container name can pass
  # DNS's 63-character limit for a label, and one Postgres runs per network at a time
  @pg_alias "bench-postgres"

  @pg_args ~w(shared_buffers=1GB work_mem=64MB maintenance_work_mem=512MB max_parallel_workers_per_gather=0
              max_parallel_maintenance_workers=0 jit=off effective_cache_size=3GB random_page_cost=1.1
              fsync=off synchronous_commit=off full_page_writes=off max_wal_size=8GB
              checkpoint_timeout=30min max_connections=50 track_io_timing=on)

  # bench_user: non-superuser, owns nothing, gets DML on app schemas and execute on functions.
  # pg_auth's schema keeps its own grants, as in production.
  @grant_sql """
  do $$ declare s text; begin
    for s in select nspname from pg_namespace
             where nspname not like 'pg\\_%' and nspname not in ('information_schema','public','authz') loop
      execute format('grant usage on schema %I to bench_user', s);
      execute format('grant execute on all functions in schema %I to bench_user', s);
      execute format('grant select, insert, update, delete on all tables in schema %I to bench_user', s);
      execute format('grant usage on all sequences in schema %I to bench_user', s);
    end loop;
  end $$;
  """

  # Every variant runs on containers of its own, started fresh: Postgres on a copy of the data
  # loaded once (a seed volume), and for pg_auth variants a new OpenFGA server (same datastore)
  # and new sidecars. Nothing a variant did (dead rows, caches, settings, indexes, hint bits in
  # shared buffers) reaches the next one, so results do not depend on variant order.
  def run(sc, o) do
    log = fn msg -> IO.puts(:stderr, "[#{sc.id}] #{msg}") end
    # the OS pid keeps names apart between runs (unique_integer restarts with every VM)
    id = "pg_auth_bench_#{sc.id}-#{System.pid()}-#{System.unique_integer([:positive])}"
    scale = o.scale || sc.meta["scale"]["default"]
    variants = if o.variants, do: Enum.filter(sc.variants, &(&1.name in o.variants)), else: sc.variants

    # one stack (engine and sidecars on a network) per engine the variants use
    engine_of = fn v -> v.engine || o.engine end
    engines = variants |> Enum.filter(& &1.pg_auth) |> Enum.map(engine_of) |> Enum.uniq()
    stacks = for e <- engines, into: %{}, do: {e, Stack.new("pg_auth_bench_#{sc.id}_#{e}", Docker.label())}
    # containers and volumes this run created, removed at the end
    {:ok, own} = Agent.start(fn -> [] end)
    owned = fn kind, name -> Agent.update(own, &[{kind, name} | &1]) && name end
    seed = owned.(:volume, "#{id}-seed")

    try do
      if engines != [] and is_nil(sc.pg_auth), do: raise("#{sc.id}: pg_auth variants need pg_auth/model.fga and pg_auth/tuples.sql")
      # sidecars start with each pg_auth variant, against that variant's Postgres
      started =
        for {e, stack} <- stacks, into: %{} do
          log.("starting #{e}")
          {e, Stack.start_engine(stack, sc.pg_auth.model, [],
                engine: e, policies: Path.join([sc.dir, "pg_auth", "cerbos"]), list_max_results: 1000,
                # the sidecar's production answer cache (1 s), like OpenFGA's own caches
                sidecar_cache_ms: nil, sidecar_env: o.sidecar_env)}
        end

      Docker.cmd!(["volume", "create", "--label", Docker.label(), seed])
      name = start_postgres(owned.(:container, "#{id}-seed"), seed, sc, o, nil, log)
      conn = connect(host_port(name), name)
      q!(conn, "create role bench_user nologin")

      log.("loading schema (scale=#{scale})")
      {us, _} = :timer.tc(fn -> psql!(name, scale, ["-f", "/scenario/schema.sql"]) end)
      log.("schema loaded in #{div(us, 1_000_000)}s")

      if engines != [] do
        q!(conn, "create extension pg_auth")
        # relationships for the engines that keep them (Cerbos decides on the rows themselves)
        for {e, engine} <- started, e != "cerbos" do
          tuples = for [o, r, u] <- q!(conn, sc.pg_auth.tuples).rows, do: %{object: o, relation: r, user: u}
          {us, :ok} = :timer.tc(fn -> Stack.write_tuples(engine, tuples) end)
          log.("wrote #{length(tuples)} tuples to #{e} in #{div(us, 1_000_000)}s")
        end
      end

      q!(conn, "vacuum analyze")
      version = q!(conn, "select version()").rows |> hd() |> hd()
      GenServer.stop(conn)
      # a clean shutdown, so every copy starts without recovery
      Docker.cmd!(["stop", "-t", "60", name])
      Docker.cmd(["rm", "-f", "-v", name])

      {results, _} =
        Enum.map_reduce(variants, started, fn v, started ->
          last? = v == List.last(variants)
          data = owned.(:volume, "#{id}-#{v.name}")
          Docker.cmd!(["volume", "create", "--label", Docker.label(), data])
          Docker.cmd!(["run", "--rm", "-v", "#{seed}:/from:ro", "-v", "#{data}:/to", "--entrypoint", "sh", o.image, "-c", "cp -a /from/. /to/"])
          stack = if v.pg_auth, do: stacks[engine_of.(v)]
          name = start_postgres(owned.(:container, "#{id}-#{v.name}"), data, sc, o, stack, log)
          conn = connect(host_port(name), name)

          {started, sidecars} =
            if v.pg_auth do
              engine = Stack.fresh_engine(started[engine_of.(v)])
              # the sidecars keep the permission index in this variant's Postgres
              {Map.put(started, engine_of.(v), engine),
               Stack.start_sidecars(engine, @sidecars, [{"PG_AUTH_DATABASE_URL", "postgres://postgres:bench@#{@pg_alias}:5432/bench"}])}
            else
              {started, []}
            end

          try do
            log.("variant #{v.name}: installing")
            {us, _} = :timer.tc(fn -> psql!(name, scale, ["-f", v.file]) end)
            psql!(name, scale, ["-c", @grant_sql])
            q!(conn, "vacuum analyze")
            install_ms = div(us, 1000)
            log.("variant #{v.name}: installed in #{install_ms}ms, measuring")
            {%{name: v.name, install_ms: install_ms, results: measure_variant(conn, sc, v, o, log)}, started}
          after
            # closed before the container goes, so a lost connection in the log means a real failure
            GenServer.stop(conn)

            if o.keep and last? do
              IO.puts(:stderr, "[#{sc.id}] --keep: #{name} left running on 127.0.0.1:#{host_port(name)} with its sidecars and OpenFGA (elixir bench/docker/run.exs --clean)")
            else
              Stack.remove(sidecars)
              Docker.cmd(["rm", "-f", "-v", name])
              Docker.cmd(["volume", "rm", "-f", data])
            end
          end
        end)

      %{
        scenario: sc.id,
        title: sc.meta["title"],
        issue: sc.meta["issue"],
        symptom: sc.meta["symptom"],
        scale: scale,
        reps: o.reps,
        resources: %{cpus: o.cpus, memory: o.memory},
        engine: if(engines != [], do: Enum.join(engines, "+")),
        postgres_args: @pg_args,
        postgres: version,
        finished_at: DateTime.utc_now() |> DateTime.to_iso8601(),
        subjects: sc.meta["subjects"],
        variants: results
      }
    after
      # everything but what --keep leaves of the last variant
      kept = if o.keep, do: [{:container, "#{id}-#{List.last(variants).name}"}, {:volume, "#{id}-#{List.last(variants).name}"}], else: []
      for {kind, n} <- Agent.get(own, & &1) -- kept do
        if kind == :container, do: Docker.cmd(["rm", "-f", "-v", n]), else: Docker.cmd(["volume", "rm", "-f", n])
      end
      Agent.stop(own)
      if !o.keep, do: for({_, stack} <- stacks, do: Stack.stop(stack))
    end
  end

  # A Postgres container on `data` (mounted where the image keeps its data directory): initialised
  # on an empty volume, started as it is on a copy.
  defp start_postgres(name, data, sc, o, authz, log) do
    log.("starting container #{name} (#{o.cpus} cpus, #{o.memory})")

    Docker.cmd!(
      ["run", "-d", "--name", name, "--label", Docker.label(), "--cpus=#{o.cpus}", "--memory=#{o.memory}",
       "--shm-size=1g", "-e", "POSTGRES_PASSWORD=bench", "-e", "POSTGRES_DB=bench", "-p", "127.0.0.1::5432",
       "-v", "#{data}:/var/lib/postgresql", "-v", "#{sc.dir}:/scenario:ro"] ++
        if(authz, do: Stack.pg_docker_args(authz) ++ ["--network-alias", @pg_alias], else: []) ++ [o.image] ++ Enum.flat_map(@pg_args, &["-c", &1])
    )

    name
  end

  defp host_port(name) do
    out = Docker.cmd!(["port", name, "5432/tcp"]) |> String.split("\n") |> hd()
    out |> String.split(":") |> List.last() |> String.to_integer()
  end

  # The official image runs a temporary socket-only server during init, so wait for TCP readiness.
  defp connect(port, name, tries \\ 180) do
    case Docker.cmd(["exec", name, "pg_isready", "-h", "127.0.0.1", "-U", "postgres", "-d", "bench"]) do
      {_, 0} ->
        {:ok, c} =
          Postgrex.start_link(hostname: "127.0.0.1", port: port, username: "postgres", password: "bench",
            database: "bench", timeout: :infinity, pool_size: 1)

        c

      _ when tries > 0 ->
        Process.sleep(1000)
        connect(port, name, tries - 1)

      _ ->
        raise "postgres in #{name} never became ready: #{Docker.cmd!(["logs", "--tail", "30", name])}"
    end
  end

  defp psql!(name, scale, args) do
    base = ["exec", name, "psql", "-X", "-q", "-U", "postgres", "-d", "bench", "-v", "ON_ERROR_STOP=1",
            "-v", "scale=#{scale}", "-o", "/dev/null"]

    case Docker.cmd(base ++ args) do
      {_, 0} -> :ok
      {out, s} -> raise "psql #{Enum.join(args, " ") |> String.slice(0, 80)} failed (#{s}):\n#{out}"
    end
  end

  defp q!(c, sql, params \\ []), do: Postgrex.query!(c, sql, params, timeout: :infinity)

  defp measure_variant(conn, sc, v, o, log) do
    for subj <- pick_subjects(sc, o), {qname, qsql} <- v.queries, only?(o, qname) do
      sql = String.replace(qsql, "{{subject}}", subj["id"])
      # -- @prepare_as: <subject>: a generic plan built for that subject, reused by this one
      prepared = if v.prepare_as, do: prepare(conn, sc, v, sql), else: {:ok, sql}

      # warmup run (also gives a result fingerprint for cross-variant correctness checks)
      warmup = fn ->
        with {:ok, stmt} <- prepared,
             {:ok, %{rows: rows}} <- in_ctx(conn, sc, v, subj, fn c -> Postgrex.query(c, stmt, [], timeout: :infinity) end),
             do: {:ok, stmt, rows}
      end

      settle(conn, sc)
      warm = warmup.()

      # the warmup may have asked the sidecar to fill permission index sets: measure the steady
      # state, once they are filled (a subject's first read, before its set exists, asks the
      # engine and may time out: warm up once more then)
      warm =
        if v.pg_auth do
          wait_for_fills(conn)
          if match?({:error, _}, warm) and is_nil(v.prepare_as), do: warmup.(), else: warm
        else
          warm
        end

      res =
        case warm do
          {:ok, stmt, rows} ->
            plans = for _ <- 1..o.reps, do: (settle(conn, sc); explain(conn, sc, v, subj, stmt, o.timing))
            summarize(plans, rows)

          {:error, e} ->
            error_result(e)
        end

      if v.prepare_as, do: q!(conn, "deallocate all")
      log.("  #{v.name} #{subj["name"]} #{qname}: #{res[:error] || Plan.fmt(res.exec_ms) <> " ms"}")
      Map.merge(res, %{subject: subj["name"], query: qname})
    end
  end

  # PREPARE the query as q and execute it once as the @prepare_as subject, which builds the
  # generic plan the measured subject then reuses. -> {:ok, statement to measure} | {:error, e}
  defp prepare(conn, sc, v, sql) do
    planner = Enum.find(sc.meta["subjects"], &(&1["name"] == v.prepare_as)) || raise("#{v.name}: no subject #{v.prepare_as}")
    q!(conn, "prepare q as " <> sql)

    with {:ok, _} <- in_ctx(conn, sc, v, planner, fn c -> Postgrex.query(c, "execute q", [], timeout: :infinity) end),
         do: {:ok, "execute q"}
  end

  # Write scenarios roll back every measured statement, which leaves its dead rows behind: with
  # "vacuum_each_run" each run starts from a vacuumed table instead of the debris of the runs and
  # queries before it (which would make a variant's numbers depend on its queries' order).
  defp settle(conn, sc), do: if(sc.meta["vacuum_each_run"], do: q!(conn, "vacuum"))

  defp only?(%{queries: nil}, _), do: true
  defp only?(%{queries: qs}, q), do: q in qs

  defp pick_subjects(sc, %{subjects: nil}), do: sc.meta["subjects"]
  defp pick_subjects(sc, %{subjects: names}), do: Enum.filter(sc.meta["subjects"], &(&1["name"] in names))

  defp explain(conn, sc, v, subj, sql, timing) do
    before = if v.pg_auth, do: authz_stats(conn)

    plan =
      in_ctx(conn, sc, v, subj, fn c ->
        case Postgrex.query(c, "explain (analyze, buffers, timing #{timing}, format json) " <> sql, [], timeout: :infinity) do
          {:ok, %{rows: [[[plan]]]}} -> {:ok, plan}
          {:ok, %{rows: [[plan]]}} when is_map(plan) -> {:ok, plan}
          {:error, e} -> {:error, e}
        end
      end)

    with {:ok, p} when before != nil <- plan do
      cost = Map.merge(authz_stats(conn), before, fn _, a, b -> a - b end)
      {:ok, Map.put(p, "pg_auth", cost)}
    end
  end

  defp wait_for_fills(conn, tries \\ 300) do
    Process.sleep(100)
    %{rows: [[filling]]} = q!(conn, "select count(*) from authz_internal.permission_sets where status = 'filling'")
    if filling > 0 and tries > 0, do: wait_for_fills(conn, tries - 1)
  end

  # This backend's cumulative round trips to the sidecar (authz.stats()); a diff costs one statement.
  defp authz_stats(conn) do
    %{rows: [[calls, ids, sidecar_ms, engine_ms]]} = q!(conn, "select sidecar_calls, ids_checked, sidecar_ms, engine_ms from authz.stats()")
    %{sidecar_calls: calls, ids_checked: ids, sidecar_ms: sidecar_ms, engine_ms: engine_ms}
  end

  defp error_result(%{postgres: %{code: :query_canceled}}), do: %{error: "timeout", exec_ms: nil}
  defp error_result(%{postgres: %{message: m}}), do: %{error: "error: " <> m, exec_ms: nil}
  defp error_result(e), do: %{error: "error: " <> inspect(e), exec_ms: nil}

  defp summarize(plans, rows) do
    case Enum.find(plans, &match?({:error, _}, &1)) do
      {:error, e} ->
        error_result(e)

      nil ->
        sums = plans |> Enum.map(fn {:ok, p} -> Map.merge(Plan.summarize(p), p["pg_auth"] || %{}) end) |> Enum.sort_by(& &1.exec_ms)
        median = Enum.at(sums, div(length(sums), 2))

        Map.merge(median, %{
          exec_min_ms: hd(sums).exec_ms,
          exec_max_ms: List.last(sums).exec_ms,
          result_rows: length(rows),
          result_hash: :erlang.phash2(rows)
        })
    end
  end

  # Runs fun inside a transaction that is always rolled back (safe for DML queries),
  # as bench_user (unless the variant is superuser) with the subject GUCs set. Settings go first,
  # while still superuser: pg_auth's subject and socket settings are superuser-only.
  defp in_ctx(conn, sc, v, subj, fun) do
    {:error, res} =
      Postgrex.transaction(
        conn,
        fn c ->
          for g <- sc.meta["subject_gucs"] || [], do: set_local(c, g, subj["id"])
          # Supabase/PostgREST request claims (auth.jwt(), auth.uid()); "sub" defaults to the id
          if claims = subj["claims"], do: set_local(c, "request.jwt.claims", JSON.encode!(Map.put_new(claims, "sub", subj["id"])))
          for {k, val} <- v.settings, do: set_local(c, k, val)
          set_local(c, "statement_timeout", sc.meta["statement_timeout"] || "60s")
          unless v.superuser, do: Postgrex.query!(c, "set local role bench_user", [])
          Postgrex.rollback(c, fun.(c))
        end,
        timeout: :infinity
      )

    res
  end

  defp set_local(c, name, val) do
    Postgrex.query!(c, "select set_config($1, $2, true)", [name, to_string(val)], timeout: :infinity)
  end
end

defmodule Bench.Report do
  alias Bench.Plan

  def write(res, dir) do
    File.mkdir_p!(dir)
    File.write!(Path.join(dir, "#{res.scenario}.json"), JSON.encode!(res))
    File.write!(Path.join(dir, "#{res.scenario}.md"), markdown(res))
  end

  defp cell(nil), do: "-"
  defp cell(%{error: e}) when is_binary(e), do: e
  defp cell(r), do: Plan.fmt(r.exec_ms)

  defp lookup(res, variant, subject, query) do
    with v when v != nil <- Enum.find(res.variants, &(&1.name == variant)) do
      Enum.find(v.results, &(&1.subject == subject and &1.query == query))
    end
  end

  def pairs(res) do
    res.variants |> Enum.flat_map(& &1.results) |> Enum.map(&{&1.subject, &1.query}) |> Enum.uniq()
  end

  # Result mismatch vs. the first variant that has a result for the same (subject, query).
  defp mismatches(res) do
    for {s, q} <- pairs(res),
        rs = for(v <- res.variants, r = lookup(res, v.name, s, q), r[:result_hash], do: {v.name, r}),
        rs != [],
        {vn, r} <- rs,
        r.result_hash != elem(hd(rs), 1).result_hash,
        do: "#{vn} / #{s} / #{q} (#{r.result_rows} rows) differs from #{elem(hd(rs), 0)} (#{elem(hd(rs), 1).result_rows} rows)"
  end

  def console(res) do
    names = Enum.map(res.variants, & &1.name)
    rows = for {s, q} <- pairs(res), do: [s, q | Enum.map(names, &cell(lookup(res, &1, s, q)))]
    IO.puts("\n#{res.scenario} (scale #{res.scale}) median exec ms")
    IO.puts(TableRex.quick_render!(rows, ["subject", "query" | names]))
  end

  def markdown(res) do
    names = Enum.map(res.variants, & &1.name)

    pivot =
      for {s, q} <- pairs(res), do: [s, q | Enum.map(names, &cell(lookup(res, &1, s, q)))]

    detail =
      for v <- res.variants, r <- v.results, do: detail_row(v.name, r)

    install = for v <- res.variants, do: [v.name, "#{v.install_ms}"]

    round_trips = for v <- res.variants, r <- v.results, Map.has_key?(r, :sidecar_calls), do: round_trip_row(v.name, r)

    round_trip_section =
      if round_trips == [],
        do: "",
        else: """

        ## pg_auth round trips

        Per statement (median rep), planning and execution together: time in Postgres itself, in
        round trips to the sidecar, and the part of those the sidecar spent waiting on the engine.

        #{table(round_trips, ["variant", "subject", "query", "plan + exec ms", "postgres ms", "sidecar ms", "of which engine ms", "sidecar calls", "ids checked"])}
        """

    mm = mismatches(res)

    """
    # #{res.scenario}: #{res.title}

    - scale: #{res.scale}, reps: #{res.reps} (median), resources: #{res.resources.cpus} cpus / #{res.resources.memory}
    - #{res.postgres |> String.split(",") |> hd()}
    - issue: #{res.issue}
    - expected symptom: #{res.symptom}

    ## Median execution time (ms)

    #{table(pivot, ["subject", "query" | names])}

    ## Plan details

    #{table(detail, ["variant", "subject", "query", "exec ms", "plan ms", "hit/read", "per-row subplan", "parts", "est/actual (q-err root/max)", "top nodes (exclusive)"])}
    #{round_trip_section}

    ## Variant install time (ms)

    #{table(install, ["variant", "install ms"])}

    ## Result consistency

    #{if mm == [], do: "All variants returned identical results (row-set hash) for every query/subject.", else: "Differences (may be benign for ties under ORDER BY ... LIMIT):\n\n" <> Enum.map_join(mm, "\n", &"- #{&1}")}
    """
  end

  defp round_trip_row(v, r) do
    total = r.exec_ms + r.plan_ms
    ms = &Plan.fmt/1
    [v, r.subject, r.query, ms.(total), ms.(max(total - r.sidecar_ms, 0)), ms.(r.sidecar_ms), ms.(r.engine_ms), r.sidecar_calls, r.ids_checked]
  end

  defp detail_row(v, %{error: e} = r), do: [v, r.subject, r.query, e, "-", "-", "-", "-", "-", "-"]

  defp detail_row(v, r) do
    [
      v,
      r.subject,
      r.query,
      Plan.fmt(r.exec_ms),
      Plan.fmt(r.plan_ms),
      "#{r.shared_hit}/#{r.shared_read}",
      if(r.per_row_subplan, do: "yes x#{r.subplan_loops}", else: "no"),
      if(r.parts_total, do: "#{r.parts_scanned}/#{r.parts_total}", else: "-"),
      "#{r.rows_est}/#{r.rows_actual} (#{r.q_error_root}/#{r.q_error_max})",
      Enum.join(r.top_nodes, "; ")
    ]
  end

  defp table(rows, headers), do: markdown_table(headers, rows)

  # Emit GitHub markdown rather than ASCII art.
  defp markdown_table(headers, rows) do
    esc = &(to_string(&1) |> String.replace("|", "\\|"))
    line = &("| " <> Enum.map_join(&1, " | ", esc) <> " |")
    Enum.join([line.(headers), line.(Enum.map(headers, fn _ -> "---" end)) | Enum.map(rows, line)], "\n")
  end

  # Aggregate: geometric-mean slowdown of each variant vs the first variant that is present
  # (baseline) over all (subject, query) pairs where both completed.
  def summary(all, dir) do
    File.mkdir_p!(dir)

    sections =
      for res <- all do
        ref = hd(res.variants).name

        rows =
          for v <- res.variants do
            ratios =
              for {s, q} <- pairs(res),
                  a = lookup(res, ref, s, q),
                  b = lookup(res, v.name, s, q),
                  a[:exec_ms] && b[:exec_ms],
                  do: max(b.exec_ms, 0.001) / max(a.exec_ms, 0.001)

            geo = if ratios == [], do: "-", else: Plan.fmt(:math.exp(Enum.sum(Enum.map(ratios, &:math.log/1)) / length(ratios)))
            worst = if ratios == [], do: "-", else: Plan.fmt(Enum.max(ratios))
            timeouts = v.results |> Enum.count(&(&1[:error] == "timeout"))
            [v.name, geo <> "x", worst <> "x", "#{timeouts}", "#{v.install_ms}"]
          end

        engine = if res[:engine], do: ", pg_auth on #{res.engine}", else: ""

        "## #{res.scenario} (scale #{res.scale}, reference: #{ref}#{engine})\n\n" <>
          table(rows, ["variant", "geomean time vs reference", "worst vs reference", "timeouts", "install ms"])
      end

    File.write!(Path.join(dir, "summary.md"), "# RLS benchmark summary\n\n" <> Enum.join(sections, "\n\n") <> "\n")
    File.write!(Path.join(dir, "summary.json"), JSON.encode!(all))
  end
end

defmodule Bench.CLI do
  alias Bench.{Docker, Scenario}

  def main(argv) do
    {opts, _} =
      OptionParser.parse!(argv,
        strict: [scenario: :string, variants: :string, queries: :string, subjects: :string, reps: :integer,
                 scale: :integer, keep: :boolean, parallel: :integer, image: :string, build: :boolean,
                 cpus: :string, memory: :string, list: :boolean, clean: :boolean, out: :string,
                 engine: :string, timing: :boolean, sidecar_env: :string]
      )

    cond do
      opts[:list] -> list()
      opts[:clean] -> IO.puts("removed #{Docker.clean()} container(s)")
      true -> go(opts)
    end
  end

  defp list do
    for id <- Scenario.list() do
      sc = Scenario.load(id)
      IO.puts("#{id}\t#{Enum.map_join(sc.variants, ",", & &1.name)}\t#{sc.meta["title"]}")
    end
  end

  defp csv(nil), do: nil
  defp csv(s), do: String.split(s, ",", trim: true)

  defp go(opts) do
    image = opts[:image] || "pg_auth-bench"
    root = Path.expand("../..", __DIR__)

    # built every run (cached layers make an unchanged tree quick): a stale image would measure
    # code other than the checkout's
    build = Keyword.get(opts, :build, true)

    if build || not Docker.image_exists?(image) do
      IO.puts(:stderr, "building image #{image}...")
      {_, 0} = System.cmd("docker", ["build", "-f", "bench/docker/Dockerfile", "-t", image, "."], cd: root, into: IO.stream(:stderr, :line))
    end

    # pg_auth_* variants also need the sidecar image
    if build || not Docker.image_exists?("pg_auth-sidecar") do
      IO.puts(:stderr, "building image pg_auth-sidecar...")
      {_, 0} = System.cmd("docker", ["build", "-t", "pg_auth-sidecar", "sidecar/query"], cd: root, into: IO.stream(:stderr, :line))
    end

    ids = if opts[:scenario] in [nil, "all"], do: Scenario.list(), else: csv(opts[:scenario])
    scenarios = Enum.map(ids, &Scenario.load/1)

    for v <- csv(opts[:variants]) || [], not Enum.any?(scenarios, fn s -> Enum.any?(s.variants, &(&1.name == v)) end) do
      raise "unknown variant #{v}"
    end

    o = %{
      image: image, reps: opts[:reps] || 5, scale: opts[:scale], keep: opts[:keep] || false,
      variants: csv(opts[:variants]), queries: csv(opts[:queries]), subjects: csv(opts[:subjects]),
      cpus: opts[:cpus] || "4", memory: opts[:memory] || "4g",
      # the engine behind pg_auth variants' sidecars: openfga (default) or spicedb, as in production
      engine: opts[:engine] || "openfga",
      # --no-timing: EXPLAIN (ANALYZE, TIMING OFF): exact statement time without per-node clock reads,
      # which inflate plans with more nodes per row (no per-node times in the report then)
      timing: Keyword.get(opts, :timing, true),
      # extra sidecar environment, e.g. CHECK_CONCURRENCY=16,CACHE_TTL_MS=0
      sidecar_env: for(kv <- String.split(opts[:sidecar_env] || "", ",", trim: true), [k, v] = String.split(kv, "=", parts: 2), do: {k, v})
    }

    out = opts[:out] || Path.join(__DIR__, "results")

    # a scenario with none of the variants asked for is not loaded at all
    scenarios = if o.variants, do: Enum.filter(scenarios, fn sc -> Enum.any?(sc.variants, &(&1.name in o.variants)) end), else: scenarios

    # a scenario that fails (a container killed under memory pressure, say) is reported and the
    # others carry on: one failure does not throw away the rest of a long run
    outcomes =
      scenarios
      |> Task.async_stream(
        fn sc ->
          try do
            res = Bench.Runner.run(sc, o)
            Bench.Report.write(res, out)
            Bench.Report.console(res)
            {:ok, res}
          rescue
            e -> {:failed, sc.id, Exception.message(e) |> String.split("\n") |> Enum.take(3) |> Enum.join(" ")}
          end
        end,
        max_concurrency: opts[:parallel] || 1,
        timeout: :infinity,
        ordered: true
      )
      |> Enum.map(fn {:ok, outcome} -> outcome end)

    all = for {:ok, res} <- outcomes, do: res
    failed = for {:failed, id, why} <- outcomes, do: {id, why}
    Bench.Report.summary(all, out)
    IO.puts(File.read!(Path.join(out, "summary.md")))

    if failed != [] do
      for {id, why} <- failed, do: IO.puts(:stderr, "FAILED #{id}: #{why}")
      IO.puts(:stderr, "rerun them with --scenario #{Enum.map_join(failed, ",", &elem(&1, 0))}")
      System.halt(1)
    end
  end
end

Bench.CLI.main(System.argv())
