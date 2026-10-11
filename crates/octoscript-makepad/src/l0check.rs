//! `l0check`: check an L0 card the way a host will, before a host runs it.
//!
//! An L0 card is data, not a program, so almost every fault it can have is
//! decidable without running it: the profile check (`check_ui_l0_named`) reads
//! syntax, level and header; realization (`realize`) binds the card against
//! data and answers every name it reads; the kit lowering
//! (`octoscript-makepad`'s `l0::prepare_offline`, injected by the binary)
//! assembles the palette, the derivations and the kit body and evaluates the
//! result in the checked Splash VM. All three already run inside card-host,
//! AppCard and the shells — this module puts them on the command line, where
//! the person writing the card is.
//!
//! Three properties the tool owes its users, each learned from a tool that got
//! it wrong:
//!
//! * **It is advisory.** It never decides admission; the store's gate does
//!   that. The house style for a read-only advisory check is `ok`/`warn`/`ERR`
//!   lines, exit 0/1/2, and never a refusal.
//! * **It reports positions.** A diagnostic without a line is a fortune
//!   cookie; every finding carries the checker's own line and column.
//! * **It says what it could not check.** A skipped tier is a note, never a
//!   silent pass.
//!
//! The core needs no VM and no kit: the lowering arrives as a [`Lower`]
//! closure, which keeps this module testable without a renderer and lets the
//! binary wire the real one.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use octoscript_ui_l0::{
    check_ui_l0_named, parse_header, realize, state_initials, RealizeLimits,
};

/// The tool's name, in output and errors.
pub const TOOL_NAME: &str = "l0check";
/// The environment variable that names the kit directory.
pub const KIT_ENV: &str = "OCTOSCRIPT_L0_KIT";
/// The file that marks a directory as an L0 kit.
const KIT_MARKER: &str = "_kit.octoscript";
/// How much of an `.octoscript` file is read to decide whether it declares the
/// L0 profile; a header is a few leading `#` lines.
const HEADER_PROBE_BYTES: usize = 8 * 1024;

/// What to check. The profile check runs in every tier — bind and lift build on
/// it, and a card that fails it cannot be realized or lowered at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tier {
    /// The L0 profile only: syntax, level, header, theme-portability lints.
    Static,
    /// The profile, then realization against data.
    Bind,
    /// The profile, then the kit lowering and its checked evaluation.
    Lift,
    /// Everything.
    All,
}

impl Tier {
    /// Parses a `--tier` value.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "static" => Some(Self::Static),
            "bind" => Some(Self::Bind),
            "lift" => Some(Self::Lift),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    /// The name as it appears in `--tier` and in the JSON report.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Bind => "bind",
            Self::Lift => "lift",
            Self::All => "all",
        }
    }

    fn runs_bind(self) -> bool {
        matches!(self, Self::Bind | Self::All)
    }

    fn runs_lift(self) -> bool {
        matches!(self, Self::Lift | Self::All)
    }
}

/// A finding's severity. A clean file has no findings; its `ok` status is the
/// absence of any.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Severity {
    Warn,
    Error,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// One finding, positioned in the card's own coordinates.
#[derive(Clone, Debug)]
pub struct Finding {
    pub severity: Severity,
    /// A stable code, so a reader can match findings without parsing prose:
    /// `l0.syntax`, `l0.header`, `l0.lint`, `l0.bind`, `l0.lift`, `l0.io`.
    pub code: &'static str,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub message: String,
}

/// What a successful lowering produced. Counts only: the tree itself stays in
/// the lowering closure, which is where the renderer lives.
#[derive(Clone, Debug)]
pub struct LiftOutcome {
    pub nodes: usize,
    pub texts: usize,
    pub images: usize,
    /// Whether the card lowered through a native kit pack.
    pub native_kit: bool,
}

/// The kit lowering, injected by the binary: this crate's
/// `l0::prepare_offline`, which realizes the card, assembles the mood's
/// palette chain and evaluates the assembled source in the checked VM.
pub type Lower<'a> = dyn Fn(&str, &serde_json::Value, &Path) -> Result<LiftOutcome, String> + 'a;

