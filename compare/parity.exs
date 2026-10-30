#!/usr/bin/env elixir
# Enforcement parity: protected tables queried as each subject must show exactly the rows the
# engine's ListObjects allows. See compare/README.md ("Enforcement parity").
#
#   elixir compare/parity.exs                       # seed 1, scale 1, every subject
#   elixir compare/parity.exs --seed 7 --subjects 50 --list-limit 5

Mix.install([{:req, "~> 0.7"}, {:postgrex, "~> 0.22"}], config: [postgrex: [json_library: JSON]])

dir = Path.join(__DIR__, "lib")
for f <- ~w(engine docker scenario loaders/random engines/openfga engines/spicedb engines/cerbos engines/cedar stack), do: Code.require_file(Path.join(dir, f <> ".exs"))

defmodule Parity.Stack do
  @moduledoc """
  Compare.Stack with two sidecars, and Postgres with pg_auth joined to it:

    * listed  - LIST_LIMIT high: every subject gets the listed-ids plan
    * per_row - LIST_LIMIT low: subjects with at least that many objects get the per-row check
  """
  alias Compare.{Docker, Stack}

  defdelegate build(root), to: Stack
  defdelegate socket(name), to: Stack
  defdelegate stop(stack), to: Stack
  defdelegate logs(stack), to: Stack

  def new, do: Stack.new("parity", "pg_auth_compare=1")

  # -> the stack with :engine (adapter state), :engine_mod and :pg_port
  def start(stack, scn, list_limit, timeout_ms, engine_opts) do
    [stage] = scn.stages
    # the sidecars keep the permission index in pg_auth's Postgres (started below, same network)
    index = [sidecar_env: [{"PG_AUTH_DATABASE_URL", "postgres://postgres:parity@#{stack.id}-postgres:5432/postgres"}]]
    stack = Stack.start_engine(stack, stage.model, [listed: 1_000_000, per_row: list_limit], engine_opts ++ index)
    :ok = Stack.write_tuples(stack, stage.tuples)
    pg = Stack.track(stack, "#{stack.id}-postgres")

    # without --timeout-ms the extension's default applies, as in production
    pg_args = ["-c", "fsync=off", "-c", "max_connections=50"] ++ if(timeout_ms, do: ["-c", "pg_auth.sidecar_timeout_ms=#{timeout_ms}"], else: [])
    {^pg, port} = Docker.run(pg, Stack.pg_image(), 5432, pg_args, Stack.pg_docker_args(stack) ++ Docker.coverage_args("postgres") ++ ["-e", "POSTGRES_PASSWORD=parity"])
    Stack.wait(fn -> match?({_, 0}, System.cmd("docker", ["exec", pg, "pg_isready", "-h", "127.0.0.1", "-U", "postgres"])) end, pg)
    Map.merge(stack, %{pg_port: port, tuples: stage.tuples})
  end

  def connect(stack, pool_size) do
    {:ok, c} =
      Postgrex.start_link(hostname: "127.0.0.1", port: stack.pg_port, username: "postgres", password: "parity",
        database: "postgres", pool_size: pool_size, timeout: :infinity)

    c
  end
end

