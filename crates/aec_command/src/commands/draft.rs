//! Commands for the **Draft** scope — 2D CAD primitive authoring,
//! editing, sheet layout, and layer state.
//!
//! Each variant mirrors a `BridgeBackend.draft*` method on the renderer
//! side (`apps/desktop/electron/bridge.ts`). The renderer JSON-stringifies
//! the params bag and the napi layer in `crates/aec_bridge` deserialises
//! it into a [`crate::commands::CommandKind`] variant before routing
//! through [`crate::commands::Command::user`] →
//! [`crate::engine::CommandEngine::execute_persistent`]. The engine
//! produces one or more [`EntityDelta`]s that are committed inside a
//! SQLCipher transaction alongside the journal entry.
//!
//! All draft entities live in the project graph under the following kinds:
//!
//! * `primitive` — body is a [`DrawPrimitive`] (entity id + primitive)
//! * `sheet`     — body is an `aec_cad::sheets::Sheet`
//! * `layer`     — body is an `aec_cad::layers::Layer`
//!
//! Editing operations ([`EditTool`]) mutate `primitive` entities in
//! place; for the variants that produce *new* entities (copy / offset /
//! fillet / chamfer), the caller supplies the new ids explicitly so the
//! command stays deterministic and replayable.

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use aec_cad::editing::{
    ChamferTool, CopyTool, ExtendTool, FilletTool, MirrorTool, MoveTool, OffsetTool, RotateTool,
    ScaleTool, StretchTool, TrimTool,
};
use aec_cad::layers::{Layer, LayerColor, LayerLineweight};
use aec_cad::primitives::{Bbox, Primitive};
use aec_cad::sheets::{Margins, Orientation, PaperSize, Sheet, SheetViewport, TitleBlock};

use crate::commands::{EntityDelta, EntityRecord, ProjectGraph};
use crate::error::{CommandError, CommandResult};

/// The on-graph payload of every `kind == "primitive"` entity. We wrap
/// the typed [`Primitive`] enum together with its stable entity id so
/// the body is self-describing — the diff layer can recover the
/// primitive without reading the row's `kind` column twice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrawPrimitive {
    pub entity_id: EntityId,
    pub primitive: Primitive,
}

impl DrawPrimitive {
    /// Reject obviously-degenerate primitives so the renderer can't seed
    /// the graph with zero-length lines / negative-radius circles etc.
    /// Each primitive's geometric invariants are enforced here so a
    /// follow-up [`EditTool`] can rely on the source being well-formed.
    pub fn validate(&self) -> CommandResult<()> {
        fn invalid(reason: impl Into<String>) -> CommandError {
            CommandError::InvalidArguments {
                tool: "draft.draw_primitive".into(),
                reason: reason.into(),
            }
        }
        match &self.primitive {
            Primitive::Line(l) => {
                if l.start == l.end {
                    return Err(invalid("line start and end must differ"));
                }
            }
            Primitive::Polyline(p) => {
                if p.vertices.len() < 2 {
                    return Err(invalid("polyline requires at least two vertices"));
                }
            }
            Primitive::Arc(a) => {
                if !(a.radius.is_finite() && a.radius > 0.0) {
                    return Err(invalid("arc radius must be a finite positive value"));
                }
            }
            Primitive::Circle(c) => {
                if !(c.radius.is_finite() && c.radius > 0.0) {
                    return Err(invalid("circle radius must be a finite positive value"));
                }
            }
            Primitive::Ellipse(e) => {
                let mag = (e.major[0].powi(2) + e.major[1].powi(2)).sqrt();
                if !mag.is_finite() || mag <= 0.0 {
                    return Err(invalid(
                        "ellipse major axis must be a finite non-zero vector",
                    ));
                }
                if !e.ratio.is_finite() || e.ratio <= 0.0 {
                    return Err(invalid("ellipse ratio must be a finite positive value"));
                }
            }
            Primitive::Spline(s) => {
                if s.control_points.len() < 2 {
                    return Err(invalid("spline requires at least two control points"));
                }
            }
            Primitive::Hatch(h) => {
                if h.boundary_loops.is_empty() {
                    return Err(invalid("hatch requires at least one boundary loop"));
                }
            }
            Primitive::Text(t) => {
                if t.content.is_empty() {
                    return Err(invalid("text content must not be empty"));
                }
                if !(t.height.is_finite() && t.height > 0.0) {
                    return Err(invalid("text height must be a finite positive value"));
                }
            }
            Primitive::MText(t) => {
                if t.content.is_empty() {
                    return Err(invalid("mtext content must not be empty"));
                }
                if !(t.height.is_finite() && t.height > 0.0) {
                    return Err(invalid("mtext height must be a finite positive value"));
                }
            }
        }
        Ok(())
    }

    /// Build the create-delta. Caller is expected to have validated.
    pub fn to_delta(&self) -> EntityDelta {
        EntityDelta::Create {
            record: EntityRecord {
                id: self.entity_id.clone(),
                kind: "primitive".into(),
                body: serde_json::to_value(self).expect("DrawPrimitive serialises"),
                parent: None,
            },
        }
    }
}

