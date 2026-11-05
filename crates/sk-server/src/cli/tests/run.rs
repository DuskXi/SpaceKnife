//! `run`: the SpaceSniffer command line parser and headless command sequences (test_cli_run.py).

use std::path::{MAIN_SEPARATOR, Path, PathBuf};

use sk_core::i18n::{self, Arg, Language, t};

use super::super::rich::Console;
use super::super::run::{
    Command, ParserKind, command_tail, filter_reason, group, main, parse_command_line, split_line, split_tokens,
    tail_matches, text, usage, windows_command_tail,
};
use super::{lock_language, tree};

fn cmd(name: &str, args: &[&str]) -> Command {
    Command::new(name, args)
}

fn f(template: &str, fields: &[(&str, Arg)]) -> String {
    i18n::format(t(template), fields)
}

// ------------------------------------------------------------------------------------- parsing

#[test]
fn cli_run_internal_parser_keeps_backslashes_before_quotes() {
    let _g = lock_language(Language::ZhCn);
    let raw = r#"scan "C:\" filter "*.txt;>1mb" export "Simple file list" "C:\out dir\list.txt" autoclose"#;
    let (words, errors, to_os) = split_line(raw);
    assert_eq!(
        words,
        ["scan", "C:\\", "filter", "*.txt;>1mb", "export", "Simple file list", "C:\\out dir\\list.txt", "autoclose"]
    );
    assert!(errors.is_empty() && !to_os);
}

