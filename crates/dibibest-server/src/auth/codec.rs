//! base64url without padding (RFC 4648 section 5), the encoding of signed cookies and JWT segments.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

pub fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The bytes, or `None` for anything that is not canonical base64url: padding, the standard alphabet, spaces,
/// a length no encoder produces.
pub fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(s).ok()
}
