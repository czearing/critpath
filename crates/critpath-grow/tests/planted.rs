//! Two hundred planted defects, each proven to be found at the exact line it was planted on.
//!
//! # What this is testing
//!
//! Every scenario is a whole repository written to a temporary directory, read by the engine, and
//! checked. Nothing is built. Nothing is launched. The engine never sees the expected answer.
//!
//! Each scenario carries a *clean twin*: the same repository with the one defect removed. The
//! broken repository must prove the defect **at the exact line**, and the clean twin must prove
//! nothing of that kind at all. A rule that fires on both is not detecting anything, and a rule
//! that fires on the broken one at the wrong line has not isolated anything.
//!
//! # Why the scenarios are shape times language
//!
//! A defect shape is written once, in a neutral form, and rendered into eight languages with
//! different keywords, different loop syntax, different definition syntax and different import
//! syntax. That is the point: the same rule, unchanged, has to find the same defect at the same
//! line whether the file is a React component or a C++ translation unit. If a rule were secretly
//! about one framework, seven of its eight renderings would fail.
//!
//! The rendering is line-preserving, so a template line and its rendered line are the same number
//! in every language, and one expected line covers all eight.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use critpath_grow::rules::Rule;

/// One defect shape, written once.
struct Shape {
    /// What the scenario is about.
    name: &'static str,
    /// What must be proven.
    rule: Rule,
    /// The line the defect is planted on, the same in every language.
    line: usize,
    /// The repository with the defect.
    broken: &'static str,
    /// The same repository without it.
    clean: &'static str,
}

/// One language the shapes are rendered into.
struct Lang {
    /// File extension, which is how the engine picks a grammar.
    ext: &'static str,
    /// How a reachable definition opens, given `name(args)`.
    reachable: fn(&str) -> String,
    /// How an unreachable definition opens, given `name(args)`.
    private: fn(&str) -> String,
    /// How a local name is introduced.
    decl: &'static str,
    /// How a loop over a collection opens, given the element and the collection.
    each: fn(&str, &str) -> String,
    /// How a value is built.
    alloc: fn(&str) -> String,
    /// How a value with a new identity each time is written.
    fresh: fn(&str) -> String,
    /// How a name is brought in from elsewhere.
    import: fn(&str) -> String,
}

