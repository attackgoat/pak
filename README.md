# `.pak` Asset Crate

[![Crates.io](https://img.shields.io/crates/v/pak.svg)](https://crates.io/crates/pak)
[![Docs.rs](https://docs.rs/pak/badge.svg)](https://docs.rs/pak)

## `.pak` Configuration File

Each asset package is "baked" from a configuration source file. _Example:_

_Rust code_

```rust
PakBuf::bake("game_art.toml", "game_art.pak")?;
```

_`game_art.toml`_

```toml
[content]
compression = 'snap'

[[content.group]]
assets = [
    'bitmap/**/*.png',
    'font/**/*.toml',
    'model/**/*.toml',
    'sound/**/*.ogg',
    'music/*.mp3',
    'ui/*.png',
]
```

_Note:_

Additional `[[content.group]]` tables may be appended. All groups are added to the package and these
individual groups are not distinct entities in the runtime file.

_`[content]` Schema_

Item | Description
---- | -----------
compression | May be omitted, `'snap'` or `'x'`

## 3D Models



_Example:_

```toml
[model]
src = "some_file.gltf"

```

_`[model]` Schema_

Item | Description
---- | -----------
`src` | File path to the `.gltf` or `.glb` model file. May be relative to the `[model]` TOML file or absolute where the root is the same folder as the `[content]` TOML file.

## Tests

Run tests with all features in order to include the baking code:

```bash
cargo test --all-features
```