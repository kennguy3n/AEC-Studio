//! Wire-emitters for OBJECT-supertype records that DwgWriter must
//! emit to make LibreDWG's `dxf_tables_write` (out_dxf.c:3158-3489)
//! and `dxf_entities_write` (out_dxf.c:3636-3700) walk our document
//! cleanly:
//!
//! * `LAYER_CONTROL` — entry vector for the LAYER table.
//! * `LAYER` ("0") — the always-present default layer that entities
//!   reference via `handles.layer`.
//! * `BLOCK_CONTROL` — entry vector for the BLOCK_RECORD table; the
//!   record `dxf_tables_write` inspects to enumerate every block.
//! * `BLOCK_HEADER` (*Model_Space*) — the block record `dxf_tables_write`
//!   resolves via `dwg_model_space_object` and walks via the
//!   `first_entity` / `last_entity` chain (R14, R2000) or
//!   `entities[]` vector (R2004+) to emit the ENTITIES section.
//!
//! Without these records the file parses (dwgread EXIT=0) but the
//! ENTITIES section of the DXF round-trip is silently truncated
//! after `TABLES`, because `dxf_tables_write` returns
//! `DWG_ERR_INVALIDDWG` the moment `dwg_block_control` or
//! `dwg_model_space_object` returns `NULL` — and the caller bails on
//! the whole output (out_dxf.c:3809).
//!
//! Per-version layout follows LibreDWG `dwg.spec`:
//! * BLOCK_CONTROL: spec lines 3490-3535
//! * LAYER_CONTROL / LAYER: spec lines 3667-3780
//! * BLOCK_HEADER: spec lines 3535-3650
//!
//! Strings in pre-R2007 are written inline in the data stream as TV
//! (CP1252, BS length prefix). For R2007+ they go in the separate
//! string region (`ObjectRecord::string_payload_bits`) which the
//! per-record encoder appends to the body just before the
//! `data_size` RS + `has_strings` B markers.

use crate::dwg::bits::reader::{Color, HandleRef};
use crate::dwg::bits::BitWriter;
use crate::dwg::entities::header_codec::CommonHeaderData;
use crate::dwg::entities::record::{BitBuf, ObjectCommonData, ObjectHandles, ObjectSupertype};
use crate::dwg::entities::{ObjectRecord, ObjectType};
use crate::dwg::error::DwgResult;
use crate::dwg::version::Version;

/// Standard handle assignment for the table-conformance objects.
///
/// These match the convention real AutoCAD files use for the
/// minimum-content fixture: BLOCK_CONTROL and LAYER_CONTROL get the
/// first two handles, LAYER "0" lands at 0x14, the *Model_Space*
/// BLOCK_HEADER at 0x1F, and the BLOCK / ENDBLK that frame model
/// space at 0x1D / 0x1E. User entities (LINE, CIRCLE, TEXT) then
/// follow at 0x21 onwards.
pub mod handles {
    /// `BLOCK_CONTROL` object handle. Real AutoCAD fixtures put this
    /// at handle 1 (the first allocated object after the implicit
    /// 0/null reserved handle).
    pub const BLOCK_CONTROL: u64 = 0x01;
    /// `LAYER_CONTROL` object handle.
    pub const LAYER_CONTROL: u64 = 0x02;
    /// `LAYER` "0" — the always-present default layer.
    pub const LAYER_ZERO: u64 = 0x14;
    /// `BLOCK` entity at the start of the *Model_Space block.
    pub const MODEL_SPACE_BLOCK: u64 = 0x1d;
    /// `ENDBLK` entity at the end of the *Model_Space block.
    pub const MODEL_SPACE_ENDBLK: u64 = 0x1e;
    /// `BLOCK_HEADER` for the *Model_Space block.
    pub const MODEL_SPACE_BLOCK_HEADER: u64 = 0x1f;
    /// First user-entity handle. Real AutoCAD fixtures start
    /// model-space user entities here; the value is arbitrary so
    /// long as it doesn't collide with any of the reserved table
    /// records above.
    pub const FIRST_USER_ENTITY: u64 = 0x21;
}

/// Build the `LAYER_CONTROL` table object. Owns the LAYER "0"
/// record by reference (the handle vector contains its handle).
pub fn emit_layer_control(
    version: Version,
    handle: u64,
    layer_handles: &[u64],
) -> DwgResult<ObjectRecord> {
    let mut payload = BitWriter::new();
    // num_entries BL.
    payload.write_bl(layer_handles.len() as i64)?;

    // type_extras: entries[] handles, code 2 (hard owner from the
    // control object to each child record).
    let type_extras: Vec<HandleRef> = layer_handles
        .iter()
        .map(|h| HandleRef { code: 2, value: *h })
        .collect();

    Ok(ObjectRecord {
        object_type: ObjectType::LayerControl,
        handle: HandleRef {
            code: 0,
            value: handle,
        },
        supertype: ObjectSupertype::Object,
        common: CommonHeaderData::default(),
        object_common: ObjectCommonData {
            num_reactors: 0,
            // Pre-R2004 always emits xdict; R2004+ gates on the flag
            // bit. We don't emit an xdict object, so set
            // is_xdic_missing = true for R2004+ (LibreDWG accepts the
            // gated null branch; on pre-R2004 the gate doesn't apply
            // and we emit code-3 null xdict via the encoder).
            is_xdic_missing: version >= Version::R2004,
            has_ds_data: false,
        },
        payload_bits: BitBuf::from_writer(payload),
        string_payload_bits: BitBuf::new(),
        handles: ObjectHandles {
            // Owner = handle 0 / null — LAYER_CONTROL is owned by
            // the NAMED OBJECT DICTIONARY in newer files, but
            // LibreDWG accepts a null soft-owner for the minimum
            // fixture and `dwg_block_control` / friends just walk the
            // entries[].
            owner: Some(HandleRef { code: 4, value: 0 }),
            reactors: Vec::new(),
            x_dictionary: None,
            // The remaining `ObjectHandles` fields are entity-only;
            // the encoder skips them on the OBJECT codepath.
            layer: HandleRef::default(),
            linetype: None,
            prev_entity: None,
            next_entity: None,
            material: None,
            shadow: None,
            plot_style: None,
            full_visualstyle: None,
            face_visualstyle: None,
            edge_visualstyle: None,
            type_extras,
        },
    })
}

