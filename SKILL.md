---
name: boardc
description:
  Headless native Rust PCB compiler for designing, placing, routing, zone-filling,
  checking DRC, exporting Gerbers/Excellon/BOM/CPL and SVGs, and verifying PCBs
  from TOML/JSON BoardSpec files or imported KiCad boards without a GUI. Use when
  compiling or iterating on PCB layouts, running preflight or DRC hotspot analysis,
  legalizing component placement or power vias, filling copper pours, rerouting
  specific nets, or generating and verifying manufacturing outputs.
license: MIT
---

# boardc

Headless PCB compiler (`place -> power-via -> preflight -> route -> zones -> drc -> fab/svg`) with `viahop` embedded in-process, Clipper2 zone filling, exact nanometer DRC, and independent Gerber/Excellon verification.

Use `boardc <subcommand> --help` for exhaustive flags and `boardc dump-spec --format toml` for a complete starter spec.

## Coordinate Frame & Units

Do not apply KiCad Y-down or millimeter assumptions when editing `BoardSpec` or `BoardIr`:

- All lengths and coordinates are integer nanometers (`Nm = i64`, `1 mm = 1_000_000 nm`, `1 um = 1_000 nm`).
- Coordinates are right-handed Y-up (`+X` right, `+Y` up) with counter-clockwise rotation positive in degrees (`0..360`).
- Bottom-side placements (`side = "bottom"`) flip across local `X` (`ly' = -ly`) before rotating CCW by `rot_deg` and remapping `F.* <-> B.*`.
- KiCad Y-down conversion happens automatically only at `import-kicad` and `export-kicad`.

## Agent Iteration Loop

1. **Create or import a board**:
   ```sh
   boardc dump-spec --format toml -o board.toml
   # Or import an existing KiCad board (read-only) into canonical BoardIr JSON:
   boardc import-kicad design.kicad_pcb --pro design.kicad_pro -o board.json
   ```
   Every stage subcommand accepts `.toml`, `.json` (`BoardSpec` or `BoardIr`), or `.kicad_pcb` directly.

2. **Run sub-second preflight before routing**:
   ```sh
   boardc preflight board.toml --json
   ```
   Fix hard blockers (`layout_violations`, `zone_chokes`, `sealed_pads`, `blocked_nets`, `fixed_violations`, `erc_violations`) before spending time on routing.

3. **Compile with stage caching**:
   ```sh
   boardc build board.toml --out-dir out --fail-on-preflight --json
   ```
   Outputs go to `out/` (`<name>.board.json`, `out/fab/`, `out/svg/`). Unchanged stages reuse `.boardc-cache/`.
   - Mark movable components `locked = false` to let the legalizer resolve courtyard overlaps, BGA shadows, keepouts, and `cap_proximity` rules.
   - Pass `--via-at-smd` for POFV via-in-pad fanout on dense BGAs.

4. **Triage DRC hotspots and iterate narrowly**:
   - Read `drc.hotspots` from the `--json` report (grouped by shared root-cause item and spatial proximity) and inspect `out/svg/*_top.svg` / `*_bottom.svg` (hotspot overlays are enabled by default).
   - After fixing a constraint or net, reroute only affected nets while freezing the rest:
     ```sh
     boardc build board.toml --out-dir out --reroute-nets CLK,CS --reroute-pad-mm 4.0 --json
     ```
   - To triage an external `kicad-cli` DRC report with the same clusterer:
     ```sh
     boardc drc board.json --kicad-drc kicad_drc.json --json
     ```

5. **Verify manufacturing files independently**:
   ```sh
   boardc verify out/board.board.json --fab-dir out/fab --json
   ```
   `verify` re-parses the emitted RS-274X Gerbers and Excellon drills (PTH, NPTH, and blind/buried spans), replays polarity, and checks zero opens/shorts against the netlist with zero external subprocesses. Use `boardc export-kicad out/board.board.json -o out/board.kicad_pcb` if you want a KiCad file for external inspection.

## Stage Commands

Run individual stages when debugging one pass in isolation:

```sh
boardc place board.toml -o placed.json --power-vias --json
boardc route placed.json -o routed.json --ignore GND,VCC --threads 4 --json
boardc zones routed.json -o filled.json --report zones.json
boardc erc board.toml --json
boardc drc filled.json --json
boardc fab filled.json -o fab_out --assembly --drill-map --drill-report --json
boardc svg filled.json -o preview_top.svg --view top
boardc svg filled.json -o assembly_top.svg --view assembly-top
boardc svg filled.json -o drill_map.svg --view drill-map
boardc svg filled.json -o preview_in1.svg --layer In1.Cu
```

## 3D Rendering via KiCad CLI

`boardc` stays pure Rust with no external subprocesses, and preserves footprint 3D `(model ...)` metadata across `import-kicad` and `export-kicad`. When `kicad-cli` is installed, export to `.kicad_pcb` and render 3D PNGs directly:

```sh
boardc export-kicad out/board.board.json -o out/board.kicad_pcb
kicad-cli pcb render out/board.kicad_pcb -o out/render_top.png --side top --quality high --perspective --rotate "-35,0,30"
kicad-cli pcb render out/board.kicad_pcb -o out/render_bot.png --side bottom --quality high --perspective --rotate "-35,0,-30"
```

## Spec Quick Reference

- Parametric footprints supported natively: `chip2_pin` (`0201`/`0402`/`0603`/`0805`), `bga`, `qfn`, `sot23`, `pin_header`, plus `custom` pads/courtyards/graphics/keepouts.
- External KiCad libraries & netlists: set `footprint_libs = ["path/to/lib.pretty"]` to load `.kicad_mod` footprints directly and `netlist = "path/to/design.net"` to import `.kicad_net`/`.net` pin nets, values, MPN/LCSC, DNP/BOM flags, and schematic `path`/`sheetname`/`sheetfile` metadata (preserved through `export-kicad` for `kicad-cli pcb drc --schematic-parity`).
- Electrical pin types (`pin_types` on `[footprints.<NAME>]` or `[[placements]]`): optional `in`, `out`, `bidir`, `tri`, `power_in`, `power_out`, `passive`, `nc` annotations checked by `boardc erc` and `boardc preflight` (single-pin nets, output/`power_out` conflicts, undriven `in`/`power_in`, unconnected non-`nc` pins, and connected `nc` pins).
- Board metadata & graphics: optional top-level `title` and `revision` populate KiCad `(title_block ...)` and `<name>-job.gbrjob` `"Revision"`; `[[graphics]]` supports `line`, `rect`, `circle`, `poly`, and `text` (multi-line, rotated `rot_deg`, auto-mirrored on `B.*` layers across Gerber, SVG, and KiCad).
- Stackup (`[stackup]`): `thickness_nm`, `finish`, `pofv` (via plugging + filling + capping), and `dielectric_nm` (emitted to `<name>-job.gbrjob` and KiCad `(general)`/`(setup (stackup ...))`).
- Constraints (`[constraints]`): `max_lengths`, `match_groups`, `cap_proximity`, `min_vias`, and `bga_shadows`.
- Keepout kinds: `all`, `track`, `via`, `copper` (pours only, allows tracks), `placement`, or `custom` (`tracks`/`vias`/`copper`/`placement` booleans).
- `boardc` embeds `viahop` statically; you do not need `viahop` installed separately unless you specifically need Specctra `.dsn`/`.ses` Freerouting compatibility.