/// One card's report.
#[derive(Clone, Debug)]
pub struct FileReport {
    pub path: PathBuf,
    /// The profile the card belongs to (`L0`, `L1`, `L2`), once checked.
    pub level: Option<String>,
    pub valid: Option<bool>,
    pub findings: Vec<Finding>,
    /// Short phrases for the status line: `L0`, `realizes 41 nodes`, `lowers
    /// 41 nodes / 12 texts / 2 images`.
    pub parts: Vec<String>,
    /// What was skipped and why. A note is never a pass: it is printed.
    pub notes: Vec<String>,
    /// A problem with the inputs rather than the card — a fixture that will
    /// not parse. It exits 2 (a usage problem), not 1: the card is not what
    /// needs fixing.
    pub fatal: Option<String>,
    pub lift: Option<LiftOutcome>,
}

impl FileReport {
    fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            level: None,
            valid: None,
            findings: Vec::new(),
            parts: Vec::new(),
            notes: Vec::new(),
            fatal: None,
            lift: None,
        }
    }

    fn push(&mut self, severity: Severity, code: &'static str, line: Option<usize>, column: Option<usize>, message: impl Into<String>) {
        self.findings.push(Finding { severity, code, line, column, message: message.into() });
    }

    fn error(&mut self, code: &'static str, line: Option<usize>, column: Option<usize>, message: impl Into<String>) {
        self.push(Severity::Error, code, line, column, message);
    }

    fn warn(&mut self, code: &'static str, line: Option<usize>, column: Option<usize>, message: impl Into<String>) {
        self.push(Severity::Warn, code, line, column, message);
    }

    /// The file's status: the worst severity it carries.
    pub fn severity(&self) -> Option<Severity> {
        if self.findings.iter().any(|f| f.severity == Severity::Error) {
            Some(Severity::Error)
        } else if !self.findings.is_empty() {
            Some(Severity::Warn)
        } else {
            None
        }
    }
}

/// A parsed command line.
#[derive(Clone, Debug)]
pub struct Invocation {
    pub paths: Vec<PathBuf>,
    pub tier: Tier,
    /// Whether `--tier` was given. The default tier is "everything a kit is
    /// found for"; an explicit `--tier all` is a promise to check everything,
    /// and failing to find a kit for it is a usage error rather than a note.
    pub tier_explicit: bool,
    pub data: Option<PathBuf>,
    pub kit: Option<PathBuf>,
    pub strict: bool,
    pub json: bool,
}

/// What [`parse`] produced.
#[derive(Debug)]
pub enum Parsed {
    Run(Invocation),
    Help,
    Version,
}

/// The usage text. Doubles as `--help`.
pub fn usage() -> String {
    format!(
        "\
{TOOL_NAME} — check an L0 card the way a host will, before a host runs it

usage: {TOOL_NAME} [options] <path...>

  A path is a card file (.card, .l0, or an .octoscript whose header declares
  the ui/l0 profile) or a directory to scan. A directory scan checks every
  .card and .l0 file under it, plus .octoscript files that declare the
  profile, and skips a kit/ subtree, which holds kit definitions rather than
  cards. Pass a file explicitly to check it whatever its extension.

options:
  --tier static|bind|lift|all   what to check (default: all when a kit is
                                found, otherwise static+bind)
                                  static  the L0 profile: syntax, level,
                                          header, theme-portability lints
                                  bind    realize the card against data
                                  lift    lower it through the kit and
                                          evaluate the assembled source
  --data FILE.json              fixture data for the bind and lift tiers: a
                                JSON object of declaration name to value.
                                Default: <card>.data.json beside the card,
                                plus every declared state initial.
  --kit DIR                     the L0 kit: the directory holding
                                {KIT_MARKER}. Default: ${KIT_ENV}, then the
                                checkout this was built from, then a
                                components/l0 directory above the executable
                                or the working directory. A kit named by
                                --kit or the environment that holds no
                                {KIT_MARKER} is an error, never a fallback
                                to another one.
  --strict                      theme-portability lints are errors
  --json                        one JSON report on stdout; human output
                                otherwise
  --version, --help

exit status:
  0  nothing that stops the card lowering
  1  at least one error finding
  2  a usage or I/O problem: unknown flag, missing path, unreadable file, bad
     fixture JSON, or --tier lift with no kit to lower through

{TOOL_NAME} is advisory. It never decides admission; hub check does that.
"
    )
}

