//! Export templates (SpaceSniffer's language), the built-in templates and the CSV / JSON / HTML
//! formats (port of packages/backend/tests/test_export.py).

use std::path::Path;

use chrono::TimeZone;
use sk_core::export::{
    self, BUILTINS, Blocks, ExportError, ExportTemplate, ExportTree, HtmlOptions, LocalTimes, Ordering, Severity,
    SortKey, Target, builtin_info, builtin_template, check, compare, export_csv, export_html, export_json, iso_time,
    parse_template, render, render_chunks, resolve, tag_value, write_export,
};
use sk_store::testing::{Spec, build_with, d, f, find};
use sk_store::{Entry, EntryBatch, Flags, Kind, NodeId, Nodes, Store, Times};

const R: &str = "/data";
const SEP: char = std::path::MAIN_SEPARATOR;

fn j(parts: &[&str]) -> String {
    let mut out = R.to_owned();
    for p in parts {
        out.push(SEP);
        out.push_str(p);
    }
    out
}

fn stamp() -> i64 {
    chrono::Local.with_ymd_and_hms(2024, 3, 5, 14, 7, 9).single().unwrap().timestamp()
}

fn now() -> f64 {
    (stamp() + 10 * 86400 + 5) as f64
}

fn spec() -> Spec {
    d([
        ("a", d([("one.bin", f(1000)), ("Two.TXT", f(5000)), ("sub", d([("movie.mp4", f(123456))]))])),
        ("c.txt", f(10)),
        (".bashrc", f(7)),
        ("archive.tar.gz", f(3)),
        ("empty", d([])),
    ])
}

/// alloc = size rounded up to 4 KB: root 148 KB, a 136 KB, sub 124 KB, every small file 4 KB.
fn fixture() -> Store {
    let store = build_with(&spec(), R, 4096, &[]);
    {
        let two = find(&store.read(), "a/Two.TXT");
        let mut w = store.write();
        w.set_times(two, Times { mtime: stamp() as u32, ctime: 0, atime: (stamp() + 3600) as u32 });
    }
    store
}

fn root_of(nodes: &Nodes) -> NodeId {
    nodes.roots().next().unwrap()
}

fn tpl(source: &str) -> ExportTemplate {
    parse_template(source, "")
}

fn r(nodes: &Nodes, root: NodeId, source: &str) -> String {
    render(nodes, root, &tpl(source), None, Some(now())).unwrap()
}

fn rm(nodes: &Nodes, root: NodeId, source: &str, mask: &[bool]) -> String {
    render(nodes, root, &tpl(source), Some(mask), Some(now())).unwrap()
}

fn builtin(name: &str) -> &'static str {
    builtin_info(name).unwrap().source
}

/// Files whose name ends with `suffix` (case-insensitive): a stand-in for a filter mask.
fn mask(nodes: &Nodes, suffix: &str) -> Vec<bool> {
    (0..nodes.len() as NodeId)
        .map(|i| !nodes.is_dir(i) && nodes.name(i).to_lowercase().ends_with(suffix))
        .collect()
}

fn tag_with(nodes: &Nodes, rel: &str, name: &str, m: Option<&[bool]>) -> String {
    let target = if rel.is_empty() { R.to_owned() } else { j(&rel.split('/').collect::<Vec<_>>()) };
    let tree = ExportTree::new(nodes, root_of(nodes), m, Ordering::default(), false, Some(now())).unwrap();
    let mut out = None;
    tree.walk(|el| -> Result<(), ()> {
        if export::text_of(el.path) == target {
            out = tag_value(&tree, el, name);
        }
        Ok(())
    })
    .unwrap();
    out.unwrap_or_else(|| panic!("no {rel} / {name}"))
}

fn tag(nodes: &Nodes, rel: &str, name: &str) -> String {
    tag_with(nodes, rel, name, None)
}

fn size(n: i64) -> String {
    // fmt.size of the platform: 1024-based except on macOS
    let base = if cfg!(target_os = "macos") { 1000.0 } else { 1024.0 };
    let units = ["B", "KB", "MB", "GB"];
    let (mut v, mut u) = (n as f64, 0);
    while v >= base && u < 3 {
        v /= base;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", units[u])
    } else {
        format!("{v:.1} {}", units[u])
    }
}

// ------------------------------------------------------------------------------ tags

#[test]
fn name_and_path_tags() {
    let s = fixture();
    let n = s.read();
    assert_eq!(tag(&n, "a/Two.TXT", "file"), "Two.TXT");
    assert_eq!(tag(&n, "a/Two.TXT", "filename"), "Two");
    assert_eq!(tag(&n, "a/Two.TXT", "fileext"), "TXT");
    assert_eq!(tag(&n, "a/Two.TXT", "path"), j(&["a"]));
    assert_eq!(tag(&n, "a/Two.TXT", "pathfile"), j(&["a", "Two.TXT"]));
    assert_eq!((tag(&n, "archive.tar.gz", "filename"), tag(&n, "archive.tar.gz", "fileext")), ("archive.tar".into(), "gz".into()));
    assert_eq!((tag(&n, ".bashrc", "filename"), tag(&n, ".bashrc", "fileext")), (".bashrc".into(), "".into()));
    assert_eq!((tag(&n, "a/sub", "filename"), tag(&n, "a/sub", "fileext")), ("sub".into(), "".into()));
    assert_eq!(tag(&n, "", "file"), "data");
    assert_eq!(tag(&n, "", "pathfile"), R);
    assert_eq!(tag(&n, "", "path"), "/");
}