/// A single editing gesture against one or more primitive entities.
///
/// Operations split into three buckets by output shape:
///
/// * **In-place affine** (move/rotate/scale/mirror/stretch): one
///   `Update` delta per source entity.
/// * **In-place geometric** (trim/extend/fillet-modifies/chamfer-
///   modifies): one or two `Update` deltas on the source entities.
/// * **Materialising** (copy/offset/fillet-arc/chamfer-bevel): one or
///   more `Create` deltas with caller-supplied ids.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOperation {
    /// Translate the listed entities by `(dx, dy)`.
    Move {
        entity_ids: Vec<EntityId>,
        dx: f64,
        dy: f64,
    },
    /// Clone the listed entities, translated by `(dx, dy)`. `new_ids`
    /// supplies the entity id for each clone — the lengths must match.
    Copy {
        entity_ids: Vec<EntityId>,
        new_ids: Vec<EntityId>,
        dx: f64,
        dy: f64,
    },
    /// Rotate around `center` by `angle_deg` (CCW positive).
    Rotate {
        entity_ids: Vec<EntityId>,
        center: [f64; 2],
        angle_deg: f64,
    },
    /// Uniform scale by `factor` about `center`. `factor` must be
    /// finite and non-zero.
    Scale {
        entity_ids: Vec<EntityId>,
        center: [f64; 2],
        factor: f64,
    },
    /// Reflect about the line `axis_start` → `axis_end`. When
    /// `keep_source` is false the source is overwritten in place;
    /// when true the source is preserved and clones are written to
    /// `new_ids` (must match `entity_ids` length).
    Mirror {
        entity_ids: Vec<EntityId>,
        axis_start: [f64; 2],
        axis_end: [f64; 2],
        #[serde(default)]
        keep_source: bool,
        #[serde(default)]
        new_ids: Vec<EntityId>,
    },
    /// Parallel offset of a line / polyline / arc / circle by
    /// `distance`. Creates a new entity at `new_id`.
    Offset {
        entity_id: EntityId,
        new_id: EntityId,
        distance: f64,
    },
    /// Clip a line at its intersection with `cutter_id`. `pick_point`
    /// selects which side of the intersection survives.
    Trim {
        entity_id: EntityId,
        cutter_id: EntityId,
        pick_point: [f64; 2],
    },
    /// Lengthen a line until it meets `boundary_id`. `pick_point` is
    /// the user-clicked screen point; the end closer to it is
    /// extended to the boundary.
    Extend {
        entity_id: EntityId,
        boundary_id: EntityId,
        pick_point: [f64; 2],
    },
    /// Insert a tangent arc of `radius` at the meeting `corner` of two
    /// lines. The two source lines are shortened to their tangent
    /// points and a new arc entity is written to `new_arc_id`.
    Fillet {
        entity_a_id: EntityId,
        entity_b_id: EntityId,
        new_arc_id: EntityId,
        corner: [f64; 2],
        radius: f64,
    },
    /// Replace the meeting `corner` with a straight bevel. The two
    /// source lines are clipped and a new bevel line is written to
    /// `new_bevel_id`.
    Chamfer {
        entity_a_id: EntityId,
        entity_b_id: EntityId,
        new_bevel_id: EntityId,
        corner: [f64; 2],
        setback_a: f64,
        setback_b: f64,
    },
    /// Move only the vertices inside the window `(window_min,
    /// window_max)` by `(dx, dy)`.
    Stretch {
        entity_ids: Vec<EntityId>,
        window_min: [f64; 2],
        window_max: [f64; 2],
        dx: f64,
        dy: f64,
    },
}

/// Wrapper carrying a tagged [`EditOperation`] so the renderer can
/// JSON-stringify a single object. (The CommandKind already tags by
/// tool, but the inner enum is independent.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditTool {
    #[serde(flatten)]
    pub operation: EditOperation,
}

