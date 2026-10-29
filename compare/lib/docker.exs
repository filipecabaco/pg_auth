defmodule Compare.Docker do
  @moduledoc "Tiny docker helpers. Only containers started here (label pg_auth_compare=1, unique names) are ever removed."

  def free_port do
    {:ok, s} = :gen_tcp.listen(0, [])
    {:ok, port} = :inet.port(s)
    :gen_tcp.close(s)
    port
  end

  def image?(image) do
    {out, _} = System.cmd("docker", ["images", "-q", image])
    String.trim(out) != ""
  end

  @doc "Start a container publishing `container_port` on a free host port (retries when another process wins the port). Returns {name, host_port}."
  def run(name, image, container_port, args \\ [], docker_args \\ [], tries \\ 5) do
    port = free_port()

    {out, code} =
      System.cmd(
        "docker",
        ["run", "-d", "--rm", "--label", "pg_auth_compare=1", "--name", name, "-p", "127.0.0.1:#{port}:#{container_port}"] ++
          docker_args ++ [image] ++ args,
        stderr_to_stdout: true
      )

    cond do
      code == 0 ->
        {name, port}

      tries > 1 and out =~ "address already in use" ->
        stop(name)
        run(name, image, container_port, args, docker_args, tries - 1)

      true ->
        stop(name)
        raise("docker run failed: #{out}")
    end
  end

  # -v: with the container, its anonymous volumes (a Postgres image's data directory), which
  # would otherwise be left behind by every run. Measuring coverage, it is stopped first, so its
  # processes exit and write their profiles (Postgres's image stops with SIGINT, a fast shutdown).
  def stop(name) do
    if coverage_dir(), do: System.cmd("docker", ["stop", "--time", "30", name], stderr_to_stdout: true)
    System.cmd("docker", ["rm", "-f", "-v", name], stderr_to_stdout: true)
  end

  @doc """
  With PG_AUTH_COVERAGE_DIR set (scripts/coverage.exs), `docker run` arguments that make an
  instrumented pg_auth binary write its profiles there, one file per process.
  """
  def coverage_args(kind) do
    case coverage_dir() do
      nil -> []
      dir -> ["-v", "#{dir}:/coverage", "-e", "LLVM_PROFILE_FILE=/coverage/#{kind}-%p-%m.profraw"]
    end
  end

  defp coverage_dir, do: System.get_env("PG_AUTH_COVERAGE_DIR")

  def logs(name), do: elem(System.cmd("docker", ["logs", name], stderr_to_stdout: true), 0)

  def unique(prefix), do: "#{prefix}-#{System.pid()}-#{System.unique_integer([:positive])}"
end
