# `.pak` Asset Crate

[![Crates.io](https://img.shields.io/crates/v/pak.svg)](https://crates.io/crates/pak)
[![Docs.rs](https://docs.rs/pak/badge.svg)](https://docs.rs/pak)

Bundles many assets into a single file with compression, string tables, and other game-related
special handling functions.

A `.pak` file is written at build time and streamed in at runtime. Each asset package is "baked" from a configuration source file. _Example:_

_Rust code_

```rust
PakBuf::bake("game_art.toml", "game_art.pak")?;
```

When generating bake inputs into a build/artifact directory, keep those generated files outside
the source tree and pass the source asset root explicitly:

```rust
PakBuf::bake_with_dir(
    out_dir.join("generated/game_art.toml"),
    out_dir.join("game_art.pak"),
    manifest_dir.join("assets"),
)?;
```

The explicit directory is used for asset globs, project-rooted asset paths, and pak keys. The
existing `PakBuf::bake(src, dst)` API remains available and uses the content file's parent
directory as before.

## Main `.pak` Configuration File

_`game_art.toml`:_

```toml
[content]
compression = 'snap'

[[content.group]]
assets = [
    'bitmap/**/*.png',
    'font/**/*.toml',
    'mesh/**/*.toml',
    'sound/**/*.ogg',
    'music/*.mp3',
    'ui/*.png',
]
```

### _`[content]` Schema_

All fields are optional.

Item | Description
---- | -----------
compression | `'snap'`, `'brotli'`, or unspecified (_no compression_).
buffer-size | (_`unsigned integer`_) Brotli buffer size. Used only when `compression = 'brotli'`. Defaults to `4096`.
quality | (_`unsigned integer`_) Brotli compression quality. Used only when `compression = 'brotli'`. Defaults to `8`.
window-size | (_`unsigned integer`_) Brotli window size. Used only when `compression = 'brotli'`. Defaults to `22`.

### _`[content.group]` Schema_

All fields are optional.

_Note:_

Additional `[[content.group]]` tables may be appended. All groups are added to the package and these
individual groups are not distinct entities in the runtime file.

Item | Description
---- | -----------
assets | (_`string array`_) File paths or glob patterns of assets to bake. Assets may be native file types (_such as `.png` and `.glb`_) or asset (_`.toml`_) files as detailed in the following sections.
exclude | (_`string array`_) File paths or glob patterns to exclude from the baking process when considering `assets`.
enabled | (_`boolean`_) Global flag which may be used to prevent baking of this group.

## 3D Animations

Bone structures and per-channel frame sample data may be loaded from `.gltf` or `.glb` files.

_Example, `cool-walk.toml`:_

```toml
[animation]
src = 'some_file.gltf'
```

_Or to simply load `some_file.gltf` using `some_file.toml`:_

```toml
[animation]
```

### _`[animation]` Schema_

All fields are optional.

Item | Description
---- | -----------
`src` | File path to a `.gltf` or `.glb` animation. May be relative to the `[animation]` TOML file or absolute where the root is the same folder as the `[content]` TOML file. When unspecified, attempts to load an animation with the same name as the `[animation]` TOML file.
`name` | Specific animation name (for use with files containing more than one animation).
`exclude` | Array of animation channel names to exclude from the import.

## 3D Meshes

Geometry may be loaded from `.gltf` or `.glb` files.

_Example, `large-goblet.toml`:_

```toml
[mesh]
src = 'some_file.gltf'
lod = true
max-index = 'u16'
```

### _`[mesh]` Schema_

All fields are optional.

Item | Description
---- | -----------
`data` | File path to an optional unstructured byte blob associated with this mesh.
`src` | File path to a `.gltf` or `.glb` mesh. May be relative to the `[mesh]` TOML file or absolute where the root is the same folder as the `[content]` TOML file. When unspecified, attempts to load a mesh with the same name as the `[mesh]` TOML file.
`euler` | (_`string`_) Order of operations applied to 3-channel `rotation` values (example: `xyz`, `zyx`, _etc_).
`flip-x` | (_`boolean`_) When set, flips the X component of all position vertices.
`flip-y` | (_`boolean`_) When set, flips the Y component of all position vertices.
`flip-z` | (_`boolean`_) When set, flips the Z component of all position vertices.
`ignore-skin` | (_`boolean`_) When set, any embedded skin data is ignored.
`lod` | (_`boolean`_) When set, generates level of detail meshes using MeshOpt.
`lod-lock-border` | (_`boolean`_) When set, tells MeshOpt to generate level of detail meshes using only interior vertices.
`lod-target-error` | (_`float`_) When set, tells MeshOpt to attempt to hit a certain error threshold between level of detail meshes.
`max-index` | (_`string` or `unsigned integer`_) When set, reduces and compacts the baked mesh so no LOD references an index greater than this value. Accepts `'u8'`, `'u16'`, or an exact value such as `4095` or `65535`. Uses MeshOpt simplification and may reduce the base mesh before generating LODs.
`min-lod-triangles` | (_`unsigned integer`_) When set, tells MeshOpt to stop generating level of detail meshes below this threshold.
`name` | (_`string`_) When set, imports this named mesh. Otherwise, imports the first mesh.
`normals` | (_`boolean`_) When set (default `true`), imports geometry normals.
`offset` | (_array of `float` with a length of 3_) When set, offsets geometry positions by the given amount.
`optimize` | (_`boolean`_) When set (default `true`), reorders geometry indices and vertices using MeshOpt.
`overdraw-threshold` | (_`float`_) When set (default `1.05`), controls MeshOpt optimization.
`rotation` | (_array of `float` with a length of 3 or 4_) When set, the vector (XYZ) or quaternion (XYZW) rotation applied to geometry.
`scale` | (_`float` or array of `float` with a length of 3_) When set, the uniform or vector (XYZ) scale applied to geometry.
`scene-name` | (_`string`_) When set, controls which GLTF scene is imported from the source file.
`shadow` | (_`boolean`_) When set, imports position-only geometry optimized for use in shadow or other similar rendering techniques.
`tangents` | (_`boolean`_) When set (default `true`), imports geometry tangents. If missing, tangents are generated using the MikkTSpace algorithm

`max-index` is a hard cap on the maximum index value, not just a triangle-count target. If a mesh initially needs indices above the cap, baking simplifies the mesh and compacts the vertex buffer so `IndexBuffer` can store the result as `u8` or `u16` where possible. Lower generated LODs are derived from the capped mesh.

## Raw Blobs

Arbitrary files may be baked as raw byte blobs.

_Example, `nav-data.toml`:_

```toml
[blob]
src = 'nav-data.bin'
```

### _`[blob]` Schema_

All fields are optional.

Item | Description
---- | -----------
`src` | File path to the blob. May be relative to the `[blob]` TOML file or absolute where the root is the same folder as the `[content]` TOML file. When unspecified, attempts to load a blob with the same name as the `[blob]` TOML file.

## PBR Materials

Material data for use in rendering.

All fields are optional.

_Example, `glowing-lava.toml`:_

```toml
[material]
color = 'my-texture.png'
```

### _`[material]` Schema_

Item | Description
---- | -----------
`color` | Hex string, path string, inline bitmap asset, or sequence.
`height` | Hex string, path string, inline bitmap asset, or floating point value.
`double-sided` | (`boolean`_) When set, indicates the material is double-sided.
`emissive` | Hex string, path string, inline bitmap asset, or array of three floating point values.
`metal` | Hex string, path string, inline bitmap asset, or floating point value.
`normal` | Path string or inline bitmap asset.
`rough` | Hex string, path string, inline bitmap asset, or floating point value.
`transmission` | Hex string, path string, inline bitmap asset, or floating point value.

When any material parameter is set, `MaterialInfo::params` stores the packed RGBA parameter map as `R = metal`, `G = rough`, `B = height`, and `A = transmission`. `MaterialInfo::params_used` indicates which channels were authored; missing channels are default-filled.

## Bitmaps

Variable-channel bitmap data (stored raw and compressed using the setting of the `[content]` compression).

_Example, `ui-skin-001.toml`:_

```toml
[bitmap]
src = 'my-texture.png'
```

### _`[bitmap]` Schema_

All fields are optional.

Item | Description
---- | -----------
`src` | File path to an image. May be relative to the `[bitmap]` TOML file or absolute where the root is the same folder as the `[content]` TOML file. When unspecified, attempts to load a bitmap with the same name as the `[bitmap]` TOML file.
`mip-levels` | (_`boolean` or `non-zero unsigned integer`_) When set (default `1`), allows configuration of the desired count of mip levels to be stored with a bitmap for later use by a program.
`resize` | (_`unsigned integer`_) When set, the image is uniformly resized to have this maximum dimension.
`color` | (_`string`_) When set (default `srgb`), the image is imported as either `linear` or `srgb` color data.
`swizzle` | (_`string`_) When set (default `rgba` for four channel images), the specified image color channels are imported in the given order (example: `r`, `rg` or `bgr`).

### Bitmap Fonts

Special handling is given to `[bitmap-font]` asset files.

_Example, `font-h4.toml`:_

```toml
[bitmap-font]
src = 'blocky-letters.fon'
```

The specified file is imported as a raw AngelCode bitmap font file, and any associated page images are loaded as bitmaps.

### _`[bitmap-font]` Schema_

All fields are optional.

Item | Description
---- | -----------
`src` | File path to a bitmap font definition. May be relative to the `[bitmap-font]` TOML file or absolute where the root is the same folder as the `[content]` TOML file. When unspecified, attempts to load a bitmap font definition with the same name as the `[bitmap-font]` TOML file.

## Scenes

Scene files may contain custom geometry and spatial reference data, each with the ability to store generic data as well.

_Example, `large-castle.toml`:_

```toml
[scene]

[[scene.ref]]
id = 'EnemySpawn'
rotation = [0.0, 0.0, 0.0, 1.0]
translation = [-1.0, 0.1, -6.0]
tags = [
    'bot',
]
data.type = 'monster3'

[[scene.ref]]
id = 'red-door'
rotation = [0.0, 0.0, 0.0, 1.0]
translation = [-0.9, 0.0, -12.0]
tags = [
    'door',
]
data.require = 'red-key'
data.joins = 'a,b'

[[scene.ref]]
mesh = '../kaykit/dungeon/floor_tile_large_grates.toml'
materials = [
    '../kaykit/dungeon/texture.toml',
]
rotation = [0.0, 0.0, -0.0, 1.0]
translation = [0.0, 0.0, -0.0]

[[scene.ref]]
mesh = '../kaykit/dungeon/floor_tile_large.toml'
materials = [
    '../kaykit/dungeon/texture.toml',
]
rotation = [0.0, 0.0, -0.0, 1.0]
translation = [6.0, 0.0, -2.0]

[[scene.geometry]]
id = 'a'
indices = [
    2, 0, 1,
    1, 3, 4,
    9, 11, 8,
    1, 4, 9,
    5, 2, 1,
    12, 10, 6,
    6, 5, 1,
    6, 1, 9,
    13, 14, 12,
    12, 6, 9,
    8, 13, 12,
    9, 8, 12,
]
vertices = [
    -8.399999618530273, 0.0, 7.400000095367432,
    6.400000095367432, 0.0, 7.400000095367432,
    -8.399999618530273, 0.0, -7.400000095367432,
    6.400000095367432, 0.0, -7.400000095367432,
    1.0, 0.0, -7.400000095367432,
    -3.0, 0.0, -7.400000095367432,
    -2.4000000953674316, 0.0, -8.0,
    1.0, 0.0, -7.400000095367432,
    -0.09999996423721313, 0.0, -11.40000057220459,
    0.3999999761581421, 0.0, -8.0,
    -2.4000000953674316, 0.0, -11.40000057220459,
    0.3999999761581421, 0.0, -11.40000057220459,
    -1.9000000953674316, 0.0, -11.40000057220459,
    -0.09999996423721313, 0.0, -12.0,
    -1.9000000953674316, 0.0, -12.0,
]
rotation = [0.0, 0.0, -0.0, 1.0]
translation = [1.0, 0.10000000149011612, -0.0]
tags = [
    'nav-mesh',
]

[[scene.ref]]
id = 'Camera'
rotation = [0.06613656878471375, -0.8706687092781067, 0.4718790054321289, 0.12203358113765717]
translation = [-19.0, 61.0, -53.0]
tags = [
    'camera',
]
data.type = 'persp'
data.z-near = 1.0
data.z-far = 200.0
data.fov-y = 0.349344402551651
```

### _`[scene]` Schema_

All fields are optional.

Item | Description
---- | -----------
`[[scene.geometry]]` | Inline indexed triangle geometry tables.
`[[scene.ref]]` | Scene reference tables for meshes, materials, transforms, tags, and custom data.

### _`[[scene.geometry]]` Schema_

Item | Description
---- | -----------
`id` | (_`string`_) Optional identifier. It is not required to be unique.
`euler` | (_`string`_) Order of operations applied to 3-channel `rotation` values (example: `xyz`, `zyx`, _etc_).
`indices` | (_array of `unsigned integer`_) Triangle index list. Length must be a multiple of 3.
`rotation` | (_array of `float` with a length of 3 or 4_) Euler XYZ degrees or quaternion XYZW rotation.
`translation` | (_array of `float` with a length of 3_) Translation applied to the geometry.
`vertices` | (_array of `float`_) Packed XYZ position values.
`tags` | (_array of `string`_) Program-specific tags. Tags are trimmed, lowercased, and sorted during baking.
`data` | TOML table of program-specific values. Values may be booleans, strings, i32 integers, floats, or arrays of those values.

### _`[[scene.ref]]` Schema_

Item | Description
---- | -----------
`id` | (_`string`_) Optional identifier. It is not required to be unique.
`euler` | (_`string`_) Order of operations applied to 3-channel `rotation` values (example: `xyz`, `zyx`, _etc_).
`materials` | (_array_) Material asset references. Each entry may be a material TOML path, image path, or inline material asset.
`mesh` | Mesh asset reference. May be a mesh TOML path, `.gltf`/`.glb` path, or inline mesh asset.
`rotation` | (_array of `float` with a length of 3 or 4_) Euler XYZ degrees or quaternion XYZW rotation.
`translation` | (_array of `float` with a length of 3_) Translation applied to the reference.
`tags` | (_array of `string`_) Program-specific tags. Tags are trimmed, lowercased, and sorted during baking.
`data` | TOML table of program-specific values. Values may be booleans, strings, i32 integers, floats, or arrays of those values.

## Tests

Run tests with all features in order to include the baking code:

```bash
cargo test --all-features
```