impl EditTool {
    /// Cheap structural validation — performs only checks that don't
    /// require reading the graph (length parity, finite numbers,
    /// non-empty id lists). Per-entity kind validation happens in
    /// [`Self::to_deltas`] once we can look up records.
    pub fn validate(&self) -> CommandResult<()> {
        fn invalid(reason: impl Into<String>) -> CommandError {
            CommandError::InvalidArguments {
                tool: "draft.edit_tool".into(),
                reason: reason.into(),
            }
        }
        match &self.operation {
            EditOperation::Move {
                entity_ids, dx, dy, ..
            } => {
                if entity_ids.is_empty() {
                    return Err(invalid("move: entity_ids must not be empty"));
                }
                if !dx.is_finite() || !dy.is_finite() {
                    return Err(invalid("move: dx/dy must be finite"));
                }
            }
            EditOperation::Copy {
                entity_ids,
                new_ids,
                dx,
                dy,
            } => {
                if entity_ids.is_empty() {
                    return Err(invalid("copy: entity_ids must not be empty"));
                }
                if new_ids.len() != entity_ids.len() {
                    return Err(invalid("copy: new_ids length must equal entity_ids length"));
                }
                if !dx.is_finite() || !dy.is_finite() {
                    return Err(invalid("copy: dx/dy must be finite"));
                }
            }
            EditOperation::Rotate {
                entity_ids,
                center,
                angle_deg,
            } => {
                if entity_ids.is_empty() {
                    return Err(invalid("rotate: entity_ids must not be empty"));
                }
                if !angle_deg.is_finite() {
                    return Err(invalid("rotate: angle_deg must be finite"));
                }
                if !center[0].is_finite() || !center[1].is_finite() {
                    return Err(invalid("rotate: center must be finite"));
                }
            }
            EditOperation::Scale {
                entity_ids,
                center,
                factor,
            } => {
                if entity_ids.is_empty() {
                    return Err(invalid("scale: entity_ids must not be empty"));
                }
                if !factor.is_finite() || *factor == 0.0 {
                    return Err(invalid("scale: factor must be finite and non-zero"));
                }
                if !center[0].is_finite() || !center[1].is_finite() {
                    return Err(invalid("scale: center must be finite"));
                }
            }
            EditOperation::Mirror {
                entity_ids,
                axis_start,
                axis_end,
                keep_source,
                new_ids,
            } => {
                if entity_ids.is_empty() {
                    return Err(invalid("mirror: entity_ids must not be empty"));
                }
                if axis_start == axis_end {
                    return Err(invalid("mirror: axis_start and axis_end must differ"));
                }
                if *keep_source && new_ids.len() != entity_ids.len() {
                    return Err(invalid(
                        "mirror: new_ids length must equal entity_ids length when keep_source is true",
                    ));
                }
                if !keep_source && !new_ids.is_empty() {
                    return Err(invalid(
                        "mirror: new_ids must be empty when keep_source is false",
                    ));
                }
            }
            EditOperation::Offset { distance, .. } => {
                if !distance.is_finite() || *distance == 0.0 {
                    return Err(invalid("offset: distance must be finite and non-zero"));
                }
            }
            EditOperation::Trim { pick_point, .. } => {
                if !pick_point[0].is_finite() || !pick_point[1].is_finite() {
                    return Err(invalid("trim: pick_point must be finite"));
                }
            }
            EditOperation::Extend { pick_point, .. } => {
                if !pick_point[0].is_finite() || !pick_point[1].is_finite() {
                    return Err(invalid("extend: pick_point must be finite"));
                }
            }
            EditOperation::Fillet { radius, .. } => {
                if !radius.is_finite() || *radius <= 0.0 {
                    return Err(invalid("fillet: radius must be finite and > 0"));
                }
            }
            EditOperation::Chamfer {
                setback_a,
                setback_b,
                ..
            } => {
                if !setback_a.is_finite() || *setback_a <= 0.0 {
                    return Err(invalid("chamfer: setback_a must be finite and > 0"));
                }
                if !setback_b.is_finite() || *setback_b <= 0.0 {
                    return Err(invalid("chamfer: setback_b must be finite and > 0"));
                }
            }
            EditOperation::Stretch {
                entity_ids,
                window_min,
                window_max,
                dx,
                dy,
            } => {
                if entity_ids.is_empty() {
                    return Err(invalid("stretch: entity_ids must not be empty"));
                }
                if !(window_min[0].is_finite()
                    && window_min[1].is_finite()
                    && window_max[0].is_finite()
                    && window_max[1].is_finite())
                {
                    return Err(invalid("stretch: window must be finite"));
                }
                if !dx.is_finite() || !dy.is_finite() {
                    return Err(invalid("stretch: dx/dy must be finite"));
                }
            }
        }
        Ok(())
    }

