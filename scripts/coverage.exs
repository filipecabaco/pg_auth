#!/usr/bin/env elixir
# Line coverage of the extension and the sidecar: their unit tests (the extension's run inside
# Postgres backends), the integration checks in compare/ (parity against OpenFGA and SpiceDB,
# attribute parity against Cerbos) run on instrumented images, and both together, per file.
# See docs/development.md ("Coverage").
#
#   elixir scripts/coverage.exs                  # everything, ~30 minutes
#   elixir scripts/coverage.exs --only unit      # the unit tests again, a few minutes
#   elixir scripts/coverage.exs --only report    # the report from the last runs' files
#
# Writes target/coverage/: lcov files per suite and summary.md. The instrumented images are tagged
# :coverage, so other runs never pick them up.

defmodule Coverage do
  @root Path.expand("..", __DIR__)
  @out Path.join(@root, "target/coverage")

  def main(argv) do
    {opts, _} = OptionParser.parse!(argv, strict: [only: :string])
    File.mkdir_p!(@out)
    unit = if opts[:only] in [nil, "unit"], do: unit(), else: saved("unit")
    integration = if opts[:only] in [nil, "integration"], do: integration(), else: saved("integration")
    report(unit, integration)
  end

  # ---- unit tests ---------------------------------------------------------------------------------

  defp unit do
    step("extension tests (pgrx, Postgres 18)")
    env = llvm_cov_env()
    sh!("cargo", ["llvm-cov", "clean", "--workspace"], env: env)
    sh!("cargo", ["pgrx", "test", "pg18"], env: env)
    extension = Path.join(@out, "unit-extension.lcov")
    sh!("cargo", ["llvm-cov", "report", "--lcov", "--output-path", extension], env: env)

    step("sidecar unit tests")
    sidecar = Path.join(@out, "unit-sidecar.lcov")
    sh!("cargo", ["llvm-cov", "--lcov", "--output-path", sidecar], cd: Path.join(@root, "sidecar/query"))
    %{extension: lcov(extension), sidecar: lcov(sidecar)}
  end

  # cargo llvm-cov's environment for tests it does not run itself (pgrx starts Postgres, whose
  # backends load the instrumented library and write their profiles on exit)
  defp llvm_cov_env do
    {out, 0} = System.cmd("cargo", ["llvm-cov", "show-env", "--sh"], cd: @root, stderr_to_stdout: false)
    for "export " <> pair <- String.split(out, "\n"), [k, v] <- [String.split(pair, "=", parts: 2)], do: {k, unquoted(v)}
  end

  defp unquoted("'" <> rest), do: String.trim_trailing(rest, "'")
  defp unquoted(v), do: v

  # ---- integration checks -------------------------------------------------------------------------

  defp integration do
    profiles = Path.join(@out, "profiles")
    File.rm_rf!(profiles)
    File.mkdir_p!(profiles)
    # the containers' processes (postgres, the sidecar's user) write here
    File.chmod!(profiles, 0o777)

    step("instrumented images")
    flags = "-C instrument-coverage"
    sh!("docker", ["build", "--build-arg", "RUSTFLAGS=#{flags}", "-t", "pg_auth-sidecar:coverage", "sidecar/query"])
    sh!("docker", ["build", "--build-arg", "RUSTFLAGS=#{flags}", "-f", "bench/docker/Dockerfile", "-t", "pg_auth-bench:coverage", "."])

    env = [{"PG_AUTH_IMAGE_TAG", "coverage"}, {"PG_AUTH_COVERAGE_DIR", profiles}]
    for {what, args} <- [
          {"parity against OpenFGA", ["compare/parity.exs", "--seed", "1", "--no-build"]},
          {"parity against SpiceDB", ["compare/parity.exs", "--engine", "spicedb", "--seed", "1", "--no-build"]},
          {"attribute parity against Cerbos", ["compare/attributes.exs", "--seed", "1", "--no-build"]},
          {"relationship tables against OpenFGA and SpiceDB", ["compare/relationships.exs", "--no-build"]}
        ] do
      step(what)
      sh!("elixir", args, env: env)
    end

    step("profiles")
    binaries = Path.join(@out, "binaries")
    File.mkdir_p!(binaries)
    sidecar = extract("pg_auth-sidecar:coverage", "/usr/local/bin/pg-auth-sidecar", Path.join(binaries, "pg-auth-sidecar"))
    extension = extract("pg_auth-bench:coverage", "/usr/lib/postgresql/18/lib/pg_auth.so", Path.join(binaries, "pg_auth.so"))
    %{
      sidecar: export(profiles, "sidecar", sidecar, "/src", Path.join(@root, "sidecar/query")),
      extension: export(profiles, "postgres", extension, "/src", @root)
    }
  end

  defp extract(image, path, to) do
    {id, 0} = System.cmd("docker", ["create", image])
    id = String.trim(id)
    sh!("docker", ["cp", "#{id}:#{path}", to])
    sh!("docker", ["rm", id])
    to
  end

  # the profiles of one kind of process, merged and mapped back onto this checkout's sources
  defp export(profiles, kind, binary, built_at, source) do
    raw = Path.wildcard(Path.join(profiles, "#{kind}-*.profraw"))
    if raw == [], do: raise("no #{kind} profiles in #{profiles}")
    data = Path.join(@out, "#{kind}.profdata")
    sh!(tool("llvm-profdata"), ["merge", "-sparse", "-o", data | raw])
    {lcov, 0} =
      System.cmd(tool("llvm-cov"), [
        "export", "-format=lcov", "-instr-profile", data, binary,
        "-path-equivalence=#{built_at},#{source}",
        "-ignore-filename-regex=(/\\.cargo/|/rustc/|/usr/)"
      ])
    file = Path.join(@out, "integration-#{if kind == "postgres", do: "extension", else: kind}.lcov")
    File.write!(file, lcov)
    lcov(file, {built_at <> "/", source})
  end

  # llvm-profdata and llvm-cov from the toolchain's llvm-tools component (mise.toml)
  defp tool(name) do
    {sysroot, 0} = System.cmd("rustc", ["--print", "sysroot"])
    {info, 0} = System.cmd("rustc", ["-vV"])
    [host] = for "host: " <> h <- String.split(info, "\n"), do: h
    Path.join([String.trim(sysroot), "lib/rustlib", host, "bin", name])
  end

  # a suite's lcov files from an earlier run (--only reports the others from there)
  defp saved(suite) do
    sources = [extension: @root, sidecar: Path.join(@root, "sidecar/query")]
    for {crate, file} <- [extension: "#{suite}-extension.lcov", sidecar: "#{suite}-sidecar.lcov"],
        path = Path.join(@out, file),
        File.exists?(path),
        into: %{},
        do: {crate, lcov(path, {"/src/", sources[crate]})}
  end

  # ---- report -------------------------------------------------------------------------------------

  # file (repo-relative) => %{line => hits}; `built_at` maps an image's build directory onto this
  # checkout (llvm-cov's -path-equivalence does not apply to the paths it exports)
  defp lcov(path, built_at \\ nil) do
    path
    |> File.read!()
    |> String.split("end_of_record")
    |> Enum.reduce(%{}, fn record, acc ->
      lines = String.split(record, "\n", trim: true)
      case Enum.find_value(lines, fn "SF:" <> f -> f; _ -> nil end) do
        nil -> acc
        file ->
          hits = for "DA:" <> da <- lines, into: %{} do
            [line, count | _] = String.split(da, ",")
            {String.to_integer(line), String.to_integer(count)}
          end
          file = case built_at do
            {from, to} -> if String.starts_with?(file, from), do: Path.join(to, String.replace_prefix(file, from, "")), else: file
            nil -> file
          end
          Map.update(acc, Path.relative_to(file, @root), hits, &Map.merge(&1, hits, fn _, a, b -> a + b end))
      end
    end)
    # this checkout's sources only: not dependencies, the toolchain or generated bindings
    |> Map.filter(fn {file, _} -> not String.starts_with?(file, "/") and not String.starts_with?(file, "target/") end)
  end

  defp report(unit, integration) do
    files =
      for crate <- [:extension, :sidecar], suite <- [unit, integration], m = Map.get(suite, crate), file <- Map.keys(m), uniq: true, do: {crate, file}

    rows =
      for {crate, file} <- Enum.sort(files) do
        u = get_in(unit, [crate, file]) || %{}
        i = get_in(integration, [crate, file]) || %{}
        all = Map.merge(u, i, fn _, a, b -> a + b end)
        {file, pct(u), pct(i), pct(all), map_size(all), covered(all)}
      end

    total = fn suite ->
      maps = for {_, m} <- suite, {_, lines} <- m, do: lines
      {Enum.sum(Enum.map(maps, &covered/1)), Enum.sum(Enum.map(maps, &map_size/1))}
    end
    combined = rows |> Enum.reduce({0, 0}, fn {_, _, _, _, n, c}, {cc, nn} -> {cc + c, nn + n} end)

    table =
      ["| file | unit | integration | combined | lines |", "|---|---|---|---|---|"] ++
        Enum.map(rows, fn {file, u, i, all, n, _} -> "| `#{file}` | #{u} | #{i} | #{all} | #{n} |" end) ++
        ["| **total** | #{ratio(total.(unit))} | #{ratio(total.(integration))} | #{ratio(combined)} | #{elem(combined, 1)} |"]

    summary = Enum.join(["# Coverage (#{Date.utc_today()})", "" | table], "\n") <> "\n"
    File.write!(Path.join(@out, "summary.md"), summary)
    IO.puts("\n" <> summary)
  end

  defp covered(lines), do: Enum.count(lines, fn {_, hits} -> hits > 0 end)
  defp pct(lines) when map_size(lines) == 0, do: "-"
  defp pct(lines), do: ratio({covered(lines), map_size(lines)})
  defp ratio({_, 0}), do: "-"
  defp ratio({c, n}), do: "#{Float.round(100 * c / n, 1)}%"

  # ---- helpers ------------------------------------------------------------------------------------

  defp step(what), do: IO.puts(:stderr, IO.ANSI.bright() <> "==> " <> what <> IO.ANSI.reset())

  defp sh!(cmd, args, opts \\ []) do
    opts = Keyword.merge([cd: @root, into: IO.stream(:stderr, :line), stderr_to_stdout: true], opts)
    {_, code} = System.cmd(cmd, args, opts)
    if code != 0, do: raise("#{cmd} #{Enum.join(args, " ")} exited #{code}")
  end
end

Coverage.main(System.argv())