#[test]
fn size_tags() {
    let s = fixture();
    let n = s.read();
    assert_eq!(tag(&n, "a/Two.TXT", "size"), size(5000));
    assert_eq!(tag(&n, "a/Two.TXT", "sizebytes"), "5000");
    assert_eq!(tag(&n, "a/Two.TXT", "disksize"), size(8192));
    assert_eq!(tag(&n, "a/Two.TXT", "disksizebytes"), "8192");
    assert_eq!(tag(&n, "a", "sizebytes"), (1000 + 5000 + 123456).to_string());
    assert_eq!(tag(&n, "a", "disksizebytes"), (4096 + 8192 + 126976).to_string());
    assert_eq!(tag(&n, "", "disksizebytes"), "151552");
}

#[test]
fn time_tags() {
    let s = fixture();
    let n = s.read();
    assert_eq!(tag(&n, "a/Two.TXT", "filemodifydate"), "2024-03-05");
    assert_eq!(tag(&n, "a/Two.TXT", "filemodifytime"), "14:07:09");
    assert_eq!(tag(&n, "a/Two.TXT", "filemodifyage"), "10");
    assert_eq!(tag(&n, "a/Two.TXT", "fileaccesstime"), "15:07:09");
    for unknown in ["filecreatedate", "filecreatetime", "filecreateage"] {
        assert_eq!(tag(&n, "a/Two.TXT", unknown), "");
    }
}

#[test]
fn kind_and_count_tags() {
    let s = fixture();
    let n = s.read();
    for (k, want) in [
        ("isfile", "true"),
        ("isfolder", "false"),
        ("iscontainer", "false"),
        ("containsfiles", "false"),
        ("containsfolders", "false"),
        ("containscontainers", "false"),
    ] {
        assert_eq!(tag(&n, "a/Two.TXT", k), want, "{k}");
    }
    for (k, want) in [
        ("count", "4"),
        ("localcount", "3"),
        ("filecount", "3"),
        ("localfilecount", "2"),
        ("foldercount", "1"),
        ("localfoldercount", "1"),
        ("containercount", "1"),
        ("localcontainercount", "1"),
        ("containsfiles", "true"),
        ("containsfolders", "true"),
        ("isfolder", "true"),
        ("iscontainer", "true"),
    ] {
        assert_eq!(tag(&n, "a", k), want, "{k}");
    }
    for (k, want) in [("count", "9"), ("localcount", "5"), ("filecount", "6"), ("foldercount", "3")] {
        assert_eq!(tag(&n, "", k), want, "{k}");
    }
    assert_eq!(tag(&n, "a/sub", "containsfolders"), "false");
    assert_eq!(tag(&n, "empty", "containsfiles"), "false");
    let levels: Vec<String> = ["", "a", "a/sub", "a/sub/movie.mp4"].iter().map(|p| tag(&n, p, "nestinglevel")).collect();
    assert_eq!(levels, ["0", "1", "2", "3"]);
    assert_eq!(tag(&n, "a/Two.TXT", "count"), "0");
}

#[test]
fn tags_are_case_insensitive_and_trimmed() {
    let s = fixture();
    let n = s.read();
    let sub = find(&n, "a/sub");
    assert_eq!(r(&n, sub, "<%IsFile%>,"), "false,true,");
    let t = tpl("<% PathFile %>{&br}");
    assert!(t.ok());
    let out = render(&n, sub, &t, None, None).unwrap();
    assert_eq!(out.lines().collect::<Vec<_>>(), [j(&["a", "sub"]), j(&["a", "sub", "movie.mp4"])]);
    assert_eq!(r(&n, sub, "< %file%>"), "< %file%>".repeat(2));
}

#[test]
fn unknown_and_unclosed_tags() {
    let s = fixture();
    let n = s.read();
    let root = root_of(&n);
    let t = tpl("x<%nosuch%>{&br}");
    assert_eq!(t.issues.iter().map(|i| i.code).collect::<Vec<_>>(), ["unknown_tag"]);
    assert_eq!((t.issues[0].line, t.issues[0].column, t.issues[0].severity), (1, 2, Severity::Error));
    assert!(t.issues[0].message.contains("nosuch"));
    assert_eq!(render(&n, root, &t, None, None).unwrap(), "x*BAD TAG*\n".repeat(10));
    let t = tpl("a<%file");
    assert_eq!(t.issues.iter().map(|i| i.code).collect::<Vec<_>>(), ["unclosed_tag"]);
    assert!(render(&n, root, &t, None, None).unwrap().starts_with("a<%filea<%file"));
}