    /// Resolve the operation against `graph` and emit deltas. Looks up
    /// every referenced entity by id, asserts kind=="primitive",
    /// deserialises the body, transforms via `aec_cad::editing`, and
    /// re-serialises into Update / Create deltas.
    pub fn to_deltas(&self, graph: &ProjectGraph) -> CommandResult<Vec<EntityDelta>> {
        match &self.operation {
            EditOperation::Move { entity_ids, dx, dy } => {
                let mut out = Vec::with_capacity(entity_ids.len());
                for id in entity_ids {
                    let (record, mut dp) = load_primitive(graph, id)?;
                    dp.primitive = MoveTool::apply(&dp.primitive, [*dx, *dy]);
                    out.push(write_primitive_update(record, dp)?);
                }
                Ok(out)
            }
            EditOperation::Copy {
                entity_ids,
                new_ids,
                dx,
                dy,
            } => {
                let mut out = Vec::with_capacity(entity_ids.len());
                for (id, new_id) in entity_ids.iter().zip(new_ids.iter()) {
                    let (_record, dp) = load_primitive(graph, id)?;
                    if graph.contains(new_id) {
                        return Err(CommandError::EntityAlreadyExists(new_id.to_string()));
                    }
                    let clone = DrawPrimitive {
                        entity_id: new_id.clone(),
                        primitive: CopyTool::apply(&dp.primitive, [*dx, *dy]),
                    };
                    out.push(clone.to_delta());
                }
                Ok(out)
            }
            EditOperation::Rotate {
                entity_ids,
                center,
                angle_deg,
            } => {
                let mut out = Vec::with_capacity(entity_ids.len());
                for id in entity_ids {
                    let (record, mut dp) = load_primitive(graph, id)?;
                    dp.primitive = RotateTool::apply(&dp.primitive, *center, *angle_deg);
                    out.push(write_primitive_update(record, dp)?);
                }
                Ok(out)
            }
            EditOperation::Scale {
                entity_ids,
                center,
                factor,
            } => {
                let mut out = Vec::with_capacity(entity_ids.len());
                for id in entity_ids {
                    let (record, mut dp) = load_primitive(graph, id)?;
                    dp.primitive = ScaleTool::apply(&dp.primitive, *center, *factor);
                    out.push(write_primitive_update(record, dp)?);
                }
                Ok(out)
            }
            EditOperation::Mirror {
                entity_ids,
                axis_start,
                axis_end,
                keep_source,
                new_ids,
            } => {
                let mut out = Vec::with_capacity(entity_ids.len());
                for (i, id) in entity_ids.iter().enumerate() {
                    let (record, mut dp) = load_primitive(graph, id)?;
                    let mirrored = MirrorTool::apply(&dp.primitive, *axis_start, *axis_end);
                    if *keep_source {
                        let new_id = &new_ids[i];
                        if graph.contains(new_id) {
                            return Err(CommandError::EntityAlreadyExists(new_id.to_string()));
                        }
                        out.push(
                            DrawPrimitive {
                                entity_id: new_id.clone(),
                                primitive: mirrored,
                            }
                            .to_delta(),
                        );
                    } else {
                        dp.primitive = mirrored;
                        out.push(write_primitive_update(record, dp)?);
                    }
                }
                Ok(out)
            }
            EditOperation::Offset {
                entity_id,
                new_id,
                distance,
            } => {
                let (_record, dp) = load_primitive(graph, entity_id)?;
                if graph.contains(new_id) {
                    return Err(CommandError::EntityAlreadyExists(new_id.to_string()));
                }
                let offset_prim = match &dp.primitive {
                    Primitive::Line(l) => Primitive::Line(OffsetTool::offset_line(l, *distance)),
                    Primitive::Circle(c) => Primitive::Circle(
                        OffsetTool::offset_circle(c, *distance).ok_or_else(|| {
                            CommandError::InvalidArguments {
                                tool: "draft.edit_tool".into(),
                                reason: "offset: resulting circle would have non-positive radius"
                                    .into(),
                            }
                        })?,
                    ),
                    Primitive::Arc(a) => {
                        Primitive::Arc(OffsetTool::offset_arc(a, *distance).ok_or_else(|| {
                            CommandError::InvalidArguments {
                                tool: "draft.edit_tool".into(),
                                reason: "offset: resulting arc would have non-positive radius"
                                    .into(),
                            }
                        })?)
                    }
                    Primitive::Polyline(p) => {
                        Primitive::Polyline(OffsetTool::offset_polyline(p, *distance))
                    }
                    other => {
                        return Err(CommandError::InvalidArguments {
                            tool: "draft.edit_tool".into(),
                            reason: format!(
                                "offset: unsupported primitive kind for offset: {}",
                                primitive_kind_name(other)
                            ),
                        })
                    }
                };
                Ok(vec![DrawPrimitive {
                    entity_id: new_id.clone(),
                    primitive: offset_prim,
                }
                .to_delta()])
            }
            EditOperation::Trim {
                entity_id,
                cutter_id,
                pick_point,
            } => {
                let (src_record, src_dp) = load_primitive(graph, entity_id)?;
                let (_, cut_dp) = load_primitive(graph, cutter_id)?;
                let trimmed = match (&src_dp.primitive, &cut_dp.primitive) {
                    (Primitive::Line(line), Primitive::Line(cutter)) => {
                        TrimTool::trim_line_at_line(line, cutter, *pick_point).ok_or_else(|| {
                            CommandError::InvalidArguments {
                                tool: "draft.edit_tool".into(),
                                reason:
                                    "trim: source and cutter lines do not intersect on segments"
                                        .into(),
                            }
                        })?
                    }
                    (Primitive::Line(line), Primitive::Circle(circle)) => {
                        TrimTool::trim_line_at_circle(line, circle, *pick_point).ok_or_else(
                            || CommandError::InvalidArguments {
                                tool: "draft.edit_tool".into(),
                                reason: "trim: source line does not intersect cutter circle".into(),
                            },
                        )?
                    }
                    _ => {
                        return Err(CommandError::InvalidArguments {
                            tool: "draft.edit_tool".into(),
                            reason:
                                "trim: only line-vs-line and line-vs-circle trimming is supported"
                                    .into(),
                        })
                    }
                };
                let new_dp = DrawPrimitive {
                    entity_id: src_dp.entity_id.clone(),
                    primitive: Primitive::Line(trimmed),
                };
                Ok(vec![write_primitive_update(src_record, new_dp)?])
            }
            EditOperation::Extend {
                entity_id,
                boundary_id,
                pick_point,
            } => {
                let (src_record, src_dp) = load_primitive(graph, entity_id)?;
                let (_, bnd_dp) = load_primitive(graph, boundary_id)?;
                let line = match &src_dp.primitive {
                    Primitive::Line(l) => l.clone(),
                    _ => {
                        return Err(CommandError::InvalidArguments {
                            tool: "draft.edit_tool".into(),
                            reason: "extend: only line entities can be extended".into(),
                        })
                    }
                };
                let extended_line = match &bnd_dp.primitive {
                    Primitive::Line(boundary) => {
                        ExtendTool::extend_to_line(&line, boundary, *pick_point)
                    }
                    Primitive::Circle(circle) => {
                        ExtendTool::extend_to_circle(&line, circle, *pick_point)
                    }
                    _ => {
                        return Err(CommandError::InvalidArguments {
                            tool: "draft.edit_tool".into(),
                            reason: "extend: boundary must be a line or circle".into(),
                        })
                    }
                }
                .ok_or_else(|| CommandError::InvalidArguments {
                    tool: "draft.edit_tool".into(),
                    reason: "extend: source line does not project onto boundary".into(),
                })?;
                let new_dp = DrawPrimitive {
                    entity_id: src_dp.entity_id.clone(),
                    primitive: Primitive::Line(extended_line),
                };
                Ok(vec![write_primitive_update(src_record, new_dp)?])
            }
            EditOperation::Fillet {
                entity_a_id,
                entity_b_id,
                new_arc_id,
                corner,
                radius,
            } => {
                let (a_record, a_dp) = load_primitive(graph, entity_a_id)?;
                let (b_record, b_dp) = load_primitive(graph, entity_b_id)?;
                if graph.contains(new_arc_id) {
                    return Err(CommandError::EntityAlreadyExists(new_arc_id.to_string()));
                }
                let (line_a, line_b) = match (&a_dp.primitive, &b_dp.primitive) {
                    (Primitive::Line(a), Primitive::Line(b)) => (a.clone(), b.clone()),
                    _ => {
                        return Err(CommandError::InvalidArguments {
                            tool: "draft.edit_tool".into(),
                            reason: "fillet: both source entities must be lines".into(),
                        })
                    }
                };
                let result = FilletTool::fillet_lines(&line_a, &line_b, *corner, *radius)
                    .ok_or_else(|| CommandError::InvalidArguments {
                        tool: "draft.edit_tool".into(),
                        reason: "fillet: lines do not form a fillet-able corner".into(),
                    })?;
                let updated_a = DrawPrimitive {
                    entity_id: a_dp.entity_id.clone(),
                    primitive: Primitive::Line(result.line_a),
                };
                let updated_b = DrawPrimitive {
                    entity_id: b_dp.entity_id.clone(),
                    primitive: Primitive::Line(result.line_b),
                };
                let new_arc = DrawPrimitive {
                    entity_id: new_arc_id.clone(),
                    primitive: Primitive::Arc(result.arc),
                };
                Ok(vec![
                    write_primitive_update(a_record, updated_a)?,
                    write_primitive_update(b_record, updated_b)?,
                    new_arc.to_delta(),
                ])
            }
            EditOperation::Chamfer {
                entity_a_id,
                entity_b_id,
                new_bevel_id,
                corner,
                setback_a,
                setback_b,
            } => {
                let (a_record, a_dp) = load_primitive(graph, entity_a_id)?;
                let (b_record, b_dp) = load_primitive(graph, entity_b_id)?;
                if graph.contains(new_bevel_id) {
                    return Err(CommandError::EntityAlreadyExists(new_bevel_id.to_string()));
                }
                let (line_a, line_b) = match (&a_dp.primitive, &b_dp.primitive) {
                    (Primitive::Line(a), Primitive::Line(b)) => (a.clone(), b.clone()),
                    _ => {
                        return Err(CommandError::InvalidArguments {
                            tool: "draft.edit_tool".into(),
                            reason: "chamfer: both source entities must be lines".into(),
                        })
                    }
                };
                let result =
                    ChamferTool::chamfer_lines(&line_a, &line_b, *corner, *setback_a, *setback_b)
                        .ok_or_else(|| CommandError::InvalidArguments {
                        tool: "draft.edit_tool".into(),
                        reason: "chamfer: lines do not form a chamfer-able corner".into(),
                    })?;
                let updated_a = DrawPrimitive {
                    entity_id: a_dp.entity_id.clone(),
                    primitive: Primitive::Line(result.line_a),
                };
                let updated_b = DrawPrimitive {
                    entity_id: b_dp.entity_id.clone(),
                    primitive: Primitive::Line(result.line_b),
                };
                let new_bevel = DrawPrimitive {
                    entity_id: new_bevel_id.clone(),
                    primitive: Primitive::Line(result.bevel),
                };
                Ok(vec![
                    write_primitive_update(a_record, updated_a)?,
                    write_primitive_update(b_record, updated_b)?,
                    new_bevel.to_delta(),
                ])
            }
            EditOperation::Stretch {
                entity_ids,
                window_min,
                window_max,
                dx,
                dy,
            } => {
                let bbox = Bbox {
                    min: *window_min,
                    max: *window_max,
                };
                let mut out = Vec::with_capacity(entity_ids.len());
                for id in entity_ids {
                    let (record, mut dp) = load_primitive(graph, id)?;
                    dp.primitive = StretchTool::apply(&dp.primitive, &bbox, [*dx, *dy]);
                    out.push(write_primitive_update(record, dp)?);
                }
                Ok(out)
            }
        }
    }
}

