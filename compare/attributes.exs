#!/usr/bin/env elixir
# Attribute parity: rows protected by their attributes (authz.protect attributes =>) must show each
# principal exactly what Cerbos's CheckResources allows for those rows, through the compiled filter,
# through one cached plan reused by every principal, and through the per-row policy. See
# compare/README.md ("Attribute parity").
#
#   elixir compare/attributes.exs                  # seed 1: 400 rows, 80 principals
#   elixir compare/attributes.exs --seed 7 --rows 2000 --principals 200 --no-build

Mix.install([{:req, "~> 0.7"}, {:postgrex, "~> 0.22"}], config: [postgrex: [json_library: JSON]])

dir = Path.join(__DIR__, "lib")
for f <- ~w(engine docker engines/openfga engines/spicedb engines/cerbos stack), do: Code.require_file(Path.join(dir, f <> ".exs"))

defmodule Attributes do
  alias Compare.{Docker, Stack}
  alias Compare.Engines.Cerbos

  @roles ~w(admin user reviewer scorer tenant flagger outsider auditor guest)
  @attributes ~w(owner status size score flag tenant)
  @modes [:compiled, :cached, :per_row]

  def main(argv) do
    {opts, _} = OptionParser.parse!(argv, strict: [seed: :integer, rows: :integer, principals: :integer, build: :boolean])
    seed = opts[:seed] || 1
    :rand.seed(:exsss, {seed, seed, seed})
    root = Path.expand("..", __DIR__)
    if Keyword.get(opts, :build, true), do: Stack.build(root)

    Process.flag(:trap_exit, true)
    stack = Stack.new("attributes", "pg_auth_compare=1")

    ok? =
      try do
        stack = Stack.start_engine(stack, "", [default: 1000], engine: "cerbos", policies: Path.join(__DIR__, "scenarios/cerbos"))
        {conn, cached} = start_postgres(stack)
        rows = load(conn, opts[:rows] || 400)
        principals = for i <- 1..(opts[:principals] || 80), do: principal(i)
        IO.puts("seed #{seed}: #{length(rows)} rows, #{length(principals)} principals, #{length(@modes)} modes")

        results =
          for p <- principals, mode <- @modes do
            want = Cerbos.allowed(stack.engine, cerbos_principal(p), "item", "read", rows)
            got = visible(if(mode == :cached, do: cached, else: conn), p, mode)
            %{principal: p.claims, mode: mode, want: MapSet.size(want), ok: got == want,
              missing: MapSet.difference(want, got) |> Enum.take(5), extra: MapSet.difference(got, want) |> Enum.take(5)}
          end

        for b <- results |> Enum.reject(& &1.ok) |> Enum.uniq_by(& &1.principal.role) |> Enum.take(3) do
          IO.puts("plan for #{inspect(b.principal)} (#{b.mode}):\n" <> explain(conn, %{claims: b.principal}))
        end
        report(results)
      catch
        kind, reason ->
          for {c, log} <- Stack.logs(stack), do: IO.puts(:stderr, "---- #{c}\n#{String.slice(log, -2000, 2000)}")
          IO.puts(:stderr, Exception.format(kind, reason, __STACKTRACE__))
          false
      after
        Stack.stop(stack)
      end

    if not ok?, do: System.halt(1)
  end

  defp start_postgres(stack) do
    pg = Stack.track(stack, "#{stack.id}-postgres")
    {^pg, port} = Docker.run(pg, Stack.pg_image(), 5432, ["-c", "fsync=off"], Stack.pg_docker_args(stack) ++ Docker.coverage_args("postgres") ++ ["-e", "POSTGRES_PASSWORD=attributes"])
    Stack.wait(fn -> match?({_, 0}, System.cmd("docker", ["exec", pg, "pg_isready", "-h", "127.0.0.1", "-U", "postgres"])) end, pg)
    connect = fn -> Postgrex.start_link(hostname: "127.0.0.1", port: port, username: "postgres", password: "attributes", database: "postgres", pool_size: 1) end
    {:ok, conn} = retry(connect, 20)
    for sql <- ["create extension pg_auth", "alter system set pg_auth.sidecar_socket = '#{Stack.socket("default")}'", "select pg_reload_conf()",
                "select authz.configure(subject => 'jwt')", "create role reader"], do: Postgrex.query!(conn, sql, [])
    # the cached mode's own connection: one generic plan, reused by every principal
    {:ok, cached} = connect.()
    {conn, cached}
  end

  defp retry(f, 0), do: f.()

  defp retry(f, n) do
    case f.() do
      {:ok, c} -> {:ok, c}
      _ -> Process.sleep(500) && retry(f, n - 1)
    end
  end

  # Random rows, every attribute sometimes null; returns them as Cerbos sees them (the policy's
  # jsonb_build_object, read back), with their ids.
  defp load(conn, n) do
    Postgrex.query!(conn, "create table items (id int primary key, owner text, status text, size int, score numeric(4,1), flag boolean, tenant text)", [])
    pick = fn list -> if :rand.uniform(10) == 1, do: nil, else: Enum.random(list) end
    for chunk <- Enum.chunk_every(1..n, 500) do
      values =
        Enum.map_join(chunk, ", ", fn i ->
          row = [i, pick.(~w(u1 u2 u3 u4 u5 u6)), pick.(~w(draft published archived)), pick.(Enum.to_list(0..30)),
                 pick.(Enum.map(0..100, &(&1 / 10))), pick.([true, false]), pick.(~w(t1 t2 t3 t4))]
          "(" <> Enum.map_join(row, ", ", &literal/1) <> ")"
        end)
      Postgrex.query!(conn, "insert into items values #{values}", [])
    end
    attrs = Enum.map_join(@attributes, ", ", &"'#{&1}', #{&1}")
    Postgrex.query!(conn, "select authz.protect('items', 'item', 'id', 'read', attributes => array[#{Enum.map_join(@attributes, ", ", &"'#{&1}'")}])", [])
    for sql <- ["create index on items (owner)", "grant select on items to reader", "analyze items"], do: Postgrex.query!(conn, sql, [])
    for [id, attr] <- Postgrex.query!(conn, "select id::text, jsonb_build_object(#{attrs})::text from items", []).rows, do: %{id: id, attr: JSON.decode!(attr)}
  end

  defp literal(nil), do: "null"
  defp literal(v) when is_binary(v), do: "'#{v}'"
  defp literal(v), do: to_string(v)

  defp principal(i) do
    claims = %{sub: "u#{rem(i, 6) + 1}", role: Enum.random(@roles), level: Enum.random(0..10), tenants: Enum.take_random(~w(t1 t2 t3 t4), Enum.random(0..3))}
    %{claims: claims}
  end

  # the principal the extension sends: the claims' sub, its roles from the claim `role`, its
  # attributes the claims
  defp cerbos_principal(%{claims: c}), do: %{id: c.sub, roles: [c.role], attr: c}

  defp visible(conn, %{claims: claims}, mode) do
    {:ok, ids} =
      Postgrex.transaction(conn, fn c ->
        Postgrex.query!(c, "select set_config('request.jwt.claims', $1, true)", [JSON.encode!(claims)])
        if mode == :per_row, do: Postgrex.query!(c, "set local pg_auth.plan_lookup = off", [])
        Postgrex.query!(c, "set local role reader", [])
        case mode do
          :cached ->
            Postgrex.query!(c, "set local plan_cache_mode = force_generic_plan", [])
            prepared = Postgrex.query!(c, "select count(*) from pg_prepared_statements where name = 'page'", []).rows
            if prepared == [[0]], do: Postgrex.query!(c, "prepare page as select id::text from items", [])
            Postgrex.query!(c, "execute page", []).rows
          _ ->
            Postgrex.query!(c, "select id::text from items", []).rows
        end
      end)

    MapSet.new(ids, &hd/1)
  end

  defp explain(conn, %{claims: claims}) do
    {:ok, plan} =
      Postgrex.transaction(conn, fn c ->
        Postgrex.query!(c, "select set_config('request.jwt.claims', $1, true)", [JSON.encode!(claims)])
        Postgrex.query!(c, "set local role reader", [])
        Postgrex.query!(c, "explain (verbose, costs off) select id from items", []).rows |> Enum.map_join("\n", &hd/1)
      end)
    plan
  end

  defp report(results) do
    for mode <- @modes do
      rs = Enum.filter(results, &(&1.mode == mode))
      bad = Enum.reject(rs, & &1.ok)
      IO.puts("#{mode}: #{length(rs) - length(bad)}/#{length(rs)} principals match (#{Enum.sum(Enum.map(rs, & &1.want))} rows allowed in total)")
      for b <- Enum.take(bad, 5), do: IO.puts("  #{inspect(b.principal)}: missing #{inspect(b.missing)}, extra #{inspect(b.extra)}")
    end

    ok = Enum.all?(results, & &1.ok)
    IO.puts(if ok, do: "All principals match.", else: "Mismatches.")
    ok
  end
end

Attributes.main(System.argv())
