// cargo.rs - Rust package manager

#[legacy_exports];

use syntax::{ast, codemap, parse, visit, attr};
use syntax::diagnostic::span_handler;
use codemap::span;
use rustc::metadata::filesearch::{get_cargo_root, get_cargo_root_nearest,
                                     get_cargo_sysroot, libdir};
use syntax::diagnostic;

use result::{Ok, Err};
use io::WriterUtil;
use send_map::linear::LinearMap;
use std::{map, json, tempfile, term, sort, getopts};
use map::HashMap;
use to_str::to_str;
use getopts::{optflag, optopt, opt_present};
use dvec::DVec;

struct Package {
    name: ~str,
    uuid: ~str,
    url: ~str,
    method: ~str,
    description: ~str,
    reference: Option<~str>,
    tags: ~[~str],
    versions: ~[(~str, ~str)]
}

impl Package : cmp::Ord {
    pure fn lt(other: &Package) -> bool {
        if self.name.lt(&(*other).name) { return true; }
        if (*other).name.lt(&self.name) { return false; }
        if self.uuid.lt(&(*other).uuid) { return true; }
        if (*other).uuid.lt(&self.uuid) { return false; }
        if self.url.lt(&(*other).url) { return true; }
        if (*other).url.lt(&self.url) { return false; }
        if self.method.lt(&(*other).method) { return true; }
        if (*other).method.lt(&self.method) { return false; }
        if self.description.lt(&(*other).description) { return true; }
        if (*other).description.lt(&self.description) { return false; }
        if self.tags.lt(&(*other).tags) { return true; }
        if (*other).tags.lt(&self.tags) { return false; }
        if self.versions.lt(&(*other).versions) { return true; }
        return false;
    }
    pure fn le(other: &Package) -> bool { !(*other).lt(&self) }
    pure fn ge(other: &Package) -> bool { !self.lt(other)     }
    pure fn gt(other: &Package) -> bool { (*other).lt(&self)  }
}

struct Source {
    name: ~str,
    mut url: ~str,
    mut method: ~str,
    mut key: Option<~str>,
    mut keyfp: Option<~str>,
    packages: DVec<Package>
}

struct Cargo {
    pgp: bool,
    root: Path,
    installdir: Path,
    bindir: Path,
    libdir: Path,
    workdir: Path,
    sourcedir: Path,
    sources: map::HashMap<~str, @Source>,
    mut current_install: ~str,
    dep_cache: map::HashMap<~str, bool>,
    opts: Options
}

struct Crate {
    name: ~str,
    vers: ~str,
    uuid: ~str,
    desc: Option<~str>,
    sigs: Option<~str>,
    crate_type: Option<~str>,
    deps: ~[~str]
}

struct Options {
    test: bool,
    mode: Mode,
    free: ~[~str],
    help: bool,
}

enum Mode { SystemMode, UserMode, LocalMode }

impl Mode : cmp::Eq {
    pure fn eq(other: &Mode) -> bool {
        (self as uint) == ((*other) as uint)
    }
    pure fn ne(other: &Mode) -> bool { !self.eq(other) }
}

fn opts() -> ~[getopts::Opt] {
    ~[optflag(~"g"), optflag(~"G"), optflag(~"test"),
     optflag(~"h"), optflag(~"help")]
}

fn info(msg: ~str) {
    let out = io::stdout();

    if term::color_supported() {
        term::fg(out, term::color_green);
        out.write_str(~"info: ");
        term::reset(out);
        out.write_line(msg);
    } else { out.write_line(~"info: " + msg); }
}

fn warn(msg: ~str) {
    let out = io::stdout();

    if term::color_supported() {
        term::fg(out, term::color_yellow);
        out.write_str(~"warning: ");
        term::reset(out);
        out.write_line(msg);
    }else { out.write_line(~"warning: " + msg); }
}

fn error(msg: ~str) {
    let out = io::stdout();

    if term::color_supported() {
        term::fg(out, term::color_red);
        out.write_str(~"error: ");
        term::reset(out);
        out.write_line(msg);
    }
    else { out.write_line(~"error: " + msg); }
}

fn is_uuid(id: ~str) -> bool {
    let parts = str::split_str(id, ~"-");
    if vec::len(parts) == 5u {
        let mut correct = 0u;
        for vec::eachi(parts) |i, part| {
            fn is_hex_digit(+ch: char) -> bool {
                ('0' <= ch && ch <= '9') ||
                ('a' <= ch && ch <= 'f') ||
                ('A' <= ch && ch <= 'F')
            }

            if !part.all(is_hex_digit) {
                return false;
            }

            match i {
                0u => {
                    if part.len() == 8u {
                        correct += 1u;
                    }
                }
                1u | 2u | 3u => {
                    if part.len() == 4u {
                        correct += 1u;
                    }
                }
                4u => {
                    if part.len() == 12u {
                        correct += 1u;
                    }
                }
                _ => { }
            }
        }
        if correct >= 5u {
            return true;
        }
    }
    return false;
}

#[test]
fn test_is_uuid() {
    assert is_uuid(~"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaafAF09");
    assert !is_uuid(~"aaaaaaaa-aaaa-aaaa-aaaaa-aaaaaaaaaaaa");
    assert !is_uuid(~"");
    assert !is_uuid(~"aaaaaaaa-aaa -aaaa-aaaa-aaaaaaaaaaaa");
    assert !is_uuid(~"aaaaaaaa-aaa!-aaaa-aaaa-aaaaaaaaaaaa");
    assert !is_uuid(~"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa-a");
    assert !is_uuid(~"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaป");
}

// FIXME (#2661): implement url/URL parsing so we don't have to resort
// to weak checks

fn has_archive_extension(p: ~str) -> bool {
    str::ends_with(p, ~".tar") ||
    str::ends_with(p, ~".tar.gz") ||
    str::ends_with(p, ~".tar.bz2") ||
    str::ends_with(p, ~".tar.Z") ||
    str::ends_with(p, ~".tar.lz") ||
    str::ends_with(p, ~".tar.xz") ||
    str::ends_with(p, ~".tgz") ||
    str::ends_with(p, ~".tbz") ||
    str::ends_with(p, ~".tbz2") ||
    str::ends_with(p, ~".tb2") ||
    str::ends_with(p, ~".taz") ||
    str::ends_with(p, ~".tlz") ||
    str::ends_with(p, ~".txz")
}

fn is_archive_path(u: ~str) -> bool {
    has_archive_extension(u) && os::path_exists(&Path(u))
}

fn is_archive_url(u: ~str) -> bool {
    // FIXME (#2661): this requires the protocol bit - if we had proper
    // url parsing, we wouldn't need it

    match str::find_str(u, ~"://") {
        option::Some(_) => has_archive_extension(u),
        _ => false
    }
}