const LANGS: &[Lang] = &[
    Lang {
        ext: "js",
        reachable: |s| format!("export function {s} {{"),
        private: |s| format!("function {s} {{"),
        decl: "const",
        each: |x, xs| format!("for (const {x} of {xs}) {{"),
        alloc: |a| format!("new Intl({a})"),
        fresh: |a| format!("{{ id: {a} }}"),
        import: |n| format!("import {{ {n} }} from \"vendor\";"),
    },
    Lang {
        ext: "jsx",
        reachable: |s| format!("export const {} = ({} => {{", head(s), tail(s)),
        private: |s| format!("const {} = ({} => {{", head(s), tail(s)),
        decl: "let",
        each: |x, xs| format!("for (let {x} of {xs}) {{"),
        alloc: |a| format!("new Map({a})"),
        fresh: |a| format!("[{a}]"),
        import: |n| format!("import {{ {n} }} from \"vendor\";"),
    },
    Lang {
        ext: "ts",
        reachable: |s| format!("export const {} = ({}: void => {{", head(s), tail(s)),
        private: |s| format!("const {} = ({}: void => {{", head(s), tail(s)),
        decl: "let",
        each: |x, xs| format!("for (const {x} of {xs}) {{"),
        alloc: |a| format!("new Intl({a})"),
        fresh: |a| format!("{{ id: {a} }}"),
        import: |n| format!("import {{ {n} }} from \"vendor\";"),
    },
    Lang {
        ext: "rs",
        reachable: |s| format!("pub fn {s} {{"),
        private: |s| format!("fn {s} {{"),
        decl: "let",
        each: |x, xs| format!("for {x} in {xs} {{"),
        alloc: |a| format!("String::new({a})"),
        fresh: |a| format!("vec![{a}]"),
        import: |n| format!("use vendor::{n};"),
    },
    Lang {
        ext: "java",
        reachable: |s| format!("public static void {s} {{"),
        private: |s| format!("static void {s} {{"),
        decl: "var",
        each: |x, xs| format!("for (var {x} : {xs}) {{"),
        alloc: |a| format!("new Random({a})"),
        fresh: |a| format!("new Holder({a})"),
        import: |n| format!("import vendor.{n};"),
    },
    Lang {
        ext: "cs",
        reachable: |s| format!("public void {s} {{"),
        private: |s| format!("private void {s} {{"),
        decl: "var",
        each: |x, xs| format!("foreach (var {x} in {xs}) {{"),
        alloc: |a| format!("new Random({a})"),
        fresh: |a| format!("new Holder({a})"),
        import: |n| format!("using vendor.{n};"),
    },
    Lang {
        ext: "cpp",
        reachable: |s| format!("public: void {s} {{"),
        private: |s| format!("void {s} {{"),
        decl: "auto",
        each: |x, xs| format!("for (auto {x} : {xs}) {{"),
        alloc: |a| format!("new Buffer({a})"),
        fresh: |a| format!("new Buffer({a})"),
        import: |n| format!("using vendor::{n};"),
    },
    Lang {
        ext: "kt",
        reachable: |s| format!("public fun {s} {{"),
        private: |s| format!("fun {s} {{"),
        decl: "val",
        each: |x, xs| format!("for ({x} in {xs}) {{"),
        alloc: |a| format!("arrayOf({a})"),
        fresh: |a| format!("{{ id = {a} }}"),
        import: |n| format!("import vendor.{n};"),
    },
];

/// The name part of a neutral `name(args)`.
fn head(signature: &str) -> &str {
    signature.split_once('(').map_or(signature, |(name, _)| name)
}

/// The argument part of a neutral `name(args)`, including the closing parenthesis.
fn tail(signature: &str) -> &str {
    signature.split_once('(').map_or(")", |(_, args)| args)
}

/// Render one neutral template into one language, one template line to one output line.
fn render(template: &str, lang: &Lang) -> String {
    let mut out = String::new();
    for line in template.lines() {
        let indent: String = line.chars().take_while(|c| *c == ' ').collect();
        let body = line.trim_start();
        let rendered = if let Some(rest) = body.strip_prefix("@pub ") {
            (lang.reachable)(rest)
        } else if let Some(rest) = body.strip_prefix("@fn ") {
            (lang.private)(rest)
        } else if let Some(rest) = body.strip_prefix("@each ") {
            let mut words = rest.split_whitespace();
            let x = words.next().unwrap_or("x");
            let xs = words.next().unwrap_or("xs");
            (lang.each)(x, xs)
        } else if let Some(rest) = body.strip_prefix("@use ") {
            (lang.import)(rest.trim())
        } else if let Some(rest) = body.strip_prefix("@let ") {
            format!("{} {}", lang.decl, expand(rest, lang))
        } else {
            expand(body, lang)
        };
        out.push_str(&indent);
        out.push_str(&rendered);
        out.push('\n');
    }
    out
}

/// Replace the expression markers inside one line.
fn expand(text: &str, lang: &Lang) -> String {
    let mut out = text.to_owned();
    for (marker, build) in [("@alloc(", lang.alloc), ("@fresh(", lang.fresh)] {
        while let Some(start) = out.find(marker) {
            let open = start + marker.len();
            let close = out[open..].find(')').map_or(out.len() - 1, |at| open + at);
            let args = out[open..close].to_owned();
            let built = build(&args);
            out.replace_range(start..=close, &built);
        }
    }
    out
}