defmodule Parity do
  alias Parity.Stack

  # table, primary key, protected column, column type, object type, permission
  @tables [
    %{name: "public.folders", col: "id", type: "folder", perm: "viewer", sql_type: "int8", text: "name"},
    %{name: "public.documents", col: "id", type: "document", perm: "can_view", sql_type: "int4", text: "title"},
    %{name: "public.comments", col: "document_id", type: "document", perm: "can_edit", sql_type: "text", text: "body"}
  ]

  # files in the folder tree, protected by folder#viewer declared as a hierarchy (hierarchy_check)
  @hierarchy_table %{name: "public.folder_files", col: "folder_id", type: "folder", perm: "viewer", sql_type: "int8", text: "name"}

  @modes [:listed, :per_row, :per_row_unbatched, :generic_listed_plan, :generic_per_row_plan, :indexed]

  def main(argv) do
    {opts, _} =
      OptionParser.parse!(argv,
        strict: [seed: :integer, scale: :integer, subjects: :integer, list_limit: :integer, timeout_ms: :integer, build: :boolean, name: :string,
                 quick: :boolean, modes: :string, engine: :string]
      )

    # --quick: for iterating; the full run (every subject, mode and plan) is the one to trust
    quick = opts[:quick] || false
    modes =
      cond do
        opts[:modes] -> opts[:modes] |> String.split(",", trim: true) |> Enum.map(&String.to_existing_atom/1)
        quick -> @modes -- [:per_row_unbatched]
        true -> @modes
      end

    root = Path.expand("..", __DIR__)
    timed(:build, fn -> if Keyword.get(opts, :build, true), do: Stack.build(root) end)

    list_limit = opts[:list_limit] || 1
    scn = scenario(opts[:seed] || 1, opts[:scale] || 1)
    dim = scn.dataset.dim
    subjects = (for(i <- 0..(dim.users - 1), do: "u#{i}") |> Enum.take(opts[:subjects] || if(quick, do: 40, else: dim.users))) ++ ["nobody"]

    IO.puts("#{scn.name}: #{scn.dataset.tuples} tuples, #{length(subjects)} subjects, per-row LIST_LIMIT #{list_limit}")
    # a crashing task then exits this process through `after`, so the containers are removed
    Process.flag(:trap_exit, true)
    stack = Stack.new()

    ok? =
      try do
        # the engine as in production
        engine_opts = [engine: opts[:engine] || "openfga"]
        stack = timed(:stack, fn -> Stack.start(stack, scn, list_limit, opts[:timeout_ms], engine_opts) end)
        # the modes run at once and share this pool (the cached-plan modes open their own)
        conn = Stack.connect(stack, 16)
        rows = timed(:install, fn -> install(conn, dim) end)
        IO.puts("asking #{stack.engine_mod.name()} for every subject's objects")
        reference = timed(:reference, fn -> reference(stack, subjects) end)
        ctx = %{conn: conn, stack: stack, rows: rows, reference: reference, subjects: subjects, list_limit: list_limit,
                explain_every: if(quick, do: 10, else: 1)}

        IO.puts("modes #{Enum.join(modes, ", ")}")
        results =
          modes
          |> Task.async_stream(fn mode -> timed(mode, fn -> run(mode, ctx) end) end, max_concurrency: length(modes), timeout: :infinity)
          |> collect()
          |> Enum.concat()

        read_only = timed(:read_only, fn -> read_only_check(ctx) end)
        hierarchy = timed(:hierarchy, fn -> hierarchy_check(ctx) end)
        # after every mode: it changes the engine's data, which the modes compare against
        revoked = if :indexed in modes, do: timed(:revocation, fn -> revocation(ctx) end), else: []
        GenServer.stop(conn)
        report(results ++ read_only ++ hierarchy ++ revoked, scn, list_limit, opts)
      catch
        kind, reason ->
          for {c, log} <- Stack.logs(stack), do: IO.puts(:stderr, "---- #{c}\n#{String.slice(log, -3000, 3000)}")
          IO.puts(:stderr, Exception.format(kind, reason, __STACKTRACE__))
          false
      after
        Stack.stop(stack)
      end

    if not ok?, do: System.halt(1)
  end

  # Run f, recording how long it took under `phase` for the report.
  defp timed(phase, f) do
    {us, result} = :timer.tc(f)
    :persistent_term.put({__MODULE__, :timings}, [{phase, us / 1_000_000} | :persistent_term.get({__MODULE__, :timings}, [])])
    result
  end

  defp timings, do: :persistent_term.get({__MODULE__, :timings}, []) |> Enum.reverse()

  # The random scenario with numeric object ids, so the tables can use integer keys. A folder's
  # grants are written as viewer_direct (folder#viewer unchanged: viewer_direct or viewer from
  # parent), so hierarchy_check can declare the tree to pg_auth.
  defp scenario(seed, scale) do
    [scn] = Compare.Loader.Random.load(seed: seed, scale: scale, ops: 1)
    renumber = fn s -> s |> String.replace(~r/^document:d(\d+)/, "document:\\1") |> String.replace(~r/^folder:f(\d+)/, "folder:\\1") end
    direct = fn
      %{object: "folder:" <> _, relation: "viewer"} = t -> %{t | relation: "viewer_direct"}
      t -> t
    end
    folder_viewer = "define viewer: [user, group#member] or viewer from parent\n"
    model = fn m ->
      if not String.contains?(m, folder_viewer), do: raise("the random model's folder#viewer changed")
      String.replace(m, folder_viewer, "define viewer_direct: [user, group#member]\n      define viewer: viewer_direct or viewer from parent\n", global: false)
    end
    stages = for st <- scn.stages, do: %{st | model: model.(st.model), ops: [], tuples: Enum.map(st.tuples, &(%{&1 | object: renumber.(&1.object), user: renumber.(&1.user)} |> direct.()))}
    %{scn | stages: stages}
  end

  defp install(conn, dim) do
    q!(conn, "create extension pg_auth")

    for sql <- [
          "create table public.folders (id int8 primary key, name text)",
          "create table public.documents (id int4 primary key, folder_id int8, title text)",
          # several comments per document, keyed by its id as text; one points at no document
          "create table public.comments (id int4 primary key, document_id text not null, body text)",
          "create index on public.comments (document_id)",
          "insert into public.folders select i, 'folder ' || i from generate_series(0, #{dim.folders - 1}) i",
          "insert into public.documents select i, i % #{dim.folders}, 'doc ' || i from generate_series(0, #{dim.docs - 1}) i",
          "insert into public.comments select d * 3 + k, d::text, 'c' from generate_series(0, #{dim.docs - 1}) d, generate_series(0, d % 3) k",
          "insert into public.comments values (-1, '999999', 'orphan')",
          "select authz_internal.protect('public.folders', 'folder', 'id', 'viewer')",
          "select authz_internal.protect('public.documents', 'document', 'id', 'can_view')",
          "select authz_internal.protect('public.comments', 'document', 'document_id', 'can_edit')",
          "create role parity_user nologin",
          "grant select on all tables in schema public to parity_user",
          "analyze"
        ],
        do: q!(conn, sql)

    # every row's primary key and the object id that decides it, read before protection applies
    for t <- @tables, into: %{} do
      %{rows: rows} = q!(conn, "select id, #{t.col}::text from #{t.name}")
      {t.name, rows}
    end
  end

  defp reference(stack, subjects) do
    pairs = for s <- subjects, t <- Enum.uniq_by(@tables, &{&1.type, &1.perm}), do: {s, t.type, t.perm}

    pairs
    |> Task.async_stream(
      fn {s, type, perm} ->
        op = %{kind: :list_objects, subject: "user:#{s}", relation: perm, type: type, contextual_tuples: [], context: %{}}
        {:ok, objects} = stack.engine_mod.list_objects(stack.engine, op)
        {{s, type, perm}, MapSet.new(objects, &String.replace_prefix(&1, "#{type}:", ""))}
      end,
      max_concurrency: 16,
      timeout: 120_000
    )
    |> collect()
    |> Map.new()
  end

  # Task results; exits are trapped, so a failed task arrives as {:exit, reason}: re-raise it here.
  defp collect(stream) do
    Enum.map(stream, fn
      {:ok, r} -> r
      {:exit, {e, stack}} when is_exception(e) -> reraise e, stack
      {:exit, reason} -> exit(reason)
    end)
  end

  defp expected(ctx, s, t) do
    allowed = ctx.reference[{s, t.type, t.perm}]
    for [pk, obj] <- ctx.rows[t.name], MapSet.member?(allowed, obj), into: MapSet.new(), do: pk
  end

  # The mode the planner should pick: listed when the engine lists the subject's objects, else
  # rows checked in batches (AuthzBatch), or one by one with pg_auth.batch_check off.
  defp expected_mode(ctx, mode, s, t) do
    n = MapSet.size(ctx.reference[{s, t.type, t.perm}])

    cond do
      mode == :listed or n < ctx.list_limit -> "listed ids"
      mode == :per_row_unbatched -> "per-row check"
      true -> "batched check"
    end
  end

  # One subject's rows through the per-row sidecar, labelled by where the answer came from.
  defp indexed_result(ctx, mode, s, t, want \\ nil) do
    {:ok, r} =
      Postgrex.transaction(ctx.conn, fn c ->
        as_subject(c, :per_row, s)
        [[checked0, looked0]] = q!(c, "select ids_checked, index_lookups from authz.stats()").rows
        {us, %{rows: rows}} = :timer.tc(fn -> q!(c, "select id from #{t.name}") end)
        [[checked, looked]] = q!(c, "select ids_checked, index_lookups from authz.stats()").rows
        from =
          cond do
            checked > checked0 -> "engine"
            looked > looked0 -> "permission index"
            true -> "listed ids"
          end

        want_from = want || if expected_mode(ctx, :per_row, s, t) == "listed ids", do: "listed ids", else: "permission index"
        result(mode, s, t, MapSet.new(rows, &hd/1), expected(ctx, s, t), from, want_from, us)
      end)

    r
  end

  # A subject too broad for the engine to list, listed from its fresh permission index set.
  defp index_listed_result(ctx, s, t) do
    {:ok, r} =
      Postgrex.transaction(ctx.conn, fn c ->
        as_subject(c, :per_row, s)
        q!(c, "set local pg_auth.index_list_limit = 10000")
        [[checked0]] = q!(c, "select ids_checked from authz.stats()").rows
        {us, %{rows: rows}} = :timer.tc(fn -> q!(c, "select id from #{t.name}") end)
        [[checked]] = q!(c, "select ids_checked from authz.stats()").rows
        from = if checked > checked0, do: "engine", else: "Postgres"
        result(:index_listed, s, t, MapSet.new(rows, &hd/1), expected(ctx, s, t), from, "Postgres", us)
      end)

    r
  end

  defp fresh_sets(ctx), do: hd(hd(q!(ctx.conn, "select count(*) from authz_internal.permission_sets where status = 'fresh'").rows))

  defp wait_until(f, ms) do
    cond do
      f.() -> true
      ms <= 0 -> false
      true -> Process.sleep(200) && wait_until(f, ms - 200)
    end
  end

  # Revoke one direct grant of a subject answered from the index: the change feed must make its
  # set stale, and once the engine's caches expire (OpenFGA) or its reads reach the write's
  # revision (SpiceDB) the subject must see exactly the engine's new answer (read consistently).
  defp revocation(ctx) do
    t = Enum.find(@tables, &(&1.type == "folder"))

    candidate =
      Enum.find_value(ctx.subjects, fn s ->
        n = MapSet.size(ctx.reference[{s, t.type, t.perm}])
        grant = n >= ctx.list_limit && Enum.find(ctx.stack.tuples, &(&1.user == "user:#{s}" and &1.relation == "viewer_direct" and String.starts_with?(&1.object, "folder:")))
        if grant, do: {s, grant}
      end)

    case candidate do
      nil ->
        [%{mode: :revocation, table: t.name, ok: true, skipped: "no indexed subject with a direct folder grant"}]

      {s, grant} ->
        %{engine: engine, engine_mod: mod} = ctx.stack
        :ok = mod.delete_tuples(engine, [grant])
        {us, stale?} = :timer.tc(fn -> wait_until(fn -> fresh_sets(ctx) == 0 end, 30_000) end)
        if not stale?, do: raise("the change feed did not invalidate the permission index within 30 s")
        IO.puts("revocation: index invalidated #{div(us, 1000)} ms after the write")
        # the engine may still answer with the old grant (OpenFGA's caches, on as in production,
        # 10 s TTL; SpiceDB's quantized revisions, 5 s): a revocation reaches pg_auth's engine
        # path only after them
        Process.sleep(mod.revocation_ms(engine) + 2_000)

        op = %{kind: :list_objects, subject: "user:#{s}", relation: t.perm, type: t.type, contextual_tuples: [], context: %{}, consistency: :strong}
        {:ok, now} = mod.list_objects(engine, op)
        ctx = put_in(ctx.reference[{s, t.type, t.perm}], MapSet.new(now, &String.replace_prefix(&1, "folder:", "")))
        # never the stale set: the engine, or listed ids when few objects are left
        want = if MapSet.size(ctx.reference[{s, t.type, t.perm}]) >= ctx.list_limit, do: "engine", else: "listed ids"
        [indexed_result(ctx, :revocation, s, t, want) | advice_check(ctx) ++ fill_wait_check(ctx, s)]
    end
  end

  # folder#viewer declared as a hierarchy over a table holding the engine's folder tree: the engine
  # lists only direct grants, Postgres expands them. Every subject must still see exactly the
  # files of the folders ListObjects returns for folder#viewer: listed ids through the listed
  # sidecar; through the per-row one, a subject with too many direct grants to list is answered
  # by the engine as before. The declaration is dropped afterwards.
  defp hierarchy_check(ctx) do
    t = @hierarchy_table
    parents = for %{object: "folder:" <> id, relation: "parent", user: "folder:" <> parent} <- ctx.stack.tuples, do: {id, parent}
    if parents == [], do: raise("the random scenario has no folder tree")
    values = Enum.map_join(parents, ", ", fn {id, parent} -> "(#{id}, #{parent})" end)

    for sql <- [
          "create table public.folder_tree (id int8 primary key, parent_id int8)",
          "insert into public.folder_tree select id, null from public.folders",
          "update public.folder_tree t set parent_id = v.parent from (values #{values}) v(id, parent) where t.id = v.id",
          "create table public.folder_files (id int4 primary key, folder_id int8 not null, name text)",
          "insert into public.folder_files select f.id * 2 + k, f.id, 'file' from public.folders f, generate_series(0, 1) k",
          "select authz_internal.protect('public.folder_files', 'folder', 'folder_id', 'viewer', hierarchy => 'public.folder_tree(id, parent_id)', direct => 'viewer_direct')",
          "grant select on public.folder_files to parity_user"
        ],
        do: q!(ctx.conn, sql)

    %{rows: rows} = q!(ctx.conn, "select id, folder_id::text from public.folder_files")
    ctx = put_in(ctx.rows[t.name], rows)

    results =
      for(s <- ctx.subjects, socket <- [:listed, :per_row], do: {s, socket})
      |> Task.async_stream(
        fn {s, socket} ->
          {:ok, r} =
            Postgrex.transaction(ctx.conn, fn c ->
              as_subject(c, socket, s)
              sql = "select id from #{t.name}"
              [[got_mode]] = q!(c, "select mode from authz.explain($1)", [sql]).rows
              {us, %{rows: rows}} = :timer.tc(fn -> q!(c, sql) end)
              # the listed sidecar lists every direct grant, so the expansion always applies
              want_mode = if socket == :listed, do: "listed ids", else: got_mode
              result(:hierarchy, s, t, MapSet.new(rows, &hd/1), expected(ctx, s, t), got_mode, want_mode, us)
            end)

          r
        end,
        max_concurrency: 8,
        timeout: 120_000
      )
      |> collect()

    q!(ctx.conn, "select authz_internal.drop_hierarchy('folder', 'viewer')")
    results
  end

  # Reads stay read-only transactions: answering a read (resolving the subject, listing, the
  # permission index, engine checks) must not assign a transaction id, which would make every
  # read request a writing transaction and fail on a read replica.
  defp read_only_check(ctx) do
    for t <- @tables,
        {listed, broad} <- [{Enum.find(ctx.subjects, &(expected_mode(ctx, :per_row, &1, t) == "listed ids")), Enum.find(ctx.subjects, &(expected_mode(ctx, :per_row, &1, t) != "listed ids"))}],
        {label, s, settings} <- [{"listed", listed, []}, {"index", broad, []}, {"engine", broad, ["set local pg_auth.permission_index = off"]}],
        s != nil do
      {:ok, xid} =
        Postgrex.transaction(ctx.conn, fn c ->
          # superuser-only settings first, before the subject's role
          for sql <- settings, do: q!(c, sql)
          as_subject(c, :per_row, s)
          q!(c, "select count(*) from #{t.name}")
          q!(c, "select count(*) from #{t.name} where id = 1")
          q!(c, "select mode from authz.explain($1)", ["select id from #{t.name}"])
          hd(hd(q!(c, "select txid_current_if_assigned()").rows))
        end)

      plan = if xid, do: "assigned xid #{xid}", else: "read-only #{label}"
      %{mode: :read_only, subject: "user:#{s}", table: t.name, ok: xid == nil, rows: 0, plan: plan, expected_plan: "read-only #{label}", ms: 0.0, extra: [], missing: []}
    end
  end

  # Every set is stale now. A heavy read (here: any read past pg_auth.fill_wait_after = 1 id
  # sent to the engine) waits for its subject's set to be refilled, then answers the rest from
  # the index, with a snapshot that sees the set committed meanwhile: same rows as the engine.
  defp fill_wait_check(ctx, revoked) do
    t = Enum.find(@tables, &(&1.type == "document"))
    broad = Enum.filter(ctx.subjects, &(expected_mode(ctx, :per_row, &1, t) != "listed ids"))

    # not the advice check's subject (its read already asked for a fill), nor the revoked one
    case Enum.find(Enum.drop(broad, 1), &(&1 != revoked)) do
      nil ->
        [%{mode: :fill_wait, table: t.name, ok: true, skipped: "no second broad subject"}]

      s ->
        {:ok, {us, rows, advice}} =
          Postgrex.transaction(ctx.conn, fn c ->
            q!(c, "set local pg_auth.fill_wait_after = 1")
            q!(c, "set local pg_auth.fill_wait_ms = 30000")
            as_subject(c, :per_row, s)
            {us, %{rows: rows}} = :timer.tc(fn -> q!(c, "select id from #{t.name}") end)
            {us, rows, q!(c, "select index_lookups, advice from authz.last_statement_advice()").rows}
          end)

        from = case advice do
          [[lookups, text]] when lookups > 0 and is_binary(text) -> if text =~ "answered the rest from Postgres", do: "waited, then permission index", else: text
          other -> inspect(other)
        end

        [result(:fill_wait, s, t, MapSet.new(rows, &hd/1), expected(ctx, s, t), from, "waited, then permission index", us)]
    end
  end

  # Every set is stale now: a broad subject's read asks the engine, and the next statement in
  # the session must explain why (authz.last_statement_advice, framed by the executor hooks).
  defp advice_check(ctx) do
    t = Enum.find(@tables, &(&1.type == "document"))

    case Enum.find(ctx.subjects, &(expected_mode(ctx, :per_row, &1, t) != "listed ids")) do
      nil ->
        []

      s ->
        {:ok, advice} =
          Postgrex.transaction(ctx.conn, fn c ->
            as_subject(c, :per_row, s)
            q!(c, "select count(*) from #{t.name}")
            q!(c, "select ids_checked, advice from authz.last_statement_advice()").rows
          end)

        ok = Enum.any?(advice, fn [ids, text] -> ids > 0 and is_binary(text) and text =~ ~r/not fresh|being filled/ end)
        [%{mode: :advice, subject: "user:#{s}", table: t.name, ok: ok, rows: 0, plan: if(ok, do: "advised", else: inspect(advice)), expected_plan: "advised", ms: 0.0, extra: [], missing: []}]
    end
  end

  # The permission index: fill the set of every subject the engine does not list, wait until the
  # sets are fresh, then every subject must see its rows with no engine checks at all.
  defp run(:indexed, ctx) do
    broad = for s <- ctx.subjects, t <- @tables, expected_mode(ctx, :per_row, s, t) != "listed ids", do: {s, t}

    # a read by the subject asks the sidecar to fill its set
    ask = fn pairs ->
      for {s, t} <- pairs do
        Postgrex.transaction(ctx.conn, fn c ->
          as_subject(c, :per_row, s)
          q!(c, "select count(*) from #{t.name}")
        end)
      end
    end

    ask.(broad)
    needed = length(Enum.uniq_by(broad, fn {s, t} -> {s, t.type, t.perm} end))

    # a set can go stale before it is used: every sidecar's feed marks all sets stale when it
    # starts following, and another sidecar may start while this one fills. Production refills it
    # on the subject's next read; so does this, every 2 s
    stale = fn ->
      sets = MapSet.new(q!(ctx.conn, "select subject, object_type, permission from authz_internal.permission_sets where status = 'stale'").rows)
      Enum.filter(broad, fn {s, t} -> MapSet.member?(sets, ["user:#{s}", t.type, t.perm]) end)
    end

    fresh? =
      Enum.reduce_while(1..150, false, fn _, _ ->
        if fresh_sets(ctx) >= needed, do: {:halt, true}, else: (Process.sleep(2000) && ask.(stale.()) && {:cont, false})
      end)

    fresh? ||
      raise "permission index sets did not become fresh: #{needed} needed, " <>
              inspect(q!(ctx.conn, "select status, count(*) from authz_internal.permission_sets group by 1 order by 1").rows)

    indexed =
      for(s <- ctx.subjects, t <- @tables, do: {s, t})
      |> Task.async_stream(fn {s, t} -> indexed_result(ctx, :indexed, s, t) end, max_concurrency: 8, timeout: 120_000)
      |> collect()

    # the same fresh sets, listed (pg_auth.index_list_limit): no engine checks either
    listed =
      broad
      |> Task.async_stream(fn {s, t} -> index_listed_result(ctx, s, t) end, max_concurrency: 8, timeout: 120_000)
      |> collect()

    indexed ++ listed
  end

  defp run(mode, ctx) when mode in [:listed, :per_row, :per_row_unbatched] do
    for(s <- ctx.subjects, t <- @tables, do: {s, t})
    |> Task.async_stream(
      fn {s, t} ->
        Postgrex.transaction(ctx.conn, fn c ->
          as_subject(c, if(mode == :listed, do: :listed, else: :per_row), s)
          if mode == :per_row_unbatched, do: q!(c, "set local pg_auth.batch_check = off")
          sql = "select id from #{t.name}"
          [[got_mode]] = q!(c, "select mode from authz.explain($1)", [sql]).rows
          {us, %{rows: rows}} = :timer.tc(fn -> q!(c, sql) end)
          # the same rows when output is computed from a column (a projection on the scan), with
          # every column it reads present
          %{rows: computed} = q!(c, "select id, length(#{t.text}) is not null from #{t.name}")
          got = if Enum.all?(computed, fn [id, present] -> id != nil and present end), do: MapSet.new(computed, &hd/1), else: MapSet.new([nil])
          got = if got == MapSet.new(rows, &hd/1), do: got, else: MapSet.put(got, :projection_differs)
          result(mode, s, t, got, expected(ctx, s, t), got_mode, expected_mode(ctx, mode, s, t), us)
        end)
        |> elem(1)
      end,
      max_concurrency: 8,
      timeout: 120_000
    )
    |> collect()
  end

  # A generic plan built for one subject and reused by every other: the planner's choice is
  # frozen, so execution must still answer for whoever runs it.
  #   generic_listed_plan:  planned for a listed subject, run through the per-row sidecar
  #   generic_per_row_plan: planned for a broad subject, run through the listed sidecar
  defp run(mode, ctx) do
    {plan_socket, run_socket, want_mode} =
      if mode == :generic_listed_plan, do: {:listed, :per_row, "InitPlan"}, else: {:per_row, :listed, "AuthzBatch"}

    @tables
    |> Task.async_stream(
      fn t ->
        case planner_subject(ctx, plan_socket, t) do
          nil -> [%{mode: mode, table: t.name, ok: true, skipped: "no subject gets the #{plan_kind(want_mode)} plan at LIST_LIMIT #{ctx.list_limit}"}]
          planner -> run_generic(mode, ctx, t, planner, plan_socket, run_socket, want_mode)
        end
      end,
      timeout: :infinity
    )
    |> collect()
    |> Enum.concat()
  end

  defp run_generic(mode, ctx, t, planner, plan_socket, run_socket, want_mode) do
    conn = Stack.connect(ctx.stack, 1)

    try do
      q!(conn, "prepare q as select id from #{t.name}")
      {plan, _, _} = execute(conn, plan_socket, planner)

      if not String.contains?(plan, want_mode),
        do: raise("#{mode} #{t.name}: expected a plan with #{want_mode} for #{planner}:\n#{plan}")

      for {s, i} <- Enum.with_index(ctx.subjects) do
        # the plan each subject runs must still be the one built for the planner subject
        # (checked for every subject, or every explain_every-th with --quick)
        {reused, rows, us} = execute(conn, run_socket, s, rem(i, ctx.explain_every) == 0)
        result(mode, s, t, MapSet.new(rows, &hd/1), expected(ctx, s, t), plan_kind(reused || plan), plan_kind(plan), us)
      end
    after
      GenServer.stop(conn)
    end
  end

  # A subject the planner gives the plan this mode reuses, or nil (none reaches LIST_LIMIT).
  defp planner_subject(ctx, socket, t) do
    want = if socket == :listed, do: "listed ids", else: "batched check"
    Enum.find(ctx.subjects, &(expected_mode(ctx, socket, &1, t) == want))
  end

  # -> {plan text, rows, microseconds}, for subject `s` through `socket`
  defp execute(conn, socket, s, explain? \\ true) do
    {:ok, out} =
      Postgrex.transaction(conn, fn c ->
        # the plan as built for the planner subject, not rebuilt for this one's breadth
        q!(c, "set local pg_auth.replan_on_breadth = off")
        as_subject(c, socket, s)
        q!(c, "set local plan_cache_mode = force_generic_plan")
        plan = if explain?, do: q!(c, "explain (costs off) execute q").rows |> Enum.map_join("\n", &hd/1)
        {us, %{rows: rows}} = :timer.tc(fn -> q!(c, "execute q") end)
        {plan, rows, us}
      end)

    out
  end

  defp plan_kind(plan) do
    cond do
      String.contains?(plan, "InitPlan") -> "listed ids"
      String.contains?(plan, "AuthzBatch") -> "batched check"
      true -> "per-row check"
    end
  end

  # Through the per-row sidecar, rows are checked: a subject with a fresh permission index set is
  # not listed from it (pg_auth.index_list_limit 0); `index_listed` covers that path.
  defp as_subject(c, socket, s) do
    limit = if socket == :per_row, do: "0", else: "10000"
    q!(c, "select set_config('pg_auth.sidecar_socket', $1, true), set_config('pg_auth.subject_id', $2, true), set_config('pg_auth.index_list_limit', $3, true)", [Stack.socket(socket), s, limit])
    q!(c, "set local role parity_user")
  end

  defp result(mode, s, t, got, want, got_mode, want_mode, us) do
    %{
      mode: mode,
      subject: "user:#{s}",
      table: t.name,
      ok: got == want and got_mode == want_mode,
      rows: MapSet.size(want),
      plan: got_mode,
      expected_plan: want_mode,
      ms: us / 1000,
      extra: MapSet.difference(got, want) |> Enum.sort() |> Enum.take(10),
      missing: MapSet.difference(want, got) |> Enum.sort() |> Enum.take(10)
    }
  end

  defp report(results, scn, list_limit, opts) do
    engine = opts[:engine] || "openfga"
    groups = Enum.group_by(results, &{&1.mode, &1.table})

    lines =
      for mode <- @modes ++ [:index_listed, :read_only, :hierarchy, :revocation, :advice, :fill_wait], t <- @tables ++ [@hierarchy_table], rs = groups[{mode, t.name}], rs != nil do
        label = "#{mode} | #{t.name} (#{t.type}##{t.perm}, #{t.sql_type})"

        case rs do
          [%{skipped: why}] -> "| #{label} | 0 | skipped: #{why} | | | |"
          _ -> line(label, rs)
        end
      end

    failures = Enum.reject(results, & &1.ok)
    skipped = Enum.filter(results, & &1[:skipped])

    md =
      Enum.join(
        [
          "# Enforcement parity: #{scn.name} on #{engine}",
          "",
          "#{scn.dataset.tuples} tuples; per-row sidecar LIST_LIMIT #{list_limit}. Each row: subjects queried, plan used,",
          "rows expected in total (from the engine's object lists), subjects whose rows or plan differed, and",
          "query latency (median / max ms, client wall time; queries run concurrently, so indicative only).",
          "",
          "| mode | table | subjects | plan | rows | mismatches | ms |",
          "|---|---|---|---|---|---|---|"
          | lines
        ] ++
          if(skipped == [], do: [], else: ["", "#{length(skipped)} skipped: lower --list-limit to cover them."]) ++
          if(failures == [], do: ["", "All subjects match."], else: ["", "## Mismatches", "" | Enum.map(Enum.take(failures, 50), &"- `#{inspect(&1, charlists: :as_lists)}`")]) ++
          ["", "Phase timings (s; modes run at once): " <> Enum.map_join(timings(), ", ", fn {p, sec} -> "#{p} #{Float.round(sec, 1)}" end)],
        "\n"
      )

    IO.puts("\n" <> md)
    run = opts[:name] || "parity-#{engine}-#{scn.name}-#{Calendar.strftime(DateTime.utc_now(), "%Y%m%d-%H%M%S")}"
    out = Path.join([__DIR__, "results", run])
    File.mkdir_p!(Path.dirname(out))
    File.write!(out <> ".md", md <> "\n")
    File.write!(out <> ".json", JSON.encode!(%{scenario: scn.name, engine: engine, list_limit: list_limit, results: results}))
    IO.puts("wrote #{Path.relative_to_cwd(out)}.{md,json}")
    failures == []
  end

  defp line(label, rs) do
    plans = rs |> Enum.frequencies_by(& &1.plan) |> Enum.map_join(", ", fn {p, n} -> "#{p} #{n}" end)
    bad = Enum.count(rs, &(not &1.ok))
    ms = rs |> Enum.map(& &1.ms) |> Enum.sort()
    p50 = Enum.at(ms, div(length(ms), 2))
    "| #{label} | #{length(rs)} | #{plans} | #{Enum.sum(Enum.map(rs, & &1.rows))} | #{bad} | #{fmt(p50)} / #{fmt(List.last(ms))} |"
  end

  defp fmt(ms), do: :erlang.float_to_binary(ms * 1.0, decimals: 1)

  defp q!(c, sql, params \\ []), do: Postgrex.query!(c, sql, params, timeout: :infinity)
end

Parity.main(System.argv())
