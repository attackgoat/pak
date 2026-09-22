use {
    crate::{
        MeshId, Pak as _, PakBuf,
        mesh::{Geometry, Lod, LodProvenance},
    },
    anyhow::Context,
    std::{
        fs::File,
        io::{BufWriter, Write},
        path::Path,
    },
};

fn row(writer: &mut impl Write, cells: &[String]) -> std::io::Result<()> {
    for (idx, cell) in cells.iter().enumerate() {
        if idx != 0 {
            writer.write_all(b",")?;
        }
        if cell.contains([',', '"', '\r', '\n']) {
            write!(writer, "\"{}\"", cell.replace('"', "\"\""))?;
        } else {
            writer.write_all(cell.as_bytes())?;
        }
    }
    writer.write_all(b"\n")
}

fn provenance_cells(provenance: Option<&LodProvenance>) -> [String; 4] {
    match provenance {
        Some(value) => [
            value.producer.clone(),
            format!(
                "{:016x}",
                crate::update_hash(
                    crate::update_hash(crate::FNV_OFFSET, value.producer.as_bytes()),
                    value.settings.as_bytes()
                )
            ),
            value.settings.clone(),
            value.stop_reason.clone(),
        ],
        None => Default::default(),
    }
}

impl PakBuf {
    /// Writes archive-derived layout sets. Consumer roles and absent-request policies are not inferred.
    pub fn write_mesh_lod_report(&mut self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        self.mesh_lod_report(path.as_ref(), false)
    }

    /// Writes generic producer/settings/termination facts for existing layout sets.
    pub fn write_mesh_lod_diagnostics(&mut self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        self.mesh_lod_report(path.as_ref(), true)
    }

    fn mesh_lod_report(&mut self, path: &Path, diagnostics: bool) -> anyhow::Result<()> {
        let mut writer = BufWriter::new(
            File::create(path)
                .with_context(|| format!("creating mesh lod report {}", path.display()))?,
        );
        let mut header = vec![
            "mesh-key",
            "mesh-id",
            "primitive",
            "material-slot",
            "layout",
            "metric",
            "lod",
        ];
        if !diagnostics {
            header.extend([
                "original-triangles",
                "original-vertices",
                "result-triangles",
                "result-vertices",
                "patches",
                "geometry-bytes",
                "reduction-ratio",
                "accumulated-error",
                "prefix-max-error",
            ]);
        }
        header.extend([
            "outcome",
            "producer",
            "settings-hash",
            "resolved-settings",
            "stop-reason",
        ]);
        row(
            &mut writer,
            &header.into_iter().map(str::to_owned).collect::<Vec<_>>(),
        )?;
        let mut keys = vec![Vec::new(); self.mesh_count()];
        for (key, id) in &self.data.ids {
            if let Some(id) = id.as_mesh() {
                keys.get_mut(id.0)
                    .context("report key references an invalid mesh id")?
                    .push(key.clone());
            }
        }
        for (id, mut keys) in keys.into_iter().enumerate() {
            if keys.is_empty() {
                keys.push(String::new());
            }
            let mesh = self
                .read_mesh_id(MeshId(id))
                .context("reading report mesh")?;
            for key in keys {
                if mesh.primitives().is_empty() {
                    let mut cells = vec![
                        key.clone(),
                        id.to_string(),
                        String::new(),
                        String::new(),
                        "base".into(),
                        "source".into(),
                        String::new(),
                    ];
                    if !diagnostics {
                        cells.extend(vec![String::new(); 9]);
                    }
                    cells.extend([
                        "unavailable".into(),
                        String::new(),
                        String::new(),
                        String::new(),
                        mesh.data("pak.mesh-lod.empty-stop")
                            .and_then(|value| value.as_str())
                            .unwrap_or("")
                            .to_owned(),
                    ]);
                    row(&mut writer, &cells)?;
                }
                for (idx, primitive) in mesh.primitives().iter().enumerate() {
                    let base = primitive.base();
                    let mut emit = |layout: String,
                                    metric: String,
                                    level: usize,
                                    geometry: &Geometry,
                                    lod: Option<&Lod>,
                                    prefix: f32,
                                    provenance: Option<&LodProvenance>|
                     -> anyhow::Result<()> {
                        let mut cells = vec![
                            key.clone(),
                            id.to_string(),
                            idx.to_string(),
                            primitive.material().to_string(),
                            layout,
                            metric,
                            level.to_string(),
                        ];
                        if !diagnostics {
                            cells.extend([
                                base.indices().triangle_count().to_string(),
                                base.vertex_count().to_string(),
                                geometry.indices().triangle_count().to_string(),
                                geometry.vertex_count().to_string(),
                                lod.map_or(0, |lod| lod.patches().len()).to_string(),
                                (geometry.vertex_data().len()
                                    + geometry.indices().index_count()
                                        * geometry.indices().index_type().stride())
                                .to_string(),
                                (1.0 - geometry.indices().triangle_count() as f64
                                    / base.indices().triangle_count() as f64)
                                    .to_string(),
                                lod.map_or(0.0, Lod::error).to_string(),
                                prefix.to_string(),
                            ]);
                        }
                        cells.push(if level == 0 { "full-detail" } else { "reduced" }.to_owned());
                        cells.extend(provenance_cells(provenance));
                        row(&mut writer, &cells)?;
                        Ok(())
                    };
                    if !diagnostics {
                        emit("base".into(), "source".into(), 0, base, None, 0.0, None)?;
                    }
                    for set in primitive.lod_sets() {
                        let mut prefix = 0.0_f32;
                        for (level, lod) in set.levels().iter().enumerate() {
                            prefix = prefix.max(lod.error());
                            if !diagnostics || level + 1 == set.levels().len() {
                                emit(
                                    format!("{}", set.vertex_type().bits()),
                                    format!("{:?}", set.metric()),
                                    level,
                                    lod.geometry(),
                                    Some(lod),
                                    prefix,
                                    set.provenance(),
                                )?;
                            }
                        }
                    }
                }
            }
        }
        writer.flush()?;
        Ok(())
    }
}
