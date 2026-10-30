#!/usr/bin/env elixir
# The sidecar reached over HTTPS (pg_auth.sidecar_url) instead of its Unix socket, on real OpenFGA
# and SpiceDB: the demo's story told with the socket pointing nowhere, then the attacks the
# signed requests are there to stop (docs/proposals/external-providers.md). A test CA and a
# certificate for the sidecar's name are made with openssl. See compare/README.md.
#
#   elixir compare/remote.exs                       # openfga, spicedb
#   elixir compare/remote.exs --engine spicedb --no-build

Mix.install([{:req, "~> 0.7"}, {:postgrex, "~> 0.22"}], config: [postgrex: [json_library: JSON]])

dir = Path.join(__DIR__, "lib")
for f <- ~w(engine docker engines/openfga engines/spicedb engines/cerbos stack), do: Code.require_file(Path.join(dir, f <> ".exs"))

defmodule Remote do
  alias Compare.{Docker, Stack}

  @rules Path.expand("../docker/demo/rules", __DIR__)
  @files %{"openfga" => {"model", "openfga.fga"}, "spicedb" => {"schema", "spicedb.zed"}}
  @bare "model\n  schema 1.1\ntype user\n"
  @host "sidecar.pg-auth.test"
  # a proxy terminating TLS in front of a sidecar on plain HTTP, as a platform would; and one that
  # also changes the answers on their way back
  @proxy "proxy.pg-auth.test"
  @tamper "tamper.pg-auth.test"
  @tls "/run/pg_auth/tls"
  @secret "a-secret-the-sidecar-and-postgres-hold-0123456789"
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
    stack = Stack.new("remote", "pg_auth_compare=1")
    certs = certs(dir(stack))

    try do
      # a sidecar on a socket, first so its image sets up the volume they share; Postgres points
      # elsewhere until the cost is measured, and it has no database (relationships and rules go
      # through the one reached over HTTPS)
      stack = Stack.start_engine(stack, @bare, [default: 1000], engine: engine, sidecar_cache_ms: nil)
      {conn, pg} = start_postgres(stack)
      for {name, path} <- certs, do: put(pg, path, "#{@tls}/#{name}")
      port = start_sidecar(stack, engine)

      settings(conn, %{
        "pg_auth.sidecar_socket" => "/run/pg_auth/nothing-listens-here.sock",
        "pg_auth.sidecar_url" => "https://#{@host}:8443",
        "pg_auth.sidecar_secret_file" => "#{@tls}/secret",
        "pg_auth.sidecar_ca_file" => "#{@tls}/ca.pem"
      })

      setup(conn, engine)
      story(conn) ++ refusals(conn) ++ attacks(port) ++ behind_a_proxy(conn, stack, dir(stack)) ++ cost(conn, stack) ++
        misconfigured(stack, engine)
    catch
      kind, reason ->
        for {c, log} <- Stack.logs(stack), do: IO.puts(:stderr, "---- #{c}\n#{String.slice(log, -3000, 3000)}")
        [{"run", "FAILED: " <> Exception.format(kind, reason, __STACKTRACE__)}]
    after
      Stack.stop(stack)
      File.rm_rf(dir(stack))
    end
  end

  defp story(conn) do
    [
      check("over HTTPS: anne, org 1's admin, sees its documents; bob none", fn ->
        eventually(fn -> visible(conn, "anne") == [1, 2, 3] end) and visible(conn, "bob") == []
      end),
      check("anne adds bob to org 1: bob sees its documents", fn ->
        as(conn, "anne", "insert into memberships values (1, 'bob', 'member')") == {:ok, 1} and
          eventually(fn -> visible(conn, "bob") == [1, 2, 3] end)
      end),
      check("a share with an end shows until then and goes after", fn ->
        as(conn, "anne", "insert into shares values (2, 'dan', now() + interval '3 seconds')") == {:ok, 1} and
          eventually(fn -> visible(conn, "dan") == [2] end) and eventually(fn -> visible(conn, "dan") == [] end)
      end)
    ]
  end

  # what the extension refuses: each fails the statement, and nothing is read
  defp refusals(conn) do
    fails = fn setting, value, message ->
      settings(conn, %{setting => value})
      result = as(conn, "anne", "select id from documents")
      settings(conn, %{setting => default(setting)})
      match?({:error, %Postgrex.Error{postgres: %{message: m}}} when is_binary(m), result) and
        elem(result, 1).postgres.message =~ message
    end

    [
      check("a secret the sidecar does not hold fails the statement", fn ->
        fails.("pg_auth.sidecar_secret_file", "#{@tls}/wrong-secret", "refused pg_auth's signature")
      end),
      check("an http:// URL fails the statement", fn ->
        fails.("pg_auth.sidecar_url", "http://#{@host}:8443", "must be https")
      end),
      check("a certificate the trusted CA did not sign fails the statement", fn ->
        fails.("pg_auth.sidecar_ca_file", "#{@tls}/other-ca.pem", "invalid peer certificate: UnknownIssuer")
      end),
      check("and with the settings back, the statement reads again", fn -> visible(conn, "anne") == [1, 2, 3] end)
    ]
  end

  defp default("pg_auth.sidecar_secret_file"), do: "#{@tls}/secret"
  defp default("pg_auth.sidecar_url"), do: "https://#{@host}:8443"
  defp default("pg_auth.sidecar_ca_file"), do: "#{@tls}/ca.pem"

  # someone who reaches the sidecar, without the secret or with a request seen before
  defp attacks(port) do
    url = "https://127.0.0.1:#{port}/v1/ask"
    body = ~s({"op":"list","subject":"user:anne","permission":"viewer","type":"document"})
    now = System.os_time(:second)
    post = fn headers, body ->
      Req.post!(url, body: body, headers: headers, retry: false, decode_body: false, connect_options: [transport_opts: [verify: :verify_none]])
    end
    signed = fn secret, at, body -> [{"pg-auth-timestamp", "#{at}"}, {"pg-auth-signature", sign(secret, "v1\n#{at}\nPOST\n/v1/ask\n" <> body)}] end

    [
      check("the sidecar refuses an unsigned request: 401", fn -> post.([], body).status == 401 end),
      check("a request signed with another secret: 401", fn ->
        post.(signed.("not-the-secret-but-at-least-thirty-two-bytes", now, body), body).status == 401
      end),
      check("a request seen a minute ago, sent again: 401", fn -> post.(signed.(@secret, now - 60, body), body).status == 401 end),
      check("a signed request whose body was changed: 401", fn ->
        post.(signed.(@secret, now, body), String.replace(body, "anne", "bob")).status == 401
      end),
      check("a signed request is answered, the answer signed for it", fn ->
        headers = signed.(@secret, now, body)
        r = post.(headers, body)
        {_, request_signature} = List.keyfind(headers, "pg-auth-signature", 0)
        r.status == 200 and r.body =~ ~s("ids") and
          r.headers["pg-auth-signature"] == [sign(@secret, "v1 response\n#{request_signature}\n200\n" <> r.body)]
      end),
      check("health answers with nothing about anyone", fn ->
        r = Req.get!("https://127.0.0.1:#{port}/healthz", retry: false, connect_options: [transport_opts: [verify: :verify_none]])
        r.status == 200 and r.body == "ok\n"
      end)
    ]
  end

  # a sidecar told to listen without a secret, or without TLS, does not start
  defp misconfigured(stack, _engine) do
    starts = fn env ->
      args = ["run", "--rm"] ++ Stack.pg_docker_args(stack) ++ engine_env(stack) ++ Enum.flat_map(env, &["-e", &1]) ++ [Stack.sidecar_image()]
      task = Task.async(fn -> System.cmd("docker", args, stderr_to_stdout: true) end)
      case Task.yield(task, 20_000) || Task.shutdown(task, :brutal_kill) do
        {:ok, {out, code}} -> {code, out}
        nil -> {:running, ""}
      end
    end

    refused = fn env, message ->
      {code, out} = starts.(["PG_AUTH_LISTEN=0.0.0.0:8443", "PG_AUTH_SOCKET=/tmp/unused.sock" | env])
      code not in [0, :running] and out =~ message
    end

    [
      check("a sidecar listening without a secret refuses to start", fn -> refused.([], "needs PG_AUTH_SECRET_FILE") end),
      check("or with a secret shorter than 32 bytes", fn -> refused.(["PG_AUTH_SECRET_FILE=/etc/hostname"], "32 bytes") end),
      check("or without TLS, unless told a platform terminates it", fn ->
        refused.(["PG_AUTH_SECRET_FILE=#{@tls}/secret"], "PG_AUTH_TLS_CERT")
      end)
    ]
  end

  defp dir(stack), do: Path.join(System.tmp_dir!(), "pg_auth-remote-#{stack.id}")

  # The sidecar on plain HTTP (PG_AUTH_TLS_TERMINATED=1) behind nginx, which holds the certificate,
  # as on a platform that terminates TLS in front of its services. The signatures cross the proxy
  # untouched, so it still cannot answer for the sidecar.
  defp behind_a_proxy(conn, stack, dir) do
    sidecar = Stack.track(stack, "#{stack.id}-plain")
    env = engine_env(stack) ++ Enum.flat_map(
      ["PG_AUTH_LISTEN=0.0.0.0:8080", "PG_AUTH_TLS_TERMINATED=1", "PG_AUTH_SECRET_FILE=#{@tls}/secret",
       "PG_AUTH_SOCKET=/tmp/unused.sock", "CACHE_TTL_MS=1000",
       # to follow the rules in force (the model the rules pushed), as every sidecar does
       "PG_AUTH_DATABASE_URL=postgres://sidecar:remote@#{stack.id}-postgres:5432/postgres"], &["-e", &1])
    {^sidecar, plain} = Docker.run(sidecar, Stack.sidecar_image(), 8080, [], Stack.pg_docker_args(stack) ++ env)

    File.write!(Path.join(dir, "nginx.conf"), """
    server {
      listen 443 ssl;
      ssl_certificate /certs/server.pem;
      ssl_certificate_key /certs/server.key;
      location / { proxy_pass http://#{sidecar}:8080; }
    }
    server {
      listen 8443 ssl;
      ssl_certificate /certs/server.pem;
      ssl_certificate_key /certs/server.key;
      location / {
        proxy_pass http://#{sidecar}:8080;
        proxy_set_header Accept-Encoding "";
        sub_filter_types application/json;
        sub_filter '"ids":[' '"ids":["4","5",';
        sub_filter_once off;
      }
    }
    """)
    File.chmod!(Path.join(dir, "server.key"), 0o644)
    proxy = Stack.track(stack, "#{stack.id}-proxy")
    {^proxy, _} =
      Docker.run(proxy, "nginx:1.27.5-alpine", 443, [],
        ["--network", stack.net, "--network-alias", @proxy, "--network-alias", @tamper, "-v", "#{dir}:/certs:ro",
         "-v", "#{Path.join(dir, "nginx.conf")}:/etc/nginx/conf.d/default.conf:ro"])
    Stack.wait(fn -> Docker.logs(proxy) =~ "start worker process" end, proxy)

    # each with a subject not asked about yet, so the sidecar is asked rather than an answer kept
    fails = fn url, subject, message ->
      settings(conn, %{"pg_auth.sidecar_url" => url})
      result = as(conn, subject, "select id from documents")
      match?({:error, %Postgrex.Error{postgres: %{message: m}}} when is_binary(m), result) and
        elem(result, 1).postgres.message =~ message
    end

    steps = [
      check("behind a proxy terminating TLS: carl reads through it", fn ->
        settings(conn, %{"pg_auth.sidecar_url" => "https://#{@proxy}"})
        (eventually(fn -> visible(conn, "carl") == [4, 5] end) or raise("carl: #{inspect(as(conn, "carl", "select id from documents"))}")) and
          Docker.logs(proxy) =~ "POST /v1/ask"
      end),
      check("a secret the sidecar does not hold fails, through the proxy", fn ->
        settings(conn, %{"pg_auth.sidecar_secret_file" => "#{@tls}/wrong-secret"})
        ok = fails.("https://#{@proxy}", "frank", "refused pg_auth's signature")
        settings(conn, %{"pg_auth.sidecar_secret_file" => "#{@tls}/secret"})
        ok
      end),
      check("an answer the proxy changed fails the statement", fn ->
        fails.("https://#{@tamper}:8443", "grace", "not signed with the secret")
      end),
      check("the sidecar's plain port, reached past the proxy, refuses unsigned: 401", fn ->
        Req.post!("http://127.0.0.1:#{plain}/v1/ask", body: ~s({"op":"ping"}), retry: false).status == 401
      end)
    ]

    settings(conn, %{"pg_auth.sidecar_url" => "https://#{@host}:8443"})
    steps
  end

  # what a round trip costs beyond the engine's own time (authz.stats()), on one backend: through
  # a sidecar on the socket, then over HTTPS (its connection kept, after a warm-up). Every read is
  # a new subject, so each asks the sidecar, and the sidecar the engine.
  defp cost(conn, stack) do
    _ = stack
    per_call = fn label ->
      for i <- 1..20, do: visible(conn, "warm-#{label}-#{i}")
      [[calls0, sidecar0, engine0]] = Postgrex.query!(conn, "select sidecar_calls, sidecar_ms, engine_ms from authz.stats()", []).rows
      for i <- 1..200, do: visible(conn, "#{label}-#{i}")
      [[calls1, sidecar1, engine1]] = Postgrex.query!(conn, "select sidecar_calls, sidecar_ms, engine_ms from authz.stats()", []).rows
      round(((sidecar1 - sidecar0) - (engine1 - engine0)) * 1000 / max(calls1 - calls0, 1))
    end

    settings(conn, %{"pg_auth.sidecar_url" => "", "pg_auth.sidecar_socket" => Stack.socket("default")})
    socket = per_call.("socket")
    settings(conn, %{"pg_auth.sidecar_url" => "https://#{@host}:8443"})
    https = per_call.("https")
    [{"a round trip beyond the engine: socket #{socket} µs, HTTPS #{https} µs", "measured"}]
  end

  defp engine_env(stack) do
    Enum.flat_map(stack.engine_mod.sidecar_env(stack.engine) ++ stack.engine_env, fn {k, v} -> ["-e", "#{k}=#{v}"] end)
  end

  defp sign(secret, text), do: "v1=" <> Base.encode16(:crypto.mac(:hmac, :sha256, secret, text), case: :lower)

  # a test CA, a certificate for the sidecar's name it signed, another CA, and the secrets
  defp certs(dir) do
    File.mkdir_p!(dir)
    ssl = fn args -> {_, 0} = System.cmd("openssl", args, cd: dir, stderr_to_stdout: true) end
    ca = fn name ->
      ssl.(~w(req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 1 -subj /CN=pg_auth-test-#{name}
              -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign -keyout #{name}.key -out #{name}.pem))
    end
    ca.("ca")
    ca.("other-ca")
    ssl.(~w(req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj /CN=sidecar -keyout server.key -out server.csr))
    File.write!(
      Path.join(dir, "ext.cnf"),
      "subjectAltName=DNS:#{@host},DNS:#{@proxy},DNS:#{@tamper}\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n"
    )
    ssl.(~w(x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 1 -extfile ext.cnf -out server.pem))
    File.write!(Path.join(dir, "secret"), @secret <> "\n")
    File.write!(Path.join(dir, "wrong-secret"), "a-secret-the-sidecar-has-never-heard-of-0123456\n")
    for f <- ~w(ca.pem other-ca.pem server.pem server.key secret wrong-secret), do: {f, Path.join(dir, f)}
  end

  # into the volume Postgres and the sidecar share, readable by both
  defp put(pg, from, to) do
    {_, 0} = System.cmd("docker", ["exec", pg, "mkdir", "-p", Path.dirname(to)])
    {_, 0} = System.cmd("docker", ["cp", from, "#{pg}:#{to}"], stderr_to_stdout: true)
    {_, 0} = System.cmd("docker", ["exec", pg, "chmod", "0644", to])
  end

  defp start_sidecar(stack, _engine) do
    name = Stack.track(stack, "#{stack.id}-remote")
    env =
      engine_env(stack) ++
        Enum.flat_map(
          ["PG_AUTH_LISTEN=0.0.0.0:8443", "PG_AUTH_SECRET_FILE=#{@tls}/secret", "PG_AUTH_TLS_CERT=#{@tls}/server.pem",
           "PG_AUTH_TLS_KEY=#{@tls}/server.key", "PG_AUTH_SOCKET=/tmp/unused.sock", "CACHE_TTL_MS=1000",
           "PG_AUTH_DATABASE_URL=postgres://sidecar:remote@#{stack.id}-postgres:5432/postgres"],
          &["-e", &1]
        )
    {^name, port} = Docker.run(name, Stack.sidecar_image(), 8443, [], ["--network-alias", @host] ++ Stack.pg_docker_args(stack) ++ env)
    health = fn -> match?({:ok, %{status: 200}}, Req.get("https://127.0.0.1:#{port}/healthz", retry: false, connect_options: [transport_opts: [verify: :verify_none]])) end
    Stack.wait(health, name)
    port
  end

  defp settings(conn, values) do
    for {k, v} <- values, do: Postgrex.query!(conn, "select set_config('#{k}', $1, false)", [v]) && Postgrex.query!(conn, "alter system set #{k} = '#{v}'", [])
    Postgrex.query!(conn, "select pg_reload_conf()", [])
    # the reload reaches the session at its next statement; set_config made it now
  end

  defp setup(conn, engine) do
    run_all(conn, [
      "create table documents (id int primary key, org_id int not null, title text not null)",
      "create table memberships (org_id int not null, user_id text not null, role text not null, primary key (org_id, user_id))",
      "create table shares (doc_id int not null, user_id text not null, expires_at timestamptz, primary key (doc_id, user_id))",
      "grant select, insert, update, delete on documents, memberships, shares to authenticated",
      "insert into documents values (1, 1, 'Roadmap'), (2, 1, 'Budget'), (3, 1, 'Launch plan'), (4, 2, 'Hiring'), (5, 2, 'Pricing')",
      "insert into memberships values (1, 'anne', 'admin'), (2, 'carl', 'admin')"
    ])

    {name, file} = @files[engine]
    Postgrex.query!(conn, "select authz.set_rules($1, $2, $3)", [engine, name, File.read!(Path.join(@rules, file))])
    eventually(fn -> Postgrex.query!(conn, "select count(*) from authz.rules() where pushed_at is null", []).rows == [[0]] end) ||
      raise("rules not pushed")
    eventually(fn -> Postgrex.query!(conn, "select count(*) from authz_internal.outbox where not_before is null", []).rows == [[0]] end) ||
      raise("relationships not delivered")
  end

  defp start_postgres(stack) do
    pg = Stack.track(stack, "#{stack.id}-postgres")
    {^pg, port} =
      Docker.run(pg, Stack.pg_image(), 5432, ["-c", "fsync=off"],
        Stack.pg_docker_args(stack) ++ Docker.coverage_args("postgres") ++ ["-e", "POSTGRES_PASSWORD=remote"])
    Stack.wait(fn -> match?({_, 0}, System.cmd("docker", ["exec", pg, "pg_isready", "-h", "127.0.0.1", "-U", "postgres"])) end, pg)
    {:ok, conn} =
      retry(fn -> Postgrex.start_link(hostname: "127.0.0.1", port: port, username: "postgres", password: "remote", database: "postgres", pool_size: 1) end, 20)
    run_all(conn, [
      "create extension pg_auth",
      "create role authenticated nologin",
      "create role sidecar login password 'remote' in role authz_sidecar",
      "select authz.configure(subject => 'jwt')"
    ])
    {conn, pg}
  end

  defp retry(f, 0), do: f.()

  defp retry(f, n) do
    case f.() do
      {:ok, c} -> {:ok, c}
      _ -> Process.sleep(500) && retry(f, n - 1)
    end
  end

  defp run_all(conn, statements), do: Enum.each(statements, &Postgrex.query!(conn, &1, []))

  defp check(what, f) do
    {what, if(f.(), do: "ok", else: "FAILED")}
  rescue
    e -> {what, "FAILED: " <> Exception.message(e)}
  end

  defp eventually(f, waited \\ 0) do
    cond do
      f.() -> true
      waited >= @deadline_ms -> false
      true -> Process.sleep(100) && eventually(f, waited + 100)
    end
  end

  # a statement as `user`, the way PostgREST runs one: its claims, the authenticated role
  defp as(conn, user, sql) do
    Postgrex.transaction(conn, fn c ->
      Postgrex.query!(c, "select set_config('request.jwt.claims', $1, true)", [JSON.encode!(%{sub: user, role: "authenticated"})])
      Postgrex.query!(c, "set local role authenticated", [])
      case Postgrex.query(c, sql, []) do
        {:ok, r} -> r.num_rows
        {:error, e} -> Postgrex.rollback(c, e)
      end
    end)
  end

  defp visible(conn, user) do
    case Postgrex.transaction(conn, fn c ->
           Postgrex.query!(c, "select set_config('request.jwt.claims', $1, true)", [JSON.encode!(%{sub: user, role: "authenticated"})])
           Postgrex.query!(c, "set local role authenticated", [])
           List.flatten(Postgrex.query!(c, "select id from documents order by id", []).rows)
         end) do
      {:ok, ids} -> ids
      _ -> :error
    end
  rescue
    _ -> :error
  end
end

Remote.main(System.argv())