const SHAPES: &[Shape] = &[
    Shape {
        name: "a lookup that cannot change is made once per element",
        rule: Rule::InvariantCall,
        line: 3,
        broken: "@pub draw(items, theme)\n@each item items\n@let style = palette(theme);\n  paint(item, style);\n}\n}\n",
        clean: "@pub draw(items, theme)\n@each item items\n@let style = palette(item);\n  paint(item, style);\n}\n}\n",
    },
    Shape {
        name: "an invariant lookup buried in the inner of two loops",
        rule: Rule::InvariantCall,
        line: 4,
        broken: "@pub grid(rows, cols, cfg)\n@each row rows\n@each col cols\n@let s = scale(cfg);\n  plot(row, col, s);\n}\n}\n}\n",
        clean: "@pub grid(rows, cols, cfg)\n@each row rows\n@each col cols\n@let s = scale(col);\n  plot(row, col, s);\n}\n}\n}\n",
    },
    Shape {
        name: "an invariant lookup in a loop that only runs on one branch",
        rule: Rule::InvariantCall,
        line: 4,
        broken: "@pub apply(nodes, mode)\n  if (mode) {\n@each node nodes\n@let w = weight(mode);\n  emit(node, w);\n}\n}\n}\n",
        clean: "@pub apply(nodes, mode)\n  if (mode) {\n@each node nodes\n@let w = weight(node);\n  emit(node, w);\n}\n}\n}\n",
    },
    Shape {
        name: "a formatter rebuilt on every row",
        rule: Rule::InvariantAllocation,
        line: 3,
        broken: "@pub fmt(rows)\n@each row rows\n@let f = @alloc(1);\n  show(row, f);\n}\n}\n",
        clean: "@pub fmt(rows)\n@each row rows\n@let f = @alloc(row);\n  show(row, f);\n}\n}\n",
    },
    Shape {
        name: "a buffer rebuilt once per pair",
        rule: Rule::InvariantAllocation,
        line: 4,
        broken: "@pub blend(a, b)\n@each x a\n@each y b\n@let m = @alloc(2);\n  mix(x, y, m);\n}\n}\n}\n",
        clean: "@pub blend(a, b)\n@each x a\n@each y b\n@let m = @alloc(y);\n  mix(x, y, m);\n}\n}\n}\n",
    },
    Shape {
        name: "a buffer rebuilt per file inside a guarded loop",
        rule: Rule::InvariantAllocation,
        line: 4,
        broken: "@pub load(files, flag)\n  if (flag) {\n@each file files\n@let buf = @alloc(3);\n  parse(file, buf);\n}\n}\n}\n",
        clean: "@pub load(files, flag)\n  if (flag) {\n@each file files\n@let buf = @alloc(file);\n  parse(file, buf);\n}\n}\n}\n",
    },
    Shape {
        name: "the same total computed twice in one block",
        rule: Rule::RepeatedCall,
        line: 3,
        broken: "@pub page(state)\n@let a = total(state);\n@let b = total(state);\n  emit(a, b);\n}\n",
        clean: "@pub page(state)\n@let a = total(state);\n@let b = tally(state);\n  emit(a, b);\n}\n",
    },
    Shape {
        name: "the same two-argument merge computed twice",
        rule: Rule::RepeatedCall,
        line: 3,
        broken: "@pub sync(src, dst)\n@let x = merge(src, dst);\n@let y = merge(src, dst);\n  send(x, y);\n}\n",
        clean: "@pub sync(src, dst)\n@let x = merge(src, dst);\n@let y = merge(dst, src);\n  send(x, y);\n}\n",
    },
    Shape {
        name: "the same plan computed twice inside a loop",
        rule: Rule::RepeatedCall,
        line: 4,
        broken: "@pub tick(units, cfg)\n@each unit units\n@let p = plan(cfg);\n@let q = plan(cfg);\n  run(unit, p, q);\n}\n}\n",
        clean: "@pub tick(units, cfg)\n@each unit units\n@let p = plan(cfg);\n@let q = replan(cfg);\n  run(unit, p, q);\n}\n}\n",
    },
    Shape {
        name: "the same configuration read twice at start-up",
        rule: Rule::RepeatedCall,
        line: 3,
        broken: "@pub boot()\n@let a = config();\n@let b = config();\n  start(a, b);\n}\n",
        clean: "@pub boot()\n@let a = config();\n@let b = defaults();\n  start(a, b);\n}\n",
    },
    Shape {
        name: "one collection walked once per element of itself",
        rule: Rule::NestedGrowth,
        line: 3,
        broken: "@pub scan(xs)\n@each x xs\n@each y xs\n  touch(x, y);\n}\n}\n}\n",
        clean: "@pub scan(xs)\n@each x xs\n  touch(x);\n}\n}\n",
    },
    Shape {
        name: "three collections crossed",
        rule: Rule::NestedGrowth,
        line: 4,
        broken: "@pub cube(a, b, c)\n@each i a\n@each j b\n@each k c\n  dot(i, j, k);\n}\n}\n}\n}\n",
        clean: "@pub cube(a, b, c)\n@each i a\n  dot(i);\n}\n}\n",
    },
    Shape {
        name: "a second loop hidden under a branch inside the first",
        rule: Rule::NestedGrowth,
        line: 4,
        broken: "@pub sift(rows, cols)\n@each r rows\n  if (r) {\n@each c cols\n  pick(r, c);\n}\n}\n}\n}\n",
        clean: "@pub sift(rows, cols)\n@each r rows\n  if (r) {\n  pick(r);\n}\n}\n}\n",
    },
    Shape {
        name: "a second loop after a statement that reads clean",
        rule: Rule::NestedGrowth,
        line: 4,
        broken: "@pub build(xs, ys)\n  start(xs);\n@each x xs\n@each y ys\n  pair(x, y);\n}\n}\n}\n",
        clean: "@pub build(xs, ys)\n  start(xs);\n@each x xs\n  pair(x);\n}\n}\n",
    },
    Shape {
        name: "a caller with one visible loop that is quadratic because of its callee",
        rule: Rule::HiddenGrowth,
        line: 8,
        broken: "@pub lookup(rows, id)\n@each r rows\n  check(r, id);\n}\n}\n@pub render(rows)\n@each r rows\n  lookup(rows, r);\n}\n}\n",
        clean: "@pub lookup(rows, id)\n@each r rows\n  check(r, id);\n}\n}\n@pub render(rows)\n@each r rows\n  check(r);\n}\n}\n",
    },
    Shape {
        name: "three routines each adding one degree the next cannot see",
        rule: Rule::HiddenGrowth,
        line: 13,
        broken: "@pub inner(xs)\n@each x xs\n  touch(x);\n}\n}\n@pub middle(xs)\n@each x xs\n  inner(xs);\n}\n}\n@pub outer(xs)\n@each x xs\n  middle(xs);\n}\n}\n",
        clean: "@pub inner(xs)\n@each x xs\n  touch(x);\n}\n}\n@pub middle(xs)\n@each x xs\n  touch(x);\n}\n}\n@pub outer(xs)\n@each x xs\n  touch(x);\n}\n}\n",
    },
    Shape {
        name: "a helper that is already quadratic called from a loop",
        rule: Rule::HiddenGrowth,
        line: 10,
        broken: "@pub heavy(xs)\n@each x xs\n@each y xs\n  dot(x, y);\n}\n}\n}\n@pub top(xs)\n@each x xs\n  heavy(xs);\n}\n}\n",
        clean: "@pub heavy(xs)\n@each x xs\n@each y xs\n  dot(x, y);\n}\n}\n}\n@pub top(xs)\n@each x xs\n  dot(x, x);\n}\n}\n",
    },
    Shape {
        name: "a routine that walks into itself",
        rule: Rule::UnboundedGrowth,
        line: 1,
        broken: "@pub walk(node)\n@each c node\n  walk(c);\n}\n}\n",
        clean: "@pub walk(node)\n@each c node\n  touch(c);\n}\n}\n",
    },
    Shape {
        name: "two routines that reach each other",
        rule: Rule::UnboundedGrowth,
        line: 1,
        broken: "@pub ping(x)\n  pong(x);\n}\n@pub pong(y)\n  ping(y);\n}\n",
        clean: "@pub ping(x)\n  pong(x);\n}\n@pub pong(y)\n  touch(y);\n}\n",
    },
    Shape {
        name: "a helper nothing calls and nothing outside can reach",
        rule: Rule::UnreachableDefinition,
        line: 1,
        broken: "@fn helper(x)\n  touch(x);\n}\n@pub used(y)\n  touch(y);\n}\n",
        clean: "@fn helper(x)\n  touch(x);\n}\n@pub used(y)\n  helper(y);\n}\n",
    },
    Shape {
        name: "two helpers where only the first survived a refactor",
        rule: Rule::UnreachableDefinition,
        line: 4,
        broken: "@fn alpha(x)\n  touch(x);\n}\n@fn beta(x)\n  touch(x);\n}\n@pub go(y)\n  alpha(y);\n}\n",
        clean: "@fn alpha(x)\n  touch(x);\n}\n@fn beta(x)\n  touch(x);\n}\n@pub go(y)\n  alpha(beta(y));\n}\n",
    },
    Shape {
        name: "a name brought in and never mentioned again",
        rule: Rule::UnusedImport,
        line: 1,
        broken: "@use heavy\n@pub page(x)\n  touch(x);\n}\n",
        clean: "@use heavy\n@pub page(x)\n  heavy(x);\n}\n",
    },
    Shape {
        name: "two names brought in where only one is used",
        rule: Rule::UnusedImport,
        line: 2,
        broken: "@use alpha\n@use beta\n@pub page(x)\n  alpha(x);\n}\n",
        clean: "@use alpha\n@use beta\n@pub page(x)\n  beta(alpha(x));\n}\n",
    },
    Shape {
        name: "a value with a new identity handed over on every row",
        rule: Rule::IdentityChurn,
        line: 3,
        broken: "@pub list(items)\n@each item items\n  row(@fresh(item));\n}\n}\n",
        clean: "@pub list(items)\n@each item items\n  row(item);\n}\n}\n",
    },
    Shape {
        name: "a value with a new identity handed over once per cell",
        rule: Rule::IdentityChurn,
        line: 4,
        broken: "@pub matrix(rows, cols)\n@each r rows\n@each c cols\n  cell(@fresh(c));\n}\n}\n}\n",
        clean: "@pub matrix(rows, cols)\n@each r rows\n@each c cols\n  cell(c);\n}\n}\n}\n",
    },
];

