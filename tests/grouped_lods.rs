#![cfg(feature = "bake")]

use {
    pak::{
        Pak as _, PakBuf,
        buf::{DerivedAssetBakeCandidate, DerivedAssetBakeOutput, DerivedAssetBaker},
        mesh::{LodMetric, Mesh, Primitive},
    },
    std::{fs, path::Path},
};

fn fixture(root: &Path) {
    let mut bytes = Vec::new();
    for y in 0..9 {
        for x in 0..9 {
            for value in [
                x as f32,
                y as f32,
                0.,
                0.,
                0.,
                1.,
                x as f32 / 8.,
                y as f32 / 8.,
            ] {
                bytes.extend(value.to_le_bytes());
            }
        }
    }
    let vertex_bytes = bytes.len();
    let mut counts = [0; 2];
    for (member, count) in counts.iter_mut().enumerate() {
        for y in 0..8 {
            for x in 0..8 {
                if (x == 4 && y == 4) != (member == 1) {
                    continue;
                }
                let i = y * 9 + x;
                for index in [i, i + 1, i + 9, i + 1, i + 10, i + 9] {
                    bytes.extend((index as u32).to_le_bytes());
                    *count += 1;
                }
            }
        }
    }
    fs::write(root.join("grid.bin"), &bytes).unwrap();
    let json = format!(
        r#"{{
      "asset": {{"version":"2.0"}},
      "buffers":[{{"uri":"grid.bin","byteLength":{}}}],
      "bufferViews":[
        {{"buffer":0,"byteOffset":0,"byteLength":{},"byteStride":32,"target":34962}},
        {{"buffer":0,"byteOffset":{},"byteLength":{},"target":34963}},
        {{"buffer":0,"byteOffset":{},"byteLength":{},"target":34963}}
      ],
      "accessors":[
        {{"bufferView":0,"byteOffset":0,"componentType":5126,"count":81,"type":"VEC3","min":[0,0,0],"max":[8,8,0]}},
        {{"bufferView":0,"byteOffset":12,"componentType":5126,"count":81,"type":"VEC3"}},
        {{"bufferView":0,"byteOffset":24,"componentType":5126,"count":81,"type":"VEC2"}},
        {{"bufferView":1,"componentType":5125,"count":{},"type":"SCALAR"}},
        {{"bufferView":2,"componentType":5125,"count":{},"type":"SCALAR"}}
      ],
      "materials":[{{}},{{}}],
      "meshes":[{{"primitives":[
        {{"attributes":{{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2}},"indices":3,"material":0}},
        {{"attributes":{{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2}},"indices":4,"material":1}}
      ]}}],
      "nodes":[{{"mesh":0,"name":"grid"}}],"scenes":[{{"nodes":[0]}}],"scene":0
    }}"#,
        bytes.len(),
        vertex_bytes,
        vertex_bytes,
        counts[0] * 4,
        vertex_bytes + counts[0] * 4,
        counts[1] * 4,
        counts[0],
        counts[1]
    );
    fs::write(root.join("grid.gltf"), json).unwrap();
    fs::write(
        root.join("on.toml"),
        "[mesh]\nsrc='grid.gltf'\nname='grid'\noptimize=false\n",
    )
    .unwrap();
    fs::write(
        root.join("off.toml"),
        "[mesh]\nsrc='grid.gltf'\n[mesh.lod]\njoin=false\nvoxelize=false\n",
    )
    .unwrap();
    fs::write(
        root.join("voxel.toml"),
        "[mesh]\nsrc='grid.gltf'\n[mesh.lod]\njoin=false\nvoxelize=true\nvoxel-size=0.5\n",
    )
    .unwrap();
    fs::write(
        root.join("capped.toml"),
        "[mesh]\nsrc='grid.gltf'\nmax-index=15\n",
    )
    .unwrap();
    fs::write(
        root.join("scene.toml"),
        "[scene]\n[[scene.ref]]\nmesh={src='grid.gltf',offset=[10.0,0.0,0.0]}\n",
    )
    .unwrap();
    fs::write(root.join("pak.toml"), "[content]\ndefault-lods=[{layout='POSITION',simplify=true},{layout='PACKED_NORMAL | TEXTURE0',simplify=false}]\n[content.lod]\njoin=true\nmin-triangles=2\ntarget-error=0.1\nlock-border=true\n[[content.group]]\nname='geometry'\nassets=['grid.gltf','on.toml','off.toml','voxel.toml','scene.toml','capped.toml']\ncompression='snap'\n").unwrap();
}

