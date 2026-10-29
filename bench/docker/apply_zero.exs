# Publish a re-zero: the table bench/docker/pg_auth_table.exs printed goes into
# bench/docker/README.md, docs/research/rls-issues.md, docs/benchmarks.md and the landing page
# (overrides/home.html, between its `bench:` markers). From the repo root:
#   elixir bench/docker/pg_auth_table.exs <results dir> > table.md
#   elixir bench/docker/apply_zero.exs table.md <date> <commit>
[table_file, date, commit] = System.argv()
table = File.read!(table_file) |> String.trim()
[summary | _] = String.split(table, "\n", parts: 2)

# README: header date, intro, the summary sentence and the table
f = "bench/docker/README.md"
s = File.read!(f)
[head, rest] = String.split(s, ~r/## Results \([0-9-]+\)\n/, parts: 2)
[intro, _old] = String.split(rest, ~r/\nOver all \d+ pairs/, parts: 2)
tail = """

Where it still loses: `missing-policy-index` keeps its premise (no index on the column) in the
pg_auth variant while best-practice RLS adds the index; pg_auth's planning work per protected
table shows on sub-millisecond statements (joins of two protected tables, short writes); and
`tenant-skew-bad-estimates` (a tenant with half the table paging one kind of row) is open.
"""
File.write!(f, head <> "## Results (#{date})\n" <> String.trim_trailing(intro) <> "\n\n" <> table <> "\n" <> tail)

# rls-issues.md: the per-scenario column
f = "docs/research/rls-issues.md"
s = File.read!(f)
geo = for [_, id, g] <- Regex.scan(~r/^\| `([a-z0-9-]+)` \| ([0-9.]+x) \|/m, table), into: %{}, do: {id, g}
s =
  Regex.replace(~r/^(\| \d+ \| `([a-z0-9-]+)` \| perf \|.*\| )([0-9.]+x)([^|]*)\|$/m, s, fn _, pre, id, old, note ->
    pre <> Map.get(geo, id, old) <> note <> "|"
  end)
s = String.replace(s, ~r/subjects, \d{4}-\d{2}-\d{2}; below 1x is faster\)/, "subjects, #{date}; below 1x is faster)")
File.write!(f, s)

# README: the commit the table measured
f = "bench/docker/README.md"
File.write!(f, Regex.replace(~r/^Commit `[0-9a-f]+`, /m, File.read!(f), "Commit `#{commit}`, "))

# docs/benchmarks.md: the results block
f = "docs/benchmarks.md"
s = File.read!(f)
block = "<!-- results:start -->\nCommit `#{commit}`, #{date}.\n" <> table <> "\n<!-- results:end -->"
File.write!(f, Regex.replace(~r/<!-- results:start -->.*<!-- results:end -->/s, s, fn _ -> block end))

# home.html: the expectations section's figures and its strip's rows, sorted
[_, pairs, ratio] = Regex.run(~r/Over all (\d+) pairs pg_auth is at ([0-9.]+)x/, summary)
# each dot on the lowest row where it clears the last one by 3% of the strip's width (its log
# scale, 0.2x to 2x): dots at nearly the same ratio stack instead of overlapping
at = fn v -> (:math.log(v) - :math.log(0.2)) / (:math.log(2) - :math.log(0.2)) end
{placed, _} =
  geo
  |> Enum.map(fn {id, g} -> {id, g |> String.trim_trailing("x") |> String.to_float()} end)
  |> Enum.sort_by(fn {id, v} -> {v, id} end)
  |> Enum.map_reduce(%{}, fn {id, v}, last ->
    row = Enum.find(Stream.iterate(0, &(&1 + 1)), fn r -> not Map.has_key?(last, r) or at.(v) - last[r] >= 0.03 end)
    {{id, v, row}, Map.put(last, row, at.(v))}
  end)
rows = Enum.map_join(placed, "\n", fn {id, v, row} -> "  (\"#{id}\", #{:erlang.float_to_binary(v, decimals: 2)}, #{row}),"  end)
stats = ~s({% set bench = {"ratio": #{ratio}, "pairs": #{pairs}, "scenarios": #{map_size(geo)}} %})
f = "overrides/home.html"
s = File.read!(f)
s = Regex.replace(~r/(\{# bench:stats:start #\}\n).*?(\s*\{# bench:stats:end #\})/s, s, fn _, a, b -> a <> String.trim_trailing(stats) <> b end)
s = Regex.replace(~r/(\{# bench:rows:start #\}\n\s*\{% set rows = \[\n).*?(\n\s*\] %\})/s, s, fn _, a, b -> a <> rows <> b end)
File.write!(f, s)
IO.puts(summary)
