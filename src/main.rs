use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;

use boardc::drc::{check_board_drc, cluster_kicad_drc_json};
use boardc::fab::{FabOptions, export_fab};
use boardc::ir::{
    BoardIr, BoardSpec, Constraints, FootprintSpec, LayerDef, LayerKind, MaxLengthRule, Placement,
    PourSpec, Pt, Side, load_board_json, load_spec_file, mm, nm_to_mm, save_board_json,
};
use boardc::kicad_export::export_kicad_pcb;
use boardc::kicad_io::import_kicad_pcb;
use boardc::pipeline::{BuildOptions, compile_board};
use boardc::place::{
    PlaceOptions, PlaceReport, PowerViaOptions, PowerViaReport, legalize_placement,
    solve_power_vias,
};
use boardc::preflight::run_preflight;
use boardc::route::{RouteOptions, route_board_ir};
use boardc::svg::{SvgOptions, SvgView, render_board_svg, write_board_svgs};
use boardc::verify::{VerifyReport, verify_fab_outputs};
use boardc::zones::fill_zones;

#[derive(Parser)]
#[command(name = "boardc", version, about = "Headless native Rust PCB compiler")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum SpecFormat {
    Json,
    Toml,
}

#[derive(Args)]
struct RouteArgs {
    #[arg(long, value_delimiter = ',')]
    ignore: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    reroute_nets: Vec<String>,
    #[arg(long)]
    reroute_pad_mm: Option<f64>,
    #[arg(long, default_value_t = 20)]
    max_passes: usize,
    #[arg(long, default_value_t = 4)]
    multi_start: usize,
    #[arg(long, default_value_t = 1)]
    threads: usize,
    #[arg(long)]
    timeout: Option<u64>,
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    via_at_smd: Option<bool>,
}

impl RouteArgs {
    fn into_options(self) -> RouteOptions {
        let reroute_set = to_net_set(self.reroute_nets);
        RouteOptions {
            max_passes: self.max_passes,
            multi_start: self.multi_start,
            threads: self.threads,
            timeout_secs: self.timeout,
            optimize: true,
            via_at_smd: self.via_at_smd,
            ignored_nets: to_net_set(self.ignore),
            reroute_nets: (!reroute_set.is_empty()).then_some(reroute_set),
            reroute_pad_mm: self.reroute_pad_mm,
            resume: false,
        }
    }
}

const EMBEDDED_SKILL: &str = include_str!("../SKILL.md");

#[derive(Subcommand)]
enum SkillAction {
    /// Install the embedded agent skill into ~/.agents/skills/boardc/SKILL.md (or --to <DIR>).
    Install {
        #[arg(long)]
        to: Option<PathBuf>,
    },
    /// Remove the installed agent skill from ~/.agents/skills/boardc (or --to <DIR>).
    Uninstall {
        #[arg(long)]
        to: Option<PathBuf>,
    },
    /// Print the embedded SKILL.md to stdout.
    Show,
}

