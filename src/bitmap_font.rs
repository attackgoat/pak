use {
    super::bitmap::Bitmap,
    serde::{Deserialize, Serialize},
};

/// Holds a `BitmapFont` in a `.pak` file. For data transport only.
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BitmapFont {
    def: String,
    pages: Vec<Bitmap>,
}

impl BitmapFont {
    pub fn new(def: String, pages: Vec<Bitmap>) -> Self {
        Self { def, pages }
    }

    // TODO: We could pre-pack this instead of raw text!
    /// Gets the main `.fnt` file in original text form
    pub fn def(&self) -> &str {
        self.def.as_str()
    }

    /// Gets the `BitmapBuf` pages within this `BitmapFont`.
    pub fn pages(&self) -> impl ExactSizeIterator<Item = &Bitmap> {
        self.pages.iter()
    }
}
