//! Folder classes (port of backend/tests/test_pathclass.py; the filter / layout parts belong to
//! those modules' tests).

use std::sync::{Arc, Mutex, MutexGuard};

use sk_core::classes::ClassTable;
use sk_core::pathclass::{self, FolderClasses};
use sk_store::testing::{Spec, build_with, d, f, find, listing_of};
use sk_store::{NodeId, Nodes, Store, mutate};

const C: &str = "C:\\";

/// The user's choices are process-wide: one test at a time touches them.
static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    pathclass::replace_folders(Vec::new());
    g
}

fn table() -> Arc<ClassTable> {
    ClassTable::builtin()
}

/// The class a row takes: by extension, under the folder classes.
fn class_of(nodes: &Nodes, fc: &FolderClasses, t: &Arc<ClassTable>, node: NodeId) -> String {
    let col = fc.column(nodes, t);
    let ext = nodes.view(node).ext;
    let base = t.index_of(t.color_class(&ext)).unwrap() as u8;
    let idx = pathclass::resolve(col[node as usize], base, t.other_index() as u8);
    t.ids()[idx as usize].clone()
}

fn classes(store: &Store, fc: &FolderClasses, t: &Arc<ClassTable>, paths: &[&str]) -> Vec<(String, String)> {
    let r = store.read();
    paths.iter().map(|p| (p.to_string(), class_of(&r, fc, t, find(&r, p)))).collect()
}

