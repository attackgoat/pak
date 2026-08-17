use {
    super::project_path,
    crate::{
        compression::{BrotliParams, Compression},
        valid_segment_name,
    },
    anyhow::Context,
    glob::glob,
    serde::Deserialize,
    std::{
        collections::{BTreeMap, BTreeSet},
        path::{Path, PathBuf},
    },
};

/// Holds a description of top-level content files which simply group other asset files for ease of
/// use.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct Content {
    compression: Option<CompressionType>,

    #[serde(default, rename = "texture-compression")]
    texture_compression: bool,

    // Brotli-specific compression parameter
    #[serde(rename = "buffer-size")]
    buffer_size: Option<usize>,

    // Brotli-specific compression parameter
    quality: Option<u32>,

    // Brotli-specific compression parameter
    #[serde(rename = "window-size")]
    window_size: Option<u32>,

    // Tables must follow values
    #[serde(default, rename = "group")]
    groups: Box<[Group]>,
}

impl Content {
    /// An iterator of grouped content file descriptions.
    #[allow(unused)]
    pub fn groups(&self) -> impl Iterator<Item = &Group> {
        self.groups.iter()
    }

    pub(crate) fn selected_asset_paths(
        &self,
        asset_root: impl AsRef<Path>,
    ) -> anyhow::Result<Vec<PathBuf>> {
        Ok(self
            .selected_assets(asset_root)?
            .into_iter()
            .map(|asset| asset.path)
            .collect())
    }

    pub(crate) fn selected_assets(
        &self,
        asset_root: impl AsRef<Path>,
    ) -> anyhow::Result<Vec<SelectedAsset>> {
        let asset_root = asset_root.as_ref();
        self.segment_names()?;
        let inherited = self.compression()?;
        let mut selected: BTreeMap<PathBuf, SelectedAsset> = BTreeMap::new();

        for (group_index, group) in self
            .groups()
            .enumerate()
            .filter(|(_, group)| group.enabled())
        {
            let compression = group.compression(inherited)?;
            let texture_compression = group.texture_compression(self.texture_compression);
            let segment = group.segment().map(str::to_owned);
            let group_name = group
                .name()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("group #{group_index}"));
            let mut excluded = BTreeMap::new();
            for pattern in group.exclude_globs() {
                for path in glob(project_path(asset_root, pattern).to_string_lossy().as_ref())? {
                    excluded.insert(path?, ());
                }
            }

            let mut group_paths = BTreeMap::new();
            for pattern in group.asset_globs() {
                for path in glob(project_path(asset_root, pattern).to_string_lossy().as_ref())
                    .context("Unable to glob source directory")?
                {
                    let path = path?;
                    if !excluded.contains_key(&path) {
                        group_paths.insert(path, ());
                    }
                }
            }

            for path in group_paths.into_keys() {
                if let Some(existing) = selected.get_mut(&path) {
                    if existing.compression != compression
                        || existing.texture_compression != texture_compression
                        || existing.segment != segment
                    {
                        anyhow::bail!(
                            "asset {} is selected by groups with conflicting storage policies: {} and {}",
                            path.display(),
                            existing.groups.join(", "),
                            group_name,
                        );
                    }
                    existing.groups.push(group_name.clone());
                } else {
                    selected.insert(
                        path.clone(),
                        SelectedAsset {
                            path,
                            compression,
                            texture_compression,
                            segment: segment.clone(),
                            groups: vec![group_name.clone()],
                        },
                    );
                }
            }
        }

        Ok(selected.into_values().collect())
    }

    pub(crate) fn compression(&self) -> anyhow::Result<Option<Compression>> {
        let compression = match self.compression {
            Some(CompressionType::Brotli) => Some(Compression::Brotli(
                self.brotli_params(BrotliParams::default()),
            )),
            Some(CompressionType::None) | None => None,
            Some(CompressionType::Snap) => Some(Compression::Snap),
        };
        if !matches!(compression, Some(Compression::Brotli(_))) && self.has_brotli_params() {
            anyhow::bail!("content Brotli parameters require compression = 'brotli'");
        }
        Ok(compression)
    }

    fn brotli_params(&self, inherited: BrotliParams) -> BrotliParams {
        BrotliParams {
            buffer_size: self.buffer_size.unwrap_or(inherited.buffer_size),
            quality: self.quality.unwrap_or(inherited.quality),
            window_size: self.window_size.unwrap_or(inherited.window_size),
        }
    }

    fn has_brotli_params(&self) -> bool {
        self.buffer_size.is_some() || self.quality.is_some() || self.window_size.is_some()
    }

    pub(crate) fn segment_names(&self) -> anyhow::Result<Vec<String>> {
        let mut names = BTreeSet::new();
        for group in self.groups().filter(|group| group.enabled()) {
            if let Some(name) = group.segment() {
                if !valid_segment_name(name) {
                    anyhow::bail!(
                        "invalid segment name {name:?}; use 1-64 lowercase ASCII letters, digits, '-' or '_'"
                    );
                }
                names.insert(name.to_owned());
            }
        }
        Ok(names.into_iter().collect())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
pub enum CompressionType {
    /// Higher compression ratio but slower to decode and encode.
    #[serde(rename = "brotli")]
    Brotli,
    /// No outer payload or header compression.
    #[serde(rename = "none")]
    None,
    /// Lower compression ratio but faster to decode and encode.
    #[serde(rename = "snap")]
    Snap,
}

/// Holds a description of asset files.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct Group {
    name: Option<String>,

    segment: Option<String>,

    compression: Option<CompressionType>,

    #[serde(rename = "texture-compression")]
    texture_compression: Option<bool>,

    #[serde(rename = "buffer-size")]
    buffer_size: Option<usize>,

    quality: Option<u32>,

    #[serde(rename = "window-size")]
    window_size: Option<u32>,

    #[serde(default)]
    assets: Vec<String>,

    #[serde(default = "Group::default_enabled")]
    enabled: bool,

    #[serde(default)]
    exclude: Vec<String>,
}