#[test]
fn grouped_bakes_cover_direct_keyed_inline_inheritance_and_archive_only_reports() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root);
    let output = root.join("groups.pak");
    PakBuf::bake_with_dir_without_cargo_watches(root.join("pak.toml"), &output, root).unwrap();
    let mut pak = PakBuf::open(&output).unwrap();
    let inline_id = pak
        .read_scene("scene")
        .unwrap()
        .refs()
        .next()
        .unwrap()
        .mesh()
        .unwrap();
    for mesh in [
        pak.read_mesh("grid.gltf").unwrap(),
        pak.read_mesh("on").unwrap(),
        pak.read_mesh_id(inline_id).unwrap(),
    ] {
        assert_eq!(mesh.lod_groups().len(), 1);
        let group = &mesh.lod_groups()[0];
        assert_eq!(group.members().len(), 2);
        assert_eq!(group.metric(), LodMetric::GeometricAbsolute);
        assert!(
            group.snapshots().len() > 1,
            "{}",
            group.provenance().stop_reason
        );
        assert_eq!(group.snapshots().last().unwrap().partitions().len(), 1);
        assert!(mesh.primitives().iter().all(|p| p.lod_sets().len() == 1));
        group.validate_source(mesh.primitives()).unwrap();
    }
    let off = pak.read_mesh("off").unwrap();
    assert!(off.lod_groups().is_empty());
    assert!(off.primitives().iter().all(|p| p.lod_sets().len() == 2));
    let voxel = pak.read_mesh("voxel").unwrap();
    assert_eq!(voxel.lod_groups().len(), 2);
    assert!(
        voxel
            .lod_groups()
            .iter()
            .all(|g| g.members().len() == 1 && g.snapshots().len() == 1)
    );
    let capped = pak.read_mesh("capped").unwrap();
    let group = &capped.lod_groups()[0];
    assert!(
        group
            .members()
            .iter()
            .any(|m| m.source_identity() != m.reference_identity())
    );
    for snapshot in group.snapshots() {
        for part in snapshot.partitions() {
            assert!(
                part.lod()
                    .geometry()
                    .indices()
                    .as_u32()
                    .iter()
                    .all(|&index| index <= 15)
            );
        }
    }
    let report = fs::read(output.with_extension("mesh-lods.csv")).unwrap();
    let diagnostics = fs::read(output.with_extension("mesh-lod-diagnostics.csv")).unwrap();
    let text = std::str::from_utf8(&report).unwrap();
    assert!(text.contains("snapshot-triangles"));
    assert!(text.contains("omitted"));
    assert!(text.contains("[finalize]"));
    assert!(text.contains("voxel-external-locks"));
    fs::remove_file(root.join("grid.gltf")).unwrap();
    fs::remove_file(root.join("grid.bin")).unwrap();
    pak.write_mesh_lod_report(root.join("regenerated.csv"))
        .unwrap();
    pak.write_mesh_lod_diagnostics(root.join("diagnostics.csv"))
        .unwrap();
    assert_eq!(fs::read(root.join("regenerated.csv")).unwrap(), report);
    assert_eq!(fs::read(root.join("diagnostics.csv")).unwrap(), diagnostics);
}

struct ChangeGroupedSource;

impl DerivedAssetBaker for ChangeGroupedSource {
    fn bake_mesh(&mut self, mesh: &mut Mesh) -> anyhow::Result<()> {
        if !mesh.lod_groups().is_empty() {
            mesh.primitives_mut()[0] = Primitive::new(255, mesh.primitives()[0].base().clone())?;
        }
        Ok(())
    }

    fn bake(
        &mut self,
        _: &DerivedAssetBakeCandidate<'_>,
    ) -> anyhow::Result<Box<[DerivedAssetBakeOutput]>> {
        Ok(Box::new([]))
    }
}

#[test]
fn stale_groups_from_mesh_hooks_fail_before_replacing_the_archive() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root);
    let output = root.join("groups.pak");
    PakBuf::bake_with_dir_without_cargo_watches(root.join("pak.toml"), &output, root).unwrap();
    let previous = fs::read(&output).unwrap();
    let error = PakBuf::bake_with_dir_and_derived_assets_without_cargo_watches(
        root.join("pak.toml"),
        &output,
        root,
        &mut ChangeGroupedSource,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("group source association mismatch"),
        "{error:#}"
    );
    assert_eq!(fs::read(output).unwrap(), previous);
}