#[derive(Subcommand)]
enum Command {
    /// Run full compilation pipeline (place -> power-via -> preflight -> route -> zones -> drc).
    Build {
        input: PathBuf,
        #[arg(long = "out-dir", short = 'o', visible_alias = "out")]
        out_dir: Option<PathBuf>,
        #[arg(long = "cache-dir", default_value = ".boardc-cache")]
        cache_dir: PathBuf,
        #[arg(long)]
        no_cache: bool,
        #[arg(long)]
        fail_on_preflight: bool,
        #[command(flatten)]
        route: RouteArgs,
        #[arg(long)]
        json: bool,
    },
    /// Run sub-second pre-routing feasibility gate.
    Preflight {
        input: PathBuf,
        #[arg(long, value_delimiter = ',')]
        ignore: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Run deterministic placement legalizer and power-via solver.
    Place {
        input: PathBuf,
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long)]
        power_vias: bool,
        #[arg(long)]
        json: bool,
    },
    /// Route board in-memory using viahop.
    Route {
        input: PathBuf,
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[command(flatten)]
        route: RouteArgs,
        #[arg(long)]
        json: bool,
    },
    /// Fill copper zones and analyze islands/chokes/necks.
    Zones {
        input: PathBuf,
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// Run continuous DRC engine and spatial hotspot clusterer.
    Drc {
        input: PathBuf,
        #[arg(long = "kicad-drc", visible_alias = "kicad-json")]
        kicad_drc: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Export RS-274X Gerbers, Gerber job file, Excellon drills, BOM, CPL, and optional drawings.
    Fab {
        input: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        /// Clip silkscreen against solder mask openings (enabled by default).
        #[arg(long, conflicts_with = "no_clip_silk")]
        subtract_soldermask: bool,
        #[arg(long)]
        no_clip_silk: bool,
        /// Emit top (and mirrored bottom for 2-sided boards) assembly drawing SVGs.
        #[arg(long)]
        assembly: bool,
        /// Emit drill map SVG with per-tool symbols and legend.
        #[arg(long)]
        drill_map: bool,
        /// Emit text drill report (<name>-drl.rpt).
        #[arg(long)]
        drill_report: bool,
        #[arg(long)]
        json: bool,
    },
    /// Render 2D SVG layer, assembly, drill map, and DRC hotspot views.
    Svg {
        input: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long)]
        layer: Option<String>,
        /// View mode: top, bottom, assembly-top, assembly-bottom, or drill-map.
        #[arg(long, conflicts_with = "layer")]
        view: Option<String>,
        #[arg(long)]
        no_hotspots: bool,
    },
    /// Independently verify generated Gerbers, Excellon drills, and copper connectivity.
    Verify {
        input: PathBuf,
        #[arg(long)]
        fab_dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Import read-only .kicad_pcb / .kicad_pro into canonical BoardIr JSON.
    ImportKicad {
        input: PathBuf,
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long)]
        pro: Option<PathBuf>,
    },
    /// Export a BoardSpec or BoardIr into KiCad .kicad_pcb (and sibling .kicad_pro).
    ExportKicad {
        input: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long)]
        no_pro: bool,
    },
    /// Emit a starter BoardSpec or validate/dump an existing spec to BoardIr JSON.
    DumpSpec {
        #[arg(short, long)]
        input: Option<PathBuf>,
        #[arg(short, long, value_enum, default_value_t = SpecFormat::Toml)]
        format: SpecFormat,
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Install, remove, or display the embedded agent skill (SKILL.md).
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },
}

fn sample_spec() -> BoardSpec {
    let footprints = BTreeMap::from([
        (
            "C_0402".into(),
            FootprintSpec::Chip2Pin {
                package: "0402".into(),
            },
        ),
        (
            "U_QFN16".into(),
            FootprintSpec::Qfn {
                pins_per_side: 4,
                pitch_nm: mm(0.5),
                body_nm: mm(3.0),
                ep_nm: mm(1.6),
            },
        ),
    ]);
    BoardSpec {
        name: "example".into(),
        size_nm: Some([mm(30.0), mm(20.0)]),
        layers: vec![
            LayerDef {
                name: "F.Cu".into(),
                kind: LayerKind::Signal,
                side: Some(Side::Top),
                thickness_nm: None,
            },
            LayerDef {
                name: "B.Cu".into(),
                kind: LayerKind::Plane,
                side: Some(Side::Bottom),
                thickness_nm: None,
            },
        ],
        footprints,
        placements: vec![
            Placement {
                refdes: "U1".into(),
                footprint: "U_QFN16".into(),
                value: "IC".into(),
                pos: Pt::new(mm(15.0), mm(10.0)),
                pin_nets: BTreeMap::from([("1".into(), "VCC".into()), ("EP".into(), "GND".into())]),
                ..Default::default()
            },
            Placement {
                refdes: "C1".into(),
                footprint: "C_0402".into(),
                value: "100nF".into(),
                pos: Pt::new(mm(10.0), mm(10.0)),
                rot_deg: 90.0,
                pin_nets: BTreeMap::from([("1".into(), "VCC".into()), ("2".into(), "GND".into())]),
                ..Default::default()
            },
        ],
        pours: vec![PourSpec {
            name: "GND_BOT".into(),
            net: "GND".into(),
            layer: "B.Cu".into(),
            outline: vec![
                Pt::new(0, 0),
                Pt::new(mm(30.0), 0),
                Pt::new(mm(30.0), mm(20.0)),
                Pt::new(0, mm(20.0)),
            ],
            ..Default::default()
        }],
        constraints: Constraints {
            max_lengths: vec![MaxLengthRule {
                nets: vec!["VCC".into()],
                max_nm: mm(25.0),
            }],
            ..Default::default()
        },
        ..Default::default()
    }
}

