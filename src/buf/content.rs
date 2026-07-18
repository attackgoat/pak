use {
    super::project_path,
    crate::compression::{BrotliParams, Compression},
    anyhow::Context,
    glob::glob,
    serde::Deserialize,
    std::{
        collections::HashSet,
        path::{Path, PathBuf},
    },
};

/// Holds a description of top-level content files which simply group other asset files for ease of
/// use.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct Content {
    compression: Option<CompressionType>,

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
        let asset_root = asset_root.as_ref();
        let enabled_groups = || self.groups().filter(|group| group.enabled());

        let mut excluded_assets = HashSet::new();
        for pattern in enabled_groups().flat_map(|group| group.exclude_globs()) {
            for path in glob(project_path(asset_root, pattern).to_string_lossy().as_ref())? {
                excluded_assets.insert(path?);
            }
        }

        let mut asset_paths = Vec::new();
        for pattern in enabled_groups().flat_map(|group| group.asset_globs()) {
            for path in glob(project_path(asset_root, pattern).to_string_lossy().as_ref())
                .context("Unable to glob source directory")?
            {
                let path = path?;
                if !excluded_assets.contains(&path) {
                    asset_paths.push(path);
                }
            }
        }

        Ok(asset_paths)
    }

    pub(crate) fn compression(&self) -> Option<Compression> {
        self.compression.map(|compression| match compression {
            CompressionType::Brotli => Compression::Brotli(BrotliParams {
                buffer_size: self
                    .buffer_size
                    .unwrap_or_else(|| BrotliParams::default().buffer_size),
                quality: self
                    .quality
                    .unwrap_or_else(|| BrotliParams::default().quality),
                window_size: self
                    .window_size
                    .unwrap_or_else(|| BrotliParams::default().window_size),
            }),
            CompressionType::Snap => Compression::Snap,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
pub enum CompressionType {
    /// Higher compression ratio but slower to decode and encode.
    #[serde(rename = "brotli")]
    Brotli,
    /// Lower compression ratio but faster to decode and encode.
    #[serde(rename = "snap")]
    Snap,
}

/// Holds a description of asset files.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct Group {
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

    /// Individual asset file specification globs to exclude from baking.
    ///
    /// May be a filename, might be folder/**/other.jpeg
    #[allow(unused)]
    pub fn exclude_globs(&self) -> impl Iterator<Item = &String> {
        self.exclude.iter()
    }
}

#[cfg(test)]
mod test {
    use super::Content;

    #[test]
    fn content_deserializes_without_groups() {
        let content = toml::from_str::<Content>("compression = 'snap'")
            .expect("content groups should be optional");

        assert_eq!(content.groups().count(), 0);
    }
}