/// Build the `LAYER` "0" table record. Color index 7 (white),
/// flags = 0 (on, not frozen, not locked).
///
/// Per `dwg.spec` line 3684 (`DWG_TABLE (LAYER)`) + `spec.h:725`
/// (`COMMON_TABLE_FLAGS`):
///
/// DATA stream (R13+):
///   T name
///   R13..R2004: B is_xref_ref, BS is_xref_resolved, B is_xref_dep
///   R2007+:     BS is_xref_resolved
///   R13..R14:   B frozen, B on, B frozen_in_new, B locked
///   R2000+:     BSx flag0
///   CMC color
///
/// HANDLE stream (after common owner / reactors / xdict):
///   H xref (5)                                  always
///   H plotstyle (5)                             R2000+
///   H material (5)                              R2007+
///   H ltype (5)                                 always
///   H visualstyle (5)                           R2013+
pub fn emit_layer_zero(
    version: Version,
    handle: u64,
    owner_handle: u64,
) -> DwgResult<ObjectRecord> {
    let mut payload = BitWriter::new();
    let mut str_payload = BitWriter::new();
    let layer_name = "0";

    if version.uses_utf16_strings() {
        // R2007+: T fields land in the string region as UTF-16.
        str_payload.write_t(layer_name)?;
    } else {
        // R13..R2004: T encodes as TV (CP1252, inline in payload).
        payload.write_tv(layer_name)?;
    }

    // is_xref_* bits from COMMON_TABLE_FLAGS. The non-xref layer "0"
    // has all three false (R13..R2004) or just `is_xref_resolved = 0`
    // on R2007+ (the other two are derived on decode).
    if version <= Version::R2004 {
        payload.write_b(false)?; // is_xref_ref
        payload.write_bs(0)?; // is_xref_resolved
        payload.write_b(false)?; // is_xref_dep
    } else {
        payload.write_bs(0)?; // is_xref_resolved (R2007+)
    }

    if version <= Version::R14 {
        // R13/R14: four B flags (frozen / on / frozen_in_new / locked).
        payload.write_b(false)?; // frozen
        payload.write_b(true)?; // on
        payload.write_b(false)?; // frozen_in_new
        payload.write_b(false)?; // locked
    } else {
        // R2000+: single BSx flag0 word. Bit semantics per dwg.spec:
        //   bit 0  = frozen        bit 1  = on (negated logic — 0 = on)
        //   bit 2  = frozen_in_new bit 3  = locked
        //   bit 4  = plotflag      bits 5..9 = linewt
        // For layer "0" we want "on, not frozen, not locked,
        // plotting enabled, linewt = default (0)":
        //   on = 1 → flag bit 1 = 0
        //   plotflag = 1 → flag bit 4 = 16
        // → flag0 = 16. BSx is identical wire-shape to BS so write_bs
        //   suffices.
        payload.write_bs(16)?;
    }

    // CMC color (R13+). Color index 7 (white) is the conventional
    // "0" layer default — visible on both light and dark backgrounds.
    payload.write_cmc_v(version, &Color::Index(7))?;

    // type_extras follow the OBJECT handle-stream tail. The first
    // entry is `xref` from COMMON_TABLE_FLAGS — LibreDWG's
    // START_HANDLE_STREAM mechanism re-orders it AFTER
    // owner/reactors/xdict but BEFORE the per-type handles
    // (encode.c:944 — `obj_flush_hdlstream` is called twice, first
    // for the common handles and second for the COMMON_TABLE_FLAGS
    // accumulated chain).
    //
    // After xref:
    //   FIELD_HANDLE (plotstyle, 5, 390);    SINCE (R_2000)
    //   FIELD_HANDLE (material, 5, 347);     SINCE (R_2007a)
    //   FIELD_HANDLE (ltype, 5, 6);          always
    //   FIELD_HANDLE (visualstyle, 5, 348);  SINCE (R_2013b)
    let mut type_extras = Vec::new();
    type_extras.push(HandleRef { code: 5, value: 0 }); // xref (null)
    if version >= Version::R2000 {
        type_extras.push(HandleRef { code: 5, value: 0 }); // plotstyle
    }
    if version >= Version::R2007 {
        type_extras.push(HandleRef { code: 5, value: 0 }); // material
    }
    type_extras.push(HandleRef { code: 5, value: 0 }); // ltype
    if version >= Version::R2013 {
        type_extras.push(HandleRef { code: 5, value: 0 }); // visualstyle
    }

    Ok(ObjectRecord {
        object_type: ObjectType::Layer,
        handle: HandleRef {
            code: 0,
            value: handle,
        },
        supertype: ObjectSupertype::Object,
        common: CommonHeaderData::default(),
        object_common: ObjectCommonData {
            num_reactors: 0,
            is_xdic_missing: version >= Version::R2004,
            has_ds_data: false,
        },
        payload_bits: BitBuf::from_writer(payload),
        string_payload_bits: BitBuf::from_writer(str_payload),
        handles: ObjectHandles {
            owner: Some(HandleRef {
                code: 4,
                value: owner_handle, // LAYER_CONTROL
            }),
            reactors: Vec::new(),
            x_dictionary: None,
            layer: HandleRef::default(),
            linetype: None,
            prev_entity: None,
            next_entity: None,
            material: None,
            shadow: None,
            plot_style: None,
            full_visualstyle: None,
            face_visualstyle: None,
            edge_visualstyle: None,
            type_extras,
        },
    })
}

