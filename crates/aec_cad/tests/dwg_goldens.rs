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
            bytes: 174,
            blake3_hex: "1ed827cae802dbdcf6c42345494c6ca716575a2f994bb6d7ff88d7a9c186358d",
        },
        DwgVersion::R2000 => Golden {
            bytes: 175,
            blake3_hex: "6cafc0fbda3c7e12b393e0923ffbc62a8a3d7dd5097e429c81a3e3a8fd3c9414",
        },
        DwgVersion::R2004 => Golden {
            bytes: 1051,
            blake3_hex: "64e3e1c025c02f80eb81e1f520d62b8daceb7644cb47685bab5071908fbc7da3",
        },
        DwgVersion::R2007 => Golden {
            bytes: 1052,
            blake3_hex: "49c1cb6e78fd563e1bba4cf09925273529eb387ed683bf0c1c50bfd6a4e070dd",
        },
        DwgVersion::R2010 => Golden {
            bytes: 1056,
            blake3_hex: "e82262622ac5932df4b28e7e5f040b0e132897c9e2416b2e55bbcd08cd997f1b",
        },
        DwgVersion::R2013 => Golden {
            bytes: 1057,
            blake3_hex: "e8f765231fa57e1170019976eb4439c354124e60143455f3349f076d58705398",
        },
        DwgVersion::R2018 => Golden {
            bytes: 1057,
            blake3_hex: "d392c478796c86b922f38be778eb16c3a6149ba79eeacb4defadcab2baae8a07",
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
