//! The shifted ephemeral key of an Orchard or Ironwood action: `[2^64] * epk`.
//!
//! A wallet does trial decryption with `[ivk] * epk`. With the Pallas GLV endomorphism φ, it
//! splits `ivk` into two 128-bit scalars. With this point as well, it splits each of them again
//! at bit 64, and the joint multiplication over `epk`, `[2^64] * epk` and their images under φ
//! needs 64 doublings instead of 128. The server computes the point once for each action, and
//! each wallet then saves 64 doublings for each action and each IVK.
//!
//! The `epk-shift` feature selects the computation. Without it, [`shifted_ephemeral_key`]
//! returns `None`, so the stored and the served records stay byte-identical to the upstream
//! compact format.

use crate::types::EphemeralKey;

/// `true` when this build computes the shifted ephemeral key.
pub const ENABLED: bool = cfg!(feature = "epk-shift");

/// The number of doublings from `epk` to the shifted point.
#[cfg(feature = "epk-shift")]
const SHIFT: u32 = 64;

/// Returns the compressed `[2^64] * epk`, or `None` when this build does not compute it.
///
/// It also returns `None` when `epk` is not the canonical encoding of a Pallas point, or is the
/// identity. Such an action cannot decrypt, so the wallet loses nothing.
///
/// ```
/// use ztreamer_chain::epk_shift::{ENABLED, shifted_ephemeral_key};
///
/// // The all-zero encoding is the identity, which has no shifted key.
/// assert_eq!(shifted_ephemeral_key(&[0; 32]), None);
/// # let _ = ENABLED;
/// ```
#[cfg(feature = "epk-shift")]
#[must_use]
pub fn shifted_ephemeral_key(epk: &EphemeralKey) -> Option<EphemeralKey> {
    use pasta_curves::{
        group::{Curve as _, Group as _, GroupEncoding as _},
        pallas,
    };

    let point =
        pallas::Point::from(Option::<pallas::Affine>::from(pallas::Affine::from_bytes(epk))?);
    if bool::from(point.is_identity()) {
        return None;
    }
    let shifted = (0..SHIFT).fold(point, |acc, _| acc.double());
    Some(shifted.to_affine().to_bytes())
}

/// Returns the compressed `[2^64] * epk`, or `None` when this build does not compute it.
///
/// It also returns `None` when `epk` is not the canonical encoding of a Pallas point, or is the
/// identity. Such an action cannot decrypt, so the wallet loses nothing.
///
/// ```
/// use ztreamer_chain::epk_shift::{ENABLED, shifted_ephemeral_key};
///
/// // The all-zero encoding is the identity, which has no shifted key.
/// assert_eq!(shifted_ephemeral_key(&[0; 32]), None);
/// # let _ = ENABLED;
/// ```
#[cfg(not(feature = "epk-shift"))]
#[must_use]
pub const fn shifted_ephemeral_key(_epk: &EphemeralKey) -> Option<EphemeralKey> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_and_invalid_encodings_have_no_shifted_key() {
        assert_eq!(shifted_ephemeral_key(&[0; 32]), None);
        assert_eq!(shifted_ephemeral_key(&[0xff; 32]), None);
    }

    #[cfg(not(feature = "epk-shift"))]
    #[test]
    fn disabled_builds_compute_nothing() {
        const { assert!(!ENABLED) };
        assert_eq!(shifted_ephemeral_key(&[1; 32]), None);
    }

    #[cfg(feature = "epk-shift")]
    #[test]
    fn matches_a_scalar_multiplication_by_two_to_the_64() {
        use pasta_curves::{
            group::{Curve as _, Group as _, GroupEncoding as _, ff::Field as _},
            pallas,
        };

        const { assert!(ENABLED) };
        let two_pow_64 = pallas::Scalar::from(2).pow_vartime([64]);
        let mut point = pallas::Point::generator();
        for _ in 0..16 {
            let epk = point.to_affine().to_bytes();
            let expected = (point * two_pow_64).to_affine().to_bytes();
            assert_eq!(shifted_ephemeral_key(&epk), Some(expected));
            point = point.double() + pallas::Point::generator();
        }
    }
}
