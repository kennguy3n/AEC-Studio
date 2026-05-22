//! Per-version DWG golden round-trip tests.
//!
//! For each supported DWG version (R12 → R2018), we take a fixed
//! canonical drawing (one LINE on layer "0", from (0,0,0) to (1,1,0))
//! and assert two properties on the bytes the writer produces:
//!
//! 1. **Determinism** — encoding the same document twice produces
//!    byte-for-byte identical output. Catches accidental
//!    timestamp-like nondeterminism.
//! 2. **Stable BLAKE3 fingerprint** — the bytes hash to a known
//!    pinned BLAKE3 value. Catches unintentional binary-level
//!    changes (e.g. somebody flips a flag default and every existing
//!    DWG file shifts shape).
//! 3. **Round-trip** — reading the written bytes back yields a
//!    document with the same single LINE entity, on the same layer,
//!    with the same endpoints.
//!
//! These three properties together cover what self-round-trip alone
//! can't: an encoder that's broken in a consistent way will still
//! round-trip to itself, but the golden hash will move.
//!
//! When the on-disk format intentionally changes (e.g. a new common-
//! header flag lands), the diff in this test is the place where the
//! intended change is recorded.

use aec_cad::dwg::{DwgReader, DwgVersion, DwgWriter};
use aec_cad::dxf::{DxfDocument, DxfEntity, DxfLine};

/// The canonical fixture: one LINE on layer "0", from (0,0,0) to
/// (1,1,0). Small enough that the bit-stream framing dominates the
/// fingerprint; large enough that every section gets exercised.
fn canonical_doc() -> DxfDocument {
    let mut doc = DxfDocument::new();
    doc.push(DxfEntity::Line(DxfLine {
        layer: "0".into(),
        start: [0.0, 0.0, 0.0],
        end: [1.0, 1.0, 0.0],
    }));
    doc
}

/// (length_in_bytes, blake3_hex) tuple for one version.
struct Golden {
    bytes: usize,
    blake3_hex: &'static str,
}

/// Pinned goldens. Update these intentionally — a flipped value
/// means the wire format for that version moved.
fn golden(v: DwgVersion) -> Golden {
    match v {
        DwgVersion::R12 => Golden {
            bytes: 1118,
            blake3_hex: "0aa4e2fccde03a087c6d0b68120e8f0b7e2e4c46bd20bc1f6b2e58a2e3de4962",
        },
        DwgVersion::R14 => Golden {
            bytes: 211,
            blake3_hex: "113e8063d2988d7b43b1f499706d19b66970929a81c07131863bbf957359c91a",
        },
        DwgVersion::R2000 => Golden {
            bytes: 212,
            blake3_hex: "fd50b45e7d05b510236c00eed38bc0d60d0409cbca03fc792ca8980c40241447",
        },
        DwgVersion::R2004 => Golden {
            bytes: 1051,
            blake3_hex: "3c1e489f4077f9d32a77a71c8a89be715f8eb373dd1d71e738fb60c77666c0f6",
        },
        DwgVersion::R2007 => Golden {
            bytes: 1052,
            blake3_hex: "90166f1bb16d994c452276e14e77f789a9fb22462e9169db20e8f36c2699bfca",
        },
        DwgVersion::R2010 => Golden {
            bytes: 1056,
            blake3_hex: "6fc4f52862463c1974857cb03eb65660493bb3ea9b7c8ed0a52742ed60a7b061",
        },
        DwgVersion::R2013 => Golden {
            bytes: 1057,
            blake3_hex: "2a2ac672f88b6b6c34567460ffb7b8bc2012e487b8246020329823b6d830d68a",
        },
        DwgVersion::R2018 => Golden {
            bytes: 1057,
            blake3_hex: "96df8434e6bb67b0d3eedf113cdfe45c7f965555039ab16dd9746ca33cfde9dd",
        },
    }
}

fn check_version(v: DwgVersion) {
    let doc = canonical_doc();

    let bytes = DwgWriter::write(&doc, v)
        .unwrap_or_else(|e| panic!("DwgWriter::write failed for {v:?}: {e:?}"));

    // 1. Determinism.
    let again = DwgWriter::write(&doc, v)
        .unwrap_or_else(|e| panic!("DwgWriter::write retry failed for {v:?}: {e:?}"));
    assert_eq!(
        again, bytes,
        "non-deterministic write for {v:?}: same DxfDocument produced two different byte streams"
    );

    // 2. Golden fingerprint.
    let want = golden(v);
    let got_hash = blake3::hash(&bytes);
    let got_hex = got_hash.to_hex();
    assert_eq!(
        bytes.len(),
        want.bytes,
        "DWG byte length for {v:?} changed: want {} bytes, got {} bytes (new hash: {}). \
         If this change is intentional, update the goldens.",
        want.bytes,
        bytes.len(),
        got_hex
    );
    assert_eq!(
        got_hex.as_str(),
        want.blake3_hex,
        "DWG BLAKE3 for {v:?} changed: want {}, got {}. \
         If this change is intentional, update the goldens.",
        want.blake3_hex,
        got_hex
    );

    // 3. Round-trip.
    let reader =
        DwgReader::new(&bytes).unwrap_or_else(|e| panic!("DwgReader::new failed for {v:?}: {e:?}"));
    assert_eq!(reader.version, v, "version detection mis-fired for {v:?}");
    let back = reader
        .into_document()
        .unwrap_or_else(|e| panic!("DwgReader::into_document failed for {v:?}: {e:?}"));
    assert_eq!(back.entities.len(), 1, "{v:?} round-trip lost the line");
    match &back.entities[0] {
        DxfEntity::Line(l) => {
            assert_eq!(l.layer, "0", "{v:?}: layer mismatch");
            assert_eq!(l.start, [0.0, 0.0, 0.0], "{v:?}: start mismatch");
            assert_eq!(l.end, [1.0, 1.0, 0.0], "{v:?}: end mismatch");
        }
        other => panic!("{v:?}: expected Line, got {other:?}"),
    }
}

#[test]
fn r12_golden() {
    check_version(DwgVersion::R12);
}

#[test]
fn r14_golden() {
    check_version(DwgVersion::R14);
}

#[test]
fn r2000_golden() {
    check_version(DwgVersion::R2000);
}

#[test]
fn r2004_golden() {
    check_version(DwgVersion::R2004);
}

#[test]
fn r2007_golden() {
    check_version(DwgVersion::R2007);
}

#[test]
fn r2010_golden() {
    check_version(DwgVersion::R2010);
}

#[test]
fn r2013_golden() {
    check_version(DwgVersion::R2013);
}

#[test]
fn r2018_golden() {
    check_version(DwgVersion::R2018);
}