fn primitive_kind_name(p: &Primitive) -> &'static str {
    match p {
        Primitive::Line(_) => "line",
        Primitive::Polyline(_) => "polyline",
        Primitive::Arc(_) => "arc",
        Primitive::Circle(_) => "circle",
        Primitive::Ellipse(_) => "ellipse",
        Primitive::Spline(_) => "spline",
        Primitive::Hatch(_) => "hatch",
        Primitive::Text(_) => "text",
        Primitive::MText(_) => "mtext",
    }
}

fn load_primitive(
    graph: &ProjectGraph,
    id: &EntityId,
) -> CommandResult<(EntityRecord, DrawPrimitive)> {
    let record = graph
        .get(id)
        .ok_or_else(|| CommandError::EntityNotFound(id.to_string()))?
        .clone();
    if record.kind != "primitive" {
        return Err(CommandError::InvalidArguments {
            tool: "draft.edit_tool".into(),
            reason: format!(
                "entity {} is kind={}, expected 'primitive'",
                id, record.kind
            ),
        });
    }
    let dp: DrawPrimitive = serde_json::from_value(record.body.clone()).map_err(|e| {
        CommandError::JournalCorrupt(format!("primitive entity {id} has invalid body: {e}"))
    })?;
    Ok((record, dp))
}

fn write_primitive_update(
    record: EntityRecord,
    new_dp: DrawPrimitive,
) -> CommandResult<EntityDelta> {
    let after = serde_json::to_value(&new_dp)?;
    Ok(EntityDelta::Update {
        id: record.id,
        before: record.body,
        after,
    })
}

// Light helpers around `OffsetTool`/`ExtendTool` whose underlying impl
// returns options. The wrappers above use these to map to the
// engine-facing `CommandError::InvalidArguments` shape.

/// Sheet creation command. Builds an `aec_cad::sheets::Sheet` from the
/// renderer-supplied parts and writes a single `kind == "sheet"`
/// entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateSheet {
    pub entity_id: EntityId,
    pub name: String,
    pub paper: PaperSize,
    #[serde(default = "default_orientation")]
    pub orientation: Orientation,
    #[serde(default)]
    pub margins: Margins,
    #[serde(default)]
    pub title_block: Option<TitleBlock>,
    #[serde(default)]
    pub viewports: Vec<SheetViewport>,
}

