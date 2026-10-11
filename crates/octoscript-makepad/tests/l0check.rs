//! The l0check core's behaviour. Self-contained: every card is inline, so the
//! tests need no sibling checkout and no kit on disk, and every assertion is
//! about the core's own API rather than about what a checkout beside it holds.

use std::fs;
use std::path::{Path, PathBuf};

use octoscript_makepad::l0check::{self, FileReport, Invocation, Lower, Parsed, Severity, Tier};

/// A clean card: one state with an initial, one view. Reads its state, so the
/// bind tier has something to resolve.
const CLEAN: &str = "\
# level: L0
# profile: ui/l0

state greeting { shape: text, initial: \"hi\" }

view root Surface(pad: .page) {
  Col(gap: 8) {
    TextBody(text: greeting, width: .fill)
  }
}
";

/// The same card reading a name nothing declares. Rule 0 of the card↔host
/// contract: `check_ui_l0` refuses it, so this is a static finding.
const UNDECLARED_READ: &str = "\
# level: L0
# profile: ui/l0

view root Surface(pad: .page) {
  Col(gap: 8) {
    TextBody(text: mystery, width: .fill)
  }
}
";

/// A card whose state has no initial and no fixture answers it. On the
/// semantic path this is not an error — the binding draws an em dash and the
/// card keeps drawing; it is the KIT path (the lift tier) where the same fault
/// fails the whole card. The bind tier's job is the opposite: to prove the
/// card realizes to a tree at all.
const UNSEEDED_STATE: &str = "\
# level: L0
# profile: ui/l0

state greeting { shape: text }

view root Surface(pad: .page) {
  Col(gap: 8) {
    TextBody(text: greeting, width: .fill)
  }
}
";

/// A colour-font glyph in a text slot: valid, but invisible in one polarity.
const EMOJI: &str = "\
# level: L0
# profile: ui/l0

view root Surface(pad: .page) {
  Col(gap: 8) {
    TextBody(text: \"storm \u{1F326}\", width: .fill)
  }
}
";

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, text).expect("write fixture");
    path
}

fn invocation(paths: Vec<PathBuf>, tier: Tier) -> Invocation {
    Invocation { paths, tier, tier_explicit: false, data: None, kit: None, strict: false, json: false }
}

/// Runs the core and returns (exit status, stdout, stderr).
fn run(inv: &Invocation, lower: Option<&Lower>) -> (i32, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let status = l0check::run(inv, lower, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).expect("utf8 stdout"),
        String::from_utf8(err).expect("utf8 stderr"),
    )
}

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("l0check-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn check(path: &Path, tier: Tier) -> FileReport {
    l0check::check_file(path, &invocation(vec![path.to_path_buf()], tier), None, None)
}

fn codes(report: &FileReport) -> Vec<&'static str> {
    report.findings.iter().map(|f| f.code).collect()
}