/// Build the `BLOCK_CONTROL` table object.
///
/// `block_handles` is the list of every BLOCK_HEADER entry the table
/// owns (typically two: *Model_Space and *Paper_Space, but we ship
/// only *Model_Space). `model_space_handle` and `paper_space_handle`
/// are the trailing two handles in the type_extras vector (per
/// `dwg.spec` line 3534).
pub fn emit_block_control(
    version: Version,
    handle: u64,
    block_handles: &[u64],
    model_space_handle: u64,
    paper_space_handle: u64,
) -> DwgResult<ObjectRecord> {
    let mut payload = BitWriter::new();
    payload.write_bl(block_handles.len() as i64)?;

    let mut type_extras: Vec<HandleRef> = block_handles
        .iter()
        .map(|h| HandleRef { code: 2, value: *h })
        .collect();
    // After entries[]: model_space (code 3, soft pointer per spec)
    // and paper_space (code 3). LibreDWG accepts code 3.
    type_extras.push(HandleRef {
        code: 3,
        value: model_space_handle,
    });
    type_extras.push(HandleRef {
        code: 3,
        value: paper_space_handle,
    });

    Ok(ObjectRecord {
        object_type: ObjectType::BlockControl,
        handle: HandleRef {
            code: 0,
            value: handle,
        },
        supertype: ObjectSupertype::Object,
        common: CommonHeaderData::default(),
        object_common: ObjectCommonData {
            num_reactors: 0,
            is_xdic_missing: version >= Version::R2004,
            has_ds_data: false,
        },
        payload_bits: BitBuf::from_writer(payload),
        string_payload_bits: BitBuf::new(),
        handles: ObjectHandles {
            owner: Some(HandleRef { code: 4, value: 0 }),
            reactors: Vec::new(),
            x_dictionary: None,
            layer: HandleRef::default(),
            linetype: None,
            prev_entity: None,
            next_entity: None,
            material: None,
            shadow: None,
            plot_style: None,
            full_visualstyle: None,
            face_visualstyle: None,
            edge_visualstyle: None,
            type_extras,
        },
    })
}

/// What body of entities the BLOCK_HEADER owns. Different per
/// version: R14/R2000 chain prev/next via entity handles
/// (`first_entity`/`last_entity`); R2004+ store an `entities[]`
/// vector inside the BLOCK_HEADER object's handle stream.
#[derive(Debug, Clone)]
pub struct ModelSpaceOwnership {
    /// Handle of the BLOCK entity at the start of the block.
    pub block_entity: u64,
    /// Handle of the ENDBLK entity at the end of the block.
    pub endblk_entity: u64,
    /// All user-entity handles, in iteration order.
    pub user_entities: Vec<u64>,
}