impl Group {
    const DEFAULT_ENABLED: bool = true;

    /// Individual asset file specification globs.
    ///
    /// May be a filename, might be folder/**/other.jpeg
    #[allow(unused)]
    pub fn asset_globs(&self) -> impl Iterator<Item = &String> {
        self.assets.iter()
    }

    fn compression(&self, inherited: Option<Compression>) -> anyhow::Result<Option<Compression>> {
        let inherited_brotli = match inherited {
            Some(Compression::Brotli(params)) => Some(params),
            _ => None,
        };
        let compression = match self.compression {
            Some(CompressionType::Brotli) => Some(Compression::Brotli(
                self.brotli_params(inherited_brotli.unwrap_or_default()),
            )),
            Some(CompressionType::None) => None,
            Some(CompressionType::Snap) => Some(Compression::Snap),
            None => inherited.map(|compression| match compression {
                Compression::Brotli(params) => Compression::Brotli(self.brotli_params(params)),
                Compression::Snap => Compression::Snap,
            }),
        };
        if !matches!(compression, Some(Compression::Brotli(_))) && self.has_brotli_params() {
            anyhow::bail!(
                "group {} has Brotli parameters but its effective compression is not Brotli",
                self.name().unwrap_or("<unnamed>")
            );
        }
        Ok(compression)
    }

    fn texture_compression(&self, inherited: bool) -> bool {
        self.texture_compression.unwrap_or(inherited)
    }

    fn brotli_params(&self, inherited: BrotliParams) -> BrotliParams {
        BrotliParams {
            buffer_size: self.buffer_size.unwrap_or(inherited.buffer_size),
            quality: self.quality.unwrap_or(inherited.quality),
            window_size: self.window_size.unwrap_or(inherited.window_size),
        }
    }

    fn has_brotli_params(&self) -> bool {
        self.buffer_size.is_some() || self.quality.is_some() || self.window_size.is_some()
    }

    const fn default_enabled() -> bool {
        Self::DEFAULT_ENABLED
    }