// ------------------------------------------------------------------------------ functions

#[test]
fn entities_escape_the_syntax() {
    let s = fixture();
    let n = s.read();
    let one = find(&n, "a/sub");
    let out = r(&n, one, "{if{<%isfile%>}=={true}?{{&curlyop}<%file%>{&curlycl}{&tab}{&angleop}%x%{&anglecl}{&br}}}");
    assert_eq!(out, "{movie.mp4}\t<%x%>\n");
    assert_eq!(r(&n, one, "{ &br }"), "\n\n");
    let t = tpl("{&nope}");
    assert_eq!(t.issues.iter().map(|i| (i.code, i.severity)).collect::<Vec<_>>(), [("unknown_entity", Severity::Warning)]);
    assert_eq!(render(&n, one, &t, None, None).unwrap(), "&nope&nope");
}

#[test]
fn if_comparisons() {
    assert!(compare("10", ">", "9"));
    assert!(compare("1.50", "==", "1.5"));
    assert!(!compare("10", "<", "9"));
    assert!(compare("b", ">", "a"));
    assert!(compare("JPG", "==", "jpg"));
    assert!(compare(" true ", "==", "true"));
    assert!(compare("a", "!=", "b"));
    assert!(compare("2", "<=", "2"));
    assert!(compare("2", ">=", "2"));
    assert!(!compare("abc", "<", "abb"));
    assert!(compare("-2", "<", "1"));
    assert!(compare(".5", "==", "0.50"));
    assert!(compare("-0", "==", "0"));
}

#[test]
fn if_branches_and_nesting() {
    let s = fixture();
    let n = s.read();
    let sub = find(&n, "a/sub");
    assert_eq!(r(&n, sub, "{if{<%isfile%>}=={true}?{F}:{D}}"), "DF");
    assert_eq!(r(&n, sub, "{if{<%isfile%>}!={true}?{D}}"), "D");
    assert_eq!(
        r(&n, sub, "{if{<%nestinglevel%>}<{1}?{root}:{if{<%fileext%>}=={MP4}?{video}:{other}}}"),
        "rootvideo"
    );
    for (op, want) in [("==", "0"), ("!=", "1"), ("<", ""), ("<=", "0"), (">", "1"), (">=", "01")] {
        assert_eq!(r(&n, sub, &format!("{{if{{<%nestinglevel%>}}{op}{{0}}?{{<%nestinglevel%>}}}}")), want, "{op}");
    }
}

#[test]
fn bad_if_is_reported_and_marked() {
    let s = fixture();
    let n = s.read();
    let sub = find(&n, "a/sub");
    for text in ["{if{a}{b}?{c}}", "{if{a}=={b}{c}}", "{if{a}==?{c}}", "{if{a}=={b}?{c}:}", "{if{a}=={b}?{c}x}", "{if}"] {
        let t = tpl(text);
        assert_eq!(t.issues.iter().map(|i| i.code).collect::<Vec<_>>(), ["bad_if"], "{text}");
        assert_eq!(render(&n, sub, &t, None, None).unwrap(), "*BAD IF*".repeat(2), "{text}");
    }
    assert_eq!(r(&n, sub, "{if{a}=={b}x}<%file%>"), "*BAD IF*sub*BAD IF*movie.mp4");
}

#[test]
fn padding() {
    let s = fixture();
    let n = s.read();
    let sub = find(&n, "a/sub");
    let detail = "{if{<%isfile%>}=={true}?{[{leftpad{<%file%>}{.}{12}}][{rightpad{<%file%>}{-}{12}}]}}";
    assert_eq!(r(&n, sub, detail), "[...movie.mp4][movie.mp4---]");
    assert_eq!(r(&n, sub, "{if{<%isfile%>}=={true}?{{leftpad{ab}{xy}{7}}}}"), "xyxyxab");
    assert_eq!(r(&n, sub, "{if{<%isfile%>}=={true}?{{leftpad{<%file%>}{ }{3}}}}"), "movie.mp4");
    assert_eq!(r(&n, sub, "{if{<%isfile%>}=={true}?{{leftpad{x}{ }{ 4 }}}}"), "   x");
    for bad in ["{leftpad{x}{}{4}}", "{leftpad{x}{ }{four}}", "{rightpad{x}{ }{-1}}", "{leftpad{x}{ }{99999999}}"] {
        let t = tpl(bad);
        assert!(t.ok(), "{bad}");
        assert_eq!(render(&n, sub, &t, None, None).unwrap(), "*BAD PAD*".repeat(2), "{bad}");
    }
    let t = tpl("{leftpad{x}{ }}");
    assert_eq!(t.issues.iter().map(|i| i.code).collect::<Vec<_>>(), ["bad_call"]);
    assert_eq!(render(&n, sub, &t, None, None).unwrap(), "*BAD PAD*".repeat(2));
}