/// Parses a command line. `Err` carries the message to print before the usage.
pub fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut paths = Vec::new();
    let mut tier = Tier::All;
    let mut data = None;
    let mut kit = None;
    let mut strict = false;
    let mut json = false;
    let mut tier_explicit = false;
    let mut flags_done = false;

    let take_value = |args: &[String], at: &mut usize, flag: &str| -> Result<String, String> {
        *at += 1;
        match args.get(*at) {
            Some(value) => Ok(value.clone()),
            None => Err(format!("{flag} needs a value")),
        }
    };

    let mut at = 0;
    while at < args.len() {
        let arg = &args[at];
        if flags_done || !arg.starts_with("--") || arg == "--" {
            if arg == "--" {
                flags_done = true;
            } else {
                paths.push(PathBuf::from(arg));
            }
            at += 1;
            continue;
        }
        match arg.as_str() {
            "--help" | "-h" => return Ok(Parsed::Help),
            "--version" | "-V" => return Ok(Parsed::Version),
            "--strict" => strict = true,
            "--json" => json = true,
            "--tier" => {
                let value = take_value(args, &mut at, "--tier")?;
                tier = Tier::parse(&value).ok_or_else(|| format!("unknown tier {value:?}: static, bind, lift or all"))?;
                tier_explicit = true;
            }
            "--data" => data = Some(PathBuf::from(take_value(args, &mut at, "--data")?)),
            "--kit" => kit = Some(PathBuf::from(take_value(args, &mut at, "--kit")?)),
            other => return Err(format!("unknown flag {other}")),
        }
        at += 1;
    }

    if paths.is_empty() {
        return Err("no card, bundle or directory given".to_owned());
    }
    Ok(Parsed::Run(Invocation { paths, tier, tier_explicit, data, kit, strict, json }))
}

/// Runs the check and returns the exit status. Output goes to `out` (findings)
/// and `err` (usage and I/O problems); `lower` is the kit lowering, or `None`
/// where no renderer is linked.
pub fn run(inv: &Invocation, lower: Option<&Lower>, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let files = match discover(&inv.paths) {
        Ok(files) => files,
        Err(message) => {
            let _ = writeln!(err, "{TOOL_NAME}: {message}");
            return 2;
        }
    };

    // The kit is resolved once, before any card is read: an unusable kit is
    // the same for every card, and saying so once is clearer than per file.
    let kit = resolve_kit(inv);
    if let Kit::Invalid(path) = &kit {
        let _ = writeln!(err, "{TOOL_NAME}: {} is not an L0 kit: no {KIT_MARKER} in it", path.display());
        return 2;
    }
    // An explicit lift tier with no kit to lower through is a usage error: the
    // caller asked for a check that cannot run. The DEFAULT tier is "all that
    // is available": with no kit it checks the profile and realization and
    // says so per card, which is the useful answer on a machine with no
    // Octoscript-Makepad checkout beside it.
    if inv.tier.runs_lift() && inv.tier_explicit && matches!(kit, Kit::Missing) {
        let _ = writeln!(
            err,
            "{TOOL_NAME}: --tier {} needs a kit to lower through: pass --kit DIR or set {KIT_ENV}",
            inv.tier.as_str()
        );
        return 2;
    }
    let kit = match kit {
        Kit::Found(kit) => Some(kit),
        _ => None,
    };

    let reports: Vec<FileReport> = files
        .iter()
        .map(|path| check_file(path, inv, kit.as_deref(), lower))
        .collect();

    if inv.json {
        print_json(&reports, inv, kit.as_deref(), out);
    } else {
        print_human(&reports, out);
    }

    // A broken fixture is the caller's problem, not the card's: the report
    // carries it (printed above, with the file that will not parse), and the
    // run then exits with the usage status rather than the findings status.
    if reports.iter().any(|report| report.fatal.is_some()) {
        return 2;
    }

    if reports
        .iter()
        .flat_map(|report| &report.findings)
        .any(|finding| finding.severity == Severity::Error)
    {
        1
    } else {
        0
    }
}