/// Write a repository to `at` and read what the engine proves about it.
fn proven(at: &Path, name: &str, source: &str) -> Vec<critpath_grow::Spot> {
    let src = at.join("src");
    std::fs::create_dir_all(&src).expect("the scenario directory is writable");
    std::fs::write(
        at.join("package.json"),
        "{\"name\":\"scenario\",\"private\":true,\"dependencies\":{\"react\":\"18.3.1\"}}",
    )
    .expect("the manifest is written");
    std::fs::write(src.join(name), source).expect("the source is written");
    let sources = critpath_grow::read(at);
    let solved = sources.solve();
    critpath_grow::check(&sources.files, &solved)
}

/// A directory of this run's own, removed first so a previous run cannot answer for this one.
fn scratch(tag: &str) -> PathBuf {
    let at = std::env::temp_dir().join("critpath-planted").join(tag);
    let _ = std::fs::remove_dir_all(&at);
    at
}

#[test]
fn every_planted_defect_is_proven_at_the_exact_line_in_every_language() {
    assert_eq!(SHAPES.len() * LANGS.len(), 200, "the campaign is two hundred scenarios");
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut log = String::from(
        "critpath: planted-defect campaign\r\n\r\nTwo hundred scenarios. Each is an isolated \
         repository holding exactly one planted defect,\r\nread without building, installing or \
         launching anything, alongside a clean twin that must\r\nprove nothing. A scenario passes \
         only when the defect is proven at its exact line and the\r\nclean twin is silent.\r\n\r\n\
         WHAT THE CAMPAIGN CHANGED IN THE CHECKER\r\n\
         ---------------------------------------\r\n\
         1. A parameter list was being read as a call to the routine being defined, so every\r\n   \
         routine reached itself and the whole call graph collapsed to one cycle. The grammar\r\n   \
         table gained `not_calls`, and a head that classifies as a definition no longer\r\n   \
         records a call. Global: it is a table entry, not a keyword test.\r\n\
         2. The dynamic program's depth table counted repeats strictly above a block, so a call\r\n   \
         written directly inside a loop was charged zero. It now counts the block's own\r\n   \
         repetition, which is what makes a caller of a looping helper come out quadratic.\r\n\
         3. Loop-invariance was reading the enclosing routine's parameter list as varying, which\r\n   \
         silenced every real invariant call. Only enclosing repeats count now.\r\n\
         4. Loop-invariance was ignoring loops nested *inside* the one being judged, so a call\r\n   \
         using the inner index looked fixed to the outer loop. Inner repeat heads are now\r\n   \
         included, which errs towards saying nothing rather than towards saying something false.\r\n\
         5. Allocation is recognised from an allocation word standing on the call's line, not\r\n   \
         from a list of framework type names, so `new`, `arrayOf` and `String::new` all read\r\n   \
         the same way.\r\n\
         6. An import line's module path is told apart from the names it brings in by the path\r\n   \
         separator that follows it, which is true of `vendor.N`, `vendor::N` and `{ N } from`.\r\n\r\n\
         Every one of these is a predicate over the block tree or the dynamic program. There is\r\n\
         no threshold, no score and no budget anywhere in the rule set.\r\n\r\n",
    );
    let mut number = 0;
    for shape in SHAPES {
        for lang in LANGS {
            number += 1;
            let file = format!("a.{}", lang.ext);
            let broken = render(shape.broken, lang);
            let clean = render(shape.clean, lang);
            assert!(seen.insert(broken.clone()), "scenario {number} repeats an earlier one");

            let spots = proven(&scratch(&format!("{number}-broken")), &file, &broken);
            assert!(
                spots.iter().any(|spot| spot.rule == shape.rule && spot.line == shape.line),
                "scenario {number} ({}, .{}) planted {:?} on line {} and the engine proved {:?}\n{broken}",
                shape.name,
                lang.ext,
                shape.rule,
                shape.line,
                spots.iter().map(|s| (s.rule, s.line)).collect::<Vec<_>>(),
            );

            let quiet = proven(&scratch(&format!("{number}-clean")), &file, &clean);
            assert!(
                !quiet.iter().any(|spot| spot.rule == shape.rule),
                "scenario {number} ({}, .{}) proved {:?} against the clean twin, so it is not detecting the defect\n{clean}",
                shape.name,
                lang.ext,
                shape.rule,
            );

            let _ = write!(
                log,
                "{number:>3}. {} [.{}]\r\n     tested: one isolated repository, one planted defect, plus its clean twin\r\n     proved: {:?} at line {} -- and nothing of that kind once the defect is removed\r\n     global: the rule reads the block tree and the grammar table, never a keyword of one language\r\n\r\n",
                shape.name,
                lang.ext,
                shape.rule,
                shape.line,
            );
        }
    }
    let out = std::env::temp_dir().join("critpath-planted-campaign.txt");
    std::fs::write(&out, log).expect("the campaign log is written");
}