#[test]
fn nest_repeats_by_level() {
    let s = fixture();
    let n = s.read();
    let out = r(&n, find(&n, "a"), "{nest{--}}<%file%>{&br}");
    assert_eq!(out.lines().collect::<Vec<_>>(), ["a", "--sub", "----movie.mp4", "--Two.TXT", "--one.bin"]);
}

#[test]
fn escape_functions() {
    let s = fixture();
    let n = s.read();
    let sub = find(&n, "a/sub");
    let detail = "{if{<%isfile%>}=={true}?{{html{<a href=\"x\">&</a>}}|{csv{a,\"b\"}}|{csv{plain}}|{json{<%file%>\"}}}}";
    assert_eq!(
        r(&n, sub, detail),
        "&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;|\"a,\"\"b\"\"\"|plain|\"movie.mp4\\\"\""
    );
}

#[test]
fn groups_and_unbalanced_braces() {
    let s = fixture();
    let n = s.read();
    let sub = find(&n, "a/sub");
    assert_eq!(r(&n, sub, "{x{y}z}"), "xyz".repeat(2));
    assert_eq!(r(&n, sub, "{{}if not a call}"), "if not a call".repeat(2));
    let t = tpl("a}b");
    assert_eq!(t.issues.iter().map(|i| (i.code, i.severity)).collect::<Vec<_>>(), [("stray_close", Severity::Warning)]);
    assert_eq!(render(&n, sub, &t, None, None).unwrap(), "a}b".repeat(2));
    let t = tpl("{a");
    assert_eq!(t.issues.iter().map(|i| i.code).collect::<Vec<_>>(), ["unclosed_group"]);
    assert_eq!(render(&n, sub, &t, None, None).unwrap(), "aa");
}

#[test]
fn script_mode_drops_line_breaks_and_tabs() {
    let s = fixture();
    let n = s.read();
    let sub = find(&n, "a/sub");
    assert_eq!(r(&n, sub, "{script}\n<%file%>\n\t[ x ]\r\n"), "sub[ x ]movie.mp4[ x ]");
    assert_eq!(r(&n, sub, "<%file%>\nx"), "sub\nxmovie.mp4\nx");
    let t = tpl("{script}\n\n  <%nope%>");
    assert_eq!((t.issues[0].line, t.issues[0].column), (3, 3));
}

// ------------------------------------------------------------------------------ template files

#[test]
fn template_file_sections_and_options() {
    let s = fixture();
    let n = s.read();
    let source = "@name=Mine\r\n# a comment\r\n@file=out.txt\r\n@blocks=folders\r\n@sort=name\r\n@order=asc\r\n\
                  @header\r\nHEAD <%file%>\r\n\r\n@detail\r\n<%nestinglevel%>\r\n@footer\r\nEND\r\n";
    let t = parse_template(&format!("\u{feff}{source}"), "");
    assert!(t.ok() && t.issues.is_empty(), "{:?}", t.issues);
    assert_eq!((t.name.as_str(), t.file_name.as_str()), ("Mine", "out.txt"));
    assert_eq!(t.ordering, Ordering { blocks: Blocks::Folders, sort: SortKey::Name, descending: false });
    assert_eq!((t.header.as_str(), t.detail.as_str(), t.footer.as_str()), ("HEAD <%file%>\n", "<%nestinglevel%>", "END"));
    assert_eq!(render(&n, find(&n, "a"), &t, None, None).unwrap(), "HEAD a\n01211END");
}

#[test]
fn template_file_diagnostics() {
    let t = tpl("@sort=sideways\n@colour=red\nnot an option\n@detail\nx\n@detail\n<%bad%>");
    assert_eq!(
        t.issues.iter().map(|i| (i.code, i.line)).collect::<Vec<_>>(),
        [("bad_option", 1), ("unknown_option", 2), ("bad_line", 3), ("duplicate_section", 6), ("unknown_tag", 7)]
    );
    assert!(t.issues[0].message.contains("'sideways'"), "{}", t.issues[0].message);
    assert_eq!(t.detail, "<%bad%>");
    assert_eq!(t.ordering, Ordering::default());
    assert!(check("<%file%>").is_empty());
}

#[test]
fn bare_template_and_final_line_break() {
    let s = fixture();
    let n = s.read();
    let sub = find(&n, "a/sub");
    assert_eq!(r(&n, sub, "<%file%>\n"), "submovie.mp4");
    assert_eq!(r(&n, sub, "<%file%>\n\n"), "sub\nmovie.mp4\n");
    assert_eq!(r(&n, sub, ""), "");
    assert_eq!(tpl("# <%file%>").detail, "# <%file%>");
}