/// Collects the cards to check: explicit files as given, directories scanned.
fn discover(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    for path in paths {
        let metadata = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if metadata.is_file() {
            found.push(path.clone());
        } else if metadata.is_dir() {
            scan_dir(path, &mut found);
        } else {
            return Err(format!("{}: not a file or directory", path.display()));
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

/// A directory scan. `kit/` is skipped: it holds kit definitions (components,
/// palettes, native packs), which are not cards and do not check as any.
fn scan_dir(dir: &Path, found: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if matches!(name.as_str(), ".git" | "target" | "kit") {
                continue;
            }
            scan_dir(&path, found);
        } else if is_l0_file(&path) {
            found.push(path);
        }
    }
}

/// Whether a directory scan should check this file. `.card` and `.l0` always;
/// `.octoscript` only when its header declares the L0 profile, since the
/// extension is shared with the agent scripting language.
fn is_l0_file(path: &Path) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("card") | Some("l0") => true,
        Some("octoscript") => {
            let head = fs::File::open(path).ok().and_then(|file| {
                use std::io::Read as _;
                let mut buf = vec![0u8; HEADER_PROBE_BYTES];
                let read = file.take(HEADER_PROBE_BYTES as u64).read(&mut buf).ok()?;
                buf.truncate(read);
                Some(String::from_utf8_lossy(&buf).into_owned())
            });
            matches!(head.as_deref().and_then(parse_header), Some(header) if declares_l0(&header))
        }
        _ => false,
    }
}

fn declares_l0(header: &octoscript_ui_l0::CardHeader) -> bool {
    header.level.as_deref().is_some_and(|level| level == "L0" || level == "L1")
        || header.profile.as_deref() == Some("ui/l0")
}

/// The kit directory, or why there is none. `Invalid` is a path the caller
/// named that is not a kit — a typo worth reporting, never a reason to lower
/// silently through a different one.
enum Kit {
    Found(PathBuf),
    Missing,
    Invalid(PathBuf),
}

/// Resolves the kit. The flag and the environment are authoritative: a named
/// kit that is not one is a typo, and lowering through a different kit than
/// the caller named is exactly the silent substitution this tool exists to
/// prevent. Without either, the checkout this binary was built from and then
/// a `components/l0` above the executable or the working directory are
/// best-effort candidates — a candidate counts only when it holds the kit
/// body, so a stale path never silently becomes the kit.
fn resolve_kit(inv: &Invocation) -> Kit {
    let named = inv
        .kit
        .clone()
        .or_else(|| std::env::var_os(KIT_ENV).map(PathBuf::from));
    if let Some(named) = named {
        return if named.join(KIT_MARKER).is_file() {
            Kit::Found(named)
        } else {
            Kit::Invalid(named)
        };
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../components/l0"));
    for start in [
        std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)),
        std::env::current_dir().ok(),
    ]
    .into_iter()
    .flatten()
    {
        for ancestor in start.ancestors() {
            candidates.push(ancestor.join("components/l0"));
        }
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.join(KIT_MARKER).is_file())
        .map(Kit::Found)
        .unwrap_or(Kit::Missing)
}

