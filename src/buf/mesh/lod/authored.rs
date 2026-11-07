use {
    super::{rounded_error, surface::Surface},
    crate::{
        buf::mesh::MeshAsset,
        mesh::{Lod, LodMetric, LodProvenance, LodSet, Mesh, Skin, VertexType},
    },
    anyhow::{Context as _, ensure},
    glam::Mat4,
    gltf::{Node, Semantic, buffer::Data, mesh::Mode},
    std::{collections::BTreeSet, path::Path},
};

impl MeshAsset {
    pub(in crate::buf::mesh) fn validate_authored_skin(
        node: &Node<'_>,
        bufs: &[Data],
    ) -> anyhow::Result<()> {
        let joint_count = node.skin().map_or(0, |skin| skin.joints().len());
        ensure!(
            joint_count <= 256,
            "authored skin exceeds packed joint palette"
        );
        for primitive in node
            .mesh()
            .context("authored lod node has no mesh")?
            .primitives()
        {
            if !matches!(
                primitive.mode(),
                Mode::TriangleFan | Mode::TriangleStrip | Mode::Triangles
            ) {
                continue;
            }
            let joints = primitive.get(&Semantic::Joints(0));
            let weights = primitive.get(&Semantic::Weights(0));
            if joint_count == 0 {
                ensure!(
                    primitive.attributes().all(|(semantic, _)| !matches!(
                        semantic,
                        Semantic::Joints(_) | Semantic::Weights(_)
                    )),
                    "authored joint attributes require a skin"
                );
                continue;
            }
            ensure!(
                primitive.attributes().all(|(semantic, _)| !matches!(semantic, Semantic::Joints(set) | Semantic::Weights(set) if set != 0)),
                "authored skin supports four influences per vertex"
            );
            let count = primitive
                .get(&Semantic::Positions)
                .context("authored skin has no positions")?
                .count();
            ensure!(
                joints.context("authored skin has no joints")?.count() == count
                    && weights.context("authored skin has no weights")?.count() == count,
                "authored skin attribute count mismatch"
            );
            let reader =
                primitive.reader(|buffer| bufs.get(buffer.index()).map(|data| data.0.as_slice()));
            for joints in reader
                .read_joints(0)
                .context("unable to read authored joints")?
                .into_u16()
            {
                ensure!(
                    joints.iter().all(|&joint| usize::from(joint) < joint_count),
                    "authored joint index exceeds palette"
                );
            }
            for weights in reader
                .read_weights(0)
                .context("unable to read authored weights")?
                .into_f32()
            {
                ensure!(
                    weights
                        .iter()
                        .all(|&weight| weight.is_finite() && (0.0..=1.0).contains(&weight))
                        && (weights.iter().sum::<f32>() - 1.0).abs() <= 1e-4,
                    "authored skin weights must be finite and normalized"
                );
                ensure!(
                    weights
                        .iter()
                        .map(|&weight| meshopt::quantize_unorm(weight, 8))
                        .sum::<i32>()
                        == 255,
                    "authored skin weights must retain a 255-unit packed sum"
                );
            }
        }
        Ok(())
    }