fn expect(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

fn check(store: &Store, fc: &FolderClasses, want: &[(&str, &str)]) {
    let paths: Vec<&str> = want.iter().map(|w| w.0).collect();
    assert_eq!(classes(store, fc, &table(), &paths), expect(want));
}

fn games() -> Spec {
    d([
        ("Program Files (x86)", d([("Steam", d([
            ("steam.exe", f(10)),
            ("steamapps", d([
                ("common", d([("Foo", d([("Foo.exe", f(1000)), ("intro.mp4", f(5000)), ("data.pak", f(3000)), ("package.json", f(10))]))])),
                ("workshop", d([("content", d([("123", d([("mod.bin", f(40))]))]))])),
            ])),
        ]))])),
        ("Games", d([
            ("Bar", d([("Bar.exe", f(100)), ("Bar_Data", d([("level0", f(200)), ("music.ogg", f(300))]))])),
            ("Baz", d([("UnityPlayer.dll", f(50)), ("Baz.exe", f(10)), ("cg.mp4", f(70))])),
            ("Qux", d([("Engine", d([("x.dll", f(5))])), ("Qux", d([("Content", d([("Paks", d([("Qux.pak", f(900))]))]))]))])),
            ("Godot", d([("game.exe", f(10)), ("game.pck", f(500))])),
            ("Blizz", d([(".build.info", f(1)), ("Data", d([("data.000", f(60))]))])),
            ("NotAGame", d([("tool.exe", f(10)), ("readme.txt", f(5))])),
        ])),
        ("Epic Games", d([("Launcher", d([("Portal.exe", f(10)), ("ui.dat", f(5))])), ("Fortnite", d([("pak.ucas", f(80))]))])),
        ("miHoYo Launcher", d([("games", d([("Genshin Impact Game", d([("YuanShen.exe", f(10)), ("blob.blk", f(700))]))]))])),
    ])
}

fn home() -> Spec {
    d([
        ("Users", d([("lenny", d([
            (".gitconfig", f(1)),
            ("code", d([
                ("app", d([
                    ("package.json", f(10)),
                    ("src", d([("main.ts", f(100))])),
                    ("weights.pt", f(9000)),
                    ("node_modules", d([("lib", d([("package.json", f(5)), ("index.js", f(50))]))])),
                    ("build", d([("Game.exe", f(10)), ("Game_Data", d([("a.assets", f(30))]))])),
                ])),
                ("Cargo.toml", f(1)),
            ])),
            ("Downloads", d([("package.json", f(10)), ("movie.mp4", f(100)), ("repo", d([(".git", d([("HEAD", f(1))])), ("x.bin", f(4))]))])),
            ("Documents", d([("My Games", d([("Save1.sav", f(20))])), ("notes.txt", f(3))])),
            ("AppData", d([("Local", d([("tool", d([("package.json", f(1)), ("blob", f(10)), ("pic.png", f(5))]))]))])),
            (".cache", d([("huggingface", d([("hub", d([("blob1", f(100))]))])), ("pip", d([("wheel", f(7))]))])),
            (".ollama", d([("models", d([("blobs", d([("sha256-abc", f(1000))]))]))])),
            (".minecraft", d([("saves", d([("level.dat", f(9))]))])),
        ]))])),
        ("System Volume Information", d([("{snap}", f(400))])),
        ("Program Files", d([("App", d([("app.exe", f(10)), ("res.xyz", f(20)), ("pic.png", f(30))]))])),
        ("Data", d([("lib", d([("setup.py", f(1)), ("m.py", f(2))]))])),
    ])
}

fn merged(a: Spec, b: Spec) -> Spec {
    let mut kids = a.children().to_vec();
    kids.extend(b.children().iter().cloned());
    Spec::Dir(kids)
}

#[test]
fn game_installs_swallow_their_files() {
    let _g = lock();
    let store = build_with(&games(), C, 1, &[]);
    let fc = FolderClasses::new();
    check(&store, &fc, &[
        ("Program Files (x86)/Steam/steamapps/common/Foo/intro.mp4", "game"),
        ("Program Files (x86)/Steam/steamapps/common/Foo/data.pak", "game"),
        ("Program Files (x86)/Steam/steamapps/common/Foo/package.json", "game"),
        ("Program Files (x86)/Steam/steamapps/workshop/content/123/mod.bin", "game"),
        ("Program Files (x86)/Steam/steam.exe", "exec"),
        ("Games/Bar/Bar_Data/music.ogg", "game"),
        ("Games/Baz/cg.mp4", "game"),
        ("Games/Qux/Engine/x.dll", "game"),
        ("Games/Godot/game.pck", "game"),
        ("Games/Blizz/Data/data.000", "game"),
        ("Games/NotAGame/readme.txt", "doc"),
        ("Epic Games/Fortnite/pak.ucas", "game"),
        ("Epic Games/Launcher/ui.dat", "other"),
        ("miHoYo Launcher/games/Genshin Impact Game/blob.blk", "game"),
    ]);
}

#[test]
fn projects_models_system_and_software() {
    let _g = lock();
    let store = build_with(&home(), C, 1, &[]);
    let fc = FolderClasses::new();
    check(&store, &fc, &[
        ("Users/lenny/code/app/weights.pt", "code"),
        ("Users/lenny/code/app/node_modules/lib/index.js", "code"),
        ("Users/lenny/code/app/build/Game_Data/a.assets", "code"),
        ("Users/lenny/Downloads/movie.mp4", "video"),
        ("Users/lenny/Downloads/repo/x.bin", "code"),
        ("Users/lenny/.gitconfig", "other"),
        ("Users/lenny/Documents/My Games/Save1.sav", "game"),
        ("Users/lenny/Documents/notes.txt", "doc"),
        ("Users/lenny/AppData/Local/tool/blob", "exec"),
        ("Users/lenny/AppData/Local/tool/pic.png", "image"),
        ("Users/lenny/AppData/Local/tool/package.json", "data"),
        ("Users/lenny/.cache/huggingface/hub/blob1", "data"),
        ("Users/lenny/.cache/pip/wheel", "other"),
        ("Users/lenny/.ollama/models/blobs/sha256-abc", "data"),
        ("Users/lenny/.minecraft/saves/level.dat", "game"),
        ("System Volume Information/{snap}", "exec"),
        ("Program Files/App/res.xyz", "exec"),
        ("Program Files/App/pic.png", "image"),
        ("Data/lib/m.py", "code"),
    ]);
}

#[test]
fn a_home_or_volume_is_never_a_project() {
    let _g = lock();
    let spec = d([
        ("Users", d([("lenny", d([(".git", d([("HEAD", f(1))])), ("photo.jpg", f(5))]))])),
        (".git", d([("HEAD", f(1))])),
        ("a.mp3", f(3)),
    ]);
    let store = build_with(&spec, C, 1, &[]);
    check(&store, &FolderClasses::new(), &[("Users/lenny/photo.jpg", "image"), ("a.mp3", "audio")]);
    let home = build_with(&d([(".git", d([("HEAD", f(1))])), ("photo.jpg", f(5))]), "C:\\Users\\lenny", 1, &[]);
    check(&home, &FolderClasses::new(), &[("photo.jpg", "image")]);
    let proj = build_with(&d([("Cargo.toml", f(1)), ("big.mp4", f(5))]), "C:\\work\\proj", 1, &[]);
    check(&proj, &FolderClasses::new(), &[("big.mp4", "code")]);
}

#[test]
fn a_rule_is_off_when_its_class_is_gone() {
    let _g = lock();
    let store = build_with(&games(), C, 1, &[]);
    let no_game = Arc::new(
        ClassTable::new(
            sk_protocol::classes::builtin_classes().iter().filter(|c| c.id != "game").cloned().collect(),
        )
        .unwrap(),
    );
    let path = "Program Files (x86)/Steam/steamapps/common/Foo/intro.mp4";
    assert_eq!(classes(&store, &FolderClasses::new(), &no_game, &[path]), expect(&[(path, "video")]));
}

#[test]
fn the_users_choices_win() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    let store = build_with(&games(), C, 1, &[]);
    let fc = FolderClasses::new();
    let foo = "Program Files (x86)/Steam/steamapps/common/Foo";
    let at = |rel: &str| {
        let r = store.read();
        r.path_lossy(find(&r, rel))
    };
    pathclass::set_folder(&at("Games/NotAGame"), Some("game"));
    pathclass::set_folder(&at(foo), Some(""));
    let intro = format!("{foo}/intro.mp4");
    let pak = format!("{foo}/data.pak");
    check(&store, &fc, &[("Games/NotAGame/readme.txt", "game"), (&intro, "video"), (&pak, "other")]);
    {
        let r = store.read();
        let why = fc.explain(&r, find(&r, "Games/NotAGame/readme.txt"), &table()).unwrap();
        assert!(why.user && why.class_id == "game");
        assert!(fc.explain(&r, find(&r, &intro), &table()).is_none());
    }
    let path = tmp.path().join("class_folders.json");
    pathclass::save(&path).unwrap();
    pathclass::set_folder(&at("Games/NotAGame"), None);
    pathclass::set_folder(&at(foo), None);
    check(&store, &fc, &[(&intro, "game")]);
    pathclass::replace_folders(pathclass::load(&path));
    assert_eq!(pathclass::folders().len(), 2);
    pathclass::replace_folders(Vec::new());
    pathclass::save(&path).unwrap();
    assert!(!path.exists());
}

