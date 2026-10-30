#!/usr/bin/env elixir
# Cedar's plans compiled into SQL against the same plans evaluated row by row: Postgres with
# pg_auth and a sidecar running Cedar, a table of generated rows (nulls in optional and required
# columns, entity ids in an integer column, folders, roles from the token), and for every subject
# the rows its compiled filter shows (pg_auth.plan_lookup on, as in production) against the rows
# the per-row evaluation of the same plan shows (off: authz.permits_resource, src/condition.rs).
# The sidecar's tests check that evaluation against Cedar's own decision, row by row
# (sidecar/query/src/cedar.rs). See compare/README.md.
#
#   elixir compare/cedar.exs                 # builds the images first
#   elixir compare/cedar.exs --no-build --rows 400 --seed 3

Mix.install([{:req, "~> 0.7"}, {:postgrex, "~> 0.22"}], config: [postgrex: [json_library: JSON]])

dir = Path.join(__DIR__, "lib")
for f <- ~w(engine docker engines/openfga engines/spicedb engines/cerbos engines/cedar stack), do: Code.require_file(Path.join(dir, f <> ".exs"))

defmodule CedarParity do
  alias Compare.{Docker, Stack}

  @schema """
  entity Role in [Role];
  // pg_auth: in from memberships(user_id).group_id: subject Group
  entity User in [Group, Role] { level: Long };
  entity Group;
  // pg_auth: in from folders(id).parent_id: subject Folder
  entity Folder in [Folder];
  // pg_auth: protects docs (id): read view, attributes (owner_id as owner, status, size, public, approved_by, org_id as org, embargo)
  // pg_auth: in from docs(id).folder_id: subject Folder
  entity Doc in [Folder] { owner: User, status?: String, size: Long, public: Bool, approved_by?: User, org: Group, embargo?: Role };
  action view appliesTo { principal: User, resource: Doc };
  """

  @policies """
  permit (principal, action, resource) when { resource.owner == principal };
  permit (principal, action, resource) when { resource.public && resource.size < 10 };
  permit (principal, action, resource) when { resource has status && ["draft", "review"].contains(resource.status) && principal.level > 2 };
  permit (principal, action, resource) when { principal in resource.org };
  permit (principal in Group::"2", action, resource in Folder::"f1");
  permit (principal, action, resource) when { resource has approved_by && resource.approved_by == principal };
  permit (principal in Role::"auditor", action, resource);
  forbid (principal, action, resource) when { [User::"1", User::"2"].contains(resource.owner) && resource.size > 50 };
  forbid (principal, action, resource) when { resource has embargo && principal in resource.embargo };
  forbid (principal, action, resource) when { resource has status && resource.status == "secret" };
  """

  def main(argv) do
    {opts, _} = OptionParser.parse!(argv, strict: [build: :boolean, rows: :integer, seed: :integer])
    if Keyword.get(opts, :build, true), do: Stack.build(Path.expand("..", __DIR__))
    :rand.seed(:exsss, {Keyword.get(opts, :seed, 1), 2, 3})
    Process.flag(:trap_exit, true)
    stack = Stack.new("cedar", "pg_auth_compare=1")

    try do
      url = "postgres://sidecar:cedar@#{stack.id}-postgres:5432/postgres"
      stack = Stack.start_engine(stack, "", [default: 1000], engine: "cedar", sidecar_env: [{"PG_AUTH_DATABASE_URL", url}])
      conn = start_postgres(stack)
      setup(conn, Keyword.get(opts, :rows, 300))
      report(compare(conn))
    catch
      kind, reason ->
        for {c, log} <- Stack.logs(stack), do: IO.puts(:stderr, "---- #{c}\n#{String.slice(log, -3000, 3000)}")
        IO.puts("FAILED: " <> Exception.format(kind, reason, __STACKTRACE__))
        System.halt(1)
    after
      Stack.stop(stack)
    end
  end

  # users 1..6 with levels and groups, roles in the token; folders f1 > f2 > f3, f4 apart
  defp setup(conn, n) do
    run_all(conn, [
      "create table memberships (user_id text, group_id text, primary key (user_id, group_id))",
      "create table folders (id text primary key, parent_id text)",
      "create table docs (id int primary key, owner_id int not null, status text, size int, public bool not null, approved_by text,
                          org_id int, embargo text, folder_id text)",
      "grant select on docs, memberships, folders to authenticated",
      "insert into memberships values ('1', 'g1'), ('2', 'g1'), ('3', 'g2'), ('4', 'g3'), ('5', 'g2'), ('5', 'g3')",
      "insert into folders values ('f1', null), ('f2', 'f1'), ('f3', 'f2'), ('f4', null)",
      "insert into docs values " <> Enum.map_join(1..n, ", ", &row/1)
    ])
    # groups are g1..g3 in memberships, and org_id 1..3 on the rows: the org's id is the group's
    run_all(conn, ["update memberships set group_id = substr(group_id, 2)"])
    {:ok, _} =
      Postgrex.transaction(conn, fn c ->
        Postgrex.query!(c, "select authz.set_rules('cedar', 'schema', $1)", [@schema])
        Postgrex.query!(c, "select authz.set_rules('cedar', 'policies', $1)", [@policies])
      end)
    eventually(fn -> Postgrex.query!(conn, "select count(*) from authz.rules() where pushed_at is null", []).rows == [[0]] end) ||
      raise("rules not taken: #{inspect(Postgrex.query!(conn, "select name, error from authz.rules()", []).rows)}")
    eventually(fn -> Postgrex.query!(conn, "select count(*) from authz_internal.outbox where not_before is null", []).rows == [[0]] end) ||
      raise("parents not delivered")
  end

  defp row(id) do
    pick = fn list -> Enum.random(list) end
    sql = fn
      nil -> "null"
      v when is_binary(v) -> "'#{v}'"
      v -> to_string(v)
    end
    values = [
      id,
      pick.(1..6),
      pick.([nil, "draft", "review", "secret", "final"]),
      pick.([nil, 3, 9, 30, 70]),
      pick.([true, false]),
      pick.([nil, "1", "3", "007", "5"]),
      pick.([nil, 1, 2, 3]),
      pick.([nil, "contractor", "staff", "restricted"]),
      pick.([nil, "f1", "f2", "f3", "f4"])
    ]
    "(" <> Enum.map_join(values, ", ", sql) <> ")"
  end

  # each user, with and without roles in their token
  defp subjects do
    for user <- ~w(1 2 3 4 5 6 007), roles <- [[], ["contractor"], ["auditor"], ["restricted", "staff"]] do
      %{sub: user, role: "authenticated", roles: roles, level: rem(String.to_integer(user), 5)}
    end
  end

  defp compare(conn) do
    for claims <- subjects() do
      compiled = visible(conn, claims, true)
      evaluated = visible(conn, claims, false)
      {claims, compiled, evaluated}
    end
  end

  defp report(results) do
    bad = Enum.reject(results, fn {_, c, e} -> c == e end)
    for {claims, c, e} <- bad do
      IO.puts("MISMATCH #{inspect(claims)}: compiled only #{inspect(c -- e)}, evaluated only #{inspect(e -- c)}")
    end
    shown = results |> Enum.map(fn {_, c, _} -> length(c) end) |> Enum.sum()
    IO.puts("#{length(results)} subjects, #{shown} rows shown in all, #{length(bad)} mismatched")
    if bad != [], do: System.halt(1)
    IO.puts("Compiled filters and per-row evaluation agree.")
  end

  # the rows a subject sees, with its plans compiled (on) or evaluated per row (off)
  defp visible(conn, claims, compiled) do
    {:ok, ids} =
      Postgrex.transaction(conn, fn c ->
        Postgrex.query!(c, "select set_config('pg_auth.plan_lookup', $1, true)", [if(compiled, do: "on", else: "off")])
        Postgrex.query!(c, "select set_config('request.jwt.claims', $1, true)", [JSON.encode!(claims)])
        Postgrex.query!(c, "set local role authenticated", [])
        if compiled do
          plan = Postgrex.query!(c, "explain (costs off) select id from docs", []).rows |> List.flatten() |> Enum.join("\n")
          (plan =~ "authz.plan_value" or plan =~ "plan_values" or not (plan =~ "permits_resource")) || raise("not compiled:\n#{plan}")
        end
        List.flatten(Postgrex.query!(c, "select id from docs order by id", []).rows)
      end)
    ids
  end

  defp start_postgres(stack) do
    pg = Stack.track(stack, "#{stack.id}-postgres")
    {^pg, port} = Docker.run(pg, Stack.pg_image(), 5432, ["-c", "fsync=off"], Stack.pg_docker_args(stack) ++ ["-e", "POSTGRES_PASSWORD=cedar"])
    Stack.wait(fn -> match?({_, 0}, System.cmd("docker", ["exec", pg, "pg_isready", "-h", "127.0.0.1", "-U", "postgres"])) end, pg)
    {:ok, conn} = retry(fn -> Postgrex.start_link(hostname: "127.0.0.1", port: port, username: "postgres", password: "cedar", database: "postgres", pool_size: 1) end, 20)
    run_all(conn, [
      "create extension pg_auth",
      "alter system set pg_auth.sidecar_socket = '#{Stack.socket("default")}'",
      "select pg_reload_conf()",
      "create role authenticated nologin",
      "create role sidecar login password 'cedar' in role authz_sidecar",
      "select authz.configure(subject => 'jwt', subject_type => 'User', roles_claim => 'roles')"
    ])
    conn
  end

  defp retry(f, 0), do: f.()

  defp retry(f, n) do
    case f.() do
      {:ok, c} -> {:ok, c}
      _ -> Process.sleep(500) && retry(f, n - 1)
    end
  end

  defp run_all(conn, statements), do: Enum.each(statements, &Postgrex.query!(conn, &1, []))

  defp eventually(f, waited \\ 0) do
    cond do
      f.() -> true
      waited >= 30_000 -> false
      true -> Process.sleep(100) && eventually(f, waited + 100)
    end
  end
end

CedarParity.main(System.argv())
