# Grouped surface LODs

Pak can bake complete alternatives across the material primitives of one imported mesh:

**Reference capture → optional Join → optional Voxelize → clustered Simplify → Redistribute → Finalize.**

## Configuration

```toml
[mesh]
src = 'prop.glb'
name = 'Prop'
lods = [
    { layout = 'POSITION', simplify = true },
    { layout = 'PACKED_NORMAL', simplify = true },
]

[mesh.lod]
join = true
voxelize = true
voxel-size = 0.05
voxel-max-cells = 262144
min-triangles = 16
target-error = 0.05
```

The same fields are supported in `[content.lod]`. Settings inherit field by field;
an explicit mesh-level `false` overrides an inherited `true`. Existing flat artist
controls (`lod-target-error`, `min-lod-triangles`, `lod-lock-border`) retain final
precedence. Source-only requests (`simplify = false`) use ordinary primitive layout
sets regardless of these processing options. At least one simplifying layout request
is needed to produce reduced snapshots.

| Join | Voxelize | Processing scope |
| --- | --- | --- |
| false | false | Existing primitive-local LOD producer |
| true | false | One joined group of eligible primitives per requested layout |
| false | true | One reconstructed group per eligible primitive and layout |
| true | true | Join first, reconstruct once, simplify the resulting working surface |

`voxel-size` must be finite and positive when voxelization is enabled. It is measured
after import transforms, in mesh-local units, and has no automatic asset-size default.
`voxel-max-cells` accepts `1..=16777216`, default `262144`. Reconstruction uses at most
that many active cells, at most eight field samples per cell, and at most 64 times
that many triangle/cell enumeration attempts. Grid-range or work-limit exhaustion
is a bake error; Pak never silently increases cell size.

## Source, preparation, and correspondence

Canonical geometry is preserved. Snapshot zero is its exact layout projection,
with zero error and patch-only triangle reordering. If import uses `max-index`, Pak
captures a separate appearance reference **before** that reduction. Every reduced
snapshot queries this same unsimplified reference. Both canonical and reference
identities are retained per member.

Joining connects compatible, exactly coincident, oppositely directed edge pairs.
Hard-normal variants remain available to the normal-aware simplifier; ambiguous
non-manifold edges and nearby disconnected surfaces are not proximity-welded.
Material-only locks inside a joined group are removed. Excluded-member locks,
cluster boundaries, and the external `lock-border` policy remain effective.

Voxelization uses a sparse signed-distance surface band and a conforming
six-tetrahedra grid. Shared grid-edge vertices produce a smooth indexed surface
with area-weighted normals. Closed components use union interior classification,
including overlapping shells. Inputs must be closed, consistently outward-wound,
nondegenerate shells with positive volume. Open material slices therefore usually
need joining first. Ambiguous interiors, unsupported topology, or external locks
produce a source-only group with an explicit `voxel-unsupported: ...` or
`voxel-external-locks` reason. Very thin/sub-voxel features may vanish; nested closed
shells are treated as solids, so an enclosed inner shell does not define a cavity.

The clustered simplifier uses the prepared surface with the existing grouping,
retry, normal-weight, and iteration controls. Only candidates strictly smaller than
canonical geometry and the previous published snapshot are retained. The triangle
floor applies to the group, bounded by its canonical count. Voxel preparation may
temporarily increase tessellation without publishing that working mesh.

Each output triangle casts seven fixed, interior area-quadrature samples against a
deterministic source BVH. Matching requires compatible face orientation and bounded
projection distance. Weighted material votes choose the original material slot;
weighted owner votes within that slot choose the original primitive. Distance and
stable IDs resolve ties. Redistribution preserves all triangle positions, winding,
working normals, and the total triangle count. Small material regions can disappear
and boundaries are approximate.

Static texture layouts are supported on the grouped path. Corner UVs are transferred
barycentrically within the chosen material/primitive and one source chart; tangent
frames are regenerated from those UVs. Attribute splits can increase vertex counts.
Mixed-material votes and chart conflicts are diagnostics, not exact reconstruction
guarantees. Texture/alpha coverage and per-instance material semantics belong to
the consumer's eligibility policy. Imported skin/morph deformation uses ordinary
source-only layout sets with `unsupported-deformation` instead of grouped reduction.

## Required Finalize phase

Every redistributed candidate is finalized as a unit:

1. Omit zero-triangle members and finish corner attributes.
2. Deduplicate complete vertex records, compact buffers, enforce `max-index`, and
   apply cache/overdraw/fetch controls. `optimize-vertex-cache` remains independent.
3. Build final patches, bounds, and group diagnostics from the resulting buffers.
4. Validate finite attributes, membership, shared error, and aggregate counts.
5. Publish the complete immutable snapshot. A failure in any partition rejects
   the entire candidate and leaves earlier snapshots available.

Use `Mesh::bounds()` after generation to obtain an envelope over canonical geometry,
ordinary LOD sets, and all group snapshots. Group snapshot bounds enclose their
final partitions. Serialization validates source association and exclusive ownership,
including after a derived-asset mesh hook; archive loading repeats validation.