fn is_git_url(url: ~str) -> bool {
    if str::ends_with(url, ~"/") { str::ends_with(url, ~".git/") }
    else {
        str::starts_with(url, ~"git://") || str::ends_with(url, ~".git")
    }
}

fn assume_source_method(url: ~str) -> ~str {
    if is_git_url(url) {
        return ~"git";
    }
    if str::starts_with(url, ~"file://") || os::path_exists(&Path(url)) {
        return ~"file";
    }

    ~"curl"
}

fn load_link(mis: ~[@ast::meta_item]) -> (Option<~str>,
                                         Option<~str>,
                                         Option<~str>) {
    let mut name = None;
    let mut vers = None;
    let mut uuid = None;
    for mis.each |a| {
        match a.node {
            ast::meta_name_value(v, {node: ast::lit_str(s), span: _}) => {
                match v {
                    ~"name" => name = Some(*s),
                    ~"vers" => vers = Some(*s),
                    ~"uuid" => uuid = Some(*s),
                    _ => { }
                }
            }
            _ => fail ~"load_link: meta items must be name-values"
        }
    }
    (name, vers, uuid)
}

fn load_crate(filename: &Path) -> Option<Crate> {
    let sess = parse::new_parse_sess(None);
    let c = parse::parse_crate_from_crate_file(filename, ~[], sess);

    let mut name = None;
    let mut vers = None;
    let mut uuid = None;
    let mut desc = None;
    let mut sigs = None;
    let mut crate_type = None;

    for c.node.attrs.each |a| {
        match a.node.value.node {
            ast::meta_name_value(v, {node: ast::lit_str(_), span: _}) => {
                match v {
                    ~"desc" => desc = Some(v),
                    ~"sigs" => sigs = Some(v),
                    ~"crate_type" => crate_type = Some(v),
                    _ => { }
                }
            }
            ast::meta_list(v, mis) => {
                if v == ~"link" {
                    let (n, v, u) = load_link(mis);
                    name = n;
                    vers = v;
                    uuid = u;
                }
            }
            _ => {
                fail ~"crate attributes may not contain " +
                     ~"meta_words";
            }
        }
    }

    type env = @{
        mut deps: ~[~str]
    };

    fn goto_view_item(ps: syntax::parse::parse_sess, e: env,
                      i: @ast::view_item) {
        match i.node {
            ast::view_item_use(ident, metas, _) => {
                let name_items =
                    attr::find_meta_items_by_name(metas, ~"name");
                let m = if name_items.is_empty() {
                    metas + ~[attr::mk_name_value_item_str(
                        ~"name", *ps.interner.get(ident))]
                } else {
                    metas
                };
                let mut attr_name = ident;
                let mut attr_vers = ~"";
                let mut attr_from = ~"";

              for m.each |item| {
                    match attr::get_meta_item_value_str(*item) {
                        Some(value) => {
                            let name = attr::get_meta_item_name(*item);

                            match name {
                                ~"vers" => attr_vers = value,
                                ~"from" => attr_from = value,
                                _ => ()
                            }
                        }
                        None => ()
                    }
                }

                let query = if !str::is_empty(attr_from) {
                    attr_from
                } else {
                    if !str::is_empty(attr_vers) {
                        ps.interner.get(attr_name) + ~"@" + attr_vers
                    } else { *ps.interner.get(attr_name) }
                };

                match *ps.interner.get(attr_name) {
                    ~"std" | ~"core" => (),
                    _ => e.deps.push(query)
                }
            }
            _ => ()
        }
    }
    fn goto_item(_e: env, _i: @ast::item) {
    }

    let e = @{
        mut deps: ~[]
    };
    let v = visit::mk_simple_visitor(@{
        visit_view_item: |a| goto_view_item(sess, e, a),
        visit_item: |a| goto_item(e, a),
        .. *visit::default_simple_visitor()
    });

    visit::visit_crate(*c, (), v);

    let deps = copy e.deps;

    match (name, vers, uuid) {
        (Some(name0), Some(vers0), Some(uuid0)) => {
            Some(Crate {
                name: name0,
                vers: vers0,
                uuid: uuid0,
                desc: desc,
                sigs: sigs,
                crate_type: crate_type,
                deps: deps })
        }
        _ => return None
    }
}

fn print(s: ~str) {
    io::stdout().write_line(s);
}

fn rest(s: ~str, start: uint) -> ~str {
    if (start >= str::len(s)) {
        ~""
    } else {
        str::slice(s, start, str::len(s))
    }
}

fn need_dir(s: &Path) {
    if os::path_is_dir(s) { return; }
    if !os::make_dir(s, 493_i32 /* oct: 755 */) {
        fail fmt!("can't make_dir %s", s.to_str());
    }
}

fn valid_pkg_name(s: &str) -> bool {
    fn is_valid_digit(+c: char) -> bool {
        ('0' <= c && c <= '9') ||
        ('a' <= c && c <= 'z') ||
        ('A' <= c && c <= 'Z') ||
        c == '-' ||
        c == '_'
    }

    s.all(is_valid_digit)
}

fn parse_source(name: ~str, j: &json::Json) -> @Source {
    if !valid_pkg_name(name) {
        fail fmt!("'%s' is an invalid source name", name);
    }

    match *j {
        json::Object(j) => {
            let mut url = match j.find(&~"url") {
                Some(json::String(u)) => u,
                _ => fail ~"needed 'url' field in source"
            };
            let method = match j.find(&~"method") {
                Some(json::String(u)) => u,
                _ => assume_source_method(url)
            };
            let key = match j.find(&~"key") {
                Some(json::String(u)) => Some(u),
                _ => None
            };
            let keyfp = match j.find(&~"keyfp") {
                Some(json::String(u)) => Some(u),
                _ => None
            };
            if method == ~"file" {
                url = os::make_absolute(&Path(url)).to_str();
            }
            return @Source {
                name: name,
                mut url: url,
                mut method: method,
                mut key: key,
                mut keyfp: keyfp,
                packages: DVec() };
        }
        _ => fail ~"needed dict value in source"
    };
}

fn try_parse_sources(filename: &Path, sources: map::HashMap<~str, @Source>) {
    if !os::path_exists(filename)  { return; }
    let c = io::read_whole_file_str(filename);
    match json::from_str(c.get()) {
        Ok(json::Object(j)) => {
            for j.each |k, v| {
                sources.insert(copy *k, parse_source(*k, v));
                debug!("source: %s", *k);
            }
        }
        Ok(_) => fail ~"malformed sources.json",
        Err(e) => fail fmt!("%s:%s", filename.to_str(), e.to_str())
    }
}

