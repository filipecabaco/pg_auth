#!/usr/bin/env elixir
# Planning cost of one query against a running stack, by how long the backend sat idle before it,
# with what pg_auth spent meanwhile (authz.stats(): sidecar round trips, sidecar and engine time).
# Each sample is its own transaction, as a request is: the claims and the sidecar socket set
# locally, the role switched, then EXPLAIN ANALYZE. See docs/development.md ("Profiling").
#
#   elixir bench/docker/run.exs --scenario rls-best-case-tenant-column --variants pg_auth_cerbos --reps 1 --keep
#   elixir bench/docker/planning.exs --port <port from the --keep line> \
#     --query "select * from app.events where id = 1" \
#     --claims '{"sub":"23c820ce-a582-728e-0298-f368a5114e3b","role":"authenticated","tenant_id":"e000342e-22c2-b525-5299-b35c4d538065"}'
#   elixir bench/docker/run.exs --clean

Mix.install([{:postgrex, "~> 0.22"}], config: [postgrex: [json_library: JSON]])

defmodule Planning do
  @switches [port: :integer, host: :string, user: :string, password: :string, database: :string, query: :string, claims: :string,
             role: :string, socket: :string, gaps: :string, samples: :integer]

  def main(argv) do
    {o, _} = OptionParser.parse!(argv, strict: @switches)
    {:ok, conn} =
      Postgrex.start_link(hostname: o[:host] || "127.0.0.1", port: o[:port] || raise("--port is required"), username: o[:user] || "postgres",
                          password: o[:password] || "bench", database: o[:database] || "bench", pool_size: 1)
    query = o[:query] || raise("--query is required")
    gaps = (o[:gaps] || "0,50,200,600,1500") |> String.split(",") |> Enum.map(&String.to_integer/1)
    IO.puts("idle gap   planning p10 / median / p90 (ms)   sidecar calls   sidecar ms   engine ms  (per statement)")
    for gap <- gaps do
      samples = o[:samples] || if(gap >= 500, do: 30, else: 100)
      report(gap, for(_ <- 1..samples, do: sample(conn, o, query, gap)))
    end
  end

  defp sample(conn, o, query, gap) do
    Process.sleep(gap)
    {:error, result} =
      Postgrex.transaction(conn, fn c ->
        if claims = o[:claims], do: Postgrex.query!(c, "select set_config('request.jwt.claims', $1, true)", [claims])
        Postgrex.query!(c, "select set_config('pg_auth.sidecar_socket', $1, true)", [o[:socket] || "/run/pg_auth/default.sock"])
        Postgrex.query!(c, "set local role #{o[:role] || "bench_user"}", [])
        before = stats(c)
        [[[plan]]] = Postgrex.query!(c, "explain (analyze, format json) " <> query, []).rows
        Postgrex.rollback(c, {plan["Planning Time"], Enum.zip_with(stats(c), before, &(&1 - &2))})
      end)
    result
  end

  defp stats(c), do: hd(Postgrex.query!(c, "select sidecar_calls, sidecar_ms, engine_ms from authz.stats()", []).rows)

  defp report(gap, samples) do
    n = length(samples)
    planning = samples |> Enum.map(&elem(&1, 0)) |> Enum.sort()
    at = fn q -> planning |> Enum.at(min(n - 1, trunc(n * q))) |> Float.round(3) end
    [calls, sidecar, engine] = samples |> Enum.map(&elem(&1, 1)) |> Enum.zip_with(&Enum.sum/1)
    IO.puts(:io_lib.format("~8w ms   ~8.3f / ~8.3f / ~8.3f        ~6.2f       ~7.3f    ~7.3f",
                           [gap, at.(0.1), at.(0.5), at.(0.9), calls / n, sidecar / n, engine / n]))
  end
end

Planning.main(System.argv())
