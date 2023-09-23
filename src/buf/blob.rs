use {
    super::{
        super::bitmap::{BitmapColor, BitmapFormat},
        bitmap::{Bitmap, BitmapSwizzle},
        file_key, re_run_if_changed, Asset, BitmapBuf, BitmapFontBuf, BitmapFontId, BlobId,
        Canonicalize, Writer,
    },
    bmfont::{BMFont, OrdinateOrientation},
    log::info,
    parking_lot::Mutex,
    serde::Deserialize,
    std::{
        fs::read_to_string,
        fs::File,
        io::{Cursor, Error, Read},
        path::{Path, PathBuf},
        sync::Arc,
    },
};

/// Holds a description of any generic file.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct Blob {
    /// The file source.
    src: PathBuf,
}

impl Blob {
    pub fn new(src: impl AsRef<Path>) -> Self {
        let src = src.as_ref().to_path_buf();

        Self { src }
    }

    /// Reads and processes arbitrary binary source files into an existing `.pak` file buffer.
    pub fn bake(
        &self,
        writer: &Arc<Mutex<Writer>>,
        project_dir: impl AsRef<Path>,
    ) -> anyhow::Result<BlobId> {
        let asset = self.clone().into();

        // Early-out if we have already baked this blob
        if let Some(id) = writer.lock().ctx.get(&asset) {
            return Ok(id.as_blob().unwrap());
        }

        let key = file_key(&project_dir, &self.src);

        info!("Baking blob: {}", key);

        re_run_if_changed(&self.src);

        let mut file = File::open(&self.src).unwrap();
        let mut value = vec![];
        file.read_to_end(&mut value).unwrap();

        let mut writer = writer.lock();
        if let Some(id) = writer.ctx.get(&asset) {
            return Ok(id.as_blob().unwrap());
        }

        let id = writer.push_blob(value, Some(key));
        writer.ctx.insert(asset, id.into());

        Ok(id)
    }

    /// Reads and processes bitmapped font source files into an existing `.pak` file buffer.
    pub(super) fn bake_bitmap_font(
        &self,
        writer: &Arc<Mutex<Writer>>,
        project_dir: impl AsRef<Path>,
        path: impl AsRef<Path>,
    ) -> Result<BitmapFontId, Error> {
        let asset = self.clone().into();

        // Early-out if we have already baked this blob
        if let Some(id) = writer.lock().ctx.get(&asset) {
            return Ok(id.as_bitmap_font().unwrap());
        }

        let key = file_key(&project_dir, &path);

        info!("Baking bitmap font: {}", key);

        re_run_if_changed(&self.src);

        // Get the fs objects for this asset
        let def_parent = self.src.parent().unwrap();
        let def_file = read_to_string(&self.src).unwrap();
        let def = BMFont::new(Cursor::new(&def_file), OrdinateOrientation::TopToBottom).unwrap();
        let pages = def
            .pages()
            .map(|page| {
                let path = def_parent.join(page);

                // Bake the pixels
                Bitmap::read_pixels(path, Some(BitmapSwizzle::RGBA), None)
            })
            .filter(|res| res.is_ok()) // TODO: Horrible!
            .map(|res| res.unwrap())
            .map(|(_, width, pixels)| {
                // TODO: Handle format correctly!
                let mut better_pixels = Vec::with_capacity(pixels.len());
                for y in 0..pixels.len() / 4 / width as usize {
                    for x in 0..width as usize {
                        let g = pixels[y * width as usize * 4 + x * 4 + 1];
                        let r = pixels[y * width as usize * 4 + x * 4 + 3];
                        if 0xff == r {
                            better_pixels.push(0xff);
                            better_pixels.push(0x00);
                        } else if 0xff == g {
                            better_pixels.push(0x00);
                            better_pixels.push(0xff);
                        } else {
                            better_pixels.push(0x00);
                            better_pixels.push(0x00);
                        }
                        better_pixels.push(0x00);
                    }
                }

                (width, better_pixels)
            })
            .collect::<Vec<_>>();

        // Panic if any page is a different size (the format says they should all be the same)
        let mut page_size = None;
        for (page_width, page_pixels) in &pages {
            let page_height = page_pixels.len() as u32 / 3 / page_width;
            if page_size.is_none() {
                page_size = Some((*page_width, page_height));
            } else if let Some((width, height)) = page_size {
                if *page_width != width || page_height != height {
                    panic!("Unexpected page size");
                }
            }
        }

        let (width, _) = page_size.unwrap();

        let page_bufs = pages
            .into_iter()
            .map(|(_, pixels)| {
                BitmapBuf::new(BitmapColor::Linear, BitmapFormat::Rgb, width, pixels)
            })
            .collect();

        let mut writer = writer.lock();
        if let Some(id) = writer.ctx.get(&asset) {
            return Ok(id.as_bitmap_font().unwrap());
        }

        let id = writer.push_bitmap_font(BitmapFontBuf::new(def_file, page_bufs), Some(key));
        writer.ctx.insert(asset, id.into());

        Ok(id)
    }

    pub fn src(&self) -> &Path {
        &self.src
    }
}

impl Canonicalize for Blob {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        self.src = Self::canonicalize_project_path(project_dir, src_dir, &self.src);
    }
}