fn spec_base_dir(path: &Path) -> Option<&Path> {
    path.parent().filter(|p| !p.as_os_str().is_empty())
}

fn is_board_spec_json(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&text)
    else {
        return false;
    };
    map.contains_key("footprint_libs") || map.contains_key("netlist") || map.contains_key("size_nm")
}

fn load_board_input(path: &Path) -> Result<BoardIr, String> {
    if path
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("kicad_pcb"))
    {
        import_kicad_pcb(path, None)
    } else if is_board_spec_json(path) {
        let spec = load_spec_file(path)?;
        BoardIr::from_spec_with_base(&spec, spec_base_dir(path))
    } else {
        match load_board_json(path) {
            Ok(ir) => Ok(ir),
            Err(_) => {
                let spec = load_spec_file(path)?;
                BoardIr::from_spec_with_base(&spec, spec_base_dir(path))
            }
        }
    }
}

fn to_net_set(items: Vec<String>) -> HashSet<String> {
    items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn print_json<T: Serialize>(value: &T, label: &str) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| format!("failed to serialize {label}: {e}"))?;
    println!("{text}");
    Ok(())
}

fn write_or_print(text: String, out: Option<PathBuf>) -> Result<(), String> {
    if let Some(out_path) = out {
        fs::write(&out_path, text)
            .map_err(|e| format!("failed to write '{}': {e}", out_path.display()))?;
    } else {
        println!("{text}");
    }
    Ok(())
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::DumpSpec { input, format, out } => {
            let text = if let Some(path) = input {
                let spec = load_spec_file(&path)?;
                let ir = BoardIr::from_spec_with_base(&spec, spec_base_dir(&path))?;
                serde_json::to_string_pretty(&ir)
                    .map_err(|e| format!("failed to serialize BoardIr: {e}"))?
            } else {
                let spec = sample_spec();
                match format {
                    SpecFormat::Json => serde_json::to_string_pretty(&spec)
                        .map_err(|e| format!("failed to serialize JSON spec: {e}"))?,
                    SpecFormat::Toml => toml::to_string_pretty(&spec)
                        .map_err(|e| format!("failed to serialize TOML spec: {e}"))?,
                }
            };
            write_or_print(text, out)
        }
        Command::Build {
            input,
            out_dir,
            cache_dir,
            no_cache,
            fail_on_preflight,
            route,
            json,
        } => {
            let ir = load_board_input(&input)?;
            let cfg = BuildOptions {
                out_dir,
                cache_dir,
                use_cache: !no_cache,
                fail_on_preflight,
                route: route.into_options(),
                ..Default::default()
            };
            let report = compile_board(ir, &cfg)?;
            if json {
                print_json(&report, "BuildReport")?;
            } else {
                let hits = report.stages.iter().filter(|s| s.cache_hit).count();
                println!(
                    "Build {}: {}/{} nets routed ({} unconnected), {} chokes, {} DRC errors, {}/{} stages cached ({}ms)",
                    if report.ok { "OK" } else { "FAILED" },
                    report.route.routed_nets,
                    report.route.routable_nets,
                    report.route.unconnected_pairs,
                    report.zones.chokes.len(),
                    report.drc.error_count,
                    hits,
                    report.stages.len(),
                    report.elapsed_ms,
                );
            }
            Ok(())
        }
        Command::Preflight {
            input,
            ignore,
            json,
        } => {
            let ir = load_board_input(&input)?;
            let summary = run_preflight(&ir, &to_net_set(ignore))?;
            if json {
                print_json(&summary, "PreflightSummary")?;
            } else {
                println!(
                    "Preflight {}: {}/{} routable nets, {} layout violations, {} zone chokes, {} sealed pads, {} blocked nets, {} fixed violations, grid={:.1}um ({}ms)",
                    if summary.ok { "PASSED" } else { "FAILED" },
                    summary.routable_nets,
                    summary.total_nets,
                    summary.layout_violations.len(),
                    summary.zone_chokes.len(),
                    summary.sealed_pads.len(),
                    summary.blocked_nets.len(),
                    summary.fixed_violations.len(),
                    summary.grid_step_um,
                    summary.elapsed_ms,
                );
            }
            Ok(())
        }
        Command::Place {
            input,
            out,
            power_vias,
            json,
        } => {
            let mut ir = load_board_input(&input)?;
            let place_report = legalize_placement(&mut ir, &PlaceOptions::default())?;
            let power_via_report = if power_vias {
                Some(solve_power_vias(&mut ir, &PowerViaOptions::default())?)
            } else {
                None
            };
            if let Some(out_path) = out {
                save_board_json(&ir, &out_path)?;
            }
            if json {
                #[derive(Serialize)]
                struct PlaceCliOutput {
                    placement: PlaceReport,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    power_vias: Option<PowerViaReport>,
                }
                print_json(
                    &PlaceCliOutput {
                        placement: place_report,
                        power_vias: power_via_report,
                    },
                    "place report",
                )?;
            } else {
                println!(
                    "Place {}: {}/{} fixed, {} moved, {} unresolvable ({}ms)",
                    if place_report.ok { "OK" } else { "FAILED" },
                    place_report.fixed_count,
                    place_report.total_placements,
                    place_report.moved_parts.len(),
                    place_report.unresolvable.len(),
                    place_report.elapsed_ms,
                );
                if let Some(pv) = power_via_report {
                    println!(
                        "PowerVias {}: {} thermal, {} plane ({} POFV), {} stubs, {} unresolved, {} chokes ({}ms)",
                        if pv.ok { "OK" } else { "FAILED" },
                        pv.thermal_vias_added,
                        pv.plane_vias_added,
                        pv.pofv_count,
                        pv.stubs_added,
                        pv.unresolved.len(),
                        pv.chokes_after,
                        pv.elapsed_ms,
                    );
                }
            }
            Ok(())
        }
        Command::Route {
            input,
            out,
            route,
            json,
        } => {
            let mut ir = load_board_input(&input)?;
            let summary = route_board_ir(&mut ir, &route.into_options())?;
            if let Some(out_path) = out {
                save_board_json(&ir, &out_path)?;
            }
            if json {
                print_json(&summary, "RouteSummary")?;
            } else {
                println!(
                    "Routed {}/{} nets ({} unconnected pairs), {} segs, {} vias, {:.2} mm wire, {} passes ({} starts), {} DRC issues, {} length issues, {}ms",
                    summary.routed_nets,
                    summary.routable_nets,
                    summary.unconnected_pairs,
                    summary.total_segments,
                    summary.total_vias,
                    summary.total_wirelength_um / 1000.0,
                    summary.passes_run,
                    summary.starts_run,
                    summary.drc_violations,
                    summary.length_violations,
                    summary.elapsed_ms,
                );
            }
            Ok(())
        }
        Command::Zones { input, out, report } => {
            let mut ir = load_board_input(&input)?;
            let zone_report = fill_zones(&mut ir);
            if let Some(report_path) = report {
                let rep_text = serde_json::to_string_pretty(&zone_report)
                    .map_err(|e| format!("failed to serialize ZoneReport: {e}"))?;
                fs::write(&report_path, rep_text)
                    .map_err(|e| format!("failed to write '{}': {e}", report_path.display()))?;
            }
            if let Some(out_path) = out {
                save_board_json(&ir, &out_path)?;
            } else {
                print_json(&ir, "BoardIr")?;
            }
            Ok(())
        }
        Command::Drc {
            input,
            kicad_drc,
            json,
        } => {
            let ir = load_board_input(&input)?;
            if let Some(kdrc_path) = kicad_drc {
                let kdrc_str = fs::read_to_string(&kdrc_path)
                    .map_err(|e| format!("failed to read '{}': {e}", kdrc_path.display()))?;
                let hotspots = cluster_kicad_drc_json(&ir, &kdrc_str)?;
                if json {
                    print_json(&hotspots, "hotspots")?;
                } else {
                    println!("KiCad DRC hotspots: {}", hotspots.len());
                    for (i, hs) in hotspots.iter().enumerate() {
                        println!(
                            "  [{}] ({:.3}, {:.3}) mm r={:.3} mm count={} root={} net={} | {}",
                            i + 1,
                            nm_to_mm(hs.centroid_nm.x),
                            nm_to_mm(hs.centroid_nm.y),
                            nm_to_mm(hs.radius_nm),
                            hs.violation_count,
                            hs.root_cause_item,
                            hs.root_cause_net,
                            hs.action_hint
                        );
                    }
                }
            } else {
                let report = check_board_drc(&ir);
                if json {
                    print_json(&report, "DrcReport")?;
                } else {
                    println!(
                        "DRC {}: {} errors, {} warnings, {} hotspots",
                        if report.passed { "PASSED" } else { "FAILED" },
                        report.error_count,
                        report.warning_count,
                        report.hotspots.len()
                    );
                    for (i, hs) in report.hotspots.iter().enumerate() {
                        println!(
                            "  hotspot {}: ({:.3}, {:.3}) mm r={:.3} mm count={} root={} | {}",
                            i + 1,
                            nm_to_mm(hs.centroid_nm.x),
                            nm_to_mm(hs.centroid_nm.y),
                            nm_to_mm(hs.radius_nm),
                            hs.violation_count,
                            hs.root_cause_item,
                            hs.action_hint
                        );
                    }
                }
            }
            Ok(())
        }
        Command::Fab {
            input,
            out,
            subtract_soldermask: _,
            no_clip_silk,
            assembly,
            drill_map,
            drill_report,
            json,
        } => {
            let ir = load_board_input(&input)?;
            let manifest = export_fab(
                &ir,
                &out,
                &FabOptions {
                    clip_silk_to_mask: !no_clip_silk,
                    assembly,
                    drill_map,
                    drill_report,
                },
            )?;
            if json {
                print_json(&manifest, "FabManifest")?;
            } else {
                let mut extras = String::new();
                if !manifest.assembly_svgs.is_empty() {
                    let noun = if manifest.assembly_svgs.len() == 1 {
                        "SVG"
                    } else {
                        "SVGs"
                    };
                    extras.push_str(&format!(
                        ", {} assembly {noun}",
                        manifest.assembly_svgs.len()
                    ));
                }
                if let Some(ref p) = manifest.drill_map_svg {
                    extras.push_str(&format!(", drill_map={}", p.display()));
                }
                if let Some(ref p) = manifest.drill_report {
                    extras.push_str(&format!(", drill_report={}", p.display()));
                }
                println!(
                    "Fab exported to '{}': {} gerbers, {} drills, job={}, BOM={}, CPL={}{extras}",
                    out.display(),
                    manifest.gerber_files.len(),
                    manifest.drill_files.len(),
                    manifest.gbrjob.display(),
                    manifest.bom_csv.display(),
                    manifest.cpl_csv.display()
                );
            }
            Ok(())
        }
        Command::Svg {
            input,
            out,
            layer,
            view,
            no_hotspots,
        } => {
            let ir = load_board_input(&input)?;
            let is_svg_file = out
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("svg"));
            if is_svg_file {
                let svg_view = if let Some(l) = layer {
                    SvgView::Layer(l)
                } else {
                    match view
                        .as_deref()
                        .unwrap_or("top")
                        .to_ascii_lowercase()
                        .as_str()
                    {
                        "top" => SvgView::Top,
                        "bottom" => SvgView::Bottom,
                        "assembly-top" | "assembly_top" => SvgView::AssemblyTop,
                        "assembly-bottom" | "assembly_bottom" => SvgView::AssemblyBottom,
                        "drill-map" | "drill_map" => SvgView::DrillMap,
                        other => {
                            return Err(format!(
                                "unknown SVG view '{other}' (expected top, bottom, assembly-top, assembly-bottom, drill-map)"
                            ));
                        }
                    }
                };
                let needs_drc = !no_hotspots
                    && !matches!(
                        svg_view,
                        SvgView::AssemblyTop | SvgView::AssemblyBottom | SvgView::DrillMap
                    );
                let drc = needs_drc.then(|| check_board_drc(&ir));
                let svg = render_board_svg(
                    &ir,
                    drc.as_ref(),
                    &SvgOptions {
                        view: svg_view,
                        show_hotspots: needs_drc,
                        ..Default::default()
                    },
                );
                if let Some(parent) = out.parent()
                    && !parent.as_os_str().is_empty()
                {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("failed to create dir '{}': {e}", parent.display()))?;
                }
                fs::write(&out, svg)
                    .map_err(|e| format!("failed to write '{}': {e}", out.display()))?;
                println!("SVG written to '{}'", out.display());
            } else {
                if layer.is_some() || view.is_some() {
                    return Err("--view and --layer require an output path ending in '.svg'".into());
                }
                let drc = (!no_hotspots).then(|| check_board_drc(&ir));
                let files = write_board_svgs(&ir, drc.as_ref(), &out)?;
                println!("SVGs written to '{}': {} files", out.display(), files.len());
            }
            Ok(())
        }
        Command::Verify {
            input,
            fab_dir,
            json,
        } => {
            let ir = load_board_input(&input)?;
            let fab_report = verify_fab_outputs(&ir, &fab_dir)?;
            let ok = fab_report.passed;
            if json {
                #[derive(Serialize)]
                struct VerifyCliOutput {
                    ok: bool,
                    fab: VerifyReport,
                }
                print_json(
                    &VerifyCliOutput {
                        ok,
                        fab: fab_report,
                    },
                    "VerifyReport",
                )?;
            } else {
                println!(
                    "Verify {}: {} gerbers, drills PTH {}/{} NPTH {}/{} ({} mismatches), connectivity {} ({} islands, {} nets, {} shorts, {} opens)",
                    if ok { "PASSED" } else { "FAILED" },
                    fab_report.gerber_files.len(),
                    fab_report.drills.pth_matched,
                    fab_report.drills.pth_expected,
                    fab_report.drills.npth_matched,
                    fab_report.drills.npth_expected,
                    fab_report.drills.mismatches,
                    if fab_report.connectivity.passed {
                        "OK"
                    } else {
                        "FAILED"
                    },
                    fab_report.connectivity.island_count,
                    fab_report.connectivity.checked_nets,
                    fab_report.connectivity.shorts.len(),
                    fab_report.connectivity.opens.len(),
                );
            }
            if !ok {
                return Err("verification failed".into());
            }
            Ok(())
        }
        Command::ImportKicad { input, out, pro } => {
            let ir = import_kicad_pcb(&input, pro.as_deref())?;
            let text = serde_json::to_string_pretty(&ir)
                .map_err(|e| format!("failed to serialize BoardIr: {e}"))?;
            write_or_print(text, out)
        }
        Command::ExportKicad { input, out, no_pro } => {
            let ir = load_board_input(&input)?;
            export_kicad_pcb(&ir, &out, !no_pro)?;
            println!("KiCad PCB written to '{}'", out.display());
            Ok(())
        }
        Command::Skill { action } => run_skill(action),
    }
}