fn default_orientation() -> Orientation {
    Orientation::Landscape
}

impl CreateSheet {
    pub fn validate(&self) -> CommandResult<()> {
        if self.name.trim().is_empty() {
            return Err(CommandError::InvalidArguments {
                tool: "draft.create_sheet".into(),
                reason: "sheet name must not be empty".into(),
            });
        }
        let (w, h) = self.paper.dimensions_mm();
        if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
            return Err(CommandError::InvalidArguments {
                tool: "draft.create_sheet".into(),
                reason: "sheet paper dimensions must be finite and positive".into(),
            });
        }
        Ok(())
    }

    pub fn to_delta(&self) -> CommandResult<EntityDelta> {
        let sheet = Sheet {
            name: self.name.clone(),
            paper: self.paper,
            orientation: self.orientation,
            margins: self.margins.clone(),
            title_block: self.title_block.clone(),
            viewports: self.viewports.clone(),
        };
        let body = serde_json::to_value(&sheet)?;
        Ok(EntityDelta::Create {
            record: EntityRecord {
                id: self.entity_id.clone(),
                kind: "sheet".into(),
                body,
                parent: None,
            },
        })
    }
}

/// Layer-state upsert.
///
/// If an entity with `entity_id` already exists with `kind == "layer"`,
/// the existing layer body is merged with the supplied overrides
/// (None fields preserve current state) and an `EntityDelta::Update`
/// is emitted. Otherwise a fresh `Layer` is created (None fields fall
/// back to `Layer::new` defaults) and an `EntityDelta::Create` is
/// emitted.
///
/// This is the long-term "comprehensive" wiring for AutoCAD-style
/// layer-state mutation (visibility, freeze, lock, color, lineweight,
/// linetype) — the renderer can call it with any subset of the fields
/// to flip individual properties without round-tripping the whole row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetLayerState {
    pub entity_id: EntityId,
    pub name: String,
    #[serde(default)]
    pub color: Option<LayerColor>,
    #[serde(default)]
    pub linetype: Option<String>,
    #[serde(default)]
    pub lineweight: Option<LayerLineweight>,
    #[serde(default)]
    pub on: Option<bool>,
    #[serde(default)]
    pub frozen: Option<bool>,
    #[serde(default)]
    pub locked: Option<bool>,
    #[serde(default)]
    pub plottable: Option<bool>,
    #[serde(default)]
    pub description: Option<String>,
}

impl SetLayerState {
    pub fn validate(&self) -> CommandResult<()> {
        // Construct a throwaway Layer to leverage the existing name
        // sanity check (rejects `<>/"`-style characters etc.).
        Layer::new(self.name.clone()).map_err(|e| CommandError::InvalidArguments {
            tool: "draft.set_layer_state".into(),
            reason: e.to_string(),
        })?;
        Ok(())
    }

