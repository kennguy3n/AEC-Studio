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

/// R2007 currently cannot write entities (assemble_r2007 does not
/// emit entity-bearing data pages; PR-C / phase 6 of the R2007
/// roadmap is the follow-up). `write_modern` returns
/// `Err(UnsupportedInVersion)` for any R2007 doc with non-empty
/// `entities`. The R2007 golden therefore exercises the same wire
/// layout (file header + classes + handle map + zero data pages)
/// using an explicitly empty document, instead of relying on the
/// previous silent-drop behavior.
fn r2007_doc() -> DxfDocument {
    DxfDocument::new()
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
            bytes: 2371,
            blake3_hex: "d9d5f8053b2e4e020d0d178baf62d9969b0fdffe469c38642ee9f7042f32948f",
        },
        DwgVersion::R14 => Golden {
            bytes: 661,
            blake3_hex: "6cf87f5374b7be378b5b6f0d46e8850813c97f19e06cc03bc7c3f73bb01e8c43",
        },
        DwgVersion::R2000 => Golden {
            bytes: 729,
            blake3_hex: "96d554b9d5363d1f0f5ed070a815aea1d870136a43648d4ebcb2ab96b94c7779",
        },
        DwgVersion::R2004 => Golden {
            bytes: 2454,
            blake3_hex: "84ec7c2b847be7b91d12ffed1f70de7c912b608935ade4ee04d172b4fd9976dc",
        },
        DwgVersion::R2007 => Golden {
            bytes: 3968,
            blake3_hex: "d78f3573167bef2dd48a60ebb6656a23b713f9364f58854a429b88c9d4c643ae",
        },
        DwgVersion::R2010 => Golden {
            bytes: 2695,
            blake3_hex: "ee5e05caebce730d90b32c652707500a7ab8a8f07183eed6cc56fe302bf5341d",
        },
        DwgVersion::R2013 => Golden {
            bytes: 2699,
            blake3_hex: "e22ab7cc2a3da1cbcbe154374945c62d2471c13ec8179d97804bead01ec06403",
        },
        DwgVersion::R2018 => Golden {
            bytes: 2711,
            blake3_hex: "2e51c09fea082e305e7ac0fbd17ff70b4b4133091dbffb2b28b20cf213020151",
        },
    }
}

fn check_version(v: DwgVersion) {
    // R2007 cannot yet round-trip entities through write_modern —
    // pass an empty doc so the test exercises the file/sections
    // layer without tripping the new `UnsupportedInVersion` guard
    // in `write_modern`.
    let doc = if v == DwgVersion::R2007 {
        r2007_doc()
    } else {
        canonical_doc()
    };

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
    if v == DwgVersion::R2007 {
        // PR-C in-flight: R2007 now goes through `assemble_r2007`
        // which emits a valid file header + sections-map but no
        // entity-bearing data pages yet. The encoder now rejects
        // entities outright (UnsupportedInVersion); the round-trip
        // therefore exercises the empty-document path and recovers
        // zero entities. Entity round-trip will be restored once
        // data-page emission lands later in the R2007 roadmap.
        assert_eq!(back.entities.len(), 0, "{v:?} empty-doc round-trip");
        return;
    }
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
