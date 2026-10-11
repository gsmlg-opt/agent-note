import Config
config :phoenix, :json_library, Jason
config :note_sync_fixture, NoteSyncFixture.Endpoint,
  adapter: Bandit.PhoenixAdapter,
  secret_key_base: String.duplicate("fixture-only-not-production", 4),
  server: true,
  pubsub_server: NoteSyncFixture.PubSub,
  https: [
    ip: {127, 0, 0, 1}, port: String.to_integer(System.get_env("SYNC_FIXTURE_PORT", "9443")),
    certfile: System.get_env("SYNC_SERVER_CERT", "server.pem"),
    keyfile: System.get_env("SYNC_SERVER_KEY", "server.key"),
    thousand_island_options: [transport_options: [
      cacertfile: System.get_env("SYNC_CA_CERT", "ca.pem"),
      verify: :verify_peer, fail_if_no_peer_cert: true
    ]]
  ]
config :logger, level: :warning