## Runtime data contract

- `Mesh::lod_groups()` contains independent chains keyed by their vertex layout and
  ordered `LodGroupMember` identities. `PrimitiveId` indexes canonical primitives.
- A primitive can belong to at most one group for a layout, and cannot also carry
  a primitive-local `LodSet` for that layout.
- `LodGroup::snapshots()[ordinal]` is a **complete** representation, not a delta.
  All partitions must use that ordinal together. Select or fall back group-wide.
- `GroupSnapshot::partition(member)` returning `None` means that member has **zero
  triangles** at that ordinal. Do not substitute its canonical geometry.
- Counts strictly decrease across the whole group. Individual materials may grow,
  disappear, or reappear. Canonical material slots are never removed or renumbered.
- `GroupSnapshot::error()` is the producing error. `LodGroup::selection_error()` is
  its prefix maximum, suitable for one group-level budget and hysteresis state.
- Consumers with unsupported metrics, material overrides, deformation, alpha/coverage
  semantics, or incomplete staging should render all canonical members together.

### Error metrics

Join-only position and packed-normal working layouts retain `GeometricAbsolute`
and `AttributeWeightedAbsolute`. Voxelization or attribute reconstruction uses
`SampledGeometricAbsolute` / `SampledAttributeWeightedAbsolute`.

The latter add a deterministic **bidirectional sampled preparation deviation** to
the accumulated simplifier error. Samples include source/prepared vertices and
seven interior points per triangle; normal-weighted metrics also charge normal
deviation multiplied by `normal-weight` and the canonical group's largest bounding
box extent. This converts unit-normal differences to mesh-local distance units,
so scaling geometry and voxel size together scales the preparation error too.
Preparation is deducted from the available canonical-relative budget.
The sum is rounded outward. This is a selection estimate, not a certified Hausdorff
or silhouette/coverage bound. Geometry error is relative to canonical LOD0, while
material/UV correspondence uses the separately retained appearance reference.

### Custom build integration

The importer routes simplifying requests automatically. Custom build hooks can use
`pak::buf::{LodGroupRequest, LodSource, LodFinalizeSettings}` alongside `LodSettings`.
Pass sorted, unique `LodSource` entries with canonical primitive IDs and material
slots; each may also borrow a pre-reduction `reference`. Pass positions requiring
external locks and the importer's final optimization/index-cap settings.

Derived asset bakers can return a `MeshLodRequests` callback from
`DerivedAssetBaker::mesh_lod_requests`. Its `MeshLodImport` input borrows resolved
settings and authored metadata, with the recipe's normal-generation flag. Additional
layout requests therefore retain appearance capture before
`max-index` and the recipe's finalization controls. Authored layout requests take
precedence. The callback may run concurrently and must not depend on invocation order.

`LodGroupRequest::process` returns a fully finalized `LodGroup`. Attach groups with
`Mesh::set_lod_groups`; this validates against canonical primitives before replacing
the existing groups. `LodRequest::process` continues to support ordinary layout sets
and rejects simplifying calls with join/voxelize enabled, preventing accidental
loss of cross-material group semantics.

## Archives, diagnostics, and consumer handoff

The archive format is **V1.11**; older archives are rejected with a rebake message.
The producer identity is `clustered-v7-surface-correspondence`. Update consumer cache keys
using `pak::buf::MESH_LOD_PRODUCER` plus recipes/manifests and rebuild assets.

Both mesh CSV reports include group and snapshot IDs, aggregate triangle counts,
preparation error, active cells, working triangles, mixed/ambiguous votes, transfer
and chart-conflict counts, maximum sample projection distance, source/reference
identities, final buffer/patch counts, and termination reasons. Absent members get
explicit `omitted` rows. Reports regenerate using only the archive, including
segmented/compressed archives and cache hits. Final optimization settings are
included in group provenance.

`LodGroup::preparation()` retains the measured preparation error, active voxel cells,
and working triangles even when preparation exhausts the budget and only canonical
snapshot zero is published. The canonical error remains zero. `None` means that
preparation did not finish; reports leave these measurements blank instead of
claiming measured zeros. Correspondence diagnostics remain snapshot-local.

For Retro integration, the receiving changes are:

1. Route eligible build requests through `LodGroupRequest`, and version the LOD policy
   sidecar for group membership, the two sampled metrics, and atomic fallback.
2. Stage each full snapshot and select one ordinal per group/consumer/instance;
   evaluate effective material eligibility across every original member.
3. Fit bounds after finalization using the complete geometry envelope.
4. Enable named pilot mesh recipes and reference those recipe keys from source-owned
   scene generators. An inline GLB reference bypasses a separate TOML recipe's settings.
5. Revive grouped material-ID/textured review, including actual selected ordinals,
   omitted members, source-only reasons, and cold/cache-hit diagnostics. Measure
   representative asset build time/memory before selecting voxel sizes and limits.

Joining is confined to a single imported mesh. Independently exported building
chunks require independent recipes and border policy. Asset opt-ins enable baking;
visible runtime LOD use requires the grouped consumer above.