fn load_one_source_package(src: @Source, p: &json::Object) {
    let name = match p.find(&~"name") {
        Some(json::String(n)) => {
            if !valid_pkg_name(n) {
                warn(~"malformed source json: "
                     + src.name + ~", '" + n + ~"'"+
                     ~" is an invalid name (alphanumeric, underscores and" +
                     ~" dashes only)");
                return;
            }
            n
        }
        _ => {
            warn(~"malformed source json: " + src.name + ~" (missing name)");
            return;
        }
    };

    let uuid = match p.find(&~"uuid") {
        Some(json::String(n)) => {
            if !is_uuid(n) {
                warn(~"malformed source json: "
                     + src.name + ~", '" + n + ~"'"+
                     ~" is an invalid uuid");
                return;
            }
            n
        }
        _ => {
            warn(~"malformed source json: " + src.name + ~" (missing uuid)");
            return;
        }
    };

    let url = match p.find(&~"url") {
        Some(json::String(n)) => n,
        _ => {
            warn(~"malformed source json: " + src.name + ~" (missing url)");
            return;
        }
    };

    let method = match p.find(&~"method") {
        Some(json::String(n)) => n,
        _ => {
            warn(~"malformed source json: "
                 + src.name + ~" (missing method)");
            return;
        }
    };

    let reference = match p.find(&~"ref") {
        Some(json::String(n)) => Some(n),
        _ => None
    };

    let mut tags = ~[];
    match p.find(&~"tags") {
        Some(json::List(js)) => {
          for js.each |j| {
                match *j {
                    json::String(ref j) => tags.grow(1u, j),
                    _ => ()
                }
            }
        }
        _ => ()
    }

    let description = match p.find(&~"description") {
        Some(json::String(n)) => n,
        _ => {
            warn(~"malformed source json: " + src.name
                 + ~" (missing description)");
            return;
        }
    };

    let newpkg = Package {
        name: name,
        uuid: uuid,
        url: url,
        method: method,
        description: description,
        reference: reference,
        tags: tags,
        versions: ~[]
    };

    match src.packages.position(|pkg| pkg.uuid == uuid) {
        Some(idx) => {
            src.packages.set_elt(idx, newpkg);
            log(debug, ~"  updated package: " + src.name + ~"/" + name);
        }
        None => {
            src.packages.push(newpkg);
        }
    }

    log(debug, ~"  loaded package: " + src.name + ~"/" + name);
}

fn load_source_info(c: &Cargo, src: @Source) {
    let dir = c.sourcedir.push(src.name);
    let srcfile = dir.push("source.json");
    if !os::path_exists(&srcfile) { return; }
    let srcstr = io::read_whole_file_str(&srcfile);
    match json::from_str(srcstr.get()) {
        Ok(ref json @ json::Object(_)) => {
            let o = parse_source(src.name, json);

            src.key = o.key;
            src.keyfp = o.keyfp;
        }
        Ok(_) => {
            warn(~"malformed source.json: " + src.name +
                 ~"(source info is not a dict)");
        }
        Err(e) => {
            warn(fmt!("%s:%s", src.name, e.to_str()));
        }
    };
}
fn load_source_packages(c: &Cargo, src: @Source) {
    log(debug, ~"loading source: " + src.name);
    let dir = c.sourcedir.push(src.name);
    let pkgfile = dir.push("packages.json");
    if !os::path_exists(&pkgfile) { return; }
    let pkgstr = io::read_whole_file_str(&pkgfile);
    match json::from_str(pkgstr.get()) {
        Ok(json::List(js)) => {
          for js.each |j| {
                match *j {
                    json::Object(p) => {
                        load_one_source_package(src, p);
                    }
                    _ => {
                        warn(~"malformed source json: " + src.name +
                             ~" (non-dict pkg)");
                    }
                }
            }
        }
        Ok(_) => {
            warn(~"malformed packages.json: " + src.name +
                 ~"(packages is not a list)");
        }
        Err(e) => {
            warn(fmt!("%s:%s", src.name, e.to_str()));
        }
    };
}

fn build_cargo_options(argv: ~[~str]) -> Options {
    let matches = match getopts::getopts(argv, opts()) {
        result::Ok(m) => m,
        result::Err(f) => {
            fail fmt!("%s", getopts::fail_str(f));
        }
    };

    let test = opt_present(matches, ~"test");
    let G    = opt_present(matches, ~"G");
    let g    = opt_present(matches, ~"g");
    let help = opt_present(matches, ~"h") || opt_present(matches, ~"help");
    let len  = vec::len(matches.free);

    let is_install = len > 1u && matches.free[1] == ~"install";
    let is_uninstall = len > 1u && matches.free[1] == ~"uninstall";

    if G && g { fail ~"-G and -g both provided"; }

    if !is_install && !is_uninstall && (g || G) {
        fail ~"-g and -G are only valid for `install` and `uninstall|rm`";
    }

    let mode =
        if (!is_install && !is_uninstall) || g { UserMode }
        else if G { SystemMode }
        else { LocalMode };

    Options {test: test, mode: mode, free: matches.free, help: help}
}

fn configure(opts: Options) -> Cargo {
    let home = match get_cargo_root() {
        Ok(home) => home,
        Err(_err) => get_cargo_sysroot().get()
    };

    let get_cargo_dir = match opts.mode {
        SystemMode => get_cargo_sysroot,
        UserMode => get_cargo_root,
        LocalMode => get_cargo_root_nearest
    };

    let p = get_cargo_dir().get();

    let sources = HashMap();
    try_parse_sources(&home.push("sources.json"), sources);
    try_parse_sources(&home.push("local-sources.json"), sources);

    let dep_cache = HashMap();

    let mut c = Cargo {
        pgp: pgp::supported(),
        root: home,
        installdir: p,
        bindir: p.push("bin"),
        libdir: p.push("lib"),
        workdir: p.push("work"),
        sourcedir: home.push("sources"),
        sources: sources,
        mut current_install: ~"",
        dep_cache: dep_cache,
        opts: opts
    };

    need_dir(&c.root);
    need_dir(&c.installdir);
    need_dir(&c.sourcedir);
    need_dir(&c.workdir);
    need_dir(&c.libdir);
    need_dir(&c.bindir);

    for sources.each_key |k| {
        let mut s = sources.get(k);
        load_source_packages(&c, s);
        sources.insert(k, s);
    }

    if c.pgp {
        pgp::init(&c.root);
    } else {
        warn(~"command `gpg` was not found");
        warn(~"you have to install gpg from source " +
             ~" or package manager to get it to work correctly");
    }

    c
}

fn for_each_package(c: &Cargo, b: fn(s: @Source, p: &Package)) {
    for c.sources.each_value |v| {
        for v.packages.each |p| {
            b(v, p);
        }
    }
}

