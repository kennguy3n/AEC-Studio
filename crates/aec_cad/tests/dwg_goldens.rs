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
            bytes: 1118,
            blake3_hex: "e1fdf01aacf6f818b5747e77b0c5df930534e2d72e29244459e2d4d70661ebb5",
        },
        DwgVersion::R14 => Golden {
            bytes: 483,
            blake3_hex: "ba8a3d3ab903430913585020987357907e16f041ec5cc9e9470d1cb5b1326b2e",
        },
        DwgVersion::R2000 => Golden {
            bytes: 545,
            blake3_hex: "e25c9dbfc186f2062bb5bf7a3184d203e42f7f97e7dcf294130ac7d60f4ab439",
        },
        DwgVersion::R2004 => Golden {
            bytes: 2280,
            blake3_hex: "0d22463894d0a571b04fbc3d06c027d6a72a86fa3f0d055ecb254508345556be",
        },
        DwgVersion::R2007 => Golden {
            bytes: 3968,
            blake3_hex: "d78f3573167bef2dd48a60ebb6656a23b713f9364f58854a429b88c9d4c643ae",
        },
        DwgVersion::R2010 => Golden {
            bytes: 2484,
            blake3_hex: "55cdca054c6386967284d8d5712dae079ec354ba4871ea233967b092c7fbe02c",
        },
        DwgVersion::R2013 => Golden {
            bytes: 2487,
            blake3_hex: "14f0b6d4545a8871de52c3430d61e2ba8b8b88acd879c5b3ae596ff7fb2be7e5",
        },
        DwgVersion::R2018 => Golden {
            bytes: 2499,
            blake3_hex: "fb271d4182415ae4294652097b24df3f63b91e76f86631a9adfa56ce56478a2c",
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