#[test]
fn ordering() {
    let s = fixture();
    let n = s.read();
    let root = root_of(&n);
    let names = "{if{<%nestinglevel%>}=={1}?{<%file%> }}";
    let with = |head: &str| r(&n, root, &format!("{head}{names}"));
    assert_eq!(r(&n, root, names), "a c.txt .bashrc archive.tar.gz empty ");
    assert_eq!(with("@blocks=files\n@detail\n"), "c.txt .bashrc archive.tar.gz a empty ");
    assert_eq!(with("@blocks=folders\n@detail\n"), "a empty c.txt .bashrc archive.tar.gz ");
    assert_eq!(with("@sort=name\n@order=asc\n@detail\n"), ".bashrc a archive.tar.gz c.txt empty ");
    assert_eq!(with("@sort=name\n@detail\n"), "empty c.txt archive.tar.gz a .bashrc ");
    assert_eq!(with("@sort=none\n@detail\n"), "a c.txt .bashrc archive.tar.gz empty ");
    assert_eq!(with("@sort=size\n@order=asc\n@detail\n"), "empty c.txt .bashrc archive.tar.gz a ");
}

// ------------------------------------------------------------------------------ built-ins

#[test]
fn builtin_templates_are_complete() {
    let names: Vec<&str> = BUILTINS.iter().map(|b| b.name).collect();
    assert_eq!(names, ["Grouped by folder", "First level folders", "Simple file list", "Summary", "Folder tree"]);
    for b in &BUILTINS {
        let t = builtin_template(b.name).unwrap();
        assert!(t.ok() && t.issues.is_empty(), "{}", b.name);
        assert!(t.name == b.name && !t.file_name.is_empty());
        assert!(!b.title().is_empty());
    }
    let only_ours: Vec<&str> = BUILTINS.iter().filter(|b| !b.spacesniffer).map(|b| b.name).collect();
    assert_eq!(only_ours, ["Folder tree"]);
    assert!(builtin_template("simple FILE list").is_some());
    assert!(builtin_template("nope").is_none());
}

#[test]
fn grouped_by_folder() {
    let s = fixture();
    let n = s.read();
    let out = r(&n, root_of(&n), builtin("Grouped by folder"));
    let (kb4, kb8, kb124) = (size(4096), size(8192), size(126976));
    let want = format!(
        "\n{R} [{}]\n  [{kb4:>8}] c.txt\n  [{kb4:>8}] .bashrc\n  [{kb4:>8}] archive.tar.gz\n\
         \n{} [{}]\n  [{kb8:>8}] Two.TXT\n  [{kb4:>8}] one.bin\n\
         \n{} [{kb124}]\n  [{kb124:>8}] movie.mp4\n",
        size(151552),
        j(&["a"]),
        size(139264),
        j(&["a", "sub"]),
    );
    assert_eq!(out, want);
}

#[test]
fn first_level_folders() {
    let s = fixture();
    let n = s.read();
    let out = r(&n, root_of(&n), builtin("First level folders"));
    assert_eq!(out, format!("\n{} [{}]\n\n{} [0 B]\n", j(&["a"]), size(139264), j(&["empty"])));
}

#[test]
fn simple_file_list() {
    let s = fixture();
    let n = s.read();
    let out = r(&n, root_of(&n), builtin("Simple file list"));
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        [
            j(&["c.txt"]),
            j(&[".bashrc"]),
            j(&["archive.tar.gz"]),
            j(&["a", "Two.TXT"]),
            j(&["a", "one.bin"]),
            j(&["a", "sub", "movie.mp4"])
        ]
    );
}

#[test]
fn summary_and_folder_tree() {
    let s = fixture();
    let n = s.read();
    let root = root_of(&n);
    assert_eq!(r(&n, root, builtin("Summary")), format!("Total size: {}\nFolders   : 3\nFiles     : 6\n", size(151552)));
    let tree = r(&n, root, builtin("Folder tree"));
    assert_eq!(
        tree.lines().collect::<Vec<_>>(),
        [
            format!("data  {}", size(151552)),
            format!("    a  {}", size(139264)),
            format!("        sub  {}", size(126976)),
            "    empty  0 B".to_string()
        ]
    );
}

// ------------------------------------------------------------------------------ the tree

#[test]
fn filter_mask_hides_and_resizes() {
    let s = fixture();
    let n = s.read();
    let root = root_of(&n);
    let m = mask(&n, ".txt");
    assert_eq!(
        rm(&n, root, builtin("Simple file list"), &m).lines().collect::<Vec<_>>(),
        [j(&["c.txt"]), j(&["a", "Two.TXT"])]
    );
    assert_eq!(
        rm(&n, root, builtin("Summary"), &m),
        format!("Total size: {}\nFolders   : 1\nFiles     : 2\n", size(4096 + 8192))
    );
    assert_eq!(
        (tag_with(&n, "a", "localcount", Some(&m)), tag_with(&n, "a", "containsfolders", Some(&m)), tag_with(&n, "a", "disksizebytes", Some(&m))),
        ("1".into(), "false".into(), "8192".into())
    );
    assert_eq!(rm(&n, root, "<%file%> ", &m), "data a Two.TXT c.txt ");
    let nothing = mask(&n, ".nothing");
    assert_eq!(rm(&n, root, "<%file%> ", &nothing), "data ");
    assert_eq!(rm(&n, root, builtin("Grouped by folder"), &nothing), "");
}

