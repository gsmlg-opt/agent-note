defmodule NoteSyncFixture.MixProject do
  use Mix.Project
  def project, do: [app: :note_sync_fixture, version: "0.1.0", elixir: "~> 1.18", deps: deps()]
  def application, do: [extra_applications: [:logger, :ssl], mod: {NoteSyncFixture.Application, []}]
  defp deps, do: [{:phoenix, "== 1.8.15"}, {:bandit, "~> 1.11"}, {:jason, "~> 1.4"}]
end