/// Checks one card. [`run`] is the command-line entry; this is the one-card
/// form a host embedding the checker would call.
pub fn check_file(path: &Path, inv: &Invocation, kit: Option<&Path>, lower: Option<&Lower>) -> FileReport {
    let mut report = FileReport::new(path);
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(e) => {
            report.error("l0.io", None, None, format!("cannot read: {e}"));
            return report;
        }
    };

    // The profile check, in every tier: bind and lift build on it, and its
    // lints are worth seeing whatever else is asked for.
    let profile = check_ui_l0_named(&path.display().to_string(), &source);
    report.level = Some(format!("{:?}", profile.level));
    report.valid = Some(profile.valid);
    report.parts.push(format!("{:?}", profile.level));
    if let Some(header) = &profile.header {
        for contradiction in &header.contradictions {
            report.error("l0.header", Some(1), Some(1), contradiction.clone());
        }
    }
    for diagnostic in &profile.diagnostics {
        report.error("l0.syntax", Some(diagnostic.line), Some(diagnostic.column), diagnostic.message.clone());
    }
    if profile.diagnostics_truncated {
        report.warn("l0.syntax", None, None, "diagnostics were truncated; fix the first ones and check again");
    }
    for lint in &profile.lints {
        let severity = if inv.strict { Severity::Error } else { Severity::Warn };
        report.push(severity, "l0.lint", Some(lint.line), Some(lint.column), lint.message.clone());
    }

    // Realization, when the card is a clean L0 card: it answers every name the
    // card reads, which is where an unresolvable or unguarded binding shows.
    // The data is loaded for the lift tier too — the lowering realizes the
    // same card against the same first frame.
    let mut data = serde_json::Value::Object(Default::default());
    if profile.valid && (inv.tier.runs_bind() || inv.tier.runs_lift()) {
        match load_data(path, &source, inv) {
            Ok(loaded) => data = loaded,
            Err(message) => {
                report.fatal = Some(message);
                return report;
            }
        }
    }
    if inv.tier.runs_bind() && profile.valid {
        let realized = realize(&source, &data, RealizeLimits::default());
        for diagnostic in &realized.diagnostics {
            report.error("l0.bind", Some(diagnostic.line), Some(diagnostic.column), diagnostic.message.clone());
        }
        if realized.truncated {
            report.error("l0.bind", None, None, "realization exceeded its resource limits (nodes, depth, collection size or work)");
        } else if realized.diagnostics.is_empty() {
            match &realized.root {
                Some(_) => report.parts.push(format!("realizes {} nodes", realized.nodes)),
                None => report.error("l0.bind", None, None, "realization produced no root"),
            }
        }
    }

    // The kit lowering: assemble the mood's palette chain and the kit body,
    // then evaluate the assembled source in the checked VM. Its failure is a
    // message, not a position: the fault is in the assembly, not the card.
    //
    // A lowering that is not available is a note when the tier was not asked
    // for by name — the default tier promises what is available, and an
    // embedder without a renderer is not a card's fault — and an error when it
    // was: the caller demanded a check that cannot run.
    if inv.tier.runs_lift() && profile.valid {
        match (kit, lower) {
            (Some(kit), Some(lower)) => match lower(&source, &data, kit) {
                Ok(outcome) => {
                    report.parts.push(format!(
                        "lowers {} nodes / {} texts / {} images{}",
                        outcome.nodes,
                        outcome.texts,
                        outcome.images,
                        if outcome.native_kit { " (native kit)" } else { "" }
                    ));
                    report.lift = Some(outcome);
                }
                Err(message) => report.error("l0.lift", None, None, message),
            },
            (Some(_), None) => {
                let message = "lift skipped: no kit lowering is linked into this build";
                if inv.tier_explicit {
                    report.error("l0.lift", None, None, message);
                } else {
                    report.notes.push(message.to_owned());
                }
            }
            (None, _) => report
                .notes
                .push("lift skipped: no kit directory found (pass --kit DIR or set OCTOSCRIPT_L0_KIT)".to_owned()),
        }
    }

    report
}

