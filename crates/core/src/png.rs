//! PNG optimisation with oxipng.

/// The eight bytes every PNG file starts with.
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Optimise PNG bytes losslessly. The settings are today's
/// `oxipng -o 4 --strip safe`, as chosen in the Step 2 evaluation (which
/// measured byte-identical output to that command). If oxipng cannot make the
/// file smaller it returns the input unchanged.
///
/// The error is a message for the user: the content is not a PNG, or oxipng
/// could not decode it (truncated or corrupt data).
pub(crate) fn optimize(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if !bytes.starts_with(&SIGNATURE) {
        return Err("not a PNG file (the content is not PNG data)".to_owned());
    }
    let mut options = oxipng::Options::from_preset(4);
    options.strip = oxipng::StripChunks::Safe;
    oxipng::optimize_from_memory(bytes, &options)
        .map_err(|error| format!("could not read the PNG data: {error}"))
}
