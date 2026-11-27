# boardc

Headless native Rust PCB compiler built around [`viahop`](https://github.com/owahltinez/viahop) (in-memory autorouting with zero Specctra DSN/SES round-trips), `clipper2-rust` (exact 2D polygon zone filling and choke analysis), and `gerber-types` / `gerber_parser` (native RS-274X Gerber, Excellon drill, BOM/CPL export, and independent Gerber-level verification).

## Install & Build

```bash
# Install from crates.io
cargo install boardc

# Install the embedded agent skill into ~/.agents/skills/boardc/SKILL.md
boardc skill install

# Or build from source
cargo build --release
cargo test --release
cargo clippy --all-targets -- -D warnings
```

Prebuilt CLI binaries are also available on [GitHub Releases](https://github.com/owahltinez/boardc/releases).

For coding agents, run `boardc skill install` (or read [SKILL.md](SKILL.md) directly).

## Architecture

- **Canonical Coordinates & Units**: Integer nanometers (`Nm = i64`, `1 mm = 1_000_000 nm`) in a right-handed 2D frame (`+X` right, `+Y` up, CCW rotation positive). Bottom-side placements flip across local `X` (`ly' = -ly`) before rotating CCW by `rot_deg` and remapping `Top <-> Bottom` layers. KiCad Y-down conversion is isolated strictly at the import (`src/kicad_io.rs`) and export (`src/kicad_export.rs`) file boundaries.
- **Core IR & Parametric Footprints** (`src/ir.rs`, `src/spec.rs`, `src/footprints.rs`, `src/geom.rs`, `src/shapes.rs`, `src/poly_index.rs`): JSON/TOML `BoardSpec` and canonical `BoardIr`, IPC-7351 parametric footprints (`0201`/`0402`/`0603`/`0805`, `BGA`, `QFN`, `SOT23`, `PinHeader`), and exact continuous 2D shape distance primitives with flat-grid polygon indexing.
- **Placement & Power Vias** (`src/place.rs`): Deterministic placement legalizer (courtyards, keepouts, edge clearance, BGA shadow rules, decoupling-capacitor proximity) and power-via / thermal-array solver landing into inner planes without sealing BGA channels or pinching pours.
- **Zone Filler & Choke Analysis** (`src/zones.rs`): Priority-ordered Clipper2 copper pours with edge/keepout/antipad clearances, thermal reliefs, dead-island and sliver pruning, plus copper-graph split-pour and narrow-neck choke detection.
- **Continuous DRC & Hotspot Clusterer** (`src/drc.rs`): Continuous `Nm` DRC across clearances, hole-to-hole, drill-to-copper, edge clearance, annular ring, keepouts, courtyards, connectivity across tracks/vias/filled pours, and length/match/proximity constraints, grouped into spatial and shared-object hotspots (plus `kicad-cli` JSON hotspot clustering).
- **In-Memory Routing & Preflight** (`src/preflight.rs`, `src/route.rs`): Sub-second pre-routing feasibility gate and direct in-memory `BoardIr -> viahop::Board` bridge with incremental `--reroute-nets` support.
- **Staged Compiler Pipeline** (`src/pipeline.rs`): Content-hashed stage caching in `.boardc-cache/` (`place -> power-via -> preflight -> route -> zones -> drc -> fab/svg`).
- **Manufacturing & Visual Outputs** (`src/fab.rs`, `src/svg.rs`): Native RS-274X/Gerber X2 copper, mask, paste, silkscreen (clipped to solder mask), and `Edge.Cuts`, metric Excellon PTH/NPTH and blind/buried span drills, JLCPCB BOM/CPL CSVs, and zero-dependency 2D SVGs with optional DRC hotspot overlays.

## Verification Philosophy

`boardc` has zero external binary dependencies (it never shells out to `kicad-cli` or `gerbv`) and never chases KiCad bug-compatibility:

- **Native Fab Verification (`src/verify.rs`)**: Re-parses every emitted Gerber via `gerber_parser`, parses PTH/NPTH and blind/buried Excellon `.drl` files independently, verifies bounds and 1-to-1 drill counts/diameters/positions, and reconstructs copper islands per layer with full `%LPD*%` / `%LPC*%` polarity replay and PTH barrel connectivity to verify zero shorts and zero opens against the `BoardIr` netlist.
- **KiCad File Interop (`import-kicad` / `export-kicad`)**: Reads `.kicad_pcb`/`.kicad_pro`/`.kicad_mod`/`.kicad_net` and writes `.kicad_pcb` + `.kicad_pro` as pure files (preserving schematic `path`/`sheetname`/`sheetfile` metadata for `kicad-cli pcb drc --schematic-parity`) so you can open or inspect a board in KiCad externally if desired.

## CLI Subcommands

```text
boardc build <INPUT> [--out-dir <DIR>] [--cache-dir .boardc-cache] [--no-cache] [--fail-on-preflight] [--reroute-nets NET,...] [--json]
boardc preflight <INPUT> [--ignore NET,...] [--json]
boardc place <INPUT> [-o <OUT_BOARD_JSON>] [--power-vias] [--json]
boardc route <INPUT> [-o <OUT_BOARD_JSON>] [--reroute-nets NET,...] [--via-at-smd] [--json]
boardc zones <INPUT> [-o <OUT_BOARD_JSON>] [--report <REPORT_JSON>]
boardc drc <INPUT> [--kicad-drc <KICAD_DRC_JSON>] [--json]
boardc fab <INPUT> -o <FAB_DIR> [--no-clip-silk] [--json]
boardc svg <INPUT> -o <OUT_SVG_OR_DIR> [--view top|bottom] [--layer <LAYER>] [--no-hotspots]
boardc verify <INPUT> --fab-dir <FAB_DIR> [--json]
boardc import-kicad <KICAD_PCB> [--pro <KICAD_PRO>] [-o <OUT_BOARD_JSON>]
boardc export-kicad <INPUT> -o <OUT_KICAD_PCB> [--no-pro]
boardc dump-spec [-i <SPEC>] [--format toml|json] [-o <OUT>]
boardc skill <install|uninstall|show> [--to <DIR>]
```

## Native BoardSpec Format (TOML/JSON)

`boardc` accepts `.toml` or `.json` `BoardSpec` files as well as compiled `BoardIr` JSON and read-only `.kicad_pcb` files. Use `boardc dump-spec --format toml` to generate a starter spec:

```toml
name = "example"
size_nm = [30000000, 20000000]

[[layers]]
name = "F.Cu"
kind = "signal"
side = "top"

[[layers]]
name = "B.Cu"
kind = "plane"
side = "bottom"

[footprints.C_0402]
kind = "chip2_pin"
package = "0402"

[[placements]]
refdes = "C1"
footprint = "C_0402"
value = "100nF"
pos = { x = 10000000, y = 10000000 }
rot_deg = 90.0
side = "top"
pin_nets = { "1" = "VCC", "2" = "GND" }

[[pours]]
name = "GND_BOT"
net = "GND"
layer = "B.Cu"
outline = [
  { x = 0, y = 0 },
  { x = 30000000, y = 0 },
  { x = 30000000, y = 20000000 },
  { x = 0, y = 20000000 },
]
```