// Runs all programs in directory <buildpath>
fn run_programs(buildpath: &Path) {
    let newv = os::list_dir_path(buildpath);
    for newv.each |ct| {
        run::run_program(ct.to_str(), ~[]);
    }
}

// Runs rustc in <path + subdir> with the given flags
// and returns <patho + subdir>
fn run_in_buildpath(what: &str, path: &Path, subdir: &Path, cf: &Path,
                    extra_flags: ~[~str]) -> Option<Path> {
    let buildpath = path.push_rel(subdir);
    need_dir(&buildpath);
    debug!("%s: %s -> %s", what, cf.to_str(), buildpath.to_str());
    let p = run::program_output(rustc_sysroot(),
                                ~[~"--out-dir",
                                  buildpath.to_str(),
                                  cf.to_str()] + extra_flags);
    if p.status != 0 {
        error(fmt!("rustc failed: %d\n%s\n%s", p.status, p.err, p.out));
        return None;
    }
    Some(buildpath)
}

fn test_one_crate(_c: &Cargo, path: &Path, cf: &Path) {
    let buildpath = match run_in_buildpath(~"testing", path,
                                           &Path("test"),
                                           cf,
                                           ~[ ~"--test"]) {
      None => return,
    Some(bp) => bp
  };
  run_programs(&buildpath);
}

fn install_one_crate(c: &Cargo, path: &Path, cf: &Path) {
    let buildpath = match run_in_buildpath(~"installing", path,
                                           &Path("build"),
                                           cf, ~[]) {
      None => return,
      Some(bp) => bp
    };
    let newv = os::list_dir_path(&buildpath);
    let exec_suffix = os::exe_suffix();
    for newv.each |ct| {
        if (exec_suffix != ~"" && str::ends_with(ct.to_str(),
                                                 exec_suffix)) ||
            (exec_suffix == ~"" &&
             !str::starts_with(ct.filename().get(),
                               ~"lib")) {
            debug!("  bin: %s", ct.to_str());
            install_to_dir(*ct, &c.bindir);
            if c.opts.mode == SystemMode {
                // FIXME (#2662): Put this file in PATH / symlink it so it can
                // be used as a generic executable
                // `cargo install -G rustray` and `rustray file.obj`
            }
        } else {
            debug!("  lib: %s", ct.to_str());
            install_to_dir(*ct, &c.libdir);
        }
    }
}


fn rustc_sysroot() -> ~str {
    match os::self_exe_path() {
        Some(path) => {
            let rustc = path.push_many([~"..", ~"bin", ~"rustc"]);
            debug!("  rustc: %s", rustc.to_str());
            rustc.to_str()
        }
        None => ~"rustc"
    }
}

fn install_source(c: &Cargo, path: &Path) {
    debug!("source: %s", path.to_str());
    os::change_dir(path);

    let mut cratefiles = ~[];
    for os::walk_dir(&Path(".")) |p| {
        if p.filetype() == Some(~".rc") {
            cratefiles.push(*p);
        }
    }

    if vec::is_empty(cratefiles) {
        fail ~"this doesn't look like a rust package (no .rc files)";
    }

    for cratefiles.each |cf| {
        match load_crate(cf) {
            None => loop,
            Some(crate) => {
              for crate.deps.each |query| {
                    // FIXME (#1356): handle cyclic dependencies
                    // (n.b. #1356 says "Cyclic dependency is an error
                    // condition")

                    let wd = get_temp_workdir(c);
                    install_query(c, &wd, *query);
                }

                os::change_dir(path);

                if c.opts.test {
                    test_one_crate(c, path, cf);
                }
                install_one_crate(c, path, cf);
            }
        }
    }
}

fn install_git(c: &Cargo, wd: &Path, url: ~str, reference: Option<~str>) {
    run::program_output(~"git", ~[~"clone", url, wd.to_str()]);
    if reference.is_some() {
        let r = reference.get();
        os::change_dir(wd);
        run::run_program(~"git", ~[~"checkout", r]);
    }

    install_source(c, wd);
}

fn install_curl(c: &Cargo, wd: &Path, url: ~str) {
    let tarpath = wd.push("pkg.tar");
    let p = run::program_output(~"curl", ~[~"-f", ~"-s", ~"-o",
                                         tarpath.to_str(), url]);
    if p.status != 0 {
        fail fmt!("fetch of %s failed: %s", url, p.err);
    }
    run::run_program(~"tar", ~[~"-x", ~"--strip-components=1",
                               ~"-C", wd.to_str(),
                               ~"-f", tarpath.to_str()]);
    install_source(c, wd);
}

fn install_file(c: &Cargo, wd: &Path, path: &Path) {
    run::program_output(~"tar", ~[~"-x", ~"--strip-components=1",
                                  ~"-C", wd.to_str(),
                                  ~"-f", path.to_str()]);
    install_source(c, wd);
}

fn install_package(c: &Cargo, src: ~str, wd: &Path, pkg: Package) {
    let url = copy pkg.url;
    let method = match pkg.method {
        ~"git" => ~"git",
        ~"file" => ~"file",
        _ => ~"curl"
    };

    info(fmt!("installing %s/%s via %s...", src, pkg.name, method));

    match method {
        ~"git" => install_git(c, wd, url, copy pkg.reference),
        ~"file" => install_file(c, wd, &Path(url)),
        ~"curl" => install_curl(c, wd, url),
        _ => ()
    }
}

fn cargo_suggestion(c: &Cargo, fallback: fn())
{
    if c.sources.size() == 0u {
        error(~"no sources defined - you may wish to run " +
              ~"`cargo init`");
        return;
    }
    fallback();
}

fn install_uuid(c: &Cargo, wd: &Path, uuid: ~str) {
    let mut ps = ~[];
    for_each_package(c, |s, p| {
        if p.uuid == uuid {
            vec::push(&mut ps, (s.name, copy *p));
        }
    });
    if vec::len(ps) == 1u {
        let (sname, p) = copy ps[0];
        install_package(c, sname, wd, p);
        return;
    } else if vec::len(ps) == 0u {
        cargo_suggestion(c, || {
            error(~"can't find package: " + uuid);
        });
        return;
    }
    error(~"found multiple packages:");
    for ps.each |elt| {
        let (sname,p) = copy *elt;
        info(~"  " + sname + ~"/" + p.uuid + ~" (" + p.name + ~")");
    }
}