/// Build the BLOCK_HEADER object for `*Model_Space`. This is the
/// record `dxf_tables_write` resolves through
/// `dwg_model_space_object` and iterates via `get_first_owned_entity`
/// / `get_next_owned_block_entity` to emit the ENTITIES section.
pub fn emit_model_space_block_header(
    version: Version,
    handle: u64,
    owner_handle: u64,
    ownership: &ModelSpaceOwnership,
) -> DwgResult<ObjectRecord> {
    let mut payload = BitWriter::new();
    let mut str_payload = BitWriter::new();
    let block_name = "*Model_Space";
    let xref_pname = ""; // empty for non-xref blocks
    let description = ""; // empty for the default model space

    // COMMON_TABLE_FLAGS body: T name + is_xref_* fields.
    if version.uses_utf16_strings() {
        str_payload.write_t(block_name)?;
    } else {
        payload.write_tv(block_name)?;
    }
    if version <= Version::R2004 {
        payload.write_b(false)?; // is_xref_ref
        payload.write_bs(0)?; // is_xref_resolved
        payload.write_b(false)?; // is_xref_dep
    } else {
        payload.write_bs(0)?; // is_xref_resolved (R2007+)
    }

    // Flags: anonymous / hasattrs / blkisxref / xrefoverlaid all
    // false for *Model_Space.
    payload.write_b(false)?; // anonymous
    payload.write_b(false)?; // hasattrs
    payload.write_b(false)?; // blkisxref
    payload.write_b(false)?; // xrefoverlaid
    if version >= Version::R2000 {
        payload.write_b(false)?; // loaded_bit (R2000+)
    }
    if version >= Version::R2004 {
        // num_owned BL (R2004+) — count of entities in the
        // entities[] vector. Includes user entities only (NOT the
        // BLOCK / ENDBLK that frame the block).
        payload.write_bl(ownership.user_entities.len() as i64)?;
    }

    // base_pt 3BD — (0, 0, 0) for *Model_Space.
    payload.write_3bd([0.0, 0.0, 0.0])?;

    if version.uses_utf16_strings() {
        str_payload.write_t(xref_pname)?;
    } else {
        payload.write_tv(xref_pname)?;
    }

    if version >= Version::R2000 {
        // num_inserts: `FIELD_NUM_INSERTS` is an RC-list (run of
        // `RC 1` bytes terminated by `RC 0`) — NOT a single RL or BL
        // field. See `encode.c:860`. For our `*Model_Space` fixture
        // we never emit a block-level INSERT, so the encoding
        // reduces to a single zero terminator byte.
        payload.write_rc(0)?;
        // description T (R2000+).
        if version.uses_utf16_strings() {
            str_payload.write_t(description)?;
        } else {
            payload.write_tv(description)?;
        }
        // preview_size BL (R2000+) — 0 means no preview bitmap
        // follows. Skipping the FIELD_BINARY in that case is safe.
        payload.write_bl(0)?;
    }
    if version >= Version::R2007 {
        // insert_units BS, explodable B, block_scaling RC. Defaults
        // (1 = inches, true, 0) match AutoCAD's empty fixture.
        payload.write_bs(1)?;
        payload.write_b(true)?;
        payload.write_rc(0)?;
    }

    // Build the OBJECT handle stream tail. Per `dwg.spec` lines
    // 3604-3660:
    //   xref (5) — from COMMON_TABLE_FLAGS; LibreDWG's
    //              START_HANDLE_STREAM reorders this to land AFTER
    //              the common handles (owner/reactors/xdict) but
    //              BEFORE the per-type handles below.
    //   block_entity (3) — the BLOCK entity that starts the block.
    //   pre-R2004: first_entity (4), last_entity (4) — chain bounds.
    //   R2004+: entities[i] (4) for each user entity.
    //   endblk_entity (3) — the ENDBLK entity.
    //   R2000+: inserts[i] (4) — INSERT entities referencing this
    //           block. We emit none.
    //   R2000+: layout (5) — soft pointer to LAYOUT dictionary. We
    //           emit null (LibreDWG accepts a null layout for the
    //           minimum fixture).
    let mut type_extras = Vec::new();
    type_extras.push(HandleRef { code: 5, value: 0 }); // xref (null)
    type_extras.push(HandleRef {
        code: 3,
        value: ownership.block_entity,
    });
    if version <= Version::R2000 {
        let first = ownership.user_entities.first().copied().unwrap_or(0);
        let last = ownership.user_entities.last().copied().unwrap_or(0);
        type_extras.push(HandleRef {
            code: 4,
            value: first,
        });
        type_extras.push(HandleRef {
            code: 4,
            value: last,
        });
    } else {
        for h in &ownership.user_entities {
            type_extras.push(HandleRef { code: 4, value: *h });
        }
    }
    type_extras.push(HandleRef {
        code: 3,
        value: ownership.endblk_entity,
    });
    if version >= Version::R2000 {
        // num_inserts == 0, so no insert handles.
        // layout handle (null).
        type_extras.push(HandleRef { code: 5, value: 0 });
    }

    Ok(ObjectRecord {
        object_type: ObjectType::BlockHeader,
        handle: HandleRef {
            code: 0,
            value: handle,
        },
        supertype: ObjectSupertype::Object,
        common: CommonHeaderData::default(),
        object_common: ObjectCommonData {
            num_reactors: 0,
            is_xdic_missing: version >= Version::R2004,
            has_ds_data: false,
        },
        payload_bits: BitBuf::from_writer(payload),
        string_payload_bits: BitBuf::from_writer(str_payload),
        handles: ObjectHandles {
            owner: Some(HandleRef {
                code: 4,
                value: owner_handle, // BLOCK_CONTROL
            }),
            reactors: Vec::new(),
            x_dictionary: None,
            layer: HandleRef::default(),
            linetype: None,
            prev_entity: None,
            next_entity: None,
            material: None,
            shadow: None,
            plot_style: None,
            full_visualstyle: None,
            face_visualstyle: None,
            edge_visualstyle: None,
            type_extras,
        },
    })
}

