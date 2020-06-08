//! Support for converting ECDSA signatures between the ASN.1 DER and "fixed"
//! encodings using a self-contained implementation of the relevant parts of
//! ASN.1 DER (i.e. `SEQUENCE` and `INTEGER`).
//!
//! Adapted from BearSSL. Copyright (c) 2016 Thomas Pornin <pornin@bolet.org>.
//! Relicensed under Apache 2.0 + MIT (from original MIT) with permission.
//!
//! <https://www.bearssl.org/gitweb/?p=BearSSL;a=blob;f=src/ec/ecdsa_atr.c>
//! <https://www.bearssl.org/gitweb/?p=BearSSL;a=blob;f=src/ec/ecdsa_rta.c>

use crate::generic_array::{typenum::Unsigned, ArrayLength, GenericArray};
use crate::{
    asn1_signature::{self, Asn1Signature},
    Curve, FixedSignature,
};
use core::{marker::PhantomData, ops::Add};
use signature::Signature;

/// ASN.1 tags
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Asn1Tag {
    /// ASN.1 `INTEGER`
    Integer = 0x02,

    /// ASN.1 `SEQUENCE`: lists of other elements
    Sequence = 0x30,
}

/// ECDSA signature `r` and `s` values, represented as slices which are at
/// most `C::ScalarSize` bytes (but *may* be smaller).
///
/// The `r` and `s` scalars are the same size as the curve's modulus, i.e.
/// for an elliptic curve over a ~256-bit prime field, they will also be
/// 256-bit (i.e. the `ScalarSize` for a particular `Curve`).
///
/// This type provides a convenient representation for converting between
/// formats, i.e. all of the serialization code is in this module.
pub struct ScalarPair<'a, C: Curve + 'a> {
    /// `r` scalar value
    r: &'a [u8],

    /// `s` scalar value
    s: &'a [u8],

    /// Placeholder for elliptic curve type
    curve: PhantomData<C>,
}

impl<'a, C: Curve + 'a> ScalarPair<'a, C>
where
    asn1_signature::MaxSize<C::ScalarSize>: ArrayLength<u8>,
    <C::ScalarSize as Add>::Output: ArrayLength<u8> + Add<asn1_signature::MaxOverhead>,
{
    /// Parse the given ASN.1 DER-encoded ECDSA signature, obtaining the
    /// `r` and `s` scalar pair
    pub(crate) fn from_asn1_signature(signature: &'a Asn1Signature<C>) -> Option<Self> {
        // Signature format is a SEQUENCE of two INTEGER values. We
        // support only integers of less than 127 bytes each (signed
        // encoding) so the resulting raw signature will have length
        // at most 254 bytes.
        let mut bytes = signature.as_bytes();

        // First byte is SEQUENCE tag.
        if bytes[0] != Asn1Tag::Sequence as u8 {
            return None;
        }

        // The SEQUENCE length will be encoded over one or two bytes. We
        // limit the total SEQUENCE contents to 255 bytes, because it
        // makes things simpler; this is enough for subgroup orders up
        // to 999 bits.
        let mut zlen = bytes[1] as usize;

        if zlen > 0x80 {
            if zlen != 0x81 {
                return None;
            }

            zlen = bytes[2] as usize;

            if zlen != bytes.len().checked_sub(3).unwrap() {
                return None;
            }

            bytes = &bytes[3..];
        } else {
            if zlen != bytes.len().checked_sub(2).unwrap() {
                return None;
            }

            bytes = &bytes[2..];
        }

        // First INTEGER (r)
        let (mut r, bytes) = Self::asn1_int_parse(bytes)?;

        // Second INTEGER (s)
        let (mut s, bytes) = Self::asn1_int_parse(bytes)?;

        if !bytes.is_empty() {
            return None;
        }

        let scalar_size = C::ScalarSize::to_usize();

        if r.len() > scalar_size {
            if r.len() != scalar_size.checked_add(1).unwrap() {
                return None;
            }

            if r[0] != 0 {
                return None;
            }

            r = &r[1..];
        }

        if s.len() > scalar_size {
            if s.len() != scalar_size.checked_add(1).unwrap() {
                return None;
            }

            if s[0] != 0 {
                return None;
            }

            s = &s[1..];
        }

        // Removing leading zeros from r and s

        while !r.is_empty() && r[0] == 0 {
            r = &r[1..];
        }

        while !s.is_empty() && s[0] == 0 {
            s = &s[1..];
        }

        Some(Self {
            r,
            s,
            curve: PhantomData,
        })
    }

    /// Parse the given fixed-size ECDSA signature, obtaining the `r` and `s`
    /// scalar pair
    pub(crate) fn from_fixed_signature(signature: &'a FixedSignature<C>) -> Self {
        let scalar_size = C::ScalarSize::to_usize();

        Self {
            r: &signature.as_ref()[..scalar_size],
            s: &signature.as_ref()[scalar_size..],
            curve: PhantomData,
        }
    }

    /// Serialize this ECDSA signature's `r` and `s` scalar pair as ASN.1 DER
    pub(crate) fn to_asn1_signature(&self) -> Asn1Signature<C> {
        let rlen = Self::asn1_int_length(self.r);
        let slen = Self::asn1_int_length(self.s);
        let mut bytes = GenericArray::default();

        // SEQUENCE header
        bytes[0] = Asn1Tag::Sequence as u8;
        let zlen = rlen.checked_add(slen).unwrap().checked_add(4).unwrap();

        let mut offset = if zlen >= 0x80 {
            bytes[1] = 0x81;
            bytes[2] = zlen as u8;
            3
        } else {
            bytes[1] = zlen as u8;
            2
        };

        // First INTEGER (r)
        Self::asn1_int_serialize(self.r, &mut bytes[offset..], rlen);
        offset = offset.checked_add(2).unwrap().checked_add(rlen).unwrap();

        // Second INTEGER (s)
        Self::asn1_int_serialize(self.s, &mut bytes[offset..], slen);

        Asn1Signature {
            bytes,
            length: offset.checked_add(2).unwrap().checked_add(slen).unwrap(),
        }
    }

    pub(crate) fn to_fixed_signature(&self) -> FixedSignature<C> {
        let mut bytes = GenericArray::default();

        let scalar_size = C::ScalarSize::to_usize();
        let rbegin = scalar_size.checked_sub(self.r.len()).unwrap();
        bytes.as_mut_slice()[rbegin..scalar_size].copy_from_slice(self.r);

        let sbegin = bytes.len().checked_sub(self.s.len()).unwrap();
        bytes.as_mut_slice()[sbegin..].copy_from_slice(self.s);

        FixedSignature::from(bytes)
    }

    /// Compute ASN.1 DER encoded length for the provided scalar. The ASN.1
    /// encoding is signed, so its leading bit must have value 0; it must also be
    /// of minimal length (so leading bytes of value 0 must be removed, except if
    /// that would contradict the rule about the sign bit).
    fn asn1_int_length(mut x: &[u8]) -> usize {
        while !x.is_empty() && x[0] == 0 {
            x = &x[1..];
        }

        if x.is_empty() || x[0] >= 0x80 {
            x.len().checked_add(1).unwrap()
        } else {
            x.len()
        }
    }

    /// Parse an integer from its ASN.1 DER serialization
    fn asn1_int_parse(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
        if bytes.len() < 3 {
            return None;
        }

        if bytes[0] != Asn1Tag::Integer as u8 {
            return None;
        }

        let len = bytes[1] as usize;

        if len >= 0x80 || len.checked_add(2).unwrap() > bytes.len() {
            return None;
        }

        let integer = &bytes[2..len.checked_add(2).unwrap()];
        let remaining = &bytes[len.checked_add(2).unwrap()..];

        Some((integer, remaining))
    }

    /// Serialize scalar as ASN.1 DER
    fn asn1_int_serialize(scalar: &[u8], out: &mut [u8], len: usize) {
        out[0] = Asn1Tag::Integer as u8;
        out[1] = len as u8;

        if len > C::ScalarSize::to_usize() {
            out[2] = 0x00;
            out[3..C::ScalarSize::to_usize().checked_add(3).unwrap()].copy_from_slice(scalar);
        } else {
            out[2..len.checked_add(2).unwrap()]
                .copy_from_slice(&scalar[C::ScalarSize::to_usize().checked_sub(len).unwrap()..]);
        }
    }
}