fn install_named(c: &Cargo, wd: &Path, name: ~str) {
    let mut ps = ~[];
    for_each_package(c, |s, p| {
        if p.name == name {
            vec::push(&mut ps, (s.name, copy *p));
        }
    });
    if vec::len(ps) == 1u {
        let (sname, p) = copy ps[0];
        install_package(c, sname, wd, p);
        return;
    } else if vec::len(ps) == 0u {
        cargo_suggestion(c, || {
            error(~"can't find package: " + name);
        });
        return;
    }
    error(~"found multiple packages:");
    for ps.each |elt| {
        let (sname,p) = copy *elt;
        info(~"  " + sname + ~"/" + p.uuid + ~" (" + p.name + ~")");
    }
}

fn install_uuid_specific(c: &Cargo, wd: &Path, src: ~str, uuid: ~str) {
    match c.sources.find(src) {
        Some(s) => {
            for s.packages.each |p| {
                if p.uuid == uuid {
                    install_package(c, src, wd, *p);
                    return;
                }
            }
        }
        _ => ()
    }
    error(~"can't find package: " + src + ~"/" + uuid);
}

fn install_named_specific(c: &Cargo, wd: &Path, src: ~str, name: ~str) {
    match c.sources.find(src) {
        Some(s) => {
            for s.packages.each |p| {
                if p.name == name {
                    install_package(c, src, wd, *p);
                    return;
                }
            }
        }
        _ => ()
    }
    error(~"can't find package: " + src + ~"/" + name);
}

fn cmd_uninstall(c: &Cargo) {
    if vec::len(c.opts.free) < 3u {
        cmd_usage();
        return;
    }

    let lib = &c.libdir;
    let bin = &c.bindir;
    let target = c.opts.free[2u];

    // FIXME (#2662): needs stronger pattern matching
    // FIXME (#2662): needs to uninstall from a specified location in a
    // cache instead of looking for it (binaries can be uninstalled by
    // name only)

    fn try_uninstall(p: &Path) -> bool {
        if os::remove_file(p) {
            info(~"uninstalled: '" + p.to_str() + ~"'");
            true
        } else {
            error(~"could not uninstall: '" +
                  p.to_str() + ~"'");
            false
        }
    }

    if is_uuid(target) {
        for os::list_dir(lib).each |file| {
            match str::find_str(*file, ~"-" + target + ~"-") {
              Some(_) => if !try_uninstall(&lib.push(*file)) { return },
              None => ()
            }
        }
        error(~"can't find package with uuid: " + target);
    } else {
        for os::list_dir(lib).each |file| {
            match str::find_str(*file, ~"lib" + target + ~"-") {
              Some(_) => if !try_uninstall(&lib.push(*file)) { return },
              None => ()
            }
        }
        for os::list_dir(bin).each |file| {
            match str::find_str(*file, target) {
              Some(_) => if !try_uninstall(&lib.push(*file)) { return },
              None => ()
            }
        }

        error(~"can't find package with name: " + target);
    }
}

fn install_query(c: &Cargo, wd: &Path, target: ~str) {
    match c.dep_cache.find(target) {
        Some(inst) => {
            if inst {
                return;
            }
        }
        None => ()
    }

    c.dep_cache.insert(target, true);

    if is_archive_path(target) {
        install_file(c, wd, &Path(target));
        return;
    } else if is_git_url(target) {
        let reference = if c.opts.free.len() >= 4u {
            Some(c.opts.free[3u])
        } else {
            None
        };
        install_git(c, wd, target, reference);
    } else if !valid_pkg_name(target) && has_archive_extension(target) {
        install_curl(c, wd, target);
        return;
    } else {
        let mut ps = copy target;

        match str::find_char(ps, '/') {
            option::Some(idx) => {
                let source = str::slice(ps, 0u, idx);
                ps = str::slice(ps, idx + 1u, str::len(ps));
                if is_uuid(ps) {
                    install_uuid_specific(c, wd, source, ps);
                } else {
                    install_named_specific(c, wd, source, ps);
                }
            }
            option::None => {
                if is_uuid(ps) {
                    install_uuid(c, wd, ps);
                } else {
                    install_named(c, wd, ps);
                }
            }
        }
    }

    // FIXME (#2662): This whole dep_cache and current_install thing is
    // a bit of a hack. It should be cleaned up in the future.

    if target == c.current_install {
        for c.dep_cache.each |k, _v| {
            c.dep_cache.remove(k);
        }

        c.current_install = ~"";
    }
}

fn get_temp_workdir(c: &Cargo) -> Path {
    match tempfile::mkdtemp(&c.workdir, "cargo") {
      Some(wd) => wd,
      None => fail fmt!("needed temp dir: %s",
                        c.workdir.to_str())
    }
}

fn cmd_install(c: &Cargo) unsafe {
    let wd = get_temp_workdir(c);

    if vec::len(c.opts.free) == 2u {
        let cwd = os::getcwd();
        let status = run::run_program(~"cp", ~[~"-R", cwd.to_str(),
                                               wd.to_str()]);

        if status != 0 {
            fail fmt!("could not copy directory: %s", cwd.to_str());
        }

        install_source(c, &wd);
        return;
    }

    sync(c);

    let query = c.opts.free[2];
    c.current_install = query.to_str();

    install_query(c, &wd, query);
}

fn sync(c: &Cargo) {
    for c.sources.each_key |k| {
        let mut s = c.sources.get(k);
        sync_one(c, s);
        c.sources.insert(k, s);
    }
}

fn sync_one_file(c: &Cargo, dir: &Path, src: @Source) -> bool {
    let name = src.name;
    let srcfile = dir.push("source.json.new");
    let destsrcfile = dir.push("source.json");
    let pkgfile = dir.push("packages.json.new");
    let destpkgfile = dir.push("packages.json");
    let keyfile = dir.push("key.gpg");
    let srcsigfile = dir.push("source.json.sig");
    let sigfile = dir.push("packages.json.sig");
    let url = Path(src.url);
    let mut has_src_file = false;

    if !os::copy_file(&url.push("packages.json"), &pkgfile) {
        error(fmt!("fetch for source %s (url %s) failed",
                   name, url.to_str()));
        return false;
    }

    if os::copy_file(&url.push("source.json"), &srcfile) {
        has_src_file = false;
    }

    os::copy_file(&url.push("source.json.sig"), &srcsigfile);
    os::copy_file(&url.push("packages.json.sig"), &sigfile);

    match copy src.key {
        Some(u) => {
            let p = run::program_output(~"curl",
                                        ~[~"-f", ~"-s",
                                          ~"-o", keyfile.to_str(), u]);
            if p.status != 0 {
                error(fmt!("fetch for source %s (key %s) failed", name, u));
                return false;
            }
            pgp::add(&c.root, &keyfile);
        }
        _ => ()
    }
    match (src.key, src.keyfp) {
        (Some(_), Some(f)) => {
            let r = pgp::verify(&c.root, &pkgfile, &sigfile, f);

            if !r {
                error(fmt!("signature verification failed for source %s",
                          name));
                return false;
            }

            if has_src_file {
                let e = pgp::verify(&c.root, &srcfile, &srcsigfile, f);

                if !e {
                    error(fmt!("signature verification failed for source %s",
                              name));
                    return false;
                }
            }
        }
        _ => ()
    }

    copy_warn(&pkgfile, &destpkgfile);

    if has_src_file {
        copy_warn(&srcfile, &destsrcfile);
    }

    os::remove_file(&keyfile);
    os::remove_file(&srcfile);
    os::remove_file(&srcsigfile);
    os::remove_file(&pkgfile);
    os::remove_file(&sigfile);

    info(fmt!("synced source: %s", name));

    return true;
}