#[test]
fn filter_mask_from_before_the_store_grew() {
    let s = fixture();
    let n = s.read();
    let movie = find(&n, "a/sub/movie.mp4") as usize;
    let short = mask(&n, ".mp4")[..movie].to_vec();
    assert_eq!(rm(&n, root_of(&n), "<%file%> ", &short), "data ");
}

#[test]
fn hardlink_duplicates_and_gone_rows() {
    let s = fixture();
    let (one, c) = {
        let n = s.read();
        (find(&n, "a/one.bin"), find(&n, "c.txt"))
    };
    s.appender().mark(one, Flags::HARDLINK_DUP);
    {
        let n = s.read();
        assert_eq!(tag(&n, "a/one.bin", "disksizebytes"), "0");
        let m = mask(&n, ".bin");
        assert_eq!(tag_with(&n, "a", "disksizebytes", Some(&m)), "0");
    }
    s.appender().mark(c, Flags::GONE);
    let n = s.read();
    assert!(!r(&n, root_of(&n), "<%file%> ").contains("c.txt"));
    assert_eq!(tag(&n, "", "filecount"), "5");
}

fn two_roots() -> Store {
    let store = Store::new();
    {
        let mut app = store.appender();
        let roots: Vec<NodeId> = ["/r1", "/r2"]
            .iter()
            .map(|p| app.add_root(p.as_bytes(), Kind::Dir, Flags::NONE, Times::default()).unwrap())
            .collect();
        for (i, root) in roots.into_iter().enumerate() {
            let mut b = EntryBatch::new();
            let name = format!("f{i}.txt");
            let size = 100 * (i as i64 + 1);
            b.push(Entry::file(name.as_bytes(), size, size));
            app.ingest_listing(root, &b).unwrap();
        }
    }
    store
}

#[test]
fn multiple_roots_render_in_turn() {
    let s = two_roots();
    let n = s.read();
    assert_eq!(r(&n, 0, "@footer\n<%file%>=<%sizebytes%>;"), "r1=100;r2=200;");
    assert!(matches!(
        ExportTree::new(&n, 99, None, Ordering::default(), true, None),
        Err(ExportError::NotAnElement(99))
    ));
}

#[test]
fn streaming_in_chunks() {
    let names: Vec<(String, Vec<String>)> = (0..25)
        .map(|i| (format!("d{i:02}"), (0..400).map(|j| format!("file{j:04}.dat")).collect()))
        .collect();
    let spec = Spec::Dir(
        names
            .iter()
            .map(|(dn, files)| {
                (dn.clone(), Spec::Dir(files.iter().enumerate().map(|(k, f)| (f.clone(), Spec::File(k as i64 + 1))).collect()))
            })
            .collect(),
    );
    let store = build_with(&spec, "/big", 1, &[]);
    let n = store.read();
    let root = root_of(&n);
    let t = tpl(builtin("Simple file list"));
    let mut chunks: Vec<String> = Vec::new();
    render_chunks(&n, root, &t, None, None, 4096, &mut |c| {
        chunks.push(c.to_owned());
        Ok(())
    })
    .unwrap();
    assert!(chunks.len() > 50);
    let all = chunks.concat();
    let longest = all.lines().map(|l| l.len() + 1).max().unwrap();
    assert!(chunks[..chunks.len() - 1].iter().all(|c| c.len() >= 4096 && c.len() < 4096 + longest));
    assert_eq!(all, render(&n, root, &t, None, None).unwrap());
    assert_eq!(all.lines().count(), 25 * 400);
}

// ------------------------------------------------------------------------------ formats

/// A minimal RFC 4180 reader (quoted fields, CRLF rows).
fn read_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            c => field.push(c),
        }
    }
    rows
}

#[test]
fn csv() {
    let s = fixture();
    let n = s.read();
    let text = export_csv(&n, root_of(&n), None).unwrap();
    assert!(text.contains("\r\n"));
    let rows = read_csv(&text);
    assert_eq!(rows[0], export::CSV_COLUMNS);
    let paths: Vec<&str> = rows[1..].iter().map(|r| r[0].as_str()).collect();
    assert_eq!(
        paths,
        [
            R.to_string(),
            j(&["a"]),
            j(&["a", "sub"]),
            j(&["a", "sub", "movie.mp4"]),
            j(&["a", "Two.TXT"]),
            j(&["a", "one.bin"]),
            j(&["c.txt"]),
            j(&[".bashrc"]),
            j(&["archive.tar.gz"]),
            j(&["empty"])
        ]
    );
    let two = &rows[5];
    assert_eq!((two[1].as_str(), two[2].as_str(), two[3].as_str()), ("Two.TXT", "file", "2"));
    assert_eq!((two[4].as_str(), two[5].as_str(), two[6].as_str(), two[7].as_str()), ("5000", "8192", "", ""));
    assert!(two[8].starts_with("2024-03-05T14:07:09") && two[9].is_empty());
    assert_eq!((rows[1][6].as_str(), rows[1][7].as_str(), rows[1][5].as_str()), ("6", "3", "151552"));
    let m = mask(&n, ".mp4");
    let filtered = read_csv(&export_csv(&n, root_of(&n), Some(&m)).unwrap());
    let names: Vec<&str> = filtered[1..].iter().map(|r| r[1].as_str()).collect();
    assert_eq!(names, ["data", "a", "sub", "movie.mp4"]);
}