/// The data a card realizes against: the fixture, then every declared state
/// initial the fixture does not already carry. Seeding the initials is what
/// makes a fixtureless check meaningful — a `state` with an `initial` resolves
/// on the first frame, so its absence is a real finding and not a missing file.
fn load_data(card: &Path, source: &str, inv: &Invocation) -> Result<serde_json::Value, String> {
    let mut data = match &inv.data {
        Some(path) => read_json(path)?,
        None => {
            let sibling = card.with_extension("data.json");
            if sibling.is_file() {
                read_json(&sibling)?
            } else {
                serde_json::Value::Object(Default::default())
            }
        }
    };
    let Some(object) = data.as_object_mut() else {
        return Err("fixture data must be a JSON object of declaration name to value".to_owned());
    };
    let initials: BTreeMap<String, serde_json::Value> = state_initials(source);
    for (name, value) in initials {
        object.entry(name).or_insert(value);
    }
    Ok(data)
}

fn read_json(path: &Path) -> Result<serde_json::Value, String> {
    if !path.is_file() {
        return Err(format!("{}: no such file", path.display()));
    }
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn print_human(reports: &[FileReport], out: &mut dyn Write) {
    let mut ok = 0usize;
    let mut warn = 0usize;
    let mut error = 0usize;
    for report in reports {
        let status = match report.severity() {
            Some(Severity::Error) => {
                error += 1;
                "ERR "
            }
            Some(Severity::Warn) => {
                warn += 1;
                "warn"
            }
            None if report.fatal.is_some() => {
                error += 1;
                "ERR "
            }
            None => {
                ok += 1;
                "ok  "
            }
        };
        let name = report.path.display();
        if let Some(fatal) = &report.fatal {
            let _ = writeln!(out, "  {status} {name} — {fatal}");
            continue;
        }
        if report.parts.is_empty() {
            let _ = writeln!(out, "  {status} {name}");
        } else {
            let _ = writeln!(out, "  {status} {name} — {}", report.parts.join(", "));
        }
        for finding in &report.findings {
            let at = match (finding.line, finding.column) {
                (Some(line), Some(column)) => format!("{line}:{column} "),
                (Some(line), None) => format!("{line} "),
                _ => String::new(),
            };
            let _ = writeln!(out, "        {}{}", at, finding.message);
        }
        for note in &report.notes {
            let _ = writeln!(out, "        note: {note}");
        }
    }
    let _ = writeln!(out, "{TOOL_NAME}: {ok} ok, {warn} warn, {error} error");
}

fn print_json(reports: &[FileReport], inv: &Invocation, kit: Option<&Path>, out: &mut dyn Write) {
    let files: Vec<serde_json::Value> = reports
        .iter()
        .map(|report| {
            let findings: Vec<serde_json::Value> = report
                .findings
                .iter()
                .map(|finding| {
                    serde_json::json!({
                        "severity": finding.severity.as_str(),
                        "code": finding.code,
                        "line": finding.line,
                        "column": finding.column,
                        "message": finding.message,
                    })
                })
                .collect();
            serde_json::json!({
                "path": report.path.display().to_string(),
                "level": report.level,
                "valid": report.valid,
                "findings": findings,
                "lift": report.lift.as_ref().map(|lift| serde_json::json!({
                    "nodes": lift.nodes,
                    "texts": lift.texts,
                    "images": lift.images,
                    "native_kit": lift.native_kit,
                })),
                "notes": report.notes,
                "fatal": report.fatal,
            })
        })
        .collect();
    let (ok, warn, error) = tally(reports);
    let report = serde_json::json!({
        "tool": TOOL_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "tier": inv.tier.as_str(),
        "kit": kit.map(|kit| kit.display().to_string()),
        "files": files,
        "summary": {"ok": ok, "warn": warn, "error": error},
    });
    let _ = writeln!(out, "{}", serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_owned()));
}

fn tally(reports: &[FileReport]) -> (usize, usize, usize) {
    let mut ok = 0usize;
    let mut warn = 0usize;
    let mut error = 0usize;
    for report in reports {
        match report.severity() {
            Some(Severity::Error) => error += 1,
            Some(Severity::Warn) => warn += 1,
            None if report.fatal.is_some() => error += 1,
            None => ok += 1,
        }
    }
    (ok, warn, error)
}