fn sync_one_git(c: &Cargo, dir: &Path, src: @Source) -> bool {
    let name = src.name;
    let srcfile = dir.push("source.json");
    let pkgfile = dir.push("packages.json");
    let keyfile = dir.push("key.gpg");
    let srcsigfile = dir.push("source.json.sig");
    let sigfile = dir.push("packages.json.sig");
    let url = src.url;

    fn rollback(name: ~str, dir: &Path, insecure: bool) {
        fn msg(name: ~str, insecure: bool) {
            error(fmt!("could not rollback source: %s", name));

            if insecure {
                warn(~"a past security check failed on source " +
                     name + ~" and rolling back the source failed -"
                     + ~" this source may be compromised");
            }
        }

        if !os::change_dir(dir) {
            msg(name, insecure);
        }
        else {
            let p = run::program_output(~"git", ~[~"reset", ~"--hard",
                                                ~"HEAD@{1}"]);

            if p.status != 0 {
                msg(name, insecure);
            }
        }
    }

    if !os::path_exists(&dir.push(".git")) {
        let p = run::program_output(~"git", ~[~"clone", url, dir.to_str()]);

        if p.status != 0 {
            error(fmt!("fetch for source %s (url %s) failed", name, url));
            return false;
        }
    }
    else {
        if !os::change_dir(dir) {
            error(fmt!("fetch for source %s (url %s) failed", name, url));
            return false;
        }

        let p = run::program_output(~"git", ~[~"pull"]);

        if p.status != 0 {
            error(fmt!("fetch for source %s (url %s) failed", name, url));
            return false;
        }
    }

    let has_src_file = os::path_exists(&srcfile);

    match copy src.key {
        Some(u) => {
            let p = run::program_output(~"curl",
                                        ~[~"-f", ~"-s",
                                          ~"-o", keyfile.to_str(), u]);
            if p.status != 0 {
                error(fmt!("fetch for source %s (key %s) failed", name, u));
                rollback(name, dir, false);
                return false;
            }
            pgp::add(&c.root, &keyfile);
        }
        _ => ()
    }
    match (src.key, src.keyfp) {
        (Some(_), Some(f)) => {
            let r = pgp::verify(&c.root, &pkgfile, &sigfile, f);

            if !r {
                error(fmt!("signature verification failed for source %s",
                          name));
                rollback(name, dir, false);
                return false;
            }

            if has_src_file {
                let e = pgp::verify(&c.root, &srcfile, &srcsigfile, f);

                if !e {
                    error(fmt!("signature verification failed for source %s",
                              name));
                    rollback(name, dir, false);
                    return false;
                }
            }
        }
        _ => ()
    }

    os::remove_file(&keyfile);

    info(fmt!("synced source: %s", name));

    return true;
}

fn sync_one_curl(c: &Cargo, dir: &Path, src: @Source) -> bool {
    let name = src.name;
    let srcfile = dir.push("source.json.new");
    let destsrcfile = dir.push("source.json");
    let pkgfile = dir.push("packages.json.new");
    let destpkgfile = dir.push("packages.json");
    let keyfile = dir.push("key.gpg");
    let srcsigfile = dir.push("source.json.sig");
    let sigfile = dir.push("packages.json.sig");
    let mut url = src.url;
    let smart = !str::ends_with(src.url, ~"packages.json");
    let mut has_src_file = false;

    if smart {
        url += ~"/packages.json";
    }

    let p = run::program_output(~"curl",
                                ~[~"-f", ~"-s",
                                  ~"-o", pkgfile.to_str(), url]);

    if p.status != 0 {
        error(fmt!("fetch for source %s (url %s) failed", name, url));
        return false;
    }
    if smart {
        url = src.url + ~"/source.json";
        let p =
            run::program_output(~"curl",
                                ~[~"-f", ~"-s",
                                  ~"-o", srcfile.to_str(), url]);

        if p.status == 0 {
            has_src_file = true;
        }
    }

    match copy src.key {
       Some(u) => {
            let p = run::program_output(~"curl",
                                        ~[~"-f", ~"-s",
                                          ~"-o", keyfile.to_str(), u]);
            if p.status != 0 {
                error(fmt!("fetch for source %s (key %s) failed", name, u));
                return false;
            }
            pgp::add(&c.root, &keyfile);
        }
        _ => ()
    }
    match (src.key, src.keyfp) {
        (Some(_), Some(f)) => {
            if smart {
                url = src.url + ~"/packages.json.sig";
            }
            else {
                url = src.url + ~".sig";
            }

            let mut p = run::program_output(~"curl",
                                            ~[~"-f", ~"-s", ~"-o",
                                              sigfile.to_str(), url]);
            if p.status != 0 {
                error(fmt!("fetch for source %s (sig %s) failed", name, url));
                return false;
            }

            let r = pgp::verify(&c.root, &pkgfile, &sigfile, f);

            if !r {
                error(fmt!("signature verification failed for source %s",
                          name));
                return false;
            }

            if smart && has_src_file {
                url = src.url + ~"/source.json.sig";

                p = run::program_output(~"curl",
                                        ~[~"-f", ~"-s", ~"-o",
                                          srcsigfile.to_str(), url]);
                if p.status != 0 {
                    error(fmt!("fetch for source %s (sig %s) failed",
                          name, url));
                    return false;
                }

                let e = pgp::verify(&c.root, &srcfile, &srcsigfile, f);

                if !e {
                    error(~"signature verification failed for " +
                          ~"source " + name);
                    return false;
                }
            }
        }
        _ => ()
    }

    copy_warn(&pkgfile, &destpkgfile);

    if smart && has_src_file {
        copy_warn(&srcfile, &destsrcfile);
    }

    os::remove_file(&keyfile);
    os::remove_file(&srcfile);
    os::remove_file(&srcsigfile);
    os::remove_file(&pkgfile);
    os::remove_file(&sigfile);

    info(fmt!("synced source: %s", name));

    return true;
}