    pub fn to_delta(&self, graph: &ProjectGraph) -> CommandResult<EntityDelta> {
        match graph.get(&self.entity_id) {
            Some(record) if record.kind == "layer" => {
                let mut layer: Layer =
                    serde_json::from_value(record.body.clone()).map_err(|e| {
                        CommandError::JournalCorrupt(format!(
                            "layer entity {} has invalid body: {e}",
                            self.entity_id
                        ))
                    })?;
                // Renaming a layer is allowed but the name must still be valid.
                if layer.name != self.name {
                    let _probe = Layer::new(self.name.clone()).map_err(|e| {
                        CommandError::InvalidArguments {
                            tool: "draft.set_layer_state".into(),
                            reason: e.to_string(),
                        }
                    })?;
                    layer.name.clone_from(&self.name);
                }
                if let Some(c) = self.color {
                    layer.color = c;
                }
                if let Some(lt) = &self.linetype {
                    layer.linetype.clone_from(lt);
                }
                if let Some(lw) = self.lineweight {
                    layer.lineweight = lw;
                }
                if let Some(on) = self.on {
                    layer.on = on;
                }
                if let Some(frozen) = self.frozen {
                    layer.frozen = frozen;
                }
                if let Some(locked) = self.locked {
                    layer.locked = locked;
                }
                if let Some(plottable) = self.plottable {
                    layer.plottable = plottable;
                }
                if let Some(desc) = &self.description {
                    layer.description = Some(desc.clone());
                }
                let after = serde_json::to_value(&layer)?;
                Ok(EntityDelta::Update {
                    id: self.entity_id.clone(),
                    before: record.body.clone(),
                    after,
                })
            }
            Some(record) => Err(CommandError::InvalidArguments {
                tool: "draft.set_layer_state".into(),
                reason: format!(
                    "entity {} is kind={}, expected 'layer'",
                    self.entity_id, record.kind
                ),
            }),
            None => {
                let mut layer =
                    Layer::new(self.name.clone()).map_err(|e| CommandError::InvalidArguments {
                        tool: "draft.set_layer_state".into(),
                        reason: e.to_string(),
                    })?;
                if let Some(c) = self.color {
                    layer.color = c;
                }
                if let Some(lt) = &self.linetype {
                    layer.linetype.clone_from(lt);
                }
                if let Some(lw) = self.lineweight {
                    layer.lineweight = lw;
                }
                if let Some(on) = self.on {
                    layer.on = on;
                }
                if let Some(frozen) = self.frozen {
                    layer.frozen = frozen;
                }
                if let Some(locked) = self.locked {
                    layer.locked = locked;
                }
                if let Some(plottable) = self.plottable {
                    layer.plottable = plottable;
                }
                if let Some(desc) = &self.description {
                    layer.description = Some(desc.clone());
                }
                Ok(EntityDelta::Create {
                    record: EntityRecord {
                        id: self.entity_id.clone(),
                        kind: "layer".into(),
                        body: serde_json::to_value(&layer)?,
                        parent: None,
                    },
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aec_cad::primitives::{Circle, Line};

    fn make_id() -> EntityId {
        EntityId::new()
    }

    #[test]
    fn draw_primitive_validates_zero_length_line() {
        let cmd = DrawPrimitive {
            entity_id: make_id(),
            primitive: Primitive::Line(Line::new("0", [0.0, 0.0], [0.0, 0.0])),
        };
        let err = cmd.validate().unwrap_err();
        assert!(err.to_string().contains("must differ"));
    }

    #[test]
    fn draw_primitive_validates_negative_radius_circle() {
        let cmd = DrawPrimitive {
            entity_id: make_id(),
            primitive: Primitive::Circle(Circle::new("0", [0.0, 0.0], -1.0)),
        };
        let err = cmd.validate().unwrap_err();
        assert!(err.to_string().contains("circle radius"));
    }

    #[test]
    fn draw_primitive_creates_entity_record() {
        let id = make_id();
        let cmd = DrawPrimitive {
            entity_id: id.clone(),
            primitive: Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0])),
        };
        cmd.validate().unwrap();
        let EntityDelta::Create { record } = cmd.to_delta() else {
            panic!("expected create");
        };
        assert_eq!(record.id, id);
        assert_eq!(record.kind, "primitive");
        let parsed: DrawPrimitive = serde_json::from_value(record.body).unwrap();
        assert_eq!(parsed, cmd);
    }

    #[test]
    fn edit_tool_move_translates_line_endpoints() {
        let mut g = ProjectGraph::new();
        let id = make_id();
        let dp = DrawPrimitive {
            entity_id: id.clone(),
            primitive: Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0])),
        };
        g.apply(&dp.to_delta()).unwrap();
        let edit = EditTool {
            operation: EditOperation::Move {
                entity_ids: vec![id.clone()],
                dx: 5.0,
                dy: 2.0,
            },
        };
        edit.validate().unwrap();
        let deltas = edit.to_deltas(&g).unwrap();
        assert_eq!(deltas.len(), 1);
        let EntityDelta::Update { after, .. } = &deltas[0] else {
            panic!("expected update");
        };
        let dp2: DrawPrimitive = serde_json::from_value(after.clone()).unwrap();
        if let Primitive::Line(l) = dp2.primitive {
            assert_eq!(l.start, [5.0, 2.0]);
            assert_eq!(l.end, [15.0, 2.0]);
        } else {
            panic!("expected line");
        }
    }

    #[test]
    fn edit_tool_copy_requires_matching_lengths() {
        let edit = EditTool {
            operation: EditOperation::Copy {
                entity_ids: vec![make_id(), make_id()],
                new_ids: vec![make_id()],
                dx: 1.0,
                dy: 0.0,
            },
        };
        let err = edit.validate().unwrap_err();
        assert!(err.to_string().contains("length"));
    }

    #[test]
    fn edit_tool_offset_circle_produces_concentric() {
        let mut g = ProjectGraph::new();
        let id = make_id();
        let dp = DrawPrimitive {
            entity_id: id.clone(),
            primitive: Primitive::Circle(Circle::new("0", [0.0, 0.0], 5.0)),
        };
        g.apply(&dp.to_delta()).unwrap();
        let new_id = make_id();
        let edit = EditTool {
            operation: EditOperation::Offset {
                entity_id: id,
                new_id: new_id.clone(),
                distance: 2.0,
            },
        };
        edit.validate().unwrap();
        let deltas = edit.to_deltas(&g).unwrap();
        assert_eq!(deltas.len(), 1);
        let EntityDelta::Create { record } = &deltas[0] else {
            panic!("expected create");
        };
        assert_eq!(record.id, new_id);
        let dp2: DrawPrimitive = serde_json::from_value(record.body.clone()).unwrap();
        if let Primitive::Circle(c) = dp2.primitive {
            assert!((c.radius - 7.0).abs() < 1e-9);
        } else {
            panic!("expected circle");
        }
    }

    #[test]
    fn edit_tool_fillet_emits_three_deltas() {
        let mut g = ProjectGraph::new();
        let a_id = make_id();
        let b_id = make_id();
        let a = DrawPrimitive {
            entity_id: a_id.clone(),
            primitive: Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0])),
        };
        let b = DrawPrimitive {
            entity_id: b_id.clone(),
            primitive: Primitive::Line(Line::new("0", [10.0, 0.0], [10.0, 10.0])),
        };
        g.apply(&a.to_delta()).unwrap();
        g.apply(&b.to_delta()).unwrap();
        let arc_id = make_id();
        let edit = EditTool {
            operation: EditOperation::Fillet {
                entity_a_id: a_id,
                entity_b_id: b_id,
                new_arc_id: arc_id.clone(),
                corner: [10.0, 0.0],
                radius: 2.0,
            },
        };
        edit.validate().unwrap();
        let deltas = edit.to_deltas(&g).unwrap();
        assert_eq!(deltas.len(), 3);
        // last delta should be the new arc
        match &deltas[2] {
            EntityDelta::Create { record } => {
                assert_eq!(record.id, arc_id);
                assert_eq!(record.kind, "primitive");
                let dp: DrawPrimitive = serde_json::from_value(record.body.clone()).unwrap();
                assert!(matches!(dp.primitive, Primitive::Arc(_)));
            }
            _ => panic!("expected arc create"),
        }
    }

    #[test]
    fn create_sheet_emits_create_delta() {
        let id = make_id();
        let cmd = CreateSheet {
            entity_id: id.clone(),
            name: "A-101".into(),
            paper: PaperSize::IsoA3,
            orientation: Orientation::Landscape,
            margins: Margins::default(),
            title_block: None,
            viewports: vec![],
        };
        cmd.validate().unwrap();
        let EntityDelta::Create { record } = cmd.to_delta().unwrap() else {
            panic!("expected create");
        };
        assert_eq!(record.kind, "sheet");
        let sheet: Sheet = serde_json::from_value(record.body).unwrap();
        assert_eq!(sheet.name, "A-101");
        assert_eq!(sheet.paper, PaperSize::IsoA3);
    }

    #[test]
    fn create_sheet_rejects_empty_name() {
        let cmd = CreateSheet {
            entity_id: make_id(),
            name: "   ".into(),
            paper: PaperSize::IsoA3,
            orientation: Orientation::Landscape,
            margins: Margins::default(),
            title_block: None,
            viewports: vec![],
        };
        assert!(cmd.validate().is_err());
    }

    #[test]
    fn set_layer_state_creates_new_layer() {
        let g = ProjectGraph::new();
        let id = make_id();
        let cmd = SetLayerState {
            entity_id: id.clone(),
            name: "WALLS".into(),
            color: Some(LayerColor(2)),
            linetype: Some("DASHED".into()),
            lineweight: Some(LayerLineweight::from_mm(0.35)),
            on: Some(true),
            frozen: Some(false),
            locked: Some(false),
            plottable: Some(true),
            description: None,
        };
        cmd.validate().unwrap();
        let EntityDelta::Create { record } = cmd.to_delta(&g).unwrap() else {
            panic!("expected create");
        };
        assert_eq!(record.kind, "layer");
        let layer: Layer = serde_json::from_value(record.body).unwrap();
        assert_eq!(layer.name, "WALLS");
        assert_eq!(layer.color, LayerColor(2));
        assert_eq!(layer.linetype, "DASHED");
    }

    #[test]
    fn set_layer_state_updates_existing_layer() {
        let mut g = ProjectGraph::new();
        let id = make_id();
        // Seed via SetLayerState create.
        let seed = SetLayerState {
            entity_id: id.clone(),
            name: "WALLS".into(),
            color: Some(LayerColor::WHITE),
            linetype: Some("CONTINUOUS".into()),
            lineweight: None,
            on: Some(true),
            frozen: Some(false),
            locked: Some(false),
            plottable: Some(true),
            description: None,
        };
        let seed_delta = seed.to_delta(&g).unwrap();
        g.apply(&seed_delta).unwrap();
        // Flip frozen on the existing layer.
        let flip = SetLayerState {
            entity_id: id,
            name: "WALLS".into(),
            color: None,
            linetype: None,
            lineweight: None,
            on: None,
            frozen: Some(true),
            locked: None,
            plottable: None,
            description: None,
        };
        let EntityDelta::Update { after, before, .. } = flip.to_delta(&g).unwrap() else {
            panic!("expected update");
        };
        let new: Layer = serde_json::from_value(after).unwrap();
        let old: Layer = serde_json::from_value(before).unwrap();
        assert!(!old.frozen);
        assert!(new.frozen);
        assert_eq!(new.name, "WALLS");
    }

    #[test]
    fn set_layer_state_rejects_invalid_name() {
        let g = ProjectGraph::new();
        let cmd = SetLayerState {
            entity_id: make_id(),
            name: "BAD/NAME".into(),
            color: None,
            linetype: None,
            lineweight: None,
            on: None,
            frozen: None,
            locked: None,
            plottable: None,
            description: None,
        };
        let err = cmd.validate().unwrap_err();
        assert!(err.to_string().contains("InvalidLayerName") || err.to_string().contains("BAD"));
        // Sanity: to_delta is gated by validate at the engine level; this
        // call exists to document the failure mode.
        let _ = g;
    }

    #[test]
    fn mirror_keep_source_emits_creates() {
        let mut g = ProjectGraph::new();
        let id = make_id();
        let dp = DrawPrimitive {
            entity_id: id.clone(),
            primitive: Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0])),
        };
        g.apply(&dp.to_delta()).unwrap();
        let new_id = make_id();
        let edit = EditTool {
            operation: EditOperation::Mirror {
                entity_ids: vec![id],
                axis_start: [0.0, 0.0],
                axis_end: [0.0, 10.0],
                keep_source: true,
                new_ids: vec![new_id.clone()],
            },
        };
        edit.validate().unwrap();
        let deltas = edit.to_deltas(&g).unwrap();
        assert_eq!(deltas.len(), 1);
        let EntityDelta::Create { record } = &deltas[0] else {
            panic!("expected create");
        };
        assert_eq!(record.id, new_id);
        // The original (10,0) endpoint should be reflected across the y-axis to (-10,0).
        let dp2: DrawPrimitive = serde_json::from_value(record.body.clone()).unwrap();
        if let Primitive::Line(l) = dp2.primitive {
            assert!((l.end[0] - -10.0).abs() < 1e-9);
        } else {
            panic!("expected line");
        }
    }

    // Trim/Extend/Stretch tests live in the engine integration tests so
    // we don't duplicate the underlying tool's coverage here. These unit
    // tests focus on the routing + delta-shape side.
}
