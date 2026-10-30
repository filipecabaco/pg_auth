# Publish a re-zero: the table bench/docker/pg_auth_table.exs printed goes into
# bench/docker/README.md, docs/research/rls-issues.md, docs/benchmarks.md and the landing page's
# figures (overrides/home.html, between its `bench:stats` markers). From the repo root:
#   elixir bench/docker/pg_auth_table.exs <results dir> > table.md
#   elixir bench/docker/apply_zero.exs table.md <date> <commit>
# A run against SpiceDB (run.exs --engine spicedb) goes into docs/benchmarks.md's SpiceDB block
# only; the landing page and the catalogue keep the OpenFGA run:
#   elixir bench/docker/apply_zero.exs table.md <date> <commit> spicedb
# Every engine side by side (bench/docker/engines_table.exs) goes into its "All engines" block:
#   elixir bench/docker/engines_table.exs <openfga run> <spicedb run> > engines.md
#   elixir bench/docker/apply_zero.exs engines.md <date> <commit> engines
# and each engine's lane on the landing page (its `bench:engines` markers):
#   elixir bench/docker/engines_table.exs --home <openfga run> <spicedb run> > lanes.jinja
#   elixir bench/docker/apply_zero.exs lanes.jinja <date> <commit> home
[table_file, date, commit | engine] = System.argv()
table = File.read!(table_file) |> String.trim()
[summary | _] = String.split(table, "\n", parts: 2)

if engine == ["home"] do
  f = "overrides/home.html"
  s = Regex.replace(~r/(\{# bench:engines:start #\}\n).*?(\n\{# bench:engines:end #\})/s, File.read!(f), fn _, a, b -> a <> table <> b end)
  File.write!(f, s)
  IO.puts("overrides/home.html: each engine's lane")
  System.halt(0)
end

if engine == ["engines"] do
  f = "docs/benchmarks.md"
  block = "<!-- results-engines:start -->\nCommit `#{commit}`, #{date}.\n\n" <> table <> "\n<!-- results-engines:end -->"
  File.write!(f, Regex.replace(~r/<!-- results-engines:start -->.*<!-- results-engines:end -->/s, File.read!(f), fn _ -> block end))
  IO.puts("docs/benchmarks.md: all engines")
  System.halt(0)
end

if engine == ["spicedb"] do
  f = "docs/benchmarks.md"
  block = "<!-- results-spicedb:start -->\nCommit `#{commit}`, #{date}.\n" <> table <> "\n<!-- results-spicedb:end -->"
  File.write!(f, Regex.replace(~r/<!-- results-spicedb:start -->.*<!-- results-spicedb:end -->/s, File.read!(f), fn _ -> block end))
  IO.puts(summary)
  System.halt(0)
end

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

# home.html: the expectations section's figures
[_, pairs, ratio] = Regex.run(~r/Over all (\d+) pairs pg_auth is at ([0-9.]+)x/, summary)
stats = ~s({% set bench = {"ratio": #{ratio}, "pairs": #{pairs}, "scenarios": #{map_size(geo)}} %})
f = "overrides/home.html"
s = File.read!(f)
s = Regex.replace(~r/(\{# bench:stats:start #\}\n).*?(\s*\{# bench:stats:end #\})/s, s, fn _, a, b -> a <> String.trim_trailing(stats) <> b end)
File.write!(f, s)
IO.puts(summary)
