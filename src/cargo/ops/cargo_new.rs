use std::env;
use std::fs::{self, File, DirEntry, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::collections::BTreeMap;
use rustc_serialize::{Decodable, Decoder};

use git2::Config as GitConfig;
use git2::Repository;

use term::color::BLACK;

use handlebars::{Handlebars, Context, no_escape};
use tempdir::TempDir;

use core::Workspace;
use util::{GitRepo, HgRepo, CargoResult, human, ChainError, internal};
use util::{Config, paths};


use toml;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VersionControl { Git, Hg, NoVcs }

pub struct NewOptions<'a> {
    pub version_control: Option<VersionControl>,
    pub bin: bool,
    pub lib: bool,
    pub path: &'a str,
    pub name: Option<&'a str>,
    pub template: Option<&'a str>,
}

struct SourceFileInformation {
    relative_path: String,
    target_name: String,
    bin: bool,
}

struct MkOptions<'a> {
    version_control: Option<VersionControl>,
    template: Option<&'a str>,
    path: &'a Path,
    name: &'a str,
    bin: bool,
}

impl Decodable for VersionControl {
    fn decode<D: Decoder>(d: &mut D) -> Result<VersionControl, D::Error> {
        Ok(match &try!(d.read_str())[..] {
            "git" => VersionControl::Git,
            "hg" => VersionControl::Hg,
            "none" => VersionControl::NoVcs,
            n => {
                let err = format!("could not decode '{}' as version control", n);
                return Err(d.error(&err));
            }
        })
    }
}