#[test]
fn a_clean_card_passes_static_and_bind() {
    let dir = tempdir("clean");
    let card = write(&dir, "page.card", CLEAN);
    let report = check(&card, Tier::Bind);
    assert_eq!(report.valid, Some(true));
    assert_eq!(report.level.as_deref(), Some("L0"));
    assert!(report.findings.is_empty(), "{report:?}");
    assert!(report.parts.iter().any(|p| p.contains("realizes")), "{report:?}");
    let (status, stdout, _) = run(&invocation(vec![card], Tier::Bind), None);
    assert_eq!(status, 0);
    assert!(stdout.contains("ok  "), "{stdout}");
    assert!(stdout.contains("realizes"), "{stdout}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_undeclared_read_is_an_error_with_a_position() {
    let dir = tempdir("undeclared");
    let card = write(&dir, "page.card", UNDECLARED_READ);
    let report = check(&card, Tier::Static);
    assert_eq!(report.valid, Some(false));
    let finding = report
        .findings
        .iter()
        .find(|f| f.severity == Severity::Error)
        .expect("an error");
    assert_eq!(finding.code, "l0.syntax");
    assert!(finding.line.is_some() && finding.column.is_some(), "{finding:?}");
    assert!(finding.message.contains("mystery"), "{}", finding.message);
    let (status, stdout, _) = run(&invocation(vec![card], Tier::Static), None);
    assert_eq!(status, 1);
    assert!(stdout.contains("ERR "), "{stdout}");
    assert!(
        stdout.lines().any(|line| line.trim_start().starts_with(|c: char| c.is_ascii_digit())),
        "a positioned line: {stdout}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_theme_portability_lint_is_a_warning_unless_strict() {
    let dir = tempdir("emoji");
    let card = write(&dir, "page.card", EMOJI);
    let report = check(&card, Tier::Static);
    assert_eq!(report.valid, Some(true), "a lint is not a rule: {report:?}");
    assert_eq!(codes(&report), vec!["l0.lint"]);
    assert_eq!(report.findings[0].severity, Severity::Warn);
    let (status, stdout, _) = run(&invocation(vec![card.clone()], Tier::Static), None);
    assert_eq!(status, 0);
    assert!(stdout.contains("warn"), "{stdout}");
    let mut strict = invocation(vec![card], Tier::Static);
    strict.strict = true;
    let (status, _, _) = run(&strict, None);
    assert_eq!(status, 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_unseeded_state_still_realizes_on_the_semantic_path() {
    let dir = tempdir("unseeded");
    let card = write(&dir, "page.card", UNSEEDED_STATE);
    let report = check(&card, Tier::Bind);
    assert_eq!(report.valid, Some(true), "the profile check passes: {report:?}");
    assert!(report.findings.is_empty(), "the semantic path draws an em dash: {report:?}");
    assert!(report.parts.iter().any(|p| p.contains("realizes")), "{report:?}");
    let (status, stdout, _) = run(&invocation(vec![card], Tier::Bind), None);
    assert_eq!(status, 0);
    assert!(stdout.contains("realizes"), "{stdout}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_fixture_beside_the_card_is_read() {
    let dir = tempdir("fixture");
    let card = write(&dir, "page.card", CLEAN);
    // The observable proof the sibling fixture is read: a broken one is fatal,
    // and it names the file it read.
    write(&dir, "page.data.json", "{not json");
    let (status, _, stderr) = run(&invocation(vec![card.clone()], Tier::Bind), None);
    assert_eq!(status, 2, "{stderr}");
    assert!(stderr.contains("page.data.json"), "{stderr}");
    // A fixture that parses but is not an object of names is fatal too.
    write(&dir, "page.data.json", "[1, 2, 3]");
    let (status, _, stderr) = run(&invocation(vec![card.clone()], Tier::Bind), None);
    assert_eq!(status, 2, "{stderr}");
    assert!(stderr.contains("JSON object"), "{stderr}");
    // A good one loads, and the card binds clean.
    write(&dir, "page.data.json", "{\"greeting\": \"hello\"}");
    let report = check(&card, Tier::Bind);
    assert!(report.findings.is_empty(), "{report:?}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_directory_scan_finds_cards_and_skips_the_kit_subtree() {
    let dir = tempdir("scan");
    fs::create_dir_all(dir.join("kit/native/dark")).expect("mkdir");
    fs::create_dir_all(dir.join("nested")).expect("mkdir");
    write(&dir, "page.card", CLEAN);
    write(&dir, "nested/notes.l0", CLEAN);
    write(&dir, "main.splash", "Label{text: \"a script app is not an L0 card\"}");
    write(&dir, "kit/native/dark/components.l0", "not a card");
    let mut inv = invocation(vec![dir.clone()], Tier::Static);
    inv.json = true;
    let (status, stdout, _) = run(&inv, None);
    assert_eq!(status, 0);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    let paths: Vec<&str> = value["files"].as_array().expect("files").iter().map(|f| f["path"].as_str().expect("path")).collect();
    assert_eq!(paths.len(), 2, "{paths:?}");
    assert!(paths.iter().any(|p| p.ends_with("page.card")));
    assert!(paths.iter().any(|p| p.ends_with("notes.l0")));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_octoscript_file_is_scanned_only_when_it_declares_the_profile() {
    let dir = tempdir("octoscript");
    write(&dir, "trip.l0", CLEAN);
    write(&dir, "declared.octoscript", "# level: L0\n\nview root Surface { TextBody(text: \"hi\") }\n");
    write(&dir, "agent.octoscript", "let x = 1 + 1\n");
    let mut inv = invocation(vec![dir.clone()], Tier::Static);
    inv.json = true;
    let (_, stdout, _) = run(&inv, None);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    let paths: Vec<&str> = value["files"].as_array().expect("files").iter().map(|f| f["path"].as_str().expect("path")).collect();
    assert_eq!(paths.len(), 2, "{paths:?}");
    assert!(paths.iter().any(|p| p.ends_with("declared.octoscript")));
    assert!(!paths.iter().any(|p| p.ends_with("agent.octoscript")));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_json_report_is_the_contract() {
    let dir = tempdir("json");
    let card = write(&dir, "page.card", EMOJI);
    let mut inv = invocation(vec![card], Tier::Bind);
    inv.json = true;
    let (status, stdout, _) = run(&inv, None);
    assert_eq!(status, 0);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("json report");
    assert_eq!(value["tool"], "l0check");
    assert_eq!(value["tier"], "bind");
    assert!(value["kit"].is_string() || value["kit"].is_null());
    let file = &value["files"][0];
    assert_eq!(file["level"], "L0");
    assert_eq!(file["valid"], true);
    let finding = &file["findings"][0];
    assert_eq!(finding["severity"], "warn");
    assert_eq!(finding["code"], "l0.lint");
    assert!(finding["line"].is_u64() && finding["column"].is_u64());
    assert!(finding["message"].is_string());
    assert_eq!(value["summary"]["warn"], 1);
    assert_eq!(value["summary"]["error"], 0);
    let _ = fs::remove_dir_all(&dir);
}

/// A kit directory the checker accepts.
fn kit_dir(dir: &Path) -> PathBuf {
    let kit = dir.join("kit");
    fs::create_dir_all(&kit).expect("mkdir");
    write(&kit, "_kit.octoscript", "// the kit marker\n");
    kit
}

#[test]
fn a_lowering_outcome_is_reported() {
    let dir = tempdir("lower");
    let card = write(&dir, "page.card", CLEAN);
    let kit = kit_dir(&dir);
    let lower = |_source: &str, _data: &serde_json::Value, _kit: &Path| -> Result<l0check::LiftOutcome, String> {
        Ok(l0check::LiftOutcome { nodes: 41, texts: 12, images: 2, native_kit: false })
    };
    let mut inv = invocation(vec![card], Tier::All);
    inv.kit = Some(kit);
    let (status, stdout, stderr) = run(&inv, Some(&lower));
    assert_eq!(status, 0, "{stderr}");
    assert!(stdout.contains("lowers 41 nodes / 12 texts / 2 images"), "{stdout}");
    inv.json = true;
    let (_, stdout, _) = run(&inv, Some(&lower));
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(value["files"][0]["lift"]["nodes"], 41);
    assert_eq!(value["files"][0]["lift"]["texts"], 12);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_lowering_is_an_error_without_a_position() {
    let dir = tempdir("lower-fail");
    let card = write(&dir, "page.card", CLEAN);
    let kit = kit_dir(&dir);
    let lower = |_source: &str, _data: &serde_json::Value, _kit: &Path| -> Result<l0check::LiftOutcome, String> {
        Err("unresolved or unsupported kit property".to_owned())
    };
    let mut inv = invocation(vec![card], Tier::Lift);
    inv.kit = Some(kit);
    let (status, stdout, _) = run(&inv, Some(&lower));
    assert_eq!(status, 1);
    assert!(stdout.contains("unresolved or unsupported kit property"), "{stdout}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_lowering_is_an_error_not_a_silent_pass() {
    let dir = tempdir("no-lower");
    let card = write(&dir, "page.card", CLEAN);
    let kit = kit_dir(&dir);
    let mut inv = invocation(vec![card], Tier::Lift);
    inv.kit = Some(kit);
    let (status, stdout, _) = run(&inv, None);
    assert_eq!(status, 1);
    assert!(stdout.contains("no kit lowering is linked"), "{stdout}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_named_kit_that_is_not_one_is_a_usage_error() {
    let dir = tempdir("bad-kit");
    let card = write(&dir, "page.card", CLEAN);
    let mut inv = invocation(vec![card], Tier::Lift);
    inv.tier_explicit = true;
    inv.kit = Some(PathBuf::from("/definitely/not/a/kit"));
    let (status, _, stderr) = run(&inv, None);
    assert_eq!(status, 2);
    assert!(stderr.contains("is not an L0 kit"), "{stderr}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_default_tier_falls_back_to_what_a_host_will_run() {
    let dir = tempdir("default-tier");
    let card = write(&dir, "page.card", CLEAN);
    // The default tier with no kit resolved: the profile and realization run,
    // the lowering is a note, and the card is not failed for it. Checked at
    // the one-card level so the test does not depend on what kit, if any, a
    // checkout beside the test happens to hold.
    let mut inv = invocation(vec![card.clone()], Tier::All);
    inv.json = false;
    let report = l0check::check_file(&card, &inv, None, None);
    assert!(report.parts.iter().any(|p| p.contains("realizes")), "{report:?}");
    assert_eq!(report.notes, vec!["lift skipped: no kit directory found (pass --kit DIR or set OCTOSCRIPT_L0_KIT)".to_owned()]);
    assert!(report.severity().is_none(), "{report:?}");
    let (status, _, _) = run(&inv, None);
    assert_eq!(status, 0);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_path_is_a_usage_error() {
    let inv = invocation(vec![PathBuf::from("/definitely/not/here.card")], Tier::Static);
    let (status, _, stderr) = run(&inv, None);
    assert_eq!(status, 2);
    assert!(stderr.contains("/definitely/not/here.card"), "{stderr}");
}

#[test]
fn a_directory_with_no_cards_is_not_an_error() {
    let dir = tempdir("empty");
    let inv = invocation(vec![dir.clone()], Tier::Static);
    let (status, stdout, _) = run(&inv, None);
    assert_eq!(status, 0);
    assert!(stdout.contains("0 ok, 0 warn, 0 error"), "{stdout}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn usage_errors_name_the_problem() {
    for args in [
        vec!["--tier".to_owned(), "everything".to_owned(), "page.card".to_owned()],
        vec!["--data".to_owned()],
        vec!["--nonsense".to_owned(), "page.card".to_owned()],
        vec![],
    ] {
        assert!(l0check::parse(&args).is_err(), "{args:?}");
    }
    assert!(matches!(l0check::parse(&["--help".to_owned()]), Ok(Parsed::Help)));
    assert!(matches!(l0check::parse(&["--version".to_owned()]), Ok(Parsed::Version)));
    match l0check::parse(&[
        "--tier".to_owned(),
        "bind".to_owned(),
        "--strict".to_owned(),
        "--".to_owned(),
        "page.card".to_owned(),
    ]) {
        Ok(Parsed::Run(inv)) => {
            assert_eq!(inv.tier, Tier::Bind);
            assert!(inv.strict);
            assert!(inv.tier_explicit);
            assert_eq!(inv.paths, vec![PathBuf::from("page.card")]);
        }
        _ => panic!("a run"),
    }
}

#[test]
fn a_bad_fixture_is_a_usage_error() {
    let dir = tempdir("bad-fixture");
    let card = write(&dir, "page.card", CLEAN);
    let data = write(&dir, "data.json", "{not json");
    let mut inv = invocation(vec![card], Tier::Bind);
    inv.data = Some(data);
    let (status, _, stderr) = run(&inv, None);
    assert_eq!(status, 2, "{stderr}");
    let _ = fs::remove_dir_all(&dir);
}
