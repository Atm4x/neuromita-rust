# Pinned model source catalog

`catalog.v1.json` is intentionally empty. The catalog is **not** an online
model registry until it contains approved, SHA-256-pinned source revisions.
Do not populate it with guessed checksums or download unpinned weights.

Each record uses the exact runtime plugin ID, immutable revision ID and every
required `(role, relative_path)` declared by the plugin's dynamic install plan.
The following example shows the shape, not a real deployable source:

```json
{
  "schema_version": 1,
  "entries": [
    {
      "plugin_id": "example_plugin",
      "revision": "approved-revision",
      "assets": [
        {
          "role": "weights",
          "relative_path": "model.bin",
          "sha256": "<64 hexadecimal digits from verified binary>",
          "source": {"kind": "https", "url": "https://models.example.org/model.bin"}
        }
      ]
    }
  ]
}
```

`kind` can instead be `local_file` with an absolute `path` to explicitly supplied
model assets. The Rust `AiProductionInstaller` accepts this catalog through
`ModelAssets::Catalog`, verifies its entries against the plugin's install plan,
then sends them through `ModelAssetStore` for immutable staging, checksum
verification and publication. An absent entry or checksum is a blocking error.

User-specific RVC voice weights and reference assets cannot be represented by a
universal official URL. They require user-approved local files and pins.