#[test]
fn csv_quotes_odd_names() {
    let store = build_with(&d([("we,ird \"name\".txt", f(1)), ("plain.txt", f(2))]), "/q", 1, &[]);
    let n = store.read();
    let rows = read_csv(&export_csv(&n, root_of(&n), None).unwrap());
    let mut names: Vec<&str> = rows[1..].iter().map(|r| r[1].as_str()).collect();
    names.sort();
    assert_eq!(names, ["plain.txt", "q", "we,ird \"name\".txt"]);
}

#[test]
fn json() {
    let s = fixture();
    let n = s.read();
    let doc: serde_json::Value = serde_json::from_str(&export_json(&n, root_of(&n), None, "*.txt").unwrap()).unwrap();
    assert_eq!((doc["format"].as_str(), doc["version"].as_i64(), doc["filter"].as_str()), (Some("spaceknife-export"), Some(1), Some("*.txt")));
    let roots = doc["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 1);
    let tree = &roots[0];
    assert_eq!((tree["path"].as_str(), tree["name"].as_str(), tree["type"].as_str()), (Some(R), Some("data"), Some("folder")));
    assert_eq!((tree["files"].as_i64(), tree["folders"].as_i64(), tree["disk_size"].as_i64()), (Some(6), Some(3), Some(151552)));
    let names = |v: &serde_json::Value| v["children"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
    assert_eq!(names(tree), ["a", "c.txt", ".bashrc", "archive.tar.gz", "empty"]);
    let a = &tree["children"][0];
    assert_eq!(names(a), ["sub", "Two.TXT", "one.bin"]);
    assert_eq!(a["children"][0]["children"][0]["name"], "movie.mp4");
    let two = &a["children"][1];
    assert!(two.get("children").is_none() && two.get("files").is_none());
    assert!(two["modified"].as_str().unwrap().starts_with("2024-03-05T14:07:09") && two["created"].is_null());
    assert_eq!(tree["children"][4]["children"].as_array().unwrap().len(), 0);
    let m = mask(&n, ".txt");
    let filtered: serde_json::Value = serde_json::from_str(&export_json(&n, root_of(&n), Some(&m), "").unwrap()).unwrap();
    assert_eq!(names(&filtered["roots"][0]), ["a", "c.txt"]);
    assert!(filtered["filter"].is_null());
}

#[test]
fn json_streams_large_trees() {
    let spec = Spec::Dir(
        (0..10)
            .map(|i| (format!("d{i}"), Spec::Dir((0..300).map(|j| (format!("f{j}.x"), Spec::File(j))).collect())))
            .collect(),
    );
    let store = build_with(&spec, "/big", 1, &[]);
    let n = store.read();
    let mut chunks = Vec::new();
    export::format("json").unwrap().chunks(&n, root_of(&n), None, "", &mut |c| {
        chunks.push(c.to_owned());
        Ok(())
    })
    .unwrap();
    assert!(chunks.len() >= 2);
    let doc: serde_json::Value = serde_json::from_str(&chunks.concat()).unwrap();
    let total: usize = doc["roots"][0]["children"].as_array().unwrap().iter().map(|d| d["children"].as_array().unwrap().len()).sum();
    assert_eq!(total, 3000);
}

#[test]
fn html_report_is_self_contained_and_escaped() {
    let spec = d([
        ("<b>&evil</b>", d([("x.txt", f(5000))])),
        ("dir", Spec::Dir((0..40).map(|i| (format!("f{i}.log"), Spec::File(i * 10))).collect())),
    ]);
    let store = build_with(&spec, "/report", 1, &[]);
    let n = store.read();
    let opts = HtmlOptions { top: 10, depth: 3, ..HtmlOptions::default() };
    let page = export_html(&n, root_of(&n), None, "*.log;<x>", &opts).unwrap();
    for bad in ["<script", "<link", "<img", "<iframe", "src=", "href="] {
        assert!(!page.contains(bad), "{bad}");
    }
    assert!(page.contains("&lt;b&gt;&amp;evil&lt;/b&gt;") && !page.contains("<b>&evil"));
    assert!(page.contains("*.log;&lt;x&gt;"));
    assert!(page.contains("其余 30 项"));
    assert_eq!(page.matches("<details").count(), page.matches("</details>").count());
    for t in ["div", "section", "table", "tr", "summary", "span", "header", "main"] {
        assert_eq!(page.matches(&format!("<{t} ")).count() + page.matches(&format!("<{t}>")).count(), page.matches(&format!("</{t}>")).count(), "{t}");
    }
    assert!(page.contains(".log") && page.contains("x.txt"));
}

#[test]
fn html_report_under_a_filter_and_for_every_root() {
    let s = fixture();
    let n = s.read();
    let m = mask(&n, ".mp4");
    let page = export_html(&n, root_of(&n), Some(&m), "", &HtmlOptions::default()).unwrap();
    assert!(page.contains("匹配过滤的文件") && page.contains("movie.mp4") && !page.contains("Two.TXT"));
    let two = two_roots();
    let n = two.read();
    let page = export_html(&n, 0, None, "", &HtmlOptions::default()).unwrap();
    assert_eq!(page.matches("class=\"root\"").count(), 2);
    assert!(page.contains("2 个位置"));
}

// ------------------------------------------------------------------------------ resolving and files

#[test]
fn resolve_names_formats_and_files() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(resolve("Summary", None).unwrap(), Target::Template(_)));
    assert!(matches!(resolve("summary", None).unwrap(), Target::Template(_)));
    let Target::Format(csv) = resolve("CSV", None).unwrap() else { panic!() };
    assert!(csv.key == "csv" && csv.bom);
    let mine = dir.path().join("mine.sktpl");
    std::fs::write(&mine, "@detail\n<%file%>").unwrap();
    let Target::Template(found) = resolve(mine.to_str().unwrap(), None).unwrap() else { panic!() };
    assert!(found.name == "mine" && found.detail == "<%file%>");
    assert!(matches!(resolve("mine.sktpl", Some(dir.path())).unwrap(), Target::Template(_)));
    let err = resolve(dir.path().join("missing.sktpl").to_str().unwrap(), None).unwrap_err();
    assert!(err.to_string().contains("找不到"), "{err}");
    let bad = dir.path().join("ansi.sktpl");
    std::fs::write(&bad, b"<%file%> \xce\xc4\xbc\xfe").unwrap(); // "文件" in GBK
    let err = resolve(bad.to_str().unwrap(), None).unwrap_err();
    assert!(err.to_string().contains("UTF-8"), "{err}");
}

