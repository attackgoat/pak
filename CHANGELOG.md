# Changelog

## 0.8.0 (unreleased)

### Upgrading from 0.7

- Pak now writes and reads archive format **V1.11**. Archives from `pak` 0.7.6
  use V1.0 and must be rebaked from their source assets. Upgrade the baker and
  runtime reader together.
- Debug baking stores reusable group payloads in `.pakseg` files. Keep the root
  `.pak` and all referenced segments together. Release bakes still produce a
  self-contained `.pak`.
- `MaterialInfo` is cloneable rather than `Copy` and now carries authored
  metadata. Rebuild consumers against the new API.

### Added

- Bake primitive-local, authored, and grouped mesh LODs, including optional
  joining and voxelization. Runtime consumers must select grouped LODs as
  complete snapshots. See [grouped surface LODs](docs/grouped-surface-lods.md).
- Read mesh LOD reports and include `pak::buf::MESH_LOD_PRODUCER` in consumer
  cache keys for generated LODs.
- Opt into high-quality BC texture mips. Add material occlusion support and
  hooks for baking derived assets such as opacity micromaps.
- Normalize asset keys across platforms. Mesh recipes can omit the second UV
  channel and control vertex-cache reordering independently.
