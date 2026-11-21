#![cfg(feature = "bake")]

use {
    pak::{Pak as _, PakBuf, mesh::Geometry},
    serde_json::{Value, json},
    std::{fs, path::Path},
};

fn fixture(root: &Path) -> Value {
    let mut bytes = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut meshes = Vec::new();
    for level in 0..3 {
        let mut primitives = Vec::new();
        for member in 0..2 {
            let steps = if level == 0 { 2 } else { 1 };
            let vertex_offset = bytes.len();
            for y in 0..=steps {
                for x in 0..=steps {
                    for value in [
                        member as f32 * 3.0 + x as f32 / steps as f32,
                        y as f32 / steps as f32,
                        level as f32 * 0.25,
                        0.0,
                        0.0,
                        1.0,
                        x as f32 / steps as f32 + level as f32,
                        y as f32 / steps as f32,
                        1.0,
                        0.0,
                        0.0,
                        1.0,
                    ] {
                        bytes.extend(value.to_le_bytes());
                    }
                }
            }
            let view = views.len();
            views.push(json!({"buffer": 0, "byteOffset": vertex_offset, "byteLength": bytes.len() - vertex_offset, "byteStride": 48}));
            let attribute = accessors.len();
            for (offset, kind) in [(0, "VEC3"), (12, "VEC3"), (24, "VEC2"), (32, "VEC4")] {
                let mut accessor = json!({"bufferView": view, "byteOffset": offset, "componentType": 5126, "count": (steps + 1) * (steps + 1), "type": kind});
                if offset == 0 {
                    accessor["min"] = json!([member as f32 * 3.0, 0.0, level as f32 * 0.25]);
                    accessor["max"] = json!([member as f32 * 3.0 + 1.0, 1.0, level as f32 * 0.25]);
                }
                accessors.push(accessor);
            }
            let index_offset = bytes.len();
            for y in 0..steps {
                for x in 0..steps {
                    let i = (y * (steps + 1) + x) as u32;
                    let width = (steps + 1) as u32;
                    for index in [i, i + 1, i + width] {
                        bytes.extend(index.to_le_bytes());
                    }
                    if level != 2 {
                        for index in [i + 1, i + width + 1, i + width] {
                            bytes.extend(index.to_le_bytes());
                        }
                    }
                }
            }
            let index_bytes = bytes.len() - index_offset;
            views.push(json!({"buffer": 0, "byteOffset": index_offset, "byteLength": index_bytes}));
            let indices = accessors.len();
            accessors.push(json!({"bufferView": view + 1, "componentType": 5125, "count": index_bytes / 4, "type": "SCALAR"}));
            primitives.push(json!({"attributes": {"POSITION": attribute, "NORMAL": attribute + 1, "TEXCOORD_0": attribute + 2, "TANGENT": attribute + 3}, "indices": indices, "material": 1 - member}));
        }
        meshes.push(json!({"primitives": primitives}));
    }
    fs::write(root.join("source.bin"), &bytes).unwrap();
    let document = json!({
        "asset": {"version": "2.0"}, "scene": 0, "scenes": [{"nodes": [0]}, {"name": "alternatives", "nodes": [1, 2]}],
        "nodes": [
            {"name": "base", "mesh": 0, "translation": [0.0, 0.0, 0.125]},
            {"name": "lod1", "mesh": 1, "translation": [0.0, 0.0, 0.125]},
            {"name": "lod2", "mesh": 2, "translation": [0.0, 0.0, 0.125]}
        ],
        "buffers": [{"uri": "source.bin", "byteLength": bytes.len()}],
        "bufferViews": views, "accessors": accessors, "materials": [{}, {}], "meshes": meshes
    });
    fs::write(
        root.join("source.gltf"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
    document
}

fn skin_fixture(root: &Path) -> Value {
    let mut document = fixture(root);
    let mut bytes = fs::read(root.join("source.bin")).unwrap();
    for level in 0..3 {
        document["nodes"][level]["skin"] = json!(0);
        for member in 0..2 {
            let positions =
                document["meshes"][level]["primitives"][member]["attributes"]["POSITION"]
                    .as_u64()
                    .unwrap() as usize;
            let count = document["accessors"][positions]["count"].as_u64().unwrap() as usize;
            let view = document["bufferViews"].as_array().unwrap().len();
            let offset = bytes.len();
            for _ in 0..count {
                for joint in [0u16, 1, 0, 0] {
                    bytes.extend(joint.to_le_bytes());
                }
            }
            document["bufferViews"].as_array_mut().unwrap().push(
                json!({"buffer": 0, "byteOffset": offset, "byteLength": bytes.len() - offset}),
            );
            let accessor = document["accessors"].as_array().unwrap().len();
            document["accessors"].as_array_mut().unwrap().push(
                json!({"bufferView": view, "componentType": 5123, "count": count, "type": "VEC4"}),
            );
            document["meshes"][level]["primitives"][member]["attributes"]["JOINTS_0"] =
                json!(accessor);
            let offset = bytes.len();
            for _ in 0..count {
                for weight in [128.0f32 / 255.0, 127.0 / 255.0, 0.0, 0.0] {
                    bytes.extend(weight.to_le_bytes());
                }
            }
            document["bufferViews"].as_array_mut().unwrap().push(
                json!({"buffer": 0, "byteOffset": offset, "byteLength": bytes.len() - offset}),
            );
            document["accessors"].as_array_mut().unwrap().push(json!({"bufferView": view + 1, "componentType": 5126, "count": count, "type": "VEC4"}));
            document["meshes"][level]["primitives"][member]["attributes"]["WEIGHTS_0"] =
                json!(accessor + 1);
        }
    }
    let offset = bytes.len();
    for joint in 0..2 {
        let mut matrix = [0.0f32; 16];
        for axis in 0..4 {
            matrix[axis * 5] = 1.0;
        }
        matrix[13] = -(joint as f32);
        bytes.extend(matrix.into_iter().flat_map(f32::to_le_bytes));
    }
    let view = document["bufferViews"].as_array().unwrap().len();
    document["bufferViews"]
        .as_array_mut()
        .unwrap()
        .push(json!({"buffer": 0, "byteOffset": offset, "byteLength": bytes.len() - offset}));
    let inverse = document["accessors"].as_array().unwrap().len();
    document["accessors"]
        .as_array_mut()
        .unwrap()
        .push(json!({"bufferView": view, "componentType": 5126, "count": 2, "type": "MAT4"}));
    document["nodes"].as_array_mut().unwrap().extend([
        json!({"name": "root", "children": [4]}),
        json!({"name": "branch", "translation": [0.0, 1.0, 0.0]}),
        json!({"name": "root", "children": [6]}),
        json!({"name": "branch", "translation": [0.0, 1.0, 0.0]}),
    ]);
    document["skins"] = json!([
        {"joints": [3, 4], "inverseBindMatrices": inverse},
        {"joints": [5, 6], "inverseBindMatrices": inverse}
    ]);
    for level in [1, 2] {
        document["nodes"][level]["skin"] = json!(1);
    }
    document["scenes"][0]["nodes"] = json!([0, 3]);
    document["scenes"][1]["nodes"] = json!([1, 2, 5]);
    document["buffers"][0]["byteLength"] = json!(bytes.len());
    fs::write(root.join("source.bin"), bytes).unwrap();
    fs::write(
        root.join("source.gltf"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
    document
}

fn recipe(name: &str, extra: &str) -> String {
    let scene = if name == "base" {
        ""
    } else {
        "scene-name='alternatives'\n"
    };
    format!(
        "[mesh]\nsrc='source.gltf'\nname='{name}'\n{scene}optimize=false\nscale=[2.0,3.0,4.0]\n{extra}\n"
    )
}

fn triangles(geometry: &Geometry) -> Vec<[u32; 3]> {
    let mut faces = geometry
        .indices()
        .as_u32()
        .chunks_exact(3)
        .map(|face| {
            let [a, b, c] = <[u32; 3]>::try_from(face).unwrap();
            [a, b, c].min([b, c, a]).min([c, a, b])
        })
        .collect::<Vec<_>>();
    faces.sort_unstable();
    faces
}

#[test]
fn authored_lods_round_trip_exact_geometry_and_replace_automatic_source_chains() {
    check_round_trip(false);
}

#[test]
fn authored_skinned_lods_round_trip_exact_attributes_and_one_compatible_palette() {
    check_round_trip(true);
}

fn check_round_trip(skinned: bool) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    if skinned {
        skin_fixture(root);
    } else {
        fixture(root);
    }
    fs::write(
        root.join("authored.toml"),
        recipe("base", "authored-lods=['lod1','lod2']"),
    )
    .unwrap();
    for name in ["base", "lod1", "lod2"] {
        fs::write(
            root.join(format!("{name}.toml")),
            recipe(name, "inherit-lods=false\nsource-lods=false"),
        )
        .unwrap();
    }
    fs::write(root.join("scene.toml"), "[scene]\n[[scene.ref]]\nmesh={src='source.gltf',name='base',authored-lods=['lod1','lod2'],optimize=false,scale=[2.0,3.0,4.0]}\n").unwrap();
    fs::write(root.join("pak.toml"), "[content]\ndefault-source-lods=true\n[[content.group]]\nassets=['authored.toml','base.toml','lod1.toml','lod2.toml','scene.toml']\n").unwrap();
    let path = root.join("test.pak");
    PakBuf::bake_with_dir_without_cargo_watches(root.join("pak.toml"), &path, root).unwrap();
    let mut pak = PakBuf::open(path).unwrap();
    let reference = ["base", "lod1", "lod2"].map(|key| pak.read_mesh(key).unwrap());
    let scene = pak.read_scene("scene").unwrap();
    let inline = pak
        .read_mesh_id(scene.refs().next().unwrap().mesh().unwrap())
        .unwrap();
    for mesh in [pak.read_mesh("authored").unwrap(), inline] {
        assert_eq!(mesh.primitives().len(), 2);
        assert!(mesh.lod_groups().is_empty());
        assert_eq!(mesh.skin().is_some(), skinned);
        if let Some(skin) = mesh.skin() {
            assert_eq!(skin.joints().len(), 2);
            assert_eq!(skin.joints()[0].name, "root");
            assert_eq!(skin.joints()[1].name, "branch");
            assert_eq!(skin.joints()[1].parent_index, 0);
            for expected in &reference {
                for (actual, expected) in
                    skin.joints().iter().zip(expected.skin().unwrap().joints())
                {
                    assert_eq!(actual.name, expected.name);
                    assert_eq!(actual.parent_index, expected.parent_index);
                    assert_eq!(actual.inverse_bind, expected.inverse_bind);
                }
            }
        }
        for (idx, primitive) in mesh.primitives().iter().enumerate() {
            assert_eq!(primitive.material(), (1 - idx) as u8);
            assert_eq!(primitive.base(), reference[0].primitives()[idx].base());
            assert_eq!(primitive.lod_sets().len(), 1);
            let set = &primitive.lod_sets()[0];
            assert_eq!(set.metric(), pak::mesh::LodMetric::SampledGeometricAbsolute);
            assert_eq!(set.provenance().unwrap().stop_reason, "authored-levels");
            assert_eq!(set.levels().len(), 3);
            for (level, expected_triangles) in [8, 2, 1].into_iter().enumerate() {
                let geometry = set.levels()[level].geometry();
                let expected = reference[level].primitives()[idx].base();
                assert_eq!(geometry.vertex_data(), expected.vertex_data());
                assert_eq!(geometry.vertex_type(), expected.vertex_type());
                assert_eq!(triangles(geometry), triangles(expected));
                assert_eq!(geometry.indices().triangle_count(), expected_triangles);
            }
            assert!((set.levels()[1].error() - 1.0).abs() < 1e-5);
            assert!(set.levels()[2].error() >= 2.0);
        }
    }
}

#[test]
fn authored_skinned_lods_reject_mismatched_palettes_and_invalid_influences_before_packing() {
    for case in [
        "missing-skin",
        "joint-order",
        "joint-name",
        "parent",
        "inverse-bind",
        "missing-joints",
        "extra-influences",
        "index",
        "wide-index",
        "weight",
        "packed-sum",
        "palette-size",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut document = skin_fixture(root);
        let mut bytes = fs::read(root.join("source.bin")).unwrap();
        match case {
            "missing-skin" => {
                document["nodes"][1].as_object_mut().unwrap().remove("skin");
            }
            "joint-order" => document["skins"][1]["joints"] = json!([6, 5]),
            "joint-name" => document["nodes"][6]["name"] = json!("unrelated"),
            "parent" => {
                document["nodes"][5]
                    .as_object_mut()
                    .unwrap()
                    .remove("children");
            }
            "inverse-bind" => {
                let old = document["skins"][1]["inverseBindMatrices"]
                    .as_u64()
                    .unwrap() as usize;
                let mut accessor = document["accessors"][old].clone();
                let view = accessor["bufferView"].as_u64().unwrap() as usize;
                let offset = document["bufferViews"][view]["byteOffset"]
                    .as_u64()
                    .unwrap() as usize;
                let mut matrices = bytes[offset..offset + 128].to_vec();
                matrices[64 + 48..64 + 52].copy_from_slice(&0.25f32.to_le_bytes());
                let view = document["bufferViews"].as_array().unwrap().len();
                document["bufferViews"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"buffer": 0, "byteOffset": bytes.len(), "byteLength": 128}));
                bytes.extend(matrices);
                accessor["bufferView"] = json!(view);
                let index = document["accessors"].as_array().unwrap().len();
                document["accessors"].as_array_mut().unwrap().push(accessor);
                document["skins"][1]["inverseBindMatrices"] = json!(index);
            }
            "missing-joints" => {
                document["meshes"][1]["primitives"][0]["attributes"]
                    .as_object_mut()
                    .unwrap()
                    .remove("JOINTS_0");
            }
            "extra-influences" => {
                document["meshes"][1]["primitives"][0]["attributes"]["JOINTS_1"] =
                    document["meshes"][1]["primitives"][0]["attributes"]["JOINTS_0"].clone();
                document["meshes"][1]["primitives"][0]["attributes"]["WEIGHTS_1"] =
                    document["meshes"][1]["primitives"][0]["attributes"]["WEIGHTS_0"].clone();
            }
            "index" | "wide-index" | "weight" | "packed-sum" => {
                let level = usize::from(case != "wide-index");
                let semantic = if matches!(case, "index" | "wide-index") {
                    "JOINTS_0"
                } else {
                    "WEIGHTS_0"
                };
                let accessor = document["meshes"][level]["primitives"][0]["attributes"][semantic]
                    .as_u64()
                    .unwrap() as usize;
                let view = document["accessors"][accessor]["bufferView"]
                    .as_u64()
                    .unwrap() as usize;
                let offset = document["bufferViews"][view]["byteOffset"]
                    .as_u64()
                    .unwrap() as usize;
                match case {
                    "index" => bytes[offset..offset + 2].copy_from_slice(&2u16.to_le_bytes()),
                    "wide-index" => {
                        bytes[offset..offset + 2].copy_from_slice(&256u16.to_le_bytes())
                    }
                    "weight" => bytes[offset..offset + 4].copy_from_slice(&f32::NAN.to_le_bytes()),
                    "packed-sum" => {
                        bytes[offset..offset + 4].copy_from_slice(&0.5f32.to_le_bytes());
                        bytes[offset + 4..offset + 8].copy_from_slice(&0.5f32.to_le_bytes());
                    }
                    _ => unreachable!(),
                }
            }
            "palette-size" => document["skins"][0]["joints"] = json!(vec![3; 257]),
            _ => unreachable!(),
        }
        document["buffers"][0]["byteLength"] = json!(bytes.len());
        fs::write(root.join("source.bin"), bytes).unwrap();
        fs::write(
            root.join("source.gltf"),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        fs::write(
            root.join("mesh.toml"),
            recipe("base", "authored-lods=['lod1','lod2']"),
        )
        .unwrap();
        fs::write(
            root.join("pak.toml"),
            "[[content.group]]\nassets=['mesh.toml']\n",
        )
        .unwrap();
        let result = PakBuf::bake_with_dir_without_cargo_watches(
            root.join("pak.toml"),
            root.join("bad.pak"),
            root,
        );
        assert!(
            result.is_err(),
            "accepted invalid authored skin case {case}"
        );
    }
}

#[test]
fn authored_lods_reject_ambiguous_missing_deformed_or_incompatible_levels() {
    for case in [
        "empty",
        "missing",
        "duplicate",
        "source",
        "ambiguous",
        "materials",
        "parts",
        "layout",
        "morph",
        "skin",
        "cap",
        "nonreducing",
        "conflict",
        "group-conflict",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut document = fixture(root);
        let mut extra = "authored-lods=['lod1','lod2']";
        match case {
            "empty" => extra = "authored-lods=[]",
            "missing" => extra = "authored-lods=['absent']",
            "duplicate" => extra = "authored-lods=['lod1','lod1']",
            "source" => extra = "authored-lods=['base']",
            "ambiguous" => document["nodes"][2]["name"] = json!("lod1"),
            "materials" => document["meshes"][1]["primitives"][0]["material"] = json!(0),
            "parts" => {
                document["meshes"][1]["primitives"]
                    .as_array_mut()
                    .unwrap()
                    .pop();
            }
            "layout" => {
                document["meshes"][1]["primitives"][0]["attributes"]
                    .as_object_mut()
                    .unwrap()
                    .remove("TEXCOORD_0");
            }
            "morph" => document["meshes"][1]["primitives"][0]["targets"] = json!([{"POSITION": 0}]),
            "skin" => {
                document["skins"] = json!([{"joints": [0]}]);
                document["nodes"][1]["skin"] = json!(0);
            }
            "cap" => extra = "authored-lods=['lod1','lod2']\nmax-index='u16'",
            "nonreducing" => document["nodes"][2]["mesh"] = json!(1),
            "conflict" => {
                extra = "authored-lods=['lod1','lod2']\nlods=[{layout='NORMAL | TEXTURE0 | TANGENT',simplify=false}]"
            }
            "group-conflict" => {
                extra = "authored-lods=['lod1','lod2']\nlods=[{layout='NORMAL | TEXTURE0 | TANGENT',simplify=true}]\n[mesh.lod]\njoin=true"
            }
            _ => unreachable!(),
        }
        fs::write(
            root.join("source.gltf"),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        fs::write(root.join("mesh.toml"), recipe("base", extra)).unwrap();
        fs::write(
            root.join("pak.toml"),
            "[[content.group]]\nassets=['mesh.toml']\n",
        )
        .unwrap();
        let result = PakBuf::bake_with_dir_without_cargo_watches(
            root.join("pak.toml"),
            root.join("bad.pak"),
            root,
        );
        assert!(result.is_err(), "accepted invalid authored lod case {case}");
    }
}
