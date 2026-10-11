defmodule NoteSyncFixture.Application do
  use Application
  def start(_type, _args) do
    Supervisor.start_link([
      {Phoenix.PubSub, name: NoteSyncFixture.PubSub}, NoteSyncFixture.Endpoint
    ], strategy: :one_for_one, name: NoteSyncFixture.Supervisor)
  end
end

defmodule NoteSyncFixture.Endpoint do
  use Phoenix.Endpoint, otp_app: :note_sync_fixture
  socket "/note-sync", NoteSyncFixture.Socket, websocket: true, longpoll: false
end

defmodule NoteSyncFixture.Socket do
  use Phoenix.Socket
  channel "note_sync:v1", NoteSyncFixture.Channel
  def connect(_params, socket, _connect_info), do: {:ok, socket}
  def id(_socket), do: nil
end

defmodule NoteSyncFixture.Channel do
  use Phoenix.Channel
  @peer "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa"
  def join("note_sync:v1", %{"protocol_version" => 1} = hello, socket) do
    if path=System.get_env("SYNC_SCOPES_FILE"), do: File.write!(path,Jason.encode!(hello["selectors"]) <> "\n",[:append])
    {:ok, %{hello | "node_id" => @peer}, assign(socket, transfer: nil, committed: nil, selectors: hello["selectors"])}
  end

  def handle_in("inventory_start", _, socket) do
    send(self(), :push_inventory)
    {:reply, {:ok, %{}}, socket}
  end
  def handle_in("inventory_complete", _, socket) do
    marker=System.get_env("SYNC_RECONNECT_ONCE")
    if marker && not File.exists?(marker) do
      File.write!(marker,"completed first inventory")
      {:stop,:normal,socket}
    else
      {:reply,{:ok,%{}},socket}
    end
  end
  def handle_in("transfer_begin", payload, socket) do
    transfer = Map.merge(payload, %{"blobs" => %{"event" => ""}, "event" => nil})
    pause = System.get_env("SYNC_PAUSE_FIRST_BEGIN")
    if pause && not File.exists?(pause) do
      File.write!(pause, "begin received after durable lease")
      {:noreply, assign(socket, :transfer, transfer)}
    else
      {:reply, {:ok, %{}}, assign(socket, :transfer, transfer)}
    end
  end
  def handle_in("attachment_begin", %{"blob_id" => id}, socket) do
    transfer = Map.update!(socket.assigns.transfer, "blobs", &Map.put(&1, id, ""))
    {:reply, {:ok, %{}}, assign(socket, :transfer, transfer)}
  end
  def handle_in("blob_chunk", %{"blob_id" => id, "data" => data, "offset" => offset}, socket) do
    transfer = socket.assigns.transfer
    bytes = transfer["blobs"][id]
    if byte_size(bytes) != offset, do: raise("invalid fixture chunk offset")
    transfer = put_in(transfer["blobs"][id], bytes <> Base.decode64!(data))
    {:noreply, assign(socket, :transfer, transfer)}
  end
  def handle_in("blob_end", %{"blob_id" => id}, socket) do
    transfer = socket.assigns.transfer
    bytes = transfer["blobs"][id]
    expected = if id == "event", do: transfer, else: Enum.find(transfer["event"]["snapshot"]["attachments"], &("attachment:" <> &1["id"] == id))
    if byte_size(bytes) != expected["size_bytes"] or Base.encode16(:crypto.hash(:sha256, bytes), case: :lower) != expected["checksum_sha256"], do: raise("invalid fixture blob")
    transfer = if id == "event", do: Map.put(transfer, "event", Jason.decode!(bytes)), else: transfer
    {:reply, {:ok, %{}}, assign(socket, :transfer, transfer)}
  end
  def handle_in("transfer_commit", _, socket) do
    event = socket.assigns.transfer["event"]
    if event["note_id"] == System.get_env("SYNC_REJECT_NOTE_ID") do
      File.write!(System.fetch_env!("SYNC_REJECTIONS_FILE"),event["event_id"] <> "\n",[:append])
      # A permanent per-note failure rolls back before discarding transfer state.
      rejection=%{code: "note_rejected", event_id: event["event_id"], note_id: event["note_id"], reason: "catalog_conflict"}
      {:reply,{:error,rejection},assign(socket,:transfer,nil)}
    else
      # Fixture acknowledgements occur after persistence to a filesystem record.
      base = System.fetch_env!("SYNC_COMMIT_FILE") <> "." <> event["note_id"]
      File.write!(base, Jason.encode!(event))
      File.write!(base <> ".scopes",Jason.encode!(socket.assigns.selectors))
      for {id, bytes} <- socket.assigns.transfer["blobs"], id != "event", do: File.write!(base <> "." <> String.replace_prefix(id, "attachment:", ""), bytes)
      ack = Map.take(event, ["event_id", "note_id", "generation", "control", "version", "kind", "snapshot_hash"])
      drop = System.get_env("SYNC_DROP_FIRST_ACK")
      if drop && not File.exists?(drop) do
        File.write!(drop, "committed before ACK")
        {:stop, :normal, assign(socket, transfer: nil, committed: event)}
      else
        push(socket,"ack",ack)
        {:reply, {:ok, ack}, assign(socket, transfer: nil, committed: event)}
      end
    end
  end
  def handle_in("ack", ack, socket) do
    File.write!(System.fetch_env!("SYNC_ACK_FILE"), Jason.encode!(ack))
    {:noreply, socket}
  end
  def handle_info(:push_inventory, socket) do
    case System.get_env("SYNC_INBOUND_JSON") do
      nil -> :ok
      path ->
        event = File.read!(path)
        id = "bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb"
        push(socket, "transfer_begin", %{transfer_id: id, size_bytes: byte_size(event), checksum_sha256: Base.encode16(:crypto.hash(:sha256, event), case: :lower)})
        event |> :binary.bin_to_list() |> Enum.chunk_every(65536) |> Enum.with_index() |> Enum.each(fn {chunk, index} ->
          push(socket, "blob_chunk", %{transfer_id: id, blob_id: "event", offset: index * 65536, data: Base.encode64(:binary.list_to_bin(chunk))})
        end)
        push(socket, "blob_end", %{transfer_id: id, blob_id: "event"})
        for attachment <- Jason.decode!(event)["snapshot"]["attachments"] do
          blob = File.read!(Path.join(System.fetch_env!("SYNC_INBOUND_BLOBS"), attachment["id"]))
          blob_id = "attachment:" <> attachment["id"]
          push(socket, "attachment_begin", %{transfer_id: id, blob_id: blob_id})
          blob |> :binary.bin_to_list() |> Enum.chunk_every(65536) |> Enum.with_index() |> Enum.each(fn {chunk, index} ->
            push(socket, "blob_chunk", %{transfer_id: id, blob_id: blob_id, offset: index * 65536, data: Base.encode64(:binary.list_to_bin(chunk))})
          end)
          push(socket, "blob_end", %{transfer_id: id, blob_id: blob_id})
        end
        push(socket, "transfer_commit", %{transfer_id: id})
    end
    {:noreply, socket}
  end
end
