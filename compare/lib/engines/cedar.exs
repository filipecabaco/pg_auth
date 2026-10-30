defmodule Compare.Engines.Cedar do
  @moduledoc """
  Cedar, evaluated by the pg_auth sidecar itself (PG_AUTH_ENGINE=cedar): no container, no model
  to load and no tuples to write here; its schema and policies are stored with authz.set_rules
  and its parents delivered from the relationship tables, through Postgres (PG_AUTH_DATABASE_URL,
  which the caller gives the sidecars).
  """
  @behaviour Compare.Engine

  @impl true
  def name, do: "cedar"
  @impl true
  def version(_), do: "cedar (in the sidecar)"
  @impl true
  def capabilities(_), do: []
  @impl true
  def setup(_opts), do: {:ok, %{container: nil, db: nil}}
  @impl true
  def teardown(_), do: :ok
  @impl true
  def reset(state), do: {:ok, state}
  @impl true
  def load_model(_, _), do: :ok
  @impl true
  def write_tuples(_, tuples), do: {:ok, Enum.map(tuples, &%{tuple: &1, error: "cedar's parents come from relationship tables"})}
  @impl true
  def check(_, _), do: {:unsupported, "cedar is asked through pg_auth"}
  @impl true
  def list_objects(_, _), do: {:unsupported, "cedar is asked through pg_auth"}
  @impl true
  def list_users(_, _), do: {:unsupported, "cedar is asked through pg_auth"}
  @impl true
  def sidecar_env(_), do: [{"PG_AUTH_ENGINE", "cedar"}]
  @impl true
  def fresh_server(state), do: state
  @impl true
  def staleness_ms(_), do: 0
  @impl true
  def revocation_ms(_), do: 0
end