impl<'a> NewOptions<'a> {
    pub fn new(version_control: Option<VersionControl>,
           bin: bool,
           lib: bool,
           path: &'a str,
           name: Option<&'a str>,
           template: Option<&'a str>) -> NewOptions<'a> {

        // default to lib
        let is_lib = if !bin {
            true
       }
        else {
            lib
        };

        NewOptions {
            version_control: version_control,
            bin: bin,
            lib: is_lib,
            path: path,
            name: name,
            template: template,
        }
    }
}

struct CargoNewConfig {
    name: Option<String>,
    email: Option<String>,
    version_control: Option<VersionControl>,
}

// When we manage a template, we either have an existing directory (Normal) or
// we write the template files to a temporary directory (Temp) and use this
// location.
enum TemplateDirectory {
    Temp(TempDir),
    Normal(PathBuf)
}

impl TemplateDirectory {
    fn path(&self) -> &Path {
        match *self {
            TemplateDirectory::Temp(ref tempdir) => tempdir.path(),
            TemplateDirectory::Normal(ref path) => path.as_path()
        }
    }
}


fn get_name<'a>(path: &'a Path, opts: &'a NewOptions, config: &Config) -> CargoResult<&'a str> {
    if let Some(name) = opts.name {
        return Ok(name);
    }

    if path.file_name().is_none() {
        bail!("cannot auto-detect project name from path {:?} ; use --name to override",
                              path.as_os_str());
    }

    let dir_name = try!(path.file_name().and_then(|s| s.to_str()).chain_error(|| {
        human(&format!("cannot create a project with a non-unicode name: {:?}",
                       path.file_name().unwrap()))
    }));

    if opts.bin {
        Ok(dir_name)
    } else {
        let new_name = strip_rust_affixes(dir_name);
        if new_name != dir_name {
            let message = format!(
                "note: package will be named `{}`; use --name to override",
                new_name);
            try!(config.shell().say(&message, BLACK));
        }
        Ok(new_name)
    }
}

fn check_name(name: &str) -> CargoResult<()> {

    // Ban keywords + test list found at
    // https://doc.rust-lang.org/grammar.html#keywords
    let blacklist = ["abstract", "alignof", "as", "become", "box",
        "break", "const", "continue", "crate", "do",
        "else", "enum", "extern", "false", "final",
        "fn", "for", "if", "impl", "in",
        "let", "loop", "macro", "match", "mod",
        "move", "mut", "offsetof", "override", "priv",
        "proc", "pub", "pure", "ref", "return",
        "self", "sizeof", "static", "struct",
        "super", "test", "trait", "true", "type", "typeof",
        "unsafe", "unsized", "use", "virtual", "where",
        "while", "yield"];
    if blacklist.contains(&name) {
        bail!("The name `{}` cannot be used as a crate name\n\
               use --name to override crate name",
               name)
    }

    for c in name.chars() {
        if c.is_alphanumeric() { continue }
        if c == '_' || c == '-' { continue }
        bail!("Invalid character `{}` in crate name: `{}`\n\
               use --name to override crate name",
              c, name)
    }
    Ok(())
}

fn detect_source_paths_and_types(project_path : &Path,
                                 project_name: &str,
                                 detected_files: &mut Vec<SourceFileInformation>,
                                 ) -> CargoResult<()> {
    let path = project_path;
    let name = project_name;

    enum H {
        Bin,
        Lib,
        Detect,
    }

    struct Test {
        proposed_path: String,
        handling: H,
    }

    let tests = vec![
        Test { proposed_path: format!("src/main.rs"),     handling: H::Bin },
        Test { proposed_path: format!("main.rs"),         handling: H::Bin },
        Test { proposed_path: format!("src/{}.rs", name), handling: H::Detect },
        Test { proposed_path: format!("{}.rs", name),     handling: H::Detect },
        Test { proposed_path: format!("src/lib.rs"),      handling: H::Lib },
        Test { proposed_path: format!("lib.rs"),          handling: H::Lib },
    ];

    for i in tests {
        let pp = i.proposed_path;

        // path/pp does not exist or is not a file
        if !fs::metadata(&path.join(&pp)).map(|x| x.is_file()).unwrap_or(false) {
            continue;
        }

        let sfi = match i.handling {
            H::Bin => {
                SourceFileInformation {
                    relative_path: pp,
                    target_name: project_name.to_string(),
                    bin: true
                }
            }
            H::Lib => {
                SourceFileInformation {
                    relative_path: pp,
                    target_name: project_name.to_string(),
                    bin: false
                }
            }
            H::Detect => {
                let content = try!(paths::read(&path.join(pp.clone())));
                let isbin = content.contains("fn main");
                SourceFileInformation {
                    relative_path: pp,
                    target_name: project_name.to_string(),
                    bin: isbin
                }
            }
        };
        detected_files.push(sfi);
    }

    // Check for duplicate lib attempt

    let mut previous_lib_relpath : Option<&str> = None;
    let mut duplicates_checker : BTreeMap<&str, &SourceFileInformation> = BTreeMap::new();

    for i in detected_files {
        if i.bin {
            if let Some(x) = BTreeMap::get::<str>(&duplicates_checker, i.target_name.as_ref()) {
                bail!("\
multiple possible binary sources found:
  {}
  {}
cannot automatically generate Cargo.toml as the main target would be ambiguous",
                      &x.relative_path, &i.relative_path);
            }
            duplicates_checker.insert(i.target_name.as_ref(), i);
        } else {
            if let Some(plp) = previous_lib_relpath {
                return Err(human(format!("cannot have a project with \
                                         multiple libraries, \
                                         found both `{}` and `{}`",
                                         plp, i.relative_path)));
            }
            previous_lib_relpath = Some(&i.relative_path);
        }
    }

    Ok(())
}

fn plan_new_source_file(bin: bool, project_name: String) -> SourceFileInformation {
    if bin {
        SourceFileInformation {
             relative_path: "src/main.rs".to_string(),
             target_name: project_name,
             bin: true,
        }
    } else {
        SourceFileInformation {
             relative_path: "src/lib.rs".to_string(),
             target_name: project_name,
             bin: false,
        }
    }
}

pub fn new(opts: NewOptions, config: &Config) -> CargoResult<()> {
    let path = config.cwd().join(opts.path);
    if fs::metadata(&path).is_ok() {
        bail!("destination `{}` already exists",
              path.display())
    }

    if opts.lib && opts.bin {
        bail!("can't specify both lib and binary outputs");
    }

    let name = try!(get_name(&path, &opts, config));
    try!(check_name(name));

    let mkopts = MkOptions {
        version_control: opts.version_control,
        template: opts.template,
        path: &path,
        name: name,
        bin: opts.bin,
    };

    mk(config, &mkopts).chain_error(|| {
        human(format!("Failed to create project `{}` at `{}`",
                      name, path.display()))
    })
}

pub fn init(opts: NewOptions, config: &Config) -> CargoResult<()> {
    let path = config.cwd().join(opts.path);

    let cargotoml_path = path.join("Cargo.toml");
    if fs::metadata(&cargotoml_path).is_ok() {
        bail!("`cargo init` cannot be run on existing Cargo projects")
    }

    if opts.lib && opts.bin {
        bail!("can't specify both lib and binary outputs");
    }

    let name = try!(get_name(&path, &opts, config));
    try!(check_name(name));

    let mut src_paths_types = vec![];

    try!(detect_source_paths_and_types(&path, name, &mut src_paths_types));

    if src_paths_types.len() == 0 {
        src_paths_types.push(plan_new_source_file(opts.bin, name.to_string()));
    } else {
        // --bin option may be ignored if lib.rs or src/lib.rs present
        // Maybe when doing `cargo init --bin` inside a library project stub,
        // user may mean "initialize for library, but also add binary target"
    }

    let mut version_control = opts.version_control;

    if version_control == None {
        let mut num_detected_vsces = 0;

        if fs::metadata(&path.join(".git")).is_ok() {
            version_control = Some(VersionControl::Git);
            num_detected_vsces += 1;
        }

        if fs::metadata(&path.join(".hg")).is_ok() {
            version_control = Some(VersionControl::Hg);
            num_detected_vsces += 1;
        }

        // if none exists, maybe create git, like in `cargo new`

        if num_detected_vsces > 1 {
            bail!("both .git and .hg directories found \
                              and the ignore file can't be \
                              filled in as a result, \
                              specify --vcs to override detection");
        }
    }

    let mkopts = MkOptions {
        version_control: version_control,
        template: opts.template,
        path: &path,
        name: name,
        bin: src_paths_types.iter().any(|x|x.bin),
    };

    mk(config, &mkopts).chain_error(|| {
        human(format!("Failed to create project `{}` at `{}`",
                      name, path.display()))
    })
}

fn strip_rust_affixes(name: &str) -> &str {
    for &prefix in &["rust-", "rust_", "rs-", "rs_"] {
        if name.starts_with(prefix) {
            return &name[prefix.len()..];
        }
    }
    for &suffix in &["-rust", "_rust", "-rs", "_rs"] {
        if name.ends_with(suffix) {
            return &name[..name.len()-suffix.len()];
        }
    }
    name
}

fn existing_vcs_repo(path: &Path, cwd: &Path) -> bool {
    GitRepo::discover(path, cwd).is_ok() || HgRepo::discover(path, cwd).is_ok()
}

fn file(p: &Path, contents: &[u8]) -> io::Result<()> {
        try!(File::create(p)).write_all(contents)
}

fn mk(config: &Config, opts: &MkOptions) -> CargoResult<()> {
    let path = opts.path;
    let name = opts.name;
    let cfg = try!(global_config(config));
    let mut ignore = "target\n".to_string();
    let in_existing_vcs_repo = existing_vcs_repo(path.parent().unwrap(), config.cwd());
    if !opts.bin {
        ignore.push_str("Cargo.lock\n");
    }

    let vcs = match (opts.version_control, cfg.version_control, in_existing_vcs_repo) {
        (None, None, false) => VersionControl::Git,
        (None, Some(option), false) => option,
        (Some(option), _, _) => option,
        (_, _, true) => VersionControl::NoVcs,
    };

    match vcs {
        VersionControl::Git => {
            if !fs::metadata(&path.join(".git")).is_ok() {
                try!(GitRepo::init(path, config.cwd()));
            }
            try!(paths::append(&path.join(".gitignore"), ignore.as_bytes()));
        },
        VersionControl::Hg => {
            if !fs::metadata(&path.join(".hg")).is_ok() {
                try!(HgRepo::init(path, config.cwd()));
            }
            try!(paths::append(&path.join(".hgignore"), ignore.as_bytes()));
        },
        VersionControl::NoVcs => {
            try!(fs::create_dir_all(path));
        },
    };

    let (author_name, email) = try!(discover_author());
    // Hoo boy, sure glad we've got exhaustivenes checking behind us.
    let author = match (cfg.name, cfg.email, author_name, email) {
        (Some(name), Some(email), _, _) |
        (Some(name), None, _, Some(email)) |
        (None, Some(email), name, _) |
        (None, None, name, Some(email)) => format!("{} <{}>", name, email),
        (Some(name), None, _, None) |
        (None, None, name, None) => name,
    };

    let templates_dir = config.template_path();
    if fs::metadata(&templates_dir).is_err() {
        try!(fs::create_dir(&templates_dir));
    }
    let templates_dir = templates_dir.as_path();

    let template_dir = match opts.template {
        // given template is a remote git repository & needs to be cloned
        // This will be cloned to .cargo/templates/<repo_name> where <repo_name>
        // is the last component of the given URL. For example:
        //
        //      http://github.com/rust-lang/some-template
        //      <repo_name> = some-template
        Some(template_url) if template_url.starts_with("http") ||
                              template_url.starts_with("git@") => {
            let template_dir = try!(TempDir::new(name));
            println!("Checking out repo to dir: {}", template_dir.path().display());

            try!(config.shell().status("Cloning", template_url));
            match Repository::clone(template_url, &*template_dir.path()) {
                Ok(_) => {},
                Err(e) => {
                    return Err(human(format!(
                        "Failed to clone repository: {} - {}", path.display(), e)))
                }
            };
            TemplateDirectory::Temp(template_dir)
        }

        // given template is assumed to already be present on the users system
        // in .cargo/templates/<name>.
        Some(template_name) => {
            TemplateDirectory::Normal(templates_dir.join(template_name))
        },
        // no template given, use either "lib" or "bin" templates depending on the
        // presence of the --bin flag.
        None => {
            let temp_templates_dir = try!(TempDir::new(name));
            let temp_templates_path = temp_templates_dir.path().to_path_buf();
            if opts.bin {
                try!(create_bin_template(&temp_templates_path));
            } else {
                try!(create_lib_template(&temp_templates_path));
            }
            TemplateDirectory::Temp(temp_templates_dir)
        }
    };

    // make sure that the template exists
    if fs::metadata(&template_dir.path()).is_err() {
        return Err(human(format!("Template `{}` not found", template_dir.path().display())))
    }

    // construct the mapping used to populate the template
    // if in the future we want to make more varaibles available in
    // the templates, this would be the place to do it.
    let mut handlebars = Handlebars::new();
    // We don't want html escaping...
    handlebars.register_escape_fn(no_escape);

    let mut data = BTreeMap::new();
    data.insert("name".to_owned(), name.to_owned());
    let author_value = toml::Value::String(author).as_str().unwrap_or("").to_owned();
    data.insert("authors".to_owned(), author_value);

    // For every file found inside the given template directory, compile it as a handlebars
    // template and render it with the above data to a new file inside the target directory
    try!(walk_template_dir(&template_dir.path(), &mut |entry| {
        let entry_path = entry.path();
        let entry_str = entry_path.to_str().unwrap();
        let template_dir_str = template_dir.path().to_str().unwrap();

        // the path we have here is the absolute path to the file in the template directory
        // we need to trim this down to be just the path from the root of the template.
        // For example:
        //    /home/user/.cargo/templates/foo/Cargo.toml => Cargo.toml
        //    /home/user/.cargo/templates/foo/src/main.rs => src/main.rs
        let mut dest_file_name = entry_str.replace(template_dir_str, "");
        if dest_file_name.starts_with("/") {
            dest_file_name.remove(0);
        }

        let mut template_str = String::new();
        File::open(&entry_path).unwrap().read_to_string(&mut template_str).unwrap();
        let mut dest_path = PathBuf::from(path).join(dest_file_name);

        // dest_file_name could now refer to a file inside a directory which doesn't yet exist
        // to figure out if this is the case, get all the components in the dest_file_name and check
        // how many there are. Files in the root of the new project direcotory will have two
        // components, anything more than that means the file is in a sub-directory, so we need
        // to create it.
        {
            let components  = dest_path.components().collect::<Vec<_>>();
            if components.len() > 2 {
                if let Some(p) = dest_path.parent() {
                    if fs::metadata(&p).is_err() {
                        let _ = fs::create_dir_all(&p);
                    }
                }
            }
        }

        // if we are running init, we do not clobber existing files.
        if fs::metadata(&dest_path).is_ok() {
            return;
        }

        // if the template file has the ".hbs" extension, remove that to get the correct
        // name for the generated file
        if let Some(ext) = dest_path.clone().extension() {
            if ext.to_str().unwrap() == "hbs" {
                dest_path.set_extension("");
            }
        }

        // create the new file & render the template to it
        let mut dest_file = OpenOptions::new().write(true).create(true).open(&dest_path).unwrap();
        handlebars.template_renderw(&template_str, &Context::wraps(&data), &mut dest_file).ok();
    }));

    if let Err(e) = Workspace::new(&path.join("Cargo.toml"), config) {
        let msg = format!("compiling this new crate may not work due to invalid \
                           workspace configuration\n\n{}", e);
        try!(config.shell().warn(msg));
    }

    Ok(())
}

fn get_environment_variable(variables: &[&str] ) -> Option<String>{
    variables.iter()
             .filter_map(|var| env::var(var).ok())
             .next()
}

fn discover_author() -> CargoResult<(String, Option<String>)> {
    let git_config = GitConfig::open_default().ok();
    let git_config = git_config.as_ref();
    let name_variables = ["CARGO_NAME", "GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME",
                         "USER", "USERNAME", "NAME"];
    let name = get_environment_variable(&name_variables[0..3])
                        .or_else(|| git_config.and_then(|g| g.get_string("user.name").ok()))
                        .or_else(|| get_environment_variable(&name_variables[3..]));

    let name = match name {
        Some(name) => name,
        None => {
            let username_var = if cfg!(windows) {"USERNAME"} else {"USER"};
            bail!("could not determine the current user, please set ${}",
                  username_var)
        }
    };
    let email_variables = ["CARGO_EMAIL", "GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL",
                          "EMAIL"];
    let email = get_environment_variable(&email_variables[0..3])
                          .or_else(|| git_config.and_then(|g| g.get_string("user.email").ok()))
                          .or_else(|| get_environment_variable(&email_variables[3..]));

    let name = name.trim().to_string();
    let email = email.map(|s| s.trim().to_string());

    Ok((name, email))
}

fn global_config(config: &Config) -> CargoResult<CargoNewConfig> {
    let name = try!(config.get_string("cargo-new.name")).map(|s| s.val);
    let email = try!(config.get_string("cargo-new.email")).map(|s| s.val);
    let vcs = try!(config.get_string("cargo-new.vcs"));

    let vcs = match vcs.as_ref().map(|p| (&p.val[..], &p.definition)) {
        Some(("git", _)) => Some(VersionControl::Git),
        Some(("hg", _)) => Some(VersionControl::Hg),
        Some(("none", _)) => Some(VersionControl::NoVcs),
        Some((s, p)) => {
            return Err(internal(format!("invalid configuration for key \
                                         `cargo-new.vcs`, unknown vcs `{}` \
                                         (found in {})", s, p)))
        }
        None => None
    };
    Ok(CargoNewConfig {
        name: name,
        email: email,
        version_control: vcs,
    })
}

/// Recursively list directory contents under `dir`, only visiting files.
///
/// This will also filter out files & files types which we don't want to
/// try generate templates for. Image files, for instance.
///
/// It also filters out certain files & file types, as we don't want t
///
/// We use this instead of std::fs::walk_dir as it is marked as unstable for now
///
/// This is a modified version of the example at:
///    http://doc.rust-lang.org/std/fs/fn.read_dir.html
fn walk_template_dir(dir: &Path, cb: &mut FnMut(DirEntry)) -> CargoResult<()> {
    let attr = try!(fs::metadata(&dir));

    let ignore_files = vec!(".gitignore");
    let ignore_types = vec!("png", "jpg", "gif");

    if attr.is_dir() {
        for entry in try!(fs::read_dir(dir)) {
            let entry = try!(entry);
            let attr = try!(fs::metadata(&entry.path()));
            if attr.is_dir() {
                if !&entry.path().to_str().unwrap().contains(".git") {
                    try!(walk_template_dir(&entry.path(), cb));
                }
            } else {
                if let &Some(extension) = &entry.path().extension() {
                    if ignore_types.contains(&extension.to_str().unwrap()) {
                        continue
                    }
                }
                if let &Some(file_name) = &entry.path().file_name() {
                    if ignore_files.contains(&file_name.to_str().unwrap()) {
                        continue
                    }
                }
                cb(entry);
            }
        }
    }
    Ok(())
}

/// Create a generic template
///
/// This consists of a Cargo.toml, and a src directory.
fn create_generic_template(path: &PathBuf) -> CargoResult<()> {
    match fs::metadata(&path) {
        Ok(_) => {}
        Err(_) => { try!(fs::create_dir(&path)); }
    }
    match fs::metadata(&path.join("src")) {
        Ok(_) => {}
        Err(_) => { try!(fs::create_dir(&path.join("src"))); }
    }
    try!(file(&path.join("Cargo.toml"), br#"[package]
name = "{{name}}"
version = "0.1.0"
authors = ["{{authors}}"]
"#));
    Ok(())
}

/// Create a new "lib" project
fn create_lib_template(path: &PathBuf) -> CargoResult<()> {
    try!(create_generic_template(&path));
    try!(file(&path.join("src/lib.rs"), br#"#[test]
fn it_works() {
}
"#));
    Ok(())
}

/// Create a new "bin" project
fn create_bin_template(path: &PathBuf) -> CargoResult<()> {
    try!(create_generic_template(&path));
    try!(file(&path.join("src/main.rs"), b"\
fn main() {
    println!(\"Hello, world!\");
}
"));

    Ok(())
}



#[cfg(test)]
mod tests {
    use super::strip_rust_affixes;

    #[test]
    fn affixes_stripped() {
        assert_eq!(strip_rust_affixes("rust-foo"), "foo");
        assert_eq!(strip_rust_affixes("foo-rs"), "foo");
        assert_eq!(strip_rust_affixes("rs_foo"), "foo");
        // Only one affix is stripped
        assert_eq!(strip_rust_affixes("rs-foo-rs"), "foo-rs");
        assert_eq!(strip_rust_affixes("foo-rs-rs"), "foo-rs");
        // It shouldn't touch the middle
        assert_eq!(strip_rust_affixes("some-rust-crate"), "some-rust-crate");
    }
}