impl<'a, C: Curve> From<&'a Asn1Signature<C>> for FixedSignature<C>
where
    asn1_signature::MaxSize<C::ScalarSize>: ArrayLength<u8>,
    <C::ScalarSize as Add>::Output: ArrayLength<u8> + Add<asn1_signature::MaxOverhead>,
{
    fn from(asn1_signature: &Asn1Signature<C>) -> FixedSignature<C> {
        // We always ensure `Asn1Signature`s parse successfully, so this should always work
        ScalarPair::from_asn1_signature(asn1_signature)
            .unwrap()
            .to_fixed_signature()
    }
}

impl<'a, C: Curve> From<&'a FixedSignature<C>> for Asn1Signature<C>
where
    asn1_signature::MaxSize<C::ScalarSize>: ArrayLength<u8>,
    <C::ScalarSize as Add>::Output: ArrayLength<u8> + Add<asn1_signature::MaxOverhead>,
{
    /// Parse `r` and `s` values from a fixed-width signature and reserialize
    /// them as ASN.1 DER.
    fn from(fixed_signature: &FixedSignature<C>) -> Self {
        ScalarPair::from_fixed_signature(fixed_signature).to_asn1_signature()
    }
}

#[cfg(all(test, feature = "test-vectors"))]
mod tests {
    use crate::{
        curve::nistp256::{Asn1Signature, FixedSignature},
        test_vectors::nistp256::SHA256_FIXED_SIZE_TEST_VECTORS,
    };
    use signature::Signature;

    #[test]
    fn test_fixed_to_asn1_signature_roundtrip() {
        for vector in SHA256_FIXED_SIZE_TEST_VECTORS {
            let fixed_signature = FixedSignature::from_bytes(&vector.sig).unwrap();

            // Convert to DER and back
            let asn1_signature = Asn1Signature::from(&fixed_signature);
            let fixed_signature2 = FixedSignature::from(&asn1_signature);

            assert_eq!(fixed_signature, fixed_signature2);
        }
    }
}