fn sync_one(c: &Cargo, src: @Source) {
    let name = src.name;
    let dir = c.sourcedir.push(name);

    info(fmt!("syncing source: %s...", name));

    need_dir(&dir);

    let result = match src.method {
        ~"git" => sync_one_git(c, &dir, src),
        ~"file" => sync_one_file(c, &dir, src),
        _ => sync_one_curl(c, &dir, src)
    };

    if result {
        load_source_info(c, src);
        load_source_packages(c, src);
    }
}

fn cmd_init(c: &Cargo) {
    let srcurl = ~"http://www.rust-lang.org/cargo/sources.json";
    let sigurl = ~"http://www.rust-lang.org/cargo/sources.json.sig";

    let srcfile = c.root.push("sources.json.new");
    let sigfile = c.root.push("sources.json.sig");
    let destsrcfile = c.root.push("sources.json");

    let p =
        run::program_output(~"curl", ~[~"-f", ~"-s",
                                       ~"-o", srcfile.to_str(), srcurl]);
    if p.status != 0 {
        error(fmt!("fetch of sources.json failed: %s", p.out));
        return;
    }

    let p =
        run::program_output(~"curl", ~[~"-f", ~"-s",
                                       ~"-o", sigfile.to_str(), sigurl]);
    if p.status != 0 {
        error(fmt!("fetch of sources.json.sig failed: %s", p.out));
        return;
    }

    let r = pgp::verify(&c.root, &srcfile, &sigfile,
                        pgp::signing_key_fp());
    if !r {
        error(fmt!("signature verification failed for '%s'",
                   srcfile.to_str()));
        return;
    }

    copy_warn(&srcfile, &destsrcfile);
    os::remove_file(&srcfile);
    os::remove_file(&sigfile);

    info(fmt!("initialized .cargo in %s", c.root.to_str()));
}

fn print_pkg(s: @Source, p: &Package) {
    let mut m = s.name + ~"/" + p.name + ~" (" + p.uuid + ~")";
    if vec::len(p.tags) > 0u {
        m = m + ~" [" + str::connect(p.tags, ~", ") + ~"]";
    }
    info(m);
    if p.description != ~"" {
        print(~"   >> " + p.description + ~"\n")
    }
}

fn print_source(s: @Source) {
    info(s.name + ~" (" + s.url + ~")");

    let pks = sort::merge_sort(sys::shape_lt, s.packages.get());
    let l = vec::len(pks);

    print(io::with_str_writer(|writer| {
        let mut list = ~"   >> ";

        for vec::eachi(pks) |i, pk| {
            if str::len(list) > 78u {
                writer.write_line(list);
                list = ~"   >> ";
            }
            list += pk.name + (if l - 1u == i { ~"" } else { ~", " });
        }

        writer.write_line(list);
    }));
}

fn cmd_list(c: &Cargo) {
    sync(c);

    if vec::len(c.opts.free) >= 3u {
        let v = vec::view(c.opts.free, 2u, vec::len(c.opts.free));
        for vec::each(v) |name| {
            if !valid_pkg_name(*name) {
                error(fmt!("'%s' is an invalid source name", *name));
            } else {
                match c.sources.find(*name) {
                    Some(source) => {
                        print_source(source);
                    }
                    None => {
                        error(fmt!("no such source: %s", *name));
                    }
                }
            }
        }
    } else {
        for c.sources.each_value |v| {
            print_source(v);
        }
    }
}

fn cmd_search(c: &Cargo) {
    if vec::len(c.opts.free) < 3u {
        cmd_usage();
        return;
    }

    sync(c);

    let mut n = 0;
    let name = c.opts.free[2];
    let tags = vec::slice(c.opts.free, 3u, vec::len(c.opts.free));
    for_each_package(c, |s, p| {
        if (str::contains(p.name, name) || name == ~"*") &&
            vec::all(tags, |t| vec::contains(p.tags, t) ) {
            print_pkg(s, p);
            n += 1;
        }
    });
    info(fmt!("found %d packages", n));
}

fn install_to_dir(srcfile: &Path, destdir: &Path) {
    let newfile = destdir.push(srcfile.filename().get());

    let status = run::run_program(~"cp", ~[~"-r", srcfile.to_str(),
                                           newfile.to_str()]);
    if status == 0 {
        info(fmt!("installed: '%s'", newfile.to_str()));
    } else {
        error(fmt!("could not install: '%s'", newfile.to_str()));
    }
}

fn dump_cache(c: &Cargo) {
    need_dir(&c.root);

    let out = c.root.push("cache.json");
    let _root = json::Object(~LinearMap());

    if os::path_exists(&out) {
        copy_warn(&out, &c.root.push("cache.json.old"));
    }
}
fn dump_sources(c: &Cargo) {
    if c.sources.size() < 1u {
        return;
    }

    need_dir(&c.root);

    let out = c.root.push("sources.json");

    if os::path_exists(&out) {
        copy_warn(&out, &c.root.push("sources.json.old"));
    }

    match io::buffered_file_writer(&out) {
        result::Ok(writer) => {
            let mut hash = ~LinearMap();

            for c.sources.each |k, v| {
                let mut chash = ~LinearMap();

                chash.insert(~"url", json::String(v.url));
                chash.insert(~"method", json::String(v.method));

                match copy v.key {
                    Some(key) => {
                        chash.insert(~"key", json::String(copy key));
                    }
                    _ => ()
                }
                match copy v.keyfp {
                    Some(keyfp) => {
                        chash.insert(~"keyfp", json::String(copy keyfp));
                    }
                    _ => ()
                }

                hash.insert(copy k, json::Object(chash));
            }

            json::to_writer(writer, &json::Object(hash))
        }
        result::Err(e) => {
            error(fmt!("could not dump sources: %s", e));
        }
    }
}

fn copy_warn(srcfile: &Path, destfile: &Path) {
    if !os::copy_file(srcfile, destfile) {
        warn(fmt!("copying %s to %s failed",
                  srcfile.to_str(), destfile.to_str()));
    }
}