fn default_skills_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "HOME/USERPROFILE not set; pass --to <DIR>".to_string())?;
    Ok(PathBuf::from(home).join(".agents").join("skills"))
}

fn run_skill(action: SkillAction) -> Result<(), String> {
    match action {
        SkillAction::Show => {
            print!("{EMBEDDED_SKILL}");
            Ok(())
        }
        SkillAction::Install { to } => {
            let base = match to {
                Some(dir) => dir,
                None => default_skills_dir()?,
            };
            let dir = base.join("boardc");
            fs::create_dir_all(&dir)
                .map_err(|e| format!("failed to create '{}': {e}", dir.display()))?;
            let dest = dir.join("SKILL.md");
            let tmp = dir.join(format!("SKILL.md.tmp.{}", process::id()));
            fs::write(&tmp, EMBEDDED_SKILL)
                .map_err(|e| format!("failed to write '{}': {e}", tmp.display()))?;
            fs::rename(&tmp, &dest)
                .map_err(|e| format!("failed to install '{}': {e}", dest.display()))?;
            println!("{}", dest.display());
            Ok(())
        }
        SkillAction::Uninstall { to } => {
            let base = match to {
                Some(dir) => dir,
                None => default_skills_dir()?,
            };
            let dir = base.join("boardc");
            let dest = dir.join("SKILL.md");
            if dest.exists() {
                fs::remove_file(&dest)
                    .map_err(|e| format!("failed to remove '{}': {e}", dest.display()))?;
            }
            let _ = fs::remove_dir(&dir);
            println!("removed {}", dest.display());
            Ok(())
        }
    }
}

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli) {
        eprintln!("error: {err}");
        process::exit(1);
    }
}
