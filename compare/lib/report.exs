defmodule Compare.Report do
  alias Compare.Scenario

  # ---- correctness --------------------------------------------------------------------------

  def correctness(results, engines, meta) do
    ids = Enum.map(engines, & &1.id)
    versions = Map.new(engines, &{&1.id, &1.mod.version(&1.state)})

    per_engine =
      Map.new(ids, fn id ->
        by = Enum.frequencies(for r <- results, do: r.vs_expect[id])

        reasons =
          for(r <- results, {:unsupported, reason} <- [r.outcomes[id]], do: reason |> String.split(":") |> hd())
          |> Enum.frequencies()

        {id, %{vs_expectation: by, unsupported_reasons: reasons}}
      end)

    by_kind = results |> Enum.frequencies_by(&{&1.kind, &1.verdict})
    verdicts = Enum.frequencies_by(results, & &1.verdict)

    pairs =
      for {a, i} <- Enum.with_index(ids), b <- Enum.drop(ids, i + 1) do
        both = Enum.filter(results, &(comparable?(&1.outcomes[a]) and comparable?(&1.outcomes[b])))
        agree = Enum.count(both, &(Scenario.normalize(&1.outcomes[a]) == Scenario.normalize(&1.outcomes[b])))
        %{a: a, b: b, comparable: length(both), agree: agree, disagree: length(both) - agree}
      end

    disagreements = Enum.filter(results, &(&1.verdict == :disagree))

    summary = %{
      meta: meta,
      versions: versions,
      operations: length(results),
      verdicts: verdicts,
      by_kind_verdict: Map.new(by_kind, fn {{k, v}, n} -> {"#{k}/#{v}", n} end),
      per_engine: per_engine,
      pairs: pairs
    }

    %{summary: summary, disagreements: disagreements, results: results, ids: ids}
  end

  defp comparable?({:ok, _}), do: true
  defp comparable?({:error, _}), do: true
  defp comparable?(_), do: false

  def print_correctness(%{summary: s, disagreements: dis, ids: ids}) do
    IO.puts("\n" <> correctness_text(s, ids))

    unless dis == [] do
      IO.puts("\nfirst disagreements:")
      for d <- Enum.take(dis, 15), do: IO.puts(describe(d, ids))
    end
  end

  defp describe(d, ids) do
    outs = for id <- ids, do: "#{id}=#{Scenario.show(d.outcomes[id])}"
    exp = if d.expect, do: " expected=#{Scenario.show(d.expect)}", else: ""
    "- #{d.id}#{exp}\n    " <> Enum.join(outs, "\n    ")
  end

  defp correctness_text(s, ids) do
    w = ids |> Enum.map(&String.length/1) |> Enum.max() |> max(10)
    pad = &String.pad_trailing(to_string(&1), &2)
    lead = &String.pad_leading(to_string(&1), &2)

    head = "versions: " <> Enum.map_join(s.versions, "; ", fn {k, v} -> "#{k}=#{v}" end)

    v = s.verdicts
    verdict = "operations: #{s.operations}  agree #{v[:agree] || 0}  disagree #{v[:disagree] || 0}  single-engine #{v[:single] || 0}"

    statuses = [:pass, :fail, :no_expectation, :unsupported]

    eng =
      [pad.("engine vs expectation", w + 2) <> Enum.map_join(statuses, "", &lead.(&1, 16))] ++
        for id <- ids do
          c = s.per_engine[id].vs_expectation
          pad.(id, w + 2) <> Enum.map_join(statuses, "", &lead.(c[&1] || 0, 16))
        end

    kinds = [:check, :list_objects, :list_users]
    vs = [:agree, :disagree, :single]

    kind_rows =
      [pad.("kind", 16) <> Enum.map_join(vs, "", &lead.(&1, 12))] ++
        for k <- kinds do
          pad.(k, 16) <> Enum.map_join(vs, "", &lead.(s.by_kind_verdict["#{k}/#{&1}"] || 0, 12))
        end

    pair_rows =
      for p <- s.pairs,
          do: "#{p.a} vs #{p.b}: #{p.agree}/#{p.comparable} agree (#{pct(p.agree, p.comparable)}), #{p.disagree} disagree"

    reasons =
      for id <- ids, {r, n} <- Enum.sort(s.per_engine[id].unsupported_reasons), do: "  #{pad.(id, w)} #{pad.(r, 24)} #{n}"

    Enum.join(
      [head, verdict, "", Enum.join(eng, "\n"), "", Enum.join(kind_rows, "\n"), "", "pairwise (operations both engines answered):"] ++
        pair_rows ++ ["", "unsupported by reason:"] ++ reasons,
      "\n"
    )
  end

  defp pct(_, 0), do: "n/a"
  defp pct(a, b), do: "#{Float.round(a * 100 / b, 1)}%"

  def correctness_markdown(%{summary: s, disagreements: dis, results: results, ids: ids}, run) do
    fails =
      for id <- ids do
        {id, Enum.filter(results, &(&1.vs_expect[id] == :fail))}
      end

    """
    # Correctness comparison: #{run}

    #{meta_md(s.meta)}

    ```
    #{correctness_text(s, ids)}
    ```

    ## Disagreements between engines (#{length(dis)})

    #{if dis == [], do: "None.", else: Enum.map_join(Enum.take(dis, 50), "\n\n", &"```\n#{describe(&1, ids)}\n    request: #{inspect(&1.request, limit: 20, printable_limit: 200)}\n```")}
    #{if length(dis) > 50, do: "\n(first 50 of #{length(dis)}; see the JSON for all)", else: ""}

    ## Failures against the expectation, per engine

    #{Enum.map_join(fails, "\n", fn {id, f} -> "- #{id}: #{length(f)}" <> if(f == [], do: "", else: " (first: " <> Enum.map_join(Enum.take(f, 5), ", ", & &1.id) <> ")") end)}
    """
  end

  defp meta_md(meta), do: Enum.map_join(meta, "\n", fn {k, v} -> "- #{k}: #{inspect(v)}" end)

  def correctness_json(%{summary: s, results: results}) do
    jsonable(%{summary: s, results: Enum.map(results, &result_json/1)})
  end

  def bench_json(rows, meta), do: jsonable(%{meta: meta, rows: rows})

  defp result_json(r) do
    %{r | outcomes: Map.new(r.outcomes, fn {id, o} -> {id, outcome_json(o)} end), expect: r.expect && Scenario.show(r.expect)}
  end

  defp outcome_json({:ok, v}), do: %{status: "ok", value: json_value(v)}
  defp outcome_json({:error, m}), do: %{status: "error", message: to_string(m)}
  defp outcome_json({:unsupported, m}), do: %{status: "unsupported", reason: m}

  defp json_value(l) when is_list(l), do: Enum.sort(l)
  defp json_value(v), do: v

  defp jsonable(%MapSet{} = m), do: m |> MapSet.to_list() |> Enum.sort()
  defp jsonable(t) when is_tuple(t), do: t |> Tuple.to_list() |> jsonable()
  defp jsonable(m) when is_map(m), do: Map.new(m, fn {k, v} -> {to_string(k), jsonable(v)} end)
  defp jsonable(l) when is_list(l), do: Enum.map(l, &jsonable/1)
  defp jsonable(a) when is_atom(a) and a not in [nil, true, false], do: Atom.to_string(a)
  defp jsonable(x), do: x

  # ---- bench --------------------------------------------------------------------------------

  def bench_markdown(rows, meta) do
    kinds = [:check, :list_objects, :list_users]

    tables =
      for k <- kinds do
        lines =
          for r <- rows do
            case r.kinds[k] do
              %{unsupported: why} ->
                "| #{r.engine} | unsupported | | | | | | #{why} |"

              %{sequential: s, concurrent: c} ->
                "| #{r.engine} | #{s.p50_ms} | #{s.p95_ms} | #{s.p99_ms} | #{s.ops_per_s} | #{c.ops_per_s} (c=#{c.concurrency}) | #{c.p95_ms} | #{s.errors + c.errors} |"
            end
          end

        "### #{k}\n\n| engine | p50 ms | p95 ms | p99 ms | ops/s (c=1) | ops/s (concurrent) | p95 ms (concurrent) | errors |\n|---|---|---|---|---|---|---|---|\n" <>
          Enum.join(lines, "\n")
      end

    load =
      "| engine | version | tuples | write ms | tuples/s | model ms | refused |\n|---|---|---|---|---|---|---|\n" <>
        Enum.map_join(rows, "\n", fn r ->
          "| #{r.engine} | #{r.version} | #{r.load.tuples} | #{r.load.write_ms} | #{r.load.tuples_per_s} | #{r.load.model_ms} | #{r.load.refused_tuples} |"
        end)


    """
    # Benchmark

    #{meta_md(meta)}

    Latency is measured around each adapter call, including its transport (OpenFGA: HTTP/1.1 with
    JSON through the Docker port mapping, a Postgres datastore). Engines run one at a time in
    containers on the same host.

    ## Load

    #{load}

    #{Enum.join(tables, "\n\n")}

    """
  end

  def print_bench(rows) do
    for r <- rows do
      IO.puts("\n#{r.engine} (#{r.version}): #{r.load.tuples} tuples in #{r.load.write_ms}ms")

      for {k, v} <- r.kinds do
        case v do
          %{unsupported: w} ->
            IO.puts("  #{String.pad_trailing(to_string(k), 13)} unsupported (#{w})")

          %{sequential: s, concurrent: c} ->
            IO.puts(
              "  #{String.pad_trailing(to_string(k), 13)} p50 #{s.p50_ms}ms p95 #{s.p95_ms}ms p99 #{s.p99_ms}ms  #{s.ops_per_s}/s  | c=#{c.concurrency}: #{c.ops_per_s}/s"
            )
        end
      end

    end
  end
end