#[test]
fn cli_run_delimiter_meta_commands() {
    let _g = lock_language(Language::ZhCn);
    let (words, errors, _) = split_line(r#"@delim={} scan {C:\a "quoted" b} @delim=| filter |*.txt| export x y"#);
    assert_eq!(words, ["scan", r#"C:\a "quoted" b"#, "filter", "*.txt", "export", "x", "y"]);
    assert!(errors.is_empty());
    let (words, _, _) = split_line(r#"scan "a" "@delim={}" {b}"#); // a delimited meta command is a word
    assert_eq!(words, ["scan", "a", "@delim={}", "{b}"]);
    let (_, errors, _) = split_line("@delim=abc scan x");
    assert!(errors.len() == 1 && errors[0].contains("@delim=abc"));
    let (words, errors, _) = split_line(r#"scan "unclosed"#);
    assert_eq!(words, ["scan", "unclosed"]);
    assert_eq!(errors.len(), 1);
}

#[test]
fn cli_run_parser_os_reparses_from_scratch() {
    let _g = lock_language(Language::ZhCn);
    let tokens = ["@delim={}", "scan", "{a", "b}", "@parser=OS", "@parser=OS"];
    let parsed = parse_command_line(&tokens, Some("@delim={} scan {a b} @parser=OS @parser=OS"));
    assert_eq!(parsed.parser, ParserKind::Os); // the system's words: "{a" and "b}" are two of them
    assert_eq!(parsed.errors[0], f(text::META_AFTER_OS, &[("word", "@delim={}".into())]));
    assert!(parsed.errors.len() == 2 && parsed.errors[1].contains("b}"));
    let parsed = parse_command_line(&["@parser=OS", "scan", "{a", "autoclose"], None);
    assert!(parsed.parser == ParserKind::Os && parsed.errors.is_empty());
    assert_eq!(parsed.commands, [cmd("scan", &["{a"]), cmd("autoclose", &[])]);
}

#[test]
fn cli_run_token_parser_joins_delimited_tokens() {
    let _g = lock_language(Language::ZhCn);
    let (words, errors, _) = split_tokens(&["@delim={}", "scan", "{/a", "b}", "filter", "{x}y", "z"]);
    assert_eq!(words, ["scan", "/a b", "filter", "x", "y", "z"]);
    assert!(errors.is_empty());
    let (words, _, _) = split_tokens(&["scan", "/path with spaces", "autoclose"]); // already split by the shell
    assert_eq!(words, ["scan", "/path with spaces", "autoclose"]);
    let (words, errors, _) = split_tokens(&["\"never", "closed"]);
    assert!(words == ["never closed"] && errors.len() == 1);
}

#[test]
fn cli_run_grouping_and_order_checks() {
    let _g = lock_language(Language::ZhCn);
    let (commands, errors) = group(&["SCAN", "/a", "Filter", "*.txt", "export", "Summary", "o.txt", "autoclose", "help"]);
    assert_eq!(
        commands,
        [
            cmd("scan", &["/a"]),
            cmd("filter", &["*.txt"]),
            cmd("export", &["Summary", "o.txt"]),
            cmd("autoclose", &[]),
            cmd("help", &[]),
        ]
    );
    assert!(errors.is_empty());
    let (_, errors) = group(&["filter", "*.txt", "scan", "/a"]);
    assert_eq!(errors, [f(text::NO_VIEW, &[("name", "filter".into())])]);
    let (_, errors) = group(&["scan", "/a", "frobnicate", "export", "x"]);
    assert!(errors.len() == 2 && errors[0].contains("frobnicate") && errors[1].contains("export"));
}

#[test]
fn cli_run_raw_command_line_tail() {
    let raw = r#""C:\Program Files\Python\python.exe" -m spaceknife run scan "C:\" autoclose"#;
    assert_eq!(command_tail(raw, "run"), Some(r#" scan "C:\" autoclose"#));
    assert_eq!(command_tail(r#""C:\run\spaceknife.exe" RUN help"#, "run"), Some(" help"));
    assert_eq!(command_tail("spaceknife scan x", "run"), None);
    assert!(tail_matches(r#" scan "C:\" autoclose"#, &["scan", "C:\" autoclose"]));
    assert!(!tail_matches(" -k something", &["scan"]));
    assert!(tail_matches("   ", &[] as &[&str]) && !tail_matches(" scan", &[] as &[&str]));
    // the test runner's own command line has no "run …" with these words
    assert_eq!(windows_command_tail(&["scan", "zzz-not-there"]), None);
}

#[test]
fn cli_run_documented_delimiter_example() {
    let _g = lock_language(Language::ZhCn);
    let raw = r#"@delim={} scan {D:\Photos} filter {"a;b.jpg";>1mb} export {Grouped by folder} {D:\out\files.txt}"#;
    let parsed = parse_command_line(&[] as &[&str], Some(raw));
    assert!(parsed.errors.is_empty());
    assert_eq!(
        parsed.commands,
        [
            cmd("scan", &[r"D:\Photos"]),
            cmd("filter", &[r#""a;b.jpg";>1mb"#]),
            cmd("export", &["Grouped by folder", r"D:\out\files.txt"]),
        ]
    );
}

// ------------------------------------------------------------------------------------- running

fn run(tokens: &[&str]) -> (i32, String, String) {
    let mut out = Console::capture(200);
    let mut err = Console::capture(200);
    let code = main(tokens, None, &mut out, &mut err);
    (code, out.take(), err.take())
}

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tree");
    tree(
        &root,
        &[("docs/a.txt", 1200), ("docs/b.txt", 300), ("docs/deep/c.txt", 50), ("media/v.bin", 9000), ("readme.md", 10)],
    );
    std::fs::create_dir(root.join("empty")).unwrap();
    (dir, root)
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path).unwrap().lines().map(str::to_owned).collect()
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[test]
fn cli_run_scan_filter_export_save_load() {
    let _g = lock_language(Language::ZhCn);
    let (dir, tree) = fixture();
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let snap = out.join("tree.sks");
    let (list, csv, json, html) = (out.join("list.txt"), out.join("t.csv"), out.join("t.json"), out.join("t.html"));
    let (code, stdout, stderr) = run(&[
        "scan", &s(&tree), "filter", "*.txt", "export", "Simple file list", &s(&list), "export", "csv", &s(&csv), "export",
        "json", &s(&json), "export", "html", &s(&html), "save", &s(&snap), "autoclose",
    ]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, ""); // autoclose: no view summary
    let mut expected: Vec<String> =
        vec![s(&tree.join("docs").join("a.txt")), s(&tree.join("docs").join("b.txt")), s(&tree.join("docs").join("deep").join("c.txt"))];
    let mut got = lines(&list);
    expected.sort();
    got.sort();
    assert_eq!(got, expected);
    let raw_csv = std::fs::read(&csv).unwrap();
    assert!(raw_csv.starts_with(b"\xef\xbb\xbf")); // BOM: Excel reads UTF-8
    let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    assert_eq!(doc["filter"], "*.txt");
    assert_eq!(doc["roots"][0]["path"], s(&tree));
    let page = std::fs::read_to_string(&html).unwrap();
    assert!(page.starts_with("<!doctype html>") && page.contains("a.txt") && !page.contains("v.bin"));
    // the snapshot holds the whole tree, not the filtered view
    let (store, header) = sk_store::snapshot::load(&snap).unwrap();
    assert_eq!(store.len() as u64, header.node_count);
    assert_eq!(header.roots, [s(&tree)]);
    assert_eq!(header.node_count - 1, 10);
    assert_eq!(header.stats["files"].as_i64(), Some(5));

    let all = out.join("all.txt");
    let (code, _, stderr) = run(&["load", &s(&snap), "export", "Simple file list", &s(&all), "autoclose"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(lines(&all).len(), 5);
    let again = out.join("again.txt");
    let (code, _, stderr) = run(&["scan", &s(&tree), "export", "Simple file list", &s(&again), "autoclose"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(lines(&again), lines(&all)); // a loaded snapshot exports like the scan
}

#[test]
fn cli_run_template_file_and_view_summary() {
    let _g = lock_language(Language::ZhCn);
    let (dir, tree) = fixture();
    let template = dir.path().join("mine.sktpl");
    std::fs::write(&template, "@sort=name\n@order=asc\n@detail\n{nest{ }}<%file%>|<%filecount%>{&br}").unwrap();
    let target = dir.path().join("mine.txt");
    let (code, stdout, stderr) = run(&["scan", &s(&tree), "export", &s(&template), &s(&target)]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        lines(&target),
        [
            "tree|5", " docs|3", "  a.txt|0", "  b.txt|0", "  deep|1", "   c.txt|0", " empty|0", " media|1", "  v.bin|0",
            " readme.md|0",
        ]
    );
    // without autoclose the views are summarised on stdout (the headless stand-in for a window)
    assert!(stdout.contains(&f(text::VIEW_TITLE, &[("index", 1.into()), ("label", format!("scan {}", s(&tree)).into())])));
    assert!(stdout.contains(&format!("docs{MAIN_SEPARATOR}")) && stdout.contains("readme.md"), "{stdout}");
}

#[test]
fn cli_run_counts_of_one_in_english() {
    // English says "1 item", "1 file", "1 folder" (#1); Chinese has no plural forms
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one");
    std::fs::create_dir_all(one.join("sub")).unwrap();
    std::fs::write(one.join("a.txt"), b"x").unwrap();
    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let saved = dir.path().join("one.sks");
    let tokens = ["scan", &s(&empty), "scan", &s(&one), "filter", "*.txt", "save", &s(&saved), "load", &s(&saved)];
    let (code, stdout, stderr) = {
        let _g = lock_language(Language::En);
        run(&tokens)
    };
    assert_eq!(code, 0, "{stderr}");
    let scanned: Vec<String> = stderr
        .lines()
        .filter(|l| l.starts_with("Scanned "))
        .map(|l| l.split_once(": ").unwrap().1.split(',').next().unwrap().to_owned())
        .collect();
    assert_eq!(scanned, ["1 item", "3 items"]); // the folder itself counts
    assert!(stderr.contains("Filter *.txt: 1 file, "), "{stderr}");
    assert!(stderr.contains(&format!("Opened snapshot {}: 3 items, ", s(&saved))), "{stderr}");
    let totals: Vec<&str> =
        stdout.lines().filter(|l| l.ends_with(" on disk")).map(|l| l.rsplit_once(", ").unwrap().0).collect();
    assert_eq!(totals, ["0 files, 0 folders", "1 file, 0 folders", "1 file, 1 folder"]); // the three views

    let _g = lock_language(Language::ZhCn);
    let (code, stdout, stderr) = run(&tokens);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("过滤 *.txt：1 个文件，"), "{stderr}");
    assert!(stdout.contains("共 1 个文件、1 个文件夹，占用 "), "{stdout}");
}

#[test]
fn cli_run_several_roots_in_one_scan() {
    let _g = lock_language(Language::ZhCn);
    let (dir, tree) = fixture();
    let target = dir.path().join("two.txt");
    let both = format!("{};{}", s(&tree.join("docs")), s(&tree.join("media")));
    let (code, _, stderr) = run(&["scan", &both, "export", "Summary", &s(&target), "autoclose"]);
    assert_eq!(code, 0, "{stderr}");
    let text = std::fs::read_to_string(&target).unwrap();
    assert_eq!(text.matches("Total size:").count(), 2); // one export per root of the view
    assert!(text.contains("Files     : 3") && text.contains("Files     : 1"), "{text}");
    let odd = dir.path().join("a;b");
    std::fs::create_dir(&odd).unwrap();
    std::fs::write(odd.join("f.txt"), b"1").unwrap();
    let (code, _, stderr) = run(&["scan", &s(&odd), "export", "Simple file list", &s(&target), "autoclose"]);
    assert_eq!(code, 0, "{stderr}"); // a folder whose name holds ";" is one path
    assert_eq!(lines(&target), [s(&odd.join("f.txt"))]);
}

#[test]
fn cli_run_errors_stop_before_anything_runs() {
    let _g = lock_language(Language::ZhCn);
    let (dir, tree) = fixture();
    let target = dir.path().join("never.txt");
    let (code, _, stderr) = run(&["scan", &s(&tree), "export", "Summary", &s(&target), "bogus"]);
    assert!(code == 2 && stderr.contains("bogus") && !target.exists());
    let (code, _, stderr) = run(&["export", "Summary", &s(&target)]);
    assert!(code == 2 && stderr.contains("export"));
    let (code, _, stderr) = run(&["scan", &s(&tree), "export", "Summary"]);
    assert!(code == 2 && stderr.contains(t(usage("export"))), "{stderr}");
}

#[test]
fn cli_run_failing_commands() {
    let _g = lock_language(Language::ZhCn);
    let (dir, tree) = fixture();
    let target = dir.path().join("x.txt");
    let (code, _, stderr) = run(&["scan", &s(&dir.path().join("missing")), "autoclose"]);
    assert!(code == 2 && stderr.contains("missing"));
    let (code, _, stderr) = run(&["scan", &s(&tree), "filter", ">1zb", "export", "Summary", &s(&target), "autoclose"]);
    assert!(code == 2 && stderr.contains(">1zb") && stderr.contains(t(filter_reason("unknown_unit"))), "{stderr}");
    assert!(!target.exists());
    let (code, _, stderr) = run(&["scan", &s(&tree), "export", "No such template", &s(&target), "autoclose"]);
    assert!(code == 2 && stderr.contains("No such template"));
    let broken = dir.path().join("broken.sktpl");
    std::fs::write(&broken, "<%nosuch%>").unwrap();
    let (code, _, stderr) = run(&["scan", &s(&tree), "export", &s(&broken), &s(&target), "autoclose"]);
    assert!(code == 2 && stderr.contains("nosuch") && !target.exists(), "{stderr}");
    let (code, _, _) = run(&["scan", &s(&tree), "export", "Summary", &s(&dir.path().join("no").join("dir.txt")), "autoclose"]);
    assert_eq!(code, 2);
    let fake = dir.path().join("old.sns");
    std::fs::write(&fake, b"SpaceSniffer binary snapshot\x00\x01").unwrap();
    let (code, _, stderr) = run(&["load", &s(&fake), "autoclose"]);
    assert!(code == 2 && stderr.contains(".sns"), "{stderr}");
    let (code, _, stderr) = run(&["load", &format!("{};{}", s(&fake), s(&fake)), "autoclose"]);
    assert!(code == 2 && stderr.contains(t(text::LOAD_MANY)));
}

#[test]
fn cli_run_filter_warnings_do_not_stop() {
    let _g = lock_language(Language::ZhCn);
    let (dir, tree) = fixture();
    let target = dir.path().join("w.txt");
    let (code, _, stderr) = run(&["scan", &s(&tree), "filter", ":attr:ads", "export", "Summary", &s(&target), "autoclose"]);
    assert_eq!(code, 0, "{stderr}"); // "ads" is not collected yet: a warning, the filter matches nothing
    assert!(stderr.contains(t(filter_reason("unsupported"))), "{stderr}");
    assert!(std::fs::read_to_string(&target).unwrap().contains("Files     : 0"));
}

#[test]
fn cli_run_help_and_empty_command_line() {
    let _g = lock_language(Language::En);
    let (code, stdout, _) = run(&["help"]);
    assert!(code == 0 && stdout.contains("spaceknife run") && stdout.contains("Grouped by folder") && stdout.contains("@parser=OS"));
    let (code, stdout2, _) = run(&[]);
    assert!(code == 0 && stdout2 == stdout);
}

#[test]
fn cli_run_wiring() {
    // the arguments after `run` reach the parser untouched (clap does not eat --flags or quotes)
    let cli = <super::super::Cli as clap::Parser>::try_parse_from(["x", "run", "scan", "--weird", "filter", "-x"]).unwrap();
    let super::super::Command::Run { args } = cli.command else { panic!() };
    assert_eq!(args, ["scan", "--weird", "filter", "-x"]);
}
