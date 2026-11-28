# boardc Architecture & Invariants

`boardc` is a headless native Rust PCB compiler built around `viahop` (in-memory, no Specctra DSN/SES round-trip), `clipper2-rust` (exact 2D polygon booleans/offsetting), and `gerber-types`/`gerber_parser` (native RS-274X + independent verification).

## 1. Canonical Units & Coordinate Frame
- **Internal unit**: `pub type Nm = i64` (nanometers). `1 mm = 1_000_000 nm`, `1 um = 1_000 nm`.
- **Canonical Board Frame**: Right-handed mathematical 2D plane: `+X` right, `+Y` UP, CCW rotation positive in degrees (`0..360`). This matches Gerber RS-274X, Excellon, and `viahop`.
- **Local Footprint Frame**: Authored as viewed from the TOP (`F.Cu`), right-handed `+X` right, `+Y` UP, CCW positive.
- **Top placement (`Side::Top`)** at `(cx, cy)` with `rot_deg`:
  - `x = cx + lx * cos(rot) - ly * sin(rot)`
  - `y = cy + lx * sin(rot) + ly * cos(rot)`
- **Bottom placement (`Side::Bottom`)** at `(cx, cy)` with `rot_deg`:
  - Physically flips the component across the local X-axis (`ly' = -ly`, keeping `lx' = lx`, matching KiCad's flip convention when mapped into Y-up), then rotates CCW by `rot_deg`:
    - `x = cx + lx * cos(rot) + ly * sin(rot)`
    - `y = cy + lx * sin(rot) - ly * cos(rot)`
  - Pad layer remaps `Top <-> Bottom` (`F.Cu <-> B.Cu`, `F.Mask <-> B.Mask`, `F.Paste <-> B.Paste`, `F.Silk <-> B.Silk`, `F.CrtYd <-> B.CrtYd`).
- **KiCad Boundary Transform (`src/kicad_io.rs` and `src/kicad_export.rs` ONLY)**:
  - KiCad uses Y-down `(x_k, y_k)` and CCW-on-screen rotation `rot_k`.
  - Mapping KiCad -> Board (Y-up): `(x_b, y_b) = (x_k, -y_k)`. Under `y_b = -y_k`, a KiCad CCW screen rotation `rot_k` maps directly to Board CCW rotation `rot_b = rot_k`.
  - Check on KiCad F.Cu: local `(lx_k, ly_k)` -> local board `(lx_b, ly_b) = (lx_k, -ly_k)`. KiCad rotates `(1, 0)` by `+90°` to `(0, -1)` (up on screen), which in Board Y-up is `(0, +1)` = `+90°` CCW.
  - Check on KiCad `.kicad_pcb` placed footprints on `B.Cu`: inside `.kicad_pcb`, KiCad already stores each pad's local `(at lx_k ly_k)` in its flipped state, whereas `.kicad_mod` library files store unflipped `F.Cu` local coordinates.

## 2. Module Map
- `src/ir.rs`, `src/spec.rs`, `src/footprints.rs`: Canonical data types (`Nm`, `Pt`, `BBoxNm`, `Side`, `LayerDef`, `PadDef`, `PadShape`, `FootprintDef`, `Placement`, `Keepout`, `PourSpec`, `FilledPour`, `Track`, `Via`, `NetDef`, `NetClass`, `DesignRules`, `Constraints`, `BoardIr`), JSON/TOML `BoardSpec` serde model, and parametric IPC-7351 footprint generators (`0201`/`0402`/`0603`/`0805`, `BGA`, `QFN`, `SOT23`, `PinHeader`).
- `src/geom.rs`, `src/shapes.rs`, `src/poly_index.rs`: Exact 2D geometry on `Nm` / `f64` (point/segment/capsule/polygon distances, transforms, polygon area, point-in-polygon, rounded-rect/oval polygon approximation, flat-grid polygon indexing, Clipper2 conversion helpers).
- `src/kicad_io.rs`: Read-only `.kicad_mod` footprint loader and read-only `.kicad_pcb` + `.kicad_pro` importer (reusing `viahop::sexp`).
- `src/zones.rs`: `clipper2-rust` 2D polygon copper pour filler (priority order, edge clearance, keepouts, other-net antipads, solid vs thermal reliefs, dead-island pruning, sliver pruning) + copper-graph island/choke/neck-width analyzer.
- `src/drc.rs`: Exact continuous DRC engine over full `BoardIr` (clearances, hole-to-hole, drill-to-copper, edge clearance, annular ring, keepouts, courtyards, opens across tracks+vias+filled pours, electrical constraints) + spatial hotspot clusterer.
- `src/route.rs`: Direct in-memory `BoardIr -> viahop::Board` bridge (zero DSN/SES serialization), `viahop::route_board` invocation, incremental `--reroute-nets` support, and route writeback into `BoardIr`.
- `src/preflight.rs`: Sub-second pre-routing feasibility gate combining courtyard/keepout/edge/cap-proximity/zone-choke checks with `viahop::run_preflight`.
- `src/place.rs`: Deterministic placement legalizer (courtyards, keepouts, edge clearance, opposite-side BGA shadow, `cap_proximity` targets) and power-via / thermal-array solver (landing in target planes without sealing BGA rows or pinching pours).
- `src/pipeline.rs`: Staged compiler (`place -> power-via -> preflight -> route -> zones -> drc -> fab/svg`) with content-hash stage caching in `.boardc-cache/`.
- `src/fab.rs`: Native RS-274X/Gerber X2 (`gerber-types`), Excellon drills (separate PTH, NPTH, and blind/buried span `.drl` files), BOM CSV, and JLCPCB CPL CSV (explicit bottom mirroring + rotation offsets).
- `src/svg.rs`: Zero-dependency 2D SVG renderer for copper layers, board outline, pads/vias/pours, and DRC hotspot overlays.
- `src/kicad_export.rs`: Pure-file `.kicad_pcb` + `.kicad_pro` exporter (exposing `boardc export-kicad`).
- `src/verify.rs`: Independent pure-Rust verification engine: re-parses generated Gerbers via `gerber_parser` + Excellon parser, validates bounds, drills, polarity, and netlist connectivity/shorts with zero subprocesses.
- `src/main.rs`: CLI (`boardc build`, `preflight`, `place`, `route`, `zones`, `drc`, `fab`, `svg`, `verify`, `import-kicad`, `export-kicad`, `dump-spec`, `skill`).

## 3. Verification Philosophy (No Bug-Compatibility)
- Never require exact polygon vertex parity with KiCad or emulate KiCad quirks, and never shell out to external EDA binaries.
- Permanent verification inspects the *emitted Gerber and Excellon files themselves* via an independent parser (`gerber_parser` + Excellon parser), checking netlist connectivity, shorts, bounds, drills, and bottom-side alignment.
- KiCad `.kicad_pcb` import and export are pure file conversions (`import-kicad` / `export-kicad`), keeping external tools strictly opt-in outside `boardc`.
