use pak::{Pak, PakBuf};

// A .pak file "Mesh" has the following basic structure:
//   Mesh -> Primitive[] -> base Geometry
//                       -> layout-keyed LodSet[] -> Lod[] -> TrianglePatch[]
//
// Where:
//   "->": Specifies "Owns a"
//   "[]": Specifies "Array of"
//   Mesh: A collection of primitives
//   Primitive: One canonical geometry and material, with optional alternatives
//   Lod: Its own vertex/index pair and producing error
//   TrianglePatch: Checked ranges, actual bounds and an optional geometric cone
//
// Each set has one vertex layout and a source level followed by requested reductions.
// Canonical rendering reads primitive.base(); alternatives never add primitives.

fn main() {
    // Opening the .pak reads a small header only
    let mut pak =
        PakBuf::open("meshes.pak").expect("Unable to open pak file - run bake_pak example first");

    // Reads the "default.toml" mesh
    let default_mesh = pak.read_mesh("mesh/lantern/default").unwrap();

    // Also read an asset configured with explicit layout requests
    let meshlets_mesh = pak.read_mesh("mesh/lantern/meshlets").unwrap();

    println!("Canonical mesh:\n{:#?}\n", default_mesh);

    println!("Layout-keyed alternatives:\n{:#?}\n", meshlets_mesh);
}