#[test]
fn reasons() {
    let _g = lock();
    let store = build_with(&merged(games(), home()), C, 1, &[]);
    let fc = FolderClasses::new();
    let r = store.read();
    let why = |p: &str| {
        let w = fc.explain(&r, find(&r, p), &table()).unwrap();
        (w.class_id, r.path_lossy(w.folder).replace('\\', "/"), w.fallback)
    };
    let folder = |p: &str| r.path_lossy(find(&r, p)).replace('\\', "/");
    assert_eq!(
        why("Program Files (x86)/Steam/steamapps/common/Foo/intro.mp4"),
        ("game".into(), folder("Program Files (x86)/Steam/steamapps"), false)
    );
    assert_eq!(why("Users/lenny/code/app/src/main.ts"), ("code".into(), folder("Users/lenny/code"), false));
    assert_eq!(why("Program Files/App/res.xyz"), ("exec".into(), folder("Program Files"), true));
    assert!(fc.explain(&r, find(&r, "Users/lenny/Documents/notes.txt"), &table()).is_none());
    let info = fc.explain(&r, find(&r, "Games/Baz/cg.mp4"), &table()).unwrap().to_protocol(&r);
    assert_eq!(info.class_id, "game");
    assert!(info.folder.replace('\\', "/").ends_with("Games/Baz"));
}

// --------------------------------------------------------------------------- following changes

fn relist(store: &Store, folder: NodeId, children: Spec) {
    let mut w = store.write();
    let batch = listing_of(children.children(), 1, &mut |e| w.ext_id(e));
    mutate::relist(&mut w, folder, &batch).unwrap();
}

fn fold(store: &Store, folder: NodeId, children: Spec) -> NodeId {
    let mut app = store.appender();
    let batch = listing_of(children.children(), 1, &mut |e| app.ext_id(e));
    app.ingest_listing(folder, &batch).unwrap()
}