fn names_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect()
}

#[test]
fn write_export_is_atomic_and_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let s = fixture();
    let n = s.read();
    let out = dir.path().join("out.csv");
    let target = resolve("csv", None).unwrap();
    let size = write_export(&out, target.bom(), |sink| export::export_chunks(&target, &n, root_of(&n), None, "", sink)).unwrap();
    let data = std::fs::read(&out).unwrap();
    assert!(size == data.len() as u64 && data.starts_with(b"\xef\xbb\xbfpath,name"));
    assert_eq!(names_in(dir.path()), ["out.csv"]);
    let err = write_export(&out, false, |sink| {
        sink("partial")?;
        Err(ExportError::Io(std::io::Error::other("boom")))
    });
    assert!(err.is_err());
    assert_eq!(std::fs::read(&out).unwrap(), data);
    assert_eq!(names_in(dir.path()), ["out.csv"]);
}

#[test]
fn names_that_are_not_unicode_are_escaped() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new();
    #[cfg(windows)]
    let raw: &[u8] = b"bad\xed\xb3\xbf.txt"; // a lone surrogate U+DCFF (WTF-8)
    #[cfg(not(windows))]
    let raw: &[u8] = b"bad\xff.txt";
    let root = {
        let mut app = store.appender();
        let root = app.add_root(b"/u", Kind::Dir, Flags::NONE, Times::default()).unwrap();
        let mut b = EntryBatch::new();
        b.push(Entry::file(raw, 0, 0));
        app.ingest_listing(root, &b).unwrap();
        root
    };
    let n = store.read();
    let out = dir.path().join("names.txt");
    let t = tpl("<%file%>|");
    write_export(&out, false, |sink| render_chunks(&n, root, &t, None, None, 1 << 16, sink)).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), b"u|bad\\udcff.txt|");
    let doc = dir.path().join("names.json");
    write_export(&doc, false, |sink| export::format("json").unwrap().chunks(&n, root, None, "", sink)).unwrap();
    let text = std::fs::read_to_string(&doc).unwrap();
    assert!(text.contains("\"name\": \"bad\\udcff.txt\""), "{text}");
}

#[test]
fn batched_local_times_match_one_by_one() {
    let mut stamps: Vec<i64> = vec![0, 1_711_846_799, 1_711_846_800, 1_711_850_400, 1_729_990_000, 1_730_000_000, stamp(), 4_000_000_000];
    stamps.extend((1_600_000_000i64..1_760_000_000).step_by(3_600_007));
    let mut times = LocalTimes::new();
    for ts in stamps {
        assert_eq!(times.get(ts), iso_time(ts), "{ts}");
    }
}