/// Build the `BLOCK` entity that opens the *Model_Space block.
///
/// BLOCK / ENDBLK use the ENTITY supertype (they have a common entity
/// header) but with `entity_mode = BlockHeader` (entmode = 0) so the
/// owner handle is emitted explicitly. The decoder uses
/// `block_entity` / `endblk_entity` on BLOCK_HEADER to locate them.
pub fn emit_model_space_block_entity(
    version: Version,
    handle: u64,
    block_header_handle: u64,
    layer_handle: u64,
) -> DwgResult<ObjectRecord> {
    let mut payload = BitWriter::new();
    let block_name = "*Model_Space";
    if version.uses_utf16_strings() {
        // ENTITY supertype doesn't use string_payload_bits today (the
        // entity R2007+ string-stream wiring is tracked alongside the
        // TEXT round-trip work). The BLOCK / ENDBLK entities frame
        // the model-space block; LibreDWG's BLOCK entity decoder
        // reads the name inline via the entity body stream, so for
        // R2007+ we emit it inline as a UTF-16 T field rather than
        // routing through the string region.
        payload.write_t(block_name)?;
    } else {
        payload.write_tv(block_name)?;
    }

    Ok(ObjectRecord {
        object_type: ObjectType::Block,
        handle: HandleRef {
            code: 0,
            value: handle,
        },
        supertype: ObjectSupertype::Entity,
        common: CommonHeaderData {
            // entity_mode = BlockHeader → owner handle emitted
            // explicitly (entmode = 0 in LibreDWG terminology).
            entity_mode: crate::dwg::entities::header_codec::EntityMode::BlockHeader,
            nolinks: false, // emit prev/next links on R14/R2000
            ..CommonHeaderData::default()
        },
        object_common: ObjectCommonData::default(),
        payload_bits: BitBuf::from_writer(payload),
        string_payload_bits: BitBuf::new(),
        handles: ObjectHandles {
            owner: Some(HandleRef {
                code: 5,
                value: block_header_handle,
            }),
            reactors: Vec::new(),
            x_dictionary: None,
            layer: HandleRef {
                code: 5,
                value: layer_handle,
            },
            linetype: None,
            // pre/next set up via wire_block_chain after the full
            // entity list is known.
            prev_entity: None,
            next_entity: None,
            material: None,
            shadow: None,
            plot_style: None,
            full_visualstyle: None,
            face_visualstyle: None,
            edge_visualstyle: None,
            type_extras: Vec::new(),
        },
    })
}

/// Build the `ENDBLK` entity that closes the *Model_Space block.
pub fn emit_model_space_endblk_entity(
    handle: u64,
    block_header_handle: u64,
    layer_handle: u64,
) -> DwgResult<ObjectRecord> {
    Ok(ObjectRecord {
        object_type: ObjectType::EndBlk,
        handle: HandleRef {
            code: 0,
            value: handle,
        },
        supertype: ObjectSupertype::Entity,
        common: CommonHeaderData {
            entity_mode: crate::dwg::entities::header_codec::EntityMode::BlockHeader,
            nolinks: false,
            ..CommonHeaderData::default()
        },
        object_common: ObjectCommonData::default(),
        payload_bits: BitBuf::new(), // ENDBLK has no per-type fields
        string_payload_bits: BitBuf::new(),
        handles: ObjectHandles {
            owner: Some(HandleRef {
                code: 5,
                value: block_header_handle,
            }),
            reactors: Vec::new(),
            x_dictionary: None,
            layer: HandleRef {
                code: 5,
                value: layer_handle,
            },
            linetype: None,
            prev_entity: None,
            next_entity: None,
            material: None,
            shadow: None,
            plot_style: None,
            full_visualstyle: None,
            face_visualstyle: None,
            edge_visualstyle: None,
            type_extras: Vec::new(),
        },
    })
}

/// Wire the prev_entity / next_entity chain across BLOCK → user
/// entities → ENDBLK for R14 / R2000. R2004+ rely on the
/// `entities[]` vector inside BLOCK_HEADER instead and don't need a
/// chain.
///
/// Caller must pass entities in order (BLOCK first, user entities,
/// ENDBLK last). On R2004+ this is a no-op.
pub fn wire_block_chain(version: Version, records: &mut [ObjectRecord]) {
    if version >= Version::R2004 {
        return;
    }
    let len = records.len();
    if len == 0 {
        return;
    }
    for i in 0..len {
        let prev = if i > 0 {
            Some(HandleRef {
                code: 4,
                value: records[i - 1].handle.value,
            })
        } else {
            None
        };
        let next = if i + 1 < len {
            Some(HandleRef {
                code: 4,
                value: records[i + 1].handle.value,
            })
        } else {
            None
        };
        records[i].handles.prev_entity = prev;
        records[i].handles.next_entity = next;
        // Enable the prev/next slot in the handle stream.
        records[i].common.nolinks = false;
    }
}