    pub(in crate::buf::mesh) fn import_authored_lods(
        &self,
        mesh: &mut Mesh,
        src: &Path,
        canonical: &Node<'_>,
        nodes: &[Node<'_>],
        bufs: &[Data],
    ) -> anyhow::Result<()> {
        let Some(names) = &self.authored_lods else {
            return Ok(());
        };
        ensure!(!names.is_empty(), "authored-lods must not be empty");
        ensure!(
            self.max_index().is_none(),
            "authored lods cannot use max-index reduction"
        );
        ensure!(
            canonical.skin().is_some() == mesh.skin().is_some(),
            "authored skin was ignored or could not be imported"
        );
        validate_palette(mesh.skin())?;
        let mut seen = BTreeSet::new();
        seen.insert(canonical.name().unwrap_or_default());
        for name in names {
            ensure!(
                !name.is_empty() && seen.insert(name),
                "duplicate or empty authored lod name"
            );
        }
        let materials = |node: &Node<'_>| -> anyhow::Result<Vec<_>> {
            node.mesh()
                .context("authored lod node has no mesh")?
                .primitives()
                .filter(|primitive| {
                    matches!(
                        primitive.mode(),
                        Mode::TriangleFan | Mode::TriangleStrip | Mode::Triangles
                    )
                })
                .map(|primitive| {
                    ensure!(
                        primitive.morph_targets().len() == 0,
                        "authored lods cannot contain morph targets"
                    );
                    Ok(primitive.material().index())
                })
                .collect::<anyhow::Result<Vec<_>>>()
        };
        let source_materials = materials(canonical)?;
        ensure!(
            !source_materials.is_empty(),
            "authored lod source has no triangle primitives"
        );
        let mut asset = self.clone();
        asset.authored_lods = None;
        asset.lods = None;
        asset.source_lods = Some(false);
        let mut alternatives = Vec::with_capacity(names.len());
        for name in names {
            let mut matches = nodes
                .iter()
                .filter(|node| node.name() == Some(name.as_str()));
            let node = matches
                .next()
                .with_context(|| format!("missing authored lod node {name}"))?;
            ensure!(
                matches.next().is_none(),
                "ambiguous authored lod node {name}"
            );
            ensure!(
                materials(node)? == source_materials,
                "authored lod primitive/material slots differ: {name}"
            );
            Self::validate_authored_skin(node, bufs)
                .with_context(|| format!("validating authored lod skin {name}"))?;
            let alternative = asset
                .to_mesh_node(src, node, bufs)
                .with_context(|| format!("importing authored lod node {name}"))?;
            ensure!(
                node.skin().is_some() == alternative.skin().is_some(),
                "authored lod skin could not be imported: {name}"
            );
            validate_palette(alternative.skin())?;
            ensure!(
                palettes_match(mesh.skin(), alternative.skin()),
                "authored lod joint palette differs: {name}"
            );
            alternatives.push(alternative);
        }
        ensure!(
            mesh.lod_groups().iter().all(|group| mesh
                .primitives()
                .iter()
                .all(|primitive| primitive.base().vertex_type() != group.vertex_type())),
            "authored lod layout already requested as a group"
        );
        let skinned = mesh.skin().is_some();
        for (idx, primitive) in mesh.primitives_mut().iter_mut().enumerate() {
            let source = primitive.base();
            let layout = source.vertex_type();
            ensure!(
                layout.contains(VertexType::JOINTS_WEIGHTS) == skinned,
                "authored lod vertex layout does not match its skin"
            );
            ensure!(
                primitive.lod_set(layout).is_none(),
                "authored lod layout already requested"
            );
            let surface = Surface::new(
                [(0, primitive.material(), source.clone())],
                VertexType::POSITION,
            )?;
            let mut levels = vec![Lod::new(source.clone(), 0.0)?];
            for (name, mesh) in names.iter().zip(&alternatives) {
                let alternative = &mesh.primitives()[idx];
                ensure!(
                    alternative.base().vertex_type() == layout,
                    "authored lod vertex layout differs: {name}"
                );
                ensure!(
                    alternative.material() == primitive.material(),
                    "authored lod material slot differs: {name}"
                );
                let other = Surface::new(
                    [(0, alternative.material(), alternative.base().clone())],
                    VertexType::POSITION,
                )?;
                let error = rounded_error(surface.deviation(&other, 0.0))?;
                levels.push(Lod::new(alternative.base().clone(), error)?);
            }
            let set = LodSet::new(
                LodMetric::SampledGeometricAbsolute,
                levels.into_boxed_slice(),
            )?
            .with_provenance(LodProvenance {
                producer: crate::buf::MESH_LOD_PRODUCER.to_owned(),
                settings: toml::to_string(&std::collections::BTreeMap::from([(
                    "authored-lods",
                    names,
                )]))?,
                stop_reason: "authored-levels".into(),
            });
            let mut sets = primitive.lod_sets().to_vec();
            sets.push(set);
            primitive.set_lods(sets.into_boxed_slice())?;
        }
        Ok(())
    }
}

fn palettes_match(source: Option<&Skin>, alternative: Option<&Skin>) -> bool {
    match (source, alternative) {
        (None, None) => true,
        (Some(source), Some(alternative)) => {
            source.joints().len() == alternative.joints().len()
                && source
                    .joints()
                    .iter()
                    .zip(alternative.joints())
                    .all(|(a, b)| {
                        a.name == b.name
                            && a.parent_index == b.parent_index
                            && a.inverse_bind == b.inverse_bind
                    })
        }
        _ => false,
    }
}

fn validate_palette(skin: Option<&Skin>) -> anyhow::Result<()> {
    if let Some(skin) = skin {
        ensure!(
            !skin.joints().is_empty() && skin.joints().len() <= 256,
            "invalid authored joint palette size"
        );
        for joint in skin.joints() {
            let matrix = Mat4::from_cols_array(&joint.inverse_bind);
            ensure!(
                matrix.is_finite()
                    && matrix.determinant().is_finite()
                    && matrix.determinant() != 0.0,
                "authored inverse bind must be finite and nonsingular"
            );
        }
    }
    Ok(())
}