/// The kept column equals one worked out from scratch, on the live rows.
fn same_as_fresh(store: &Store, fc: &FolderClasses) -> bool {
    let r = store.read();
    let kept = fc.column(&r, &table());
    let fresh = FolderClasses::new().column(&r, &table());
    (0..r.len() as NodeId)
        .filter(|&i| !r.flags(i).intersects(sk_store::Flags::GONE))
        .all(|i| kept[i as usize] == fresh[i as usize])
}

fn one(store: &Store, fc: &FolderClasses, path: &str) -> String {
    classes(store, fc, &table(), &[path]).remove(0).1
}

#[test]
fn changes_update_only_what_they_touch() {
    let _g = lock();
    let spec = d([
        ("Users", d([("lenny", d([("work", d([("a", d([("x.bin", f(5))])), ("b", d([("y.bin", f(6))]))]))]))])),
        ("Games", d([("G", d([("G.exe", f(1))]))])),
    ]);
    let store = build_with(&spec, C, 1, &[]);
    let fc = FolderClasses::new();
    let find_ = |p: &str| find(&store.read(), p);
    assert_eq!(one(&store, &fc, "Users/lenny/work/a/x.bin"), "other");
    relist(&store, find_("Users/lenny/work/a"), d([("x.bin", f(5)), ("pyproject.toml", f(1))]));
    assert!(same_as_fresh(&store, &fc));
    assert_eq!(one(&store, &fc, "Users/lenny/work/a/x.bin"), "code");
    relist(&store, find_("Games/G"), d([("G.exe", f(1)), ("G_Data", d([])), ("clip.mp4", f(9))]));
    assert!(same_as_fresh(&store, &fc));
    assert_eq!(one(&store, &fc, "Games/G/clip.mp4"), "game");
    relist(&store, find_("Users/lenny/work/a"), d([("x.bin", f(5))]));
    assert!(same_as_fresh(&store, &fc));
    assert_eq!(one(&store, &fc, "Users/lenny/work/a/x.bin"), "other");
    relist(&store, find_("Users/lenny/work/b"), d([("y.bin", f(6)), (".git", d([]))]));
    relist(&store, find_("Users/lenny/work"), d([("a", d([])), ("b", d([])), ("new.txt", f(1))]));
    assert!(same_as_fresh(&store, &fc));
    assert_eq!(one(&store, &fc, "Users/lenny/work/b/y.bin"), "code");
}

#[test]
fn a_scan_in_progress() {
    let _g = lock();
    let spec = d([("Users", d([("lenny", d([
        ("p", d([("Cargo.toml", f(1)), ("src", d([("lib.rs", f(3))]))])),
        ("q", d([("v.mp4", f(9))])),
    ]))]))]);
    let store = build_with(&spec, C, 1, &["Users/lenny/p/src"]);
    let fc = FolderClasses::new();
    fc.column(&store.read(), &table()); // looked at before the scan is done
    let src = find(&store.read(), "Users/lenny/p/src");
    fold(&store, src, d([("lib.rs", f(3)), ("big.bin", f(7))]));
    assert!(same_as_fresh(&store, &fc));
    assert_eq!(one(&store, &fc, "Users/lenny/p/src/big.bin"), "code");
}

#[test]
fn a_choice_for_a_folder_the_scan_reaches_later() {
    let _g = lock();
    let spec = d([("Users", d([("lenny", d([]))])), ("a.mp3", f(3))]);
    let store = build_with(&spec, C, 1, &["Users/lenny"]);
    let fc = FolderClasses::new();
    let lenny = find(&store.read(), "Users/lenny");
    let x_path = format!("{}{}x", store.read().path_lossy(lenny), std::path::MAIN_SEPARATOR);
    pathclass::set_folder(&x_path, Some("game"));
    fc.column(&store.read(), &table());
    let first = fold(&store, lenny, d([("x", d([])), ("b.txt", f(1))]));
    fold(&store, first, d([("c.txt", f(1))]));
    check(&store, &fc, &[("Users/lenny/x/c.txt", "game"), ("Users/lenny/b.txt", "doc")]);
    assert!(same_as_fresh(&store, &fc));
    pathclass::replace_folders(Vec::new());
}
