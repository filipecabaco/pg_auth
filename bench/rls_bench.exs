#!/usr/bin/env elixir
# Measures how RLS policy *shape* affects plans/latency for: functions in policies,
# joins in policies (nested RLS), unindexed / ORDER BY LIMIT queries, PK lookups, partitioning.
# Usage: elixir rls_bench.exs [--setup] [--reps 3]

Mix.install([{:postgrex, "~> 0.22"}, {:table_rex, "~> 4.1"}], config: [postgrex: [json_library: JSON]])

defmodule Bench do
  @docs 1_000_000
  @orgs 10_000

  def run(argv) do
    {opts, _} = OptionParser.parse!(argv, strict: [setup: :boolean, reps: :integer, only: :string])
    {:ok, c} =
      Postgrex.start_link(hostname: "localhost", port: 55432, username: "postgres", database: "postgres",
        timeout: :infinity)

    if opts[:setup], do: setup(c)
    reps = opts[:reps] || 3

    variants =
      case opts[:only] do
        nil -> variants()
        names -> Enum.filter(variants(), fn {n, _} -> n in String.split(names, ",") end)
      end

    rows =
      for {vname, policy} <- variants, {uname, uid} <- users(), {qname, sql} <- queries(policy, uid) do
        if is_binary(policy), do: apply_policy(c, policy)
        {ms, flags} = measure(c, uid, sql, reps, policy)
        IO.puts(:stderr, "#{vname} #{uname} #{qname} #{ms}")
        [vname, uname, qname, ms, flags]
      end

    TableRex.quick_render!(rows, ["policy", "user", "query", "median ms", "plan notes"]) |> IO.puts()
    File.write!(Path.join(__DIR__, "rls_bench_results.json"), JSON.encode!(rows))
  end

  # -- users: "small" sees ~200 docs (1 org); "power" sees ~50% of the table (5000 orgs)
  defp users, do: [{"small", 1}, {"power", 0}]

  defp queries(:baseline, uid) do
    allowed = "org_id in (select m.org_id from app.memberships m where m.user_id = #{uid})"

    [
      {"pk", "select * from app.documents where id = 424242 and #{allowed}"},
      {"count", "select count(*) from app.documents where #{allowed}"},
      {"recent", "select id from app.documents where #{allowed} order by created_at desc limit 20"},
      {"unindexed", "select id from app.documents where title like '%7777%' and #{allowed} limit 20"},
      {"join", "select count(*) from app.comments where document_id in (select id from app.documents where #{allowed})"},
      {"part_count", "select count(*) from app.documents_p where #{allowed}"},
      {"part_recent", "select id from app.documents_p where #{allowed} order by created_at desc limit 20"}
    ]
  end


  defp queries(_policy, _uid) do
    [
      {"pk", "select * from app.documents where id = 424242"},
      {"count", "select count(*) from app.documents"},
      {"recent", "select id from app.documents order by created_at desc limit 20"},
      {"unindexed", "select id from app.documents where title like '%7777%' limit 20"},
      {"join", "select count(*) from app.comments"},
      {"part_count", "select count(*) from app.documents_p"},
      {"part_recent", "select id from app.documents_p order by created_at desc limit 20"}
    ]
  end

  # Each variant: SQL expression used as USING clause for documents/documents_p, given `org_id`.
  defp variants do
    [
      {"plpgsql_fn", "app.is_member_plpgsql(org_id)"},
      {"sql_fn_secdef", "app.is_member_secdef(org_id)"},
      {"exists_corr",
       "exists (select 1 from app.memberships m where m.org_id = org_id_ref and m.user_id = app.uid())"},
      {"in_subquery", "org_id in (select m.org_id from app.memberships m where m.user_id = (select app.uid()))"},
      {"any_array_initplan",
       "org_id = any (array(select m.org_id from app.memberships m where m.user_id = (select app.uid())))"},
      {"any_secdef_fn_initplan", "org_id = any ((select app.my_orgs())::int[])"},
      # Not RLS: superuser + explicit semi-join, i.e. what a planner-level join could get.
      {"explicit_join_no_rls", :baseline}
    ]
  end

  defp apply_policy(c, expr) do
    for t <- ["documents", "documents_p"] do
      q!(c, "drop policy if exists p on app.#{t}")
      e = String.replace(expr, "org_id_ref", "#{t}.org_id")
      q!(c, "create policy p on app.#{t} for select to bench_user using (#{e})")
    end
  end

  defp measure(c, uid, sql, reps, policy) do
    plans =
      for _ <- 1..reps do
        Postgrex.transaction(c, fn conn ->
          if policy != :baseline, do: q!(conn, "set local role bench_user")

          q!(conn, "set local request.uid = '#{uid}'")
          q!(conn, "set local statement_timeout = '60s'")
          q!(conn, "set local max_parallel_workers_per_gather = 0")

          case Postgrex.query(conn, "explain (analyze, format json) " <> sql, []) do
            {:ok, %{rows: [[[plan]]]}} -> plan
            {:error, %{postgres: %{code: :query_canceled}}} -> :timeout
          end
        end)
        |> elem(1)
      end

    if Enum.any?(plans, &(&1 == :timeout)) do
      {">60000", "statement timeout"}
    else
      times = plans |> Enum.map(& &1["Execution Time"]) |> Enum.sort()
      {Float.round(Enum.at(times, div(reps, 2)), 2), notes(hd(plans)["Plan"])}
    end
  end

  # Summarize plan: scan types on target tables, per-row SubPlans, partitions actually scanned.
  defp notes(plan) do
    nodes = walk(plan)
    scans =
      nodes
      |> Enum.filter(&(&1["Relation Name"] in ~w(documents comments effective) or
                         String.starts_with?(&1["Relation Name"] || "", "documents_p")))
      |> Enum.map(&"#{abbrev(&1["Node Type"])}:#{&1["Relation Name"]}")
      |> Enum.uniq()

    hot_subplans =
      Enum.filter(nodes, &(String.starts_with?(&1["Subplan Name"] || "", "SubPlan") and (&1["Actual Loops"] || 0) > 1))

    parts =
      Enum.count(nodes, &(String.starts_with?(&1["Relation Name"] || "", "documents_p_") and &1["Actual Loops"] > 0))

    [
      Enum.join(scans |> Enum.reject(&String.contains?(&1, "documents_p_")), ","),
      if(parts > 0, do: "parts_scanned=#{parts}/16"),
      if(hot_subplans != [], do: "per-row SubPlan x#{hd(hot_subplans)["Actual Loops"]}")
    ]
    |> Enum.reject(&(&1 in [nil, ""]))
    |> Enum.join(" ")
  end

  defp walk(p), do: [p | Enum.flat_map(p["Plans"] || [], &walk/1)]
  defp abbrev("Seq Scan"), do: "Seq"
  defp abbrev("Index Scan"), do: "Idx"
  defp abbrev("Index Only Scan"), do: "IdxOnly"
  defp abbrev("Bitmap Heap Scan"), do: "Bitmap"
  defp abbrev(t), do: t

  defp setup(c) do
    IO.puts(:stderr, "setting up data...")

    """
    drop schema if exists app cascade;
    do $$ begin if not exists (select from pg_roles where rolname='bench_user') then create role bench_user; end if; end $$;
    create schema app;
    grant usage on schema app to bench_user;
    create function app.uid() returns int language sql stable as $f$ select current_setting('request.uid')::int $f$;

    create table app.memberships(user_id int, org_id int, primary key (user_id, org_id));
    create index on app.memberships(org_id, user_id);
    insert into app.memberships select u, 1 + (u::bigint * 7919) % #{@orgs} from generate_series(1, 50000) u;
    insert into app.memberships select u, 1 + (u::bigint * 104729) % #{@orgs} from generate_series(1, 50000) u on conflict do nothing;
    insert into app.memberships select 0, o from generate_series(1, #{@orgs}, 2) o;

    create table app.documents(id bigint primary key, org_id int not null, owner_id int not null,
                               title text not null, created_at timestamptz not null);
    insert into app.documents
      select i, 1 + (i % #{@orgs}), (i % 50000) + 1, md5(i::text), now() - (i || ' seconds')::interval
      from generate_series(1, #{@docs}) i;
    create index on app.documents(org_id);
    create index on app.documents(created_at);

    create table app.comments(id bigint primary key, document_id bigint not null, body text);
    insert into app.comments select i, 1 + (i::bigint * 31) % #{@docs}, 'c' || i from generate_series(1, #{@docs}) i;
    create index on app.comments(document_id);

    create table app.documents_p(id bigint not null, org_id int not null, owner_id int not null,
                                 title text not null, created_at timestamptz not null,
                                 primary key (org_id, id)) partition by hash (org_id);
    """
    |> exec(c)

    for i <- 0..15 do
      q!(c, "create table app.documents_p_#{i} partition of app.documents_p for values with (modulus 16, remainder #{i})")
    end

    """
    insert into app.documents_p select * from app.documents;
    create index on app.documents_p(created_at);

    -- function variants (STABLE, as people usually write them)
    create function app.is_member_plpgsql(o int) returns boolean language plpgsql stable as $f$
      begin return exists (select 1 from app.memberships where org_id = o and user_id = app.uid()); end $f$;
    create function app.is_member_secdef(o int) returns boolean language sql stable security definer as $f$
      select exists (select 1 from app.memberships where org_id = o and user_id = app.uid()) $f$;
    create function app.my_orgs() returns int[] language sql stable security definer as $f$
      select array(select org_id from app.memberships where user_id = app.uid()) $f$;

    -- nested RLS: memberships readable for own rows; comments visible iff parent document visible
    alter table app.memberships enable row level security;
    create policy p on app.memberships for select to bench_user using (user_id = (select app.uid()));
    alter table app.documents enable row level security;
    alter table app.documents_p enable row level security;
    alter table app.comments enable row level security;
    create policy p on app.comments for select to bench_user
      using (exists (select 1 from app.documents d where d.id = document_id));

    grant select on all tables in schema app to bench_user;
    grant execute on all functions in schema app to bench_user;
    analyze;
    """
    |> exec(c)
  end

  # Extended protocol runs one statement at a time; statements end with ";\n"
  # (function bodies keep their inner semicolons mid-line).
  defp exec(sql, c, opts \\ []) do
    sql
    |> String.split(~r/;\s*\n/)
    |> Enum.map(&String.trim/1)
    |> Enum.reject(&(&1 == ""))
    |> Enum.each(fn stmt ->
      {us, _} = :timer.tc(fn -> q!(c, stmt) end)
      if opts[:timed], do: IO.puts(:stderr, "#{div(us, 1000)} ms  #{String.slice(stmt, 0, 100)}")
    end)
  end

  defp q!(c, sql), do: Postgrex.query!(c, sql, [], timeout: :infinity)
end

Bench.run(System.argv())
