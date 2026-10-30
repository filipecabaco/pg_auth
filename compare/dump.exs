#!/usr/bin/env elixir
# A logical dump (pg_dump, pg_restore) carries pg_auth's configuration: the subject settings, the
# stored rules and what their notes set up, the protected and relationship tables, and the changes
# not yet delivered, with the outbox's sequence where it was. Postgres alone, no engine: the rules
# wait to be pushed. See compare/README.md.
#
#   elixir compare/dump.exs              # builds the image first
#   elixir compare/dump.exs --no-build

defmodule Dump do
  @root Path.expand("..", __DIR__)
  @image "pg_auth-bench"
  @rules Path.expand("../docker/demo/rules/openfga.fga", __DIR__)

  # what has to be the same in the database and in its restored copy
  @compared [
    {"settings in force", "select string_agg(key || '=' || value, ', ' order by key) from authz_internal.settings_in_force() where key <> 'version'"},
    {"protected tables", "select string_agg(x, ', ' order by x) from (select format('%s %s %s %s %s', rel, name, object_type, permission, write_permission) x from authz_internal.bindings) b"},
    {"relationship tables", "select string_agg(x, ', ' order by x) from (select format('%s %s', rel, name) x from authz_internal.relationships) r"},
    {"stored rules", "select string_agg(x, ', ' order by x) from (select format('%s %s %s %s', engine, name, version, md5(body)) x from authz_internal.rules) r"},
    {"setup versions", "select count(*)::text from authz_internal.setups"},
    {"changes waiting", "select format('%s up to %s', count(*), max(id)) from authz_internal.outbox"},
    {"policies", "select string_agg(x, ', ' order by x) from (select tablename || ' ' || policyname x from pg_policies where policyname like 'pg_auth%') p"},
    {"triggers", "select string_agg(x, ', ' order by x) from (select tgrelid::regclass || ' ' || tgname x from pg_trigger where not tgisinternal and tgrelid::regclass::text not like 'authz%') t"}
  ]

  def main(argv) do
    {opts, _} = OptionParser.parse!(argv, strict: [build: :boolean])
    if Keyword.get(opts, :build, true), do: {_, 0} = System.cmd("docker", ~w(build -f bench/docker/Dockerfile -t #{@image} .), cd: @root, into: IO.stream(:stderr, :line))
    name = "dump-#{System.pid()}"
    {_, 0} = System.cmd("docker", ~w(run -d --rm --label pg_auth_compare=1 --name #{name} -e POSTGRES_PASSWORD=dump #{@image}))

    try do
      wait(name)
      steps = setup(name) ++ round_trip(name)
      for {step, outcome} <- steps, do: IO.puts("#{String.pad_trailing(step, 58)} #{outcome}")
      ok = Enum.all?(steps, fn {_, o} -> not String.starts_with?(o, "FAILED") end)
      IO.puts(if ok, do: "All steps passed.", else: "Failures.")
      if not ok, do: System.halt(1)
    after
      System.cmd("docker", ["stop", name], stderr_to_stdout: true)
    end
  end

  defp setup(c) do
    sql!(c, "postgres", "create database app")
    {_, 0} = System.cmd("docker", ["cp", @rules, "#{c}:/tmp/rules.fga"])
    sql!(c, "app", """
    create extension pg_auth;
    create role authenticated nologin;
    select authz.configure(subject => 'jwt', claim => 'user_id', roles_claim => 'app_role');
    create table documents (id int primary key, org_id int not null, title text not null);
    create table memberships (org_id int not null, user_id text not null, role text not null, primary key (org_id, user_id));
    create table shares (doc_id int not null, user_id text not null, expires_at timestamptz, primary key (doc_id, user_id));
    insert into documents values (1, 1, 'Roadmap'), (2, 1, 'Budget');
    insert into memberships values (1, 'anne', 'admin');
    \\set rules `cat /tmp/rules.fga`
    select authz.set_rules('openfga', 'model', :'rules');
    insert into memberships values (1, 'bob', 'member');
    """)
    [{"pg_auth set up from the demo's rules, changes waiting", "ok"}]
  end

  defp round_trip(c) do
    {out, code} = System.cmd("docker", ["exec", c, "sh", "-c",
      "pg_dump -U postgres -Fc app > /tmp/app.dump && createdb -U postgres restored && pg_restore -U postgres --exit-on-error -d restored /tmp/app.dump"],
      stderr_to_stdout: true)

    restored = [{"pg_dump, then pg_restore into a new database", if(code == 0, do: "ok", else: "FAILED: " <> String.slice(out, 0, 600))}]

    if code != 0 do
      restored
    else
      same =
        for {what, q} <- @compared do
          {a, b} = {one(c, "app", q), one(c, "restored", q)}
          {"the same #{what}", if(a == b and a not in ["", "0"], do: "ok", else: "FAILED: #{inspect(a)} became #{inspect(b)}")}
        end

      # a change after the restore takes the next id, as it would have in the original
      before = one(c, "restored", "select max(id) from authz_internal.outbox")
      sql!(c, "restored", "insert into memberships values (1, 'carl', 'member')")
      next = one(c, "restored", "select max(id) from authz_internal.outbox")

      restored ++ same ++
        [{"a change after the restore takes the next outbox id", if(String.to_integer(next) > String.to_integer(before), do: "ok", else: "FAILED: #{before} then #{next}")},
         {"the subject settings hold in the restored database",
          if(one(c, "restored", "select value from authz_internal.settings_in_force() where key = 'claim'") == "user_id", do: "ok", else: "FAILED")}]
    end
  end

  defp wait(c, n \\ 60) do
    case System.cmd("docker", ["exec", c, "pg_isready", "-U", "postgres", "-h", "127.0.0.1"], stderr_to_stdout: true) do
      {_, 0} -> Process.sleep(1000)
      _ when n > 0 -> Process.sleep(500) && wait(c, n - 1)
      _ -> raise "postgres never became ready"
    end
  end

  # as a file, so psql's own commands (\\set) mix with SQL
  defp sql!(c, db, sql) do
    file = Path.join(System.tmp_dir!(), "dump-#{System.unique_integer([:positive])}.sql")
    File.write!(file, sql)
    {_, 0} = System.cmd("docker", ["cp", file, "#{c}:/tmp/run.sql"])
    File.rm(file)
    {out, code} = System.cmd("docker", ["exec", c, "psql", "-X", "-q", "-v", "ON_ERROR_STOP=1", "-U", "postgres", "-d", db, "-f", "/tmp/run.sql"], stderr_to_stdout: true)
    if code != 0, do: raise("#{db}: #{out}")
    out
  end

  defp one(c, db, q) do
    {out, 0} = System.cmd("docker", ["exec", c, "psql", "-X", "-qtA", "-U", "postgres", "-d", db, "-c", q], stderr_to_stdout: true)
    String.trim(out)
  end
end

Dump.main(System.argv())