    /// Allows a group to be selectively removed with a single flag, as opposed to physically
    /// removing a group from the content file.
    ///
    /// This is useful for debugging.
    #[allow(unused)]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn segment(&self) -> Option<&str> {
        self.segment.as_deref()
    }

    /// Individual asset file specification globs to exclude from baking.
    ///
    /// May be a filename, might be folder/**/other.jpeg
    #[allow(unused)]
    pub fn exclude_globs(&self) -> impl Iterator<Item = &String> {
        self.exclude.iter()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SelectedAsset {
    pub(crate) path: PathBuf,
    pub(crate) compression: Option<Compression>,
    pub(crate) texture_compression: bool,
    pub(crate) segment: Option<String>,
    pub(crate) groups: Vec<String>,
}

#[cfg(test)]
mod test {
    use {
        super::Content,
        crate::compression::Compression,
        std::{
            fs,
            sync::atomic::{AtomicU64, Ordering},
        },
    };

    fn asset_root() -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "pak-content-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn content_deserializes_without_groups() {
        let content = toml::from_str::<Content>("compression = 'snap'")
            .expect("content groups should be optional");

        assert_eq!(content.groups().count(), 0);
    }

    #[test]
    fn exclusion_is_local_to_its_group() {
        let root = asset_root();
        fs::write(root.join("a.bin"), []).unwrap();
        fs::write(root.join("b.bin"), []).unwrap();
        let content = toml::from_str::<Content>(
            "[[group]]\nassets = ['*.bin']\nexclude = ['a.bin']\n\n[[group]]\nassets = ['a.bin']",
        )
        .unwrap();

        let selected = content.selected_asset_paths(&root).unwrap();
        assert_eq!(selected, [root.join("a.bin"), root.join("b.bin")]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn same_policy_overlap_merges_group_membership() {
        let root = asset_root();
        fs::write(root.join("asset.bin"), []).unwrap();
        let content = toml::from_str::<Content>(
            "[[group]]\nname = 'first'\ncompression = 'snap'\nassets = ['asset.bin']\n\n[[group]]\nname = 'second'\ncompression = 'snap'\nassets = ['*.bin']",
        )
        .unwrap();

        let selected = content.selected_assets(&root).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].compression, Some(Compression::Snap));
        assert_eq!(selected[0].groups, ["first", "second"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn conflicting_direct_path_policies_error() {
        let root = asset_root();
        fs::write(root.join("asset.bin"), []).unwrap();
        let content = toml::from_str::<Content>(
            "[[group]]\nname = 'fast'\ncompression = 'snap'\nassets = ['asset.bin']\n\n[[group]]\nname = 'small'\ncompression = 'brotli'\nassets = ['*.bin']",
        )
        .unwrap();

        let error = content.selected_assets(&root).unwrap_err().to_string();
        assert!(error.contains("conflicting storage policies"));
        assert!(error.contains("fast"));
        assert!(error.contains("small"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn group_brotli_params_partially_override_inherited_params() {
        let content = toml::from_str::<Content>(
            "compression = 'brotli'\nbuffer-size = 1024\nquality = 4\nwindow-size = 20\n\n[[group]]\nquality = 7",
        )
        .unwrap();
        let inherited = content.compression().unwrap();

        assert_eq!(
            content
                .groups()
                .next()
                .unwrap()
                .compression(inherited)
                .unwrap(),
            Some(Compression::Brotli(crate::compression::BrotliParams {
                buffer_size: 1024,
                quality: 7,
                window_size: 20,
            }))
        );
    }

    #[test]
    fn explicit_group_brotli_inherits_unspecified_content_params() {
        let content = toml::from_str::<Content>(
            "compression = 'brotli'\nbuffer-size = 2048\nquality = 3\nwindow-size = 19\n\n[[group]]\ncompression = 'brotli'\nquality = 8",
        )
        .unwrap();
        let inherited = content.compression().unwrap();

        assert_eq!(
            content
                .groups()
                .next()
                .unwrap()
                .compression(inherited)
                .unwrap(),
            Some(Compression::Brotli(crate::compression::BrotliParams {
                buffer_size: 2048,
                quality: 8,
                window_size: 19,
            }))
        );
    }

    #[test]
    fn explicit_group_brotli_uses_defaults_without_inherited_brotli() {
        let content = toml::from_str::<Content>(
            "compression = 'snap'\n\n[[group]]\ncompression = 'brotli'\nquality = 5",
        )
        .unwrap();
        let inherited = content.compression().unwrap();
        let mut expected = crate::compression::BrotliParams::default();
        expected.quality = 5;

        assert_eq!(
            content
                .groups()
                .next()
                .unwrap()
                .compression(inherited)
                .unwrap(),
            Some(Compression::Brotli(expected))
        );
    }

    #[test]
    fn brotli_params_reject_non_brotli_effective_codec() {
        let content = toml::from_str::<Content>(
            "compression = 'snap'\n\n[[group]]\nname = 'invalid'\nquality = 5",
        )
        .unwrap();
        let inherited = content.compression().unwrap();

        let error = content
            .groups()
            .next()
            .unwrap()
            .compression(inherited)
            .unwrap_err()
            .to_string();
        assert!(error.contains("effective compression is not Brotli"));
        assert!(error.contains("invalid"));
    }

    #[test]
    fn segment_names_are_strict_sorted_and_deduplicated() {
        let content = toml::from_str::<Content>(
            "[[group]]\nsegment = 'z_data'\n\n[[group]]\nsegment = 'a-data'\n\n[[group]]\nsegment = 'z_data'",
        )
        .unwrap();
        assert_eq!(content.segment_names().unwrap(), ["a-data", "z_data"]);

        for name in ["", ".", "..", "a/b", "a\\b", "white space", "café"] {
            let content =
                toml::from_str::<Content>(&format!("[[group]]\nsegment = {name:?}")).unwrap();
            assert!(
                content.segment_names().is_err(),
                "accepted unsafe name {name:?}"
            );
        }
    }

    #[test]
    fn overlapping_path_with_conflicting_segments_errors() {
        let root = asset_root();
        fs::write(root.join("asset.bin"), []).unwrap();
        let content = toml::from_str::<Content>(
            "[[group]]\nname = 'one'\nsegment = 'one'\nassets = ['asset.bin']\n\n[[group]]\nname = 'two'\nsegment = 'two'\nassets = ['*.bin']",
        )
        .unwrap();

        let error = content.selected_assets(&root).unwrap_err().to_string();
        assert!(error.contains("conflicting storage policies"));
        fs::remove_dir_all(root).unwrap();
    }
}