fn cmd_sources(c: &Cargo) {
    if vec::len(c.opts.free) < 3u {
        for c.sources.each_value |v| {
            info(fmt!("%s (%s) via %s",
                      v.name, v.url, v.method));
        }
        return;
    }

    let action = c.opts.free[2u];

    match action {
        ~"clear" => {
          for c.sources.each_key |k| {
                c.sources.remove(k);
            }

            info(~"cleared sources");
        }
        ~"add" => {
            if vec::len(c.opts.free) < 5u {
                cmd_usage();
                return;
            }

            let name = c.opts.free[3u];
            let url = c.opts.free[4u];

            if !valid_pkg_name(name) {
                error(fmt!("'%s' is an invalid source name", name));
                return;
            }

            if c.sources.contains_key(name) {
                error(fmt!("source already exists: %s", name));
            } else {
                c.sources.insert(name, @Source {
                    name: name,
                    mut url: url,
                    mut method: assume_source_method(url),
                    mut key: None,
                    mut keyfp: None,
                    packages: DVec()
                });
                info(fmt!("added source: %s", name));
            }
        }
        ~"remove" => {
            if vec::len(c.opts.free) < 4u {
                cmd_usage();
                return;
            }

            let name = c.opts.free[3u];

            if !valid_pkg_name(name) {
                error(fmt!("'%s' is an invalid source name", name));
                return;
            }

            if c.sources.contains_key(name) {
                c.sources.remove(name);
                info(fmt!("removed source: %s", name));
            } else {
                error(fmt!("no such source: %s", name));
            }
        }
        ~"set-url" => {
            if vec::len(c.opts.free) < 5u {
                cmd_usage();
                return;
            }

            let name = c.opts.free[3u];
            let url = c.opts.free[4u];

            if !valid_pkg_name(name) {
                error(fmt!("'%s' is an invalid source name", name));
                return;
            }

            match c.sources.find(name) {
                Some(source) => {
                    let old = copy source.url;
                    let method = assume_source_method(url);

                    source.url = url;
                    source.method = method;

                    c.sources.insert(name, source);

                    info(fmt!("changed source url: '%s' to '%s'", old, url));
                }
                None => {
                    error(fmt!("no such source: %s", name));
                }
            }
        }
        ~"set-method" => {
            if vec::len(c.opts.free) < 5u {
                cmd_usage();
                return;
            }

            let name = c.opts.free[3u];
            let method = c.opts.free[4u];

            if !valid_pkg_name(name) {
                error(fmt!("'%s' is an invalid source name", name));
                return;
            }

            match c.sources.find(name) {
                Some(source) => {
                    let old = copy source.method;

                    source.method = match method {
                        ~"git" => ~"git",
                        ~"file" => ~"file",
                        _ => ~"curl"
                    };

                    c.sources.insert(name, source);

                    info(fmt!("changed source method: '%s' to '%s'", old,
                         method));
                }
                None => {
                    error(fmt!("no such source: %s", name));
                }
            }
        }
        ~"rename" => {
            if vec::len(c.opts.free) < 5u {
                cmd_usage();
                return;
            }

            let name = c.opts.free[3u];
            let newn = c.opts.free[4u];

            if !valid_pkg_name(name) {
                error(fmt!("'%s' is an invalid source name", name));
                return;
            }
            if !valid_pkg_name(newn) {
                error(fmt!("'%s' is an invalid source name", newn));
                return;
            }

            match c.sources.find(name) {
                Some(source) => {
                    c.sources.remove(name);
                    c.sources.insert(newn, source);
                    info(fmt!("renamed source: %s to %s", name, newn));
                }
                None => {
                    error(fmt!("no such source: %s", name));
                }
            }
        }
        _ => cmd_usage()
    }
}

fn cmd_usage() {
    print(~"Usage: cargo <cmd> [options] [args..]
e.g. cargo install <name>

Where <cmd> is one of:
    init, install, list, search, sources,
    uninstall, usage

Options:

    -h, --help                  Display this message
    <cmd> -h, <cmd> --help      Display help for <cmd>
");
}

fn cmd_usage_init() {
    print(~"cargo init

Re-initialize cargo in ~/.cargo. Clears all sources and then adds the
default sources from <www.rust-lang.org/sources.json>.");
}

fn cmd_usage_install() {
    print(~"cargo install
cargo install [source/]<name>[@version]
cargo install [source/]<uuid>[@version]
cargo install <git url> [ref]
cargo install <tarball url>
cargo install <tarball file>

Options:
    --test      Run crate tests before installing
    -g          Install to the user level (~/.cargo/bin/ instead of
                locally in ./.cargo/bin/ by default)
    -G          Install to the system level (/usr/local/lib/cargo/bin/)

Install a crate. If no arguments are supplied, it installs from
the current working directory. If a source is provided, only install
from that source, otherwise it installs from any source.");
}

fn cmd_usage_uninstall() {
    print(~"cargo uninstall [source/]<name>[@version]
cargo uninstall [source/]<uuid>[@version]
cargo uninstall <meta-name>[@version]
cargo uninstall <meta-uuid>[@version]

Options:
    -g          Remove from the user level (~/.cargo/bin/ instead of
                locally in ./.cargo/bin/ by default)
    -G          Remove from the system level (/usr/local/lib/cargo/bin/)

Remove a crate. If a source is provided, only remove
from that source, otherwise it removes from any source.
If a crate was installed directly (git, tarball, etc.), you can remove
it by metadata.");
}

fn cmd_usage_list() {
    print(~"cargo list [sources..]

If no arguments are provided, list all sources and their packages.
If source names are provided, list those sources and their packages.
");
}

fn cmd_usage_search() {
    print(~"cargo search <query | '*'> [tags..]

Search packages.");
}

fn cmd_usage_sources() {
    print(~"cargo sources
cargo sources add <name> <url>
cargo sources remove <name>
cargo sources rename <name> <new>
cargo sources set-url <name> <url>
cargo sources set-method <name> <method>

If no arguments are supplied, list all sources (but not their packages).

Commands:
    add             Add a source. The source method will be guessed
                    from the URL.
    remove          Remove a source.
    rename          Rename a source.
    set-url         Change the URL for a source.
    set-method      Change the method for a source.");
}

fn main() {
    let argv = os::args();
    let o = build_cargo_options(argv);

    if vec::len(o.free) < 2u {
        cmd_usage();
        return;
    }
    if o.help {
        match o.free[1] {
            ~"init" => cmd_usage_init(),
            ~"install" => cmd_usage_install(),
            ~"uninstall" => cmd_usage_uninstall(),
            ~"list" => cmd_usage_list(),
            ~"search" => cmd_usage_search(),
            ~"sources" => cmd_usage_sources(),
            _ => cmd_usage()
        }
        return;
    }
    if o.free[1] == ~"usage" {
        cmd_usage();
        return;
    }

    let mut c = configure(o);
    let home = c.root;
    let first_time = os::path_exists(&home.push("sources.json"));

    if !first_time && o.free[1] != ~"init" {
        cmd_init(&c);

        // FIXME (#2662): shouldn't need to reconfigure
        c = configure(o);
    }

    let c = &move c;

    match o.free[1] {
        ~"init" => cmd_init(c),
        ~"install" => cmd_install(c),
        ~"uninstall" => cmd_uninstall(c),
        ~"list" => cmd_list(c),
        ~"search" => cmd_search(c),
        ~"sources" => cmd_sources(c),
        _ => cmd_usage()
    }

    dump_cache(c);
    dump_sources(c);
}
