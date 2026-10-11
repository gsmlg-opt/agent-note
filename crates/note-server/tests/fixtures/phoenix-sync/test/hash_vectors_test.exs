defmodule NoteSyncFixture.HashVectorsTest do
  use ExUnit.Case
  defp canonical(value) when is_map(value) do
    Jason.OrderedObject.new(value |> Enum.sort_by(&elem(&1, 0)) |> Enum.map(fn {key, v} -> {key, canonical(v)} end))
  end
  defp canonical(value) when is_list(value), do: Enum.map(value, &canonical/1)
  defp canonical(value), do: value

  test "independent Rust/Python vectors use identical canonical UTF-8 JSON in Elixir" do
    path = Path.expand("../../../../../../docs/sync/hash-vectors-v1.json", __DIR__)
    for vector <- Jason.decode!(File.read!(path))["vectors"] do
      bytes = Jason.encode!(canonical(vector["snapshot"]))
      assert bytes == vector["canonical_utf8"], vector["name"]
      assert Base.encode16(:crypto.hash(:sha256, bytes), case: :lower) == vector["sha256"]
    end
  end
end
