use pak::{Pak, PakBuf};

// A .pak file "Mesh" has the following basic structure:
//   Mesh -> Primitive[] -> LevelOfDetail[] -> Meshlet[]
//
// Where:
//   "->": Specifies "Owns a"
//   "[]": Specifies "Array of"
//   Mesh: A collection of primitives
//   Primitive: A list of triangles with a material
//   Level of Detail: LOD0..N where each level is half the vertices
//   Meshlet: The actual index/vertex data for rendering!
//
// Meshes may be:
// - Just a mesh, index/vertex buffers
// - Mesh + Shadow mesh (same as above but shadow mesh is just positions)
// - Mesh + Shadow + LODs (mesh and shadow mesh each have separate LODs)
// - Mesh + Shadow + LODs all as meshlets (small localized groups of triangles)
//
// ...and more! See the getting started docs at:
// https://github.com/attackgoat/screen-13/blob/master/examples/getting-started.md

fn main() {
    // Opening the .pak reads a small header only
    let mut pak =
        PakBuf::open("meshes.pak").expect("Unable to open pak file - run bake_pak example first");

    // Reads the "default.toml" mesh which physically reads 155 K of index/vertex data
    let default_mesh = pak.read_mesh("mesh/lantern/default").unwrap();

    // Also read "meshlets.toml" which is the same mesh but baked into meshopt "meshlets" (172 K)
    let meshlets_mesh = pak.read_mesh("mesh/lantern/meshlets").unwrap();

    // Each mesh contains a single artist-named mesh. Notice how this file bakes each detail level
    // into a single meshlet
    println!("Regular mesh w/ baked shadow mesh:\n{:#?}\n", default_mesh);

    // Notice how this file has a bunch of meshlets for the geometry
    println!("Meshlets also w/ shadows:\n{:#?}\n", meshlets_mesh);
}