#[test]
fn rejected_preparation_reports_archive_measurements_without_changing_source_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut bytes = Vec::new();
    let mut indices = Vec::new();
    for (origin, u, v, normal) in [
        ([-1., -1., 1.], [2., 0., 0.], [0., 2., 0.], [0., 0., 1.]),
        ([-1., 1., -1.], [2., 0., 0.], [0., -2., 0.], [0., 0., -1.]),
        ([1., -1., -1.], [0., 2., 0.], [0., 0., 2.], [1., 0., 0.]),
        ([-1., -1., 1.], [0., 2., 0.], [0., 0., -2.], [-1., 0., 0.]),
        ([-1., 1., 1.], [2., 0., 0.], [0., 0., -2.], [0., 1., 0.]),
        ([-1., -1., -1.], [2., 0., 0.], [0., 0., 2.], [0., -1., 0.]),
    ] {
        let offset = bytes.len() as u32 / 24;
        for y in 0..5 {
            for x in 0..5 {
                for axis in 0..3 {
                    let value: f32 =
                        origin[axis] + u[axis] * x as f32 / 4.0 + v[axis] * y as f32 / 4.0;
                    bytes.extend(value.to_le_bytes());
                }
                for value in normal {
                    bytes.extend((value as f32).to_le_bytes());
                }
            }
        }
        for y in 0..4 {
            for x in 0..4 {
                let i = offset + y * 5 + x;
                indices.extend([i, i + 1, i + 5, i + 1, i + 6, i + 5]);
            }
        }
    }
    let vertex_bytes = bytes.len();
    bytes.extend(indices.iter().flat_map(|index| index.to_le_bytes()));
    fs::write(root.join("cube.bin"), &bytes).unwrap();
    fs::write(root.join("cube.gltf"), format!(r#"{{
      "asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0}}],
      "buffers":[{{"uri":"cube.bin","byteLength":{}}}],
      "bufferViews":[{{"buffer":0,"byteLength":{},"byteStride":24}},{{"buffer":0,"byteOffset":{},"byteLength":{}}}],
      "accessors":[
        {{"bufferView":0,"componentType":5126,"count":150,"type":"VEC3","min":[-1,-1,-1],"max":[1,1,1]}},
        {{"bufferView":0,"byteOffset":12,"componentType":5126,"count":150,"type":"VEC3"}},
        {{"bufferView":1,"componentType":5125,"count":{},"type":"SCALAR"}}
      ],"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0,"NORMAL":1}},"indices":2}}]}}]
    }}"#, bytes.len(), vertex_bytes, vertex_bytes, indices.len() * 4, indices.len())).unwrap();
    fs::write(root.join("cube.toml"), "[mesh]\nsrc='cube.gltf'\ntangents=false\nlods=[{layout='PACKED_NORMAL',simplify=true}]\n[mesh.lod]\nvoxelize=true\nvoxel-size=0.4\nmin-triangles=8\nnormal-weight=0.5\ntarget-error=0.0001\n").unwrap();
    fs::write(
        root.join("pak.toml"),
        "[[content.group]]\nassets=['cube.toml']\n",
    )
    .unwrap();
    let output = root.join("rejected.pak");
    PakBuf::bake_with_dir_without_cargo_watches(root.join("pak.toml"), &output, root).unwrap();
    let mut pak = PakBuf::open(&output).unwrap();
    let mesh = pak.read_mesh("cube").unwrap();
    let group = &mesh.lod_groups()[0];
    assert_eq!(group.snapshots().len(), 1);
    assert_eq!(group.snapshots()[0].error(), 0.0);
    let facts = group.preparation().unwrap();
    assert!(facts.error > 0.0 && facts.voxel_cells > 0 && facts.working_triangles > 0);
    let measured = format!(
        "preparation-error-budget-exhausted,0,192,0,{},{},{},",
        facts.error, facts.voxel_cells, facts.working_triangles
    );
    for extension in ["mesh-lods.csv", "mesh-lod-diagnostics.csv"] {
        let original = fs::read(output.with_extension(extension)).unwrap();
        assert!(std::str::from_utf8(&original).unwrap().contains(&measured));
        let regenerated = root.join(format!("regenerated-{extension}"));
        if extension == "mesh-lods.csv" {
            pak.write_mesh_lod_report(&regenerated).unwrap();
        } else {
            pak.write_mesh_lod_diagnostics(&regenerated).unwrap();
        }
        assert_eq!(fs::read(regenerated).unwrap(), original);
    }
}
