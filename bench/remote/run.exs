#!/usr/bin/env elixir
# What a sidecar away from Postgres costs under load: requests made as PostgREST makes them
# (claims set, role set, a read, commit) by concurrent clients, against best-practice RLS on the
# same data, with pg_auth asking its sidecar over the Unix socket, over HTTPS, and over HTTPS with
# network latency added in front of it (a relay whose packets netem delays). OpenFGA on its Postgres
# datastore with its caches on, the sidecar with its production defaults; Postgres held to 2 CPUs
# and 2 GB; fresh containers. See bench/remote/README.md.
#
#   elixir bench/remote/run.exs                         # builds the images first
#   elixir bench/remote/run.exs --no-build --seconds 20 --clients 4,16,64 --latency 1

Mix.install([{:req, "~> 0.7"}, {:postgrex, "~> 0.22"}], config: [postgrex: [json_library: JSON]])

dir = Path.expand("../../compare/lib", __DIR__)
for f <- ~w(engine docker engines/openfga engines/spicedb engines/cerbos stack), do: Code.require_file(Path.join(dir, f <> ".exs"))

defmodule Remote.Bench do
  alias Compare.{Docker, Stack}

  @rules Path.expand("../../docker/demo/rules/openfga.fga", __DIR__)
  @bare "model\n  schema 1.1\ntype user\n"
  @host "sidecar.pg-auth.test"
  @proxy "proxy.pg-auth.test"
  @tls "/run/pg_auth/tls"
  @secret "a-secret-the-sidecar-and-postgres-hold-0123456789"
  @relay "alpine:3.22"
  # 100 organisations: 20 large (4,000 documents each: their members are broad subjects, answered
  # from the permission index), 80 small (250 each: listed ids); 1,000 users, ten per organisation
  @large 20
  @orgs 100
  @users 1000

  def main(argv) do
    {opts, _} = OptionParser.parse!(argv, strict: [build: :boolean, seconds: :integer, clients: :string, latency: :integer])
    root = Path.expand("../..", __DIR__)
    if Keyword.get(opts, :build, true), do: Stack.build(root)
    seconds = Keyword.get(opts, :seconds, 20)
    clients = (opts[:clients] || "4,16,64") |> String.split(",") |> Enum.map(&String.to_integer/1)
    latency = Keyword.get(opts, :latency, 1)
    Process.flag(:trap_exit, true)
    stack = Stack.new("remote-bench", "pg_auth_compare=1")
    certs = certs(Path.join(System.tmp_dir!(), "pg_auth-remote-bench-#{stack.id}"))

    try do
      stack =
        Stack.start_engine(stack, @bare, [default: 1000],
          engine: "openfga", sidecar_cache_ms: nil,
          sidecar_env: [{"PG_AUTH_DATABASE_URL", "postgres://postgres:bench@#{stack.id}-postgres:5432/postgres"}])

      {conn, pg, port} = start_postgres(stack)
      for {name, path} <- certs, do: put(pg, path, "#{@tls}/#{name}")
      https = start_https_sidecar(stack)
      start_relay(stack, https, latency)
      load(conn)

      configs = [
        {"best-practice RLS", "documents_rls", %{}},
        {"pg_auth, Unix socket", "documents", %{"pg_auth.sidecar_url" => ""}},
        {"pg_auth, HTTPS", "documents", %{"pg_auth.sidecar_url" => "https://#{@host}:8443"}},
        {"pg_auth, HTTPS, added latency", "documents", %{"pg_auth.sidecar_url" => "https://#{@proxy}:8443"}}
      ]

      rows =
        for {label, table, settings} <- configs, n <- clients do
          settings!(conn, settings)
          IO.puts(:stderr, "#{label}, #{n} clients")
          {label, n, run(port, table, n, seconds, pg, stack.id), trips(port, table)}
        end

      report(rows, seconds, latency, root)
    after
      Stack.stop(stack)
      File.rm_rf(Path.join(System.tmp_dir!(), "pg_auth-remote-bench-#{stack.id}"))
    end
  end

  # sidecar round trips per request, and each one's time beyond the engine's, from authz.stats()
  # on one connection over 200 requests
  defp trips(port, table) do
    {:ok, c} = Postgrex.start_link(hostname: "127.0.0.1", port: port, username: "postgres", password: "bench", database: "postgres", pool_size: 1)
    read = fn -> hd(Postgrex.query!(c, "select sidecar_calls, sidecar_ms - engine_ms from authz.stats()", []).rows) end
    [calls0, ms0] = read.()
    for _ <- 1..200, do: request(c, table)
    [calls1, ms1] = read.()
    GenServer.stop(c)
    calls = calls1 - calls0
    {Float.round(calls / 200, 2), if(calls > 0, do: Float.round((ms1 - ms0) / calls, 2), else: 0.0)}
  end

  # PostgREST's shape of a request: the claims and the role set in the transaction, one read
  defp request(c, table) do
    user = :rand.uniform(@users)
    org = org_of(user)
    {sql, args} =
      if :rand.uniform(2) == 1,
        do: {"select id, title from #{table} where org_id = $1 order by id limit 20", [org]},
        else: {"select title from #{table} where id = $1", [doc_of(org)]}
    match?({:ok, _},
      Postgrex.transaction(c, fn t ->
        Postgrex.query!(t, "select set_config('request.jwt.claims', $1, true)", [JSON.encode!(%{sub: "u#{user}", role: "authenticated"})])
        Postgrex.query!(t, "set local role authenticated", [])
        Postgrex.query!(t, sql, args)
      end))
  end

  # CPU used over the measured window, read from each container's cgroup: Postgres's, and the
  # rest of the stack's (sidecars, engine, its datastore, the relay)
  defp run(port, table, n, seconds, pg, id) do
    warmup = 5_000
    stop_at = System.monotonic_time(:millisecond) + warmup + seconds * 1000
    clients = Task.async(fn -> clients(port, table, n, stop_at, warmup) end)
    Process.sleep(warmup)
    others = for c <- containers(id), c != pg, do: c
    {pg0, rest0} = {cpu_us(pg), Enum.sum(Enum.map(others, &cpu_us/1))}
    Process.sleep(seconds * 1000)
    {pg1, rest1} = {cpu_us(pg), Enum.sum(Enum.map(others, &cpu_us/1))}
    {lat, errors} = Task.await(clients, :infinity)
    {lat, errors, pg1 - pg0, rest1 - rest0}
  end

  defp containers(id) do
    {out, 0} = System.cmd("docker", ["ps", "--filter", "name=^#{id}-", "--format", "{{.Names}}"])
    String.split(out, "\n", trim: true)
  end

  defp cpu_us(container) do
    case System.cmd("docker", ["exec", container, "cat", "/sys/fs/cgroup/cpu.stat"], stderr_to_stdout: true) do
      {out, 0} -> out |> String.split("\n") |> Enum.find_value(0, &(match?("usage_usec " <> _, &1) && String.to_integer(String.trim_leading(&1, "usage_usec "))))
      _ -> 0
    end
  end

  defp clients(port, table, n, stop_at, warmup) do
    1..n
    |> Task.async_stream(
      fn i ->
        {:ok, c} = Postgrex.start_link(hostname: "127.0.0.1", port: port, username: "postgres", password: "bench", database: "postgres", pool_size: 1)
        :rand.seed(:exsss, {i, 7, 11})
        measure_from = System.monotonic_time(:millisecond) + warmup
        loop(c, table, stop_at, measure_from, [], 0)
      end,
      max_concurrency: n, timeout: :infinity
    )
    |> Enum.reduce({[], 0}, fn {:ok, {lat, errors}}, {all, e} -> {lat ++ all, e + errors} end)
  end

  defp loop(c, table, stop_at, measure_from, acc, errors) do
    now = System.monotonic_time(:millisecond)
    if now >= stop_at do
      GenServer.stop(c)
      {acc, errors}
    else
      t0 = System.monotonic_time(:microsecond)
      ok = request(c, table)
      us = System.monotonic_time(:microsecond) - t0
      cond do
        now < measure_from -> loop(c, table, stop_at, measure_from, acc, errors)
        ok -> loop(c, table, stop_at, measure_from, [us | acc], errors)
        true -> loop(c, table, stop_at, measure_from, acc, errors + 1)
      end
    end
  end

  # user u belongs to organisation (u mod 100) + 1; the first 20 are the large ones
  defp org_of(user), do: rem(user, @orgs) + 1
  defp docs_in(org), do: if(org <= @large, do: 4000, else: 250)
  defp first_doc(org), do: Enum.sum(for o <- 1..(org - 1)//1, do: docs_in(o)) + 1
  defp doc_of(org), do: first_doc(org) + :rand.uniform(docs_in(org)) - 1

  defp load(conn) do
    docs = Enum.flat_map(1..@orgs, fn o -> for d <- 0..(docs_in(o) - 1), do: {first_doc(o) + d, o} end)
    run_all(conn, [
      "create role authenticated nologin",
      "select authz.configure(subject => 'jwt')",
      "create table documents (id int primary key, org_id int not null, title text not null)",
      "create index on documents (org_id, id)",
      "create table memberships (org_id int not null, user_id text not null, role text not null, primary key (org_id, user_id))",
      "create table shares (doc_id int not null, user_id text not null, expires_at timestamptz, primary key (doc_id, user_id))",
      # best-practice RLS on a copy: an indexed membership lookup, the claim read once per statement
      "create table documents_rls (id int primary key, org_id int not null, title text not null)",
      "create index on documents_rls (org_id, id)",
      "create table memberships_rls (org_id int not null, user_id text not null, role text not null, primary key (user_id, org_id))",
      "alter table documents_rls enable row level security",
      "create policy members on documents_rls for select using (org_id in (select m.org_id from memberships_rls m where m.user_id = (select current_setting('request.jwt.claims', true)::jsonb ->> 'sub')))",
      "grant select on documents, memberships, shares, documents_rls, memberships_rls to authenticated"
    ])
    for chunk <- Enum.chunk_every(docs, 5000) do
      values = Enum.map_join(chunk, ",", fn {id, o} -> "(#{id}, #{o}, 'doc #{id}')" end)
      run_all(conn, ["insert into documents values #{values}", "insert into documents_rls values #{values}"])
    end
    members = Enum.map_join(1..@users, ",", fn u -> "(#{org_of(u)}, 'u#{u}', '#{if rem(u, 10) == 0, do: "admin", else: "member"}')" end)
    run_all(conn, ["insert into memberships values #{members}", "insert into memberships_rls values #{members}",
                   "analyze documents", "analyze documents_rls", "analyze memberships", "analyze memberships_rls"])
    Postgrex.query!(conn, "select authz.set_rules('openfga', 'model', $1)", [File.read!(@rules)], timeout: :infinity)
    IO.puts(:stderr, "delivering #{length(docs) + @users} relationships to OpenFGA")
    wait_until(fn -> Postgrex.query!(conn, "select count(*) from authz_internal.outbox where not_before is null", []).rows == [[0]] end, 1_800_000) ||
      raise "relationships not delivered"
  end

  defp report(rows, seconds, latency, root) do
    pct = fn sorted, p -> Enum.at(sorted, min(length(sorted) - 1, trunc(length(sorted) * p))) / 1000 end
    lines =
      for {label, n, {lat, errors, pg_us, rest_us}, {trips, transport}} <- rows do
        sorted = Enum.sort(lat)
        rps = round(length(sorted) / seconds)
        per = fn us -> round(us / max(length(sorted), 1)) end
        "| #{label} | #{n} | #{rps} | #{Float.round(pct.(sorted, 0.5), 2)} | #{Float.round(pct.(sorted, 0.95), 2)} | #{Float.round(pct.(sorted, 0.99), 2)} | #{trips} | #{transport} | #{per.(pg_us)} | #{per.(rest_us)} | #{errors} |"
      end
    # netem's delay is what the kernel's timers make of it (Docker Desktop's VM: several times the
    # setting), so the round trip it added is measured: a trip's time beyond the engine with the
    # relay, less without it
    per_trip = fn label -> for {^label, _, _, {_, ms}} <- rows, do: ms end
    added = Enum.zip_with(per_trip.("pg_auth, HTTPS, added latency"), per_trip.("pg_auth, HTTPS"), &(&1 - &2))
    added = Float.round(Enum.sum(added) / max(length(added), 1), 1)
    {sha, 0} = System.cmd("git", ~w(rev-parse --short HEAD), cd: root)
    text = """
    Commit `#{String.trim(sha)}`, #{Date.utc_today()}. #{seconds} s per row after a 5 s warm-up. Added latency: netem set to #{latency} ms each way, measured as #{added} ms more per round trip.

    | setup | clients | requests/s | p50 ms | p95 ms | p99 ms | sidecar round trips per request | ms per trip beyond the engine | Postgres CPU µs per request | rest of the stack's CPU µs per request | errors |
    |---|---|---|---|---|---|---|---|---|---|---|
    #{Enum.join(lines, "\n")}
    """
    IO.puts(text)
    out = Path.join(root, "bench/remote/results")
    File.mkdir_p!(out)
    File.write!(Path.join(out, "#{Date.utc_today()}-#{String.trim(sha)}.md"), text)
  end

  defp settings!(conn, values) do
    for {k, v} <- values, do: Postgrex.query!(conn, "alter system set #{k} = '#{v}'", [])
    Postgrex.query!(conn, "select pg_reload_conf()", [])
    Process.sleep(500)
  end

  # a relay with every packet it sends delayed by netem: to the sidecar on the way out, to
  # Postgres on the way back, so a request pays the added latency once each way however many
  # packets it takes
  defp start_relay(stack, upstream, ms) do
    name = Stack.track(stack, "#{stack.id}-relay")
    script =
      "apk add --no-cache iproute2 socat >/dev/null && tc qdisc add dev eth0 root netem delay #{ms}ms && " <>
        "exec socat TCP-LISTEN:8443,fork,reuseaddr,nodelay TCP:#{upstream}:8443,nodelay"
    {^name, _} = Docker.run(name, @relay, 8443, ["sh", "-c", script], ["--network", stack.net, "--network-alias", @proxy, "--cap-add", "NET_ADMIN"])
    up = fn -> match?({_, 0}, System.cmd("docker", ["exec", name, "sh", "-c", "tc qdisc show dev eth0 | grep -q netem && pgrep socat"], stderr_to_stdout: true)) end
    wait_until(up, 60_000) || raise "relay not up"
    name
  end

  defp start_https_sidecar(stack) do
    name = Stack.track(stack, "#{stack.id}-https")
    env =
      Enum.flat_map(stack.engine_mod.sidecar_env(stack.engine) ++ stack.engine_env, fn {k, v} -> ["-e", "#{k}=#{v}"] end) ++
        Enum.flat_map(
          ["PG_AUTH_LISTEN=0.0.0.0:8443", "PG_AUTH_SECRET_FILE=#{@tls}/secret", "PG_AUTH_TLS_CERT=#{@tls}/server.pem",
           "PG_AUTH_TLS_KEY=#{@tls}/server.key", "PG_AUTH_SOCKET=/tmp/unused.sock",
           "PG_AUTH_DATABASE_URL=postgres://postgres:bench@#{stack.id}-postgres:5432/postgres"],
          &["-e", &1])
    {^name, port} = Docker.run(name, Stack.sidecar_image(), 8443, [], ["--network-alias", @host] ++ Stack.pg_docker_args(stack) ++ env)
    health = fn -> match?({:ok, %{status: 200}}, Req.get("https://127.0.0.1:#{port}/healthz", retry: false, connect_options: [transport_opts: [verify: :verify_none]])) end
    Stack.wait(health, name)
    name
  end

  defp start_postgres(stack) do
    pg = Stack.track(stack, "#{stack.id}-postgres")
    {^pg, port} =
      Docker.run(pg, Stack.pg_image(), 5432, ["-c", "shared_buffers=512MB", "-c", "max_connections=100"],
        Stack.pg_docker_args(stack) ++ ["--cpus", "2", "--memory", "2g", "-e", "POSTGRES_PASSWORD=bench"])
    Stack.wait(fn -> match?({_, 0}, System.cmd("docker", ["exec", pg, "pg_isready", "-h", "127.0.0.1", "-U", "postgres"])) end, pg)
    {:ok, conn} = retry(fn -> Postgrex.start_link(hostname: "127.0.0.1", port: port, username: "postgres", password: "bench", database: "postgres", pool_size: 1) end, 20)
    run_all(conn, [
      "create extension pg_auth",
      "alter system set pg_auth.sidecar_socket = '#{Stack.socket("default")}'",
      "alter system set pg_auth.sidecar_secret_file = '#{@tls}/secret'",
      "alter system set pg_auth.sidecar_ca_file = '#{@tls}/ca.pem'",
      "select pg_reload_conf()"
    ])
    {conn, pg, port}
  end

  defp certs(dir) do
    File.mkdir_p!(dir)
    ssl = fn args -> {_, 0} = System.cmd("openssl", args, cd: dir, stderr_to_stdout: true) end
    ssl.(~w(req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 1 -subj /CN=pg_auth-bench-ca
            -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign -keyout ca.key -out ca.pem))
    ssl.(~w(req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj /CN=sidecar -keyout server.key -out server.csr))
    File.write!(Path.join(dir, "ext.cnf"), "subjectAltName=DNS:#{@host},DNS:#{@proxy}\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n")
    ssl.(~w(x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 1 -extfile ext.cnf -out server.pem))
    File.write!(Path.join(dir, "secret"), @secret <> "\n")
    for f <- ~w(ca.pem server.pem server.key secret), do: {f, Path.join(dir, f)}
  end

  defp put(pg, from, to) do
    {_, 0} = System.cmd("docker", ["exec", pg, "mkdir", "-p", Path.dirname(to)])
    {_, 0} = System.cmd("docker", ["cp", from, "#{pg}:#{to}"], stderr_to_stdout: true)
    {_, 0} = System.cmd("docker", ["exec", pg, "chmod", "0644", to])
  end

  defp retry(f, 0), do: f.()

  defp retry(f, n) do
    case f.() do
      {:ok, c} -> {:ok, c}
      _ -> Process.sleep(500) && retry(f, n - 1)
    end
  end

  defp run_all(conn, statements), do: Enum.each(statements, &Postgrex.query!(conn, &1, [], timeout: :infinity))

  defp wait_until(f, ms) do
    cond do
      f.() -> true
      ms <= 0 -> false
      true -> Process.sleep(500) && wait_until(f, ms - 500)
    end
  end
end

Remote.Bench.main(System.argv())
