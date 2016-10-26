use std::path::{Path, PathBuf};
use std::fs::File;
use std::io::{Read};

use util::{CargoResult, human, ChainError};

use handlebars::{Context, Helper, Handlebars, RenderContext, RenderError, html_escape};
use tempdir::TempDir;
use toml;

/// toml_escape_helper quotes strings in templates when they are wrapped in
/// {{#toml-escape <template-variable}}
/// So if 'name' is "foo \"bar\"" then:
/// {{name}} renders as  'foo "bar"'
/// {{#toml-escape name}} renders as '"foo \"bar\""'
pub fn toml_escape_helper(_: &Context,
                          h: &Helper,
                          _: &Handlebars,
                          rc: &mut RenderContext) -> Result<(), RenderError> {
    let param = h.param(0).unwrap();
    let txt = param.value().as_string().unwrap_or("").to_owned();
    let rendered = format!("{}", toml::Value::String(txt));
    try!(rc.writer.write(rendered.into_bytes().as_ref()));
    Ok(())
}

/// html_escape_helper escapes strings in templates using html escaping rules.
pub fn html_escape_helper(_: &Context,
                          h: &Helper,
                          _: &Handlebars,
                          rc: &mut RenderContext) -> Result<(), RenderError> {
    let param = h.param(0).unwrap();
    let rendered = html_escape(param.value().as_string().unwrap_or(""));
    try!(rc.writer.write(rendered.into_bytes().as_ref()));
    Ok(())
}

/// Trait to hold information required for rendering templated files.
pub trait TemplateFile {
    /// Path of the template output for the file being written.
    fn path(&self) -> &Path;

    /// Return the template string.
    fn template(&self) -> CargoResult<String>;
}

/// TemplateFile based on an input file.
pub struct InputFileTemplateFile {
    input_path: PathBuf,
    output_path: PathBuf,
}

impl TemplateFile for InputFileTemplateFile {
    fn path(&self) -> &Path {
        &self.output_path
    }

    fn template(&self) -> CargoResult<String> {
        let mut template_str = String::new();
        let mut entry_file = try!(File::open(&self.input_path).chain_error(|| {
            human(format!("Failed to open file for templating: {}", self.input_path.display()))
        }));
        try!(entry_file.read_to_string(&mut template_str).chain_error(|| {
            human(format!("Failed to read file for templating: {}", self.input_path.display()))
        }));
        Ok(template_str)
    }
}

impl InputFileTemplateFile {
    pub fn new(input_path: PathBuf, output_path: PathBuf) -> InputFileTemplateFile {
        InputFileTemplateFile {
            input_path: input_path,
            output_path: output_path
        }
    }
}

/// An in memory template file for --bin or --lib.
pub struct InMemoryTemplateFile {
    template_str: String,
    output_path: PathBuf,
}

impl TemplateFile for InMemoryTemplateFile {
    fn path(&self) -> &Path {
        &self.output_path
    }

    fn template(&self) -> CargoResult<String> {
        Ok(self.template_str.clone())
    }
}

impl InMemoryTemplateFile {
    pub fn new(output_path: PathBuf, template_str: String) -> InMemoryTemplateFile {
        InMemoryTemplateFile {
            template_str: template_str,
            output_path: output_path
        }
    }
}

pub enum TemplateDirectory{
    Temp(TempDir),
    Normal(PathBuf),
}

impl TemplateDirectory {
    pub fn path(&self) -> &Path {
        match *self {
            TemplateDirectory::Temp(ref tempdir) => tempdir.path(),
            TemplateDirectory::Normal(ref path) => path.as_path()
        }
    }
}

pub struct TemplateSet {
    pub template_dir: Option<TemplateDirectory>,
    pub template_files: Vec<Box<TemplateFile>>
}

// The type of template we will use.
#[derive(Debug, Eq, PartialEq)]
pub enum TemplateType<'a>  {
    GitRepo(&'a str),
    LocalDir(&'a str),
    Builtin
}

#[cfg(test)]
mod test {
    use std::collections::BTreeMap;
    use handlebars::Handlebars;
    use super::*;

    #[test]
    fn test_toml_escape_helper() {
        let mut handlebars = Handlebars::new();
        handlebars.register_helper("toml-escape", Box::new(toml_escape_helper));
        let mut data = BTreeMap::new();
        data.insert("name".to_owned(), "\"Iron\" Mike Tyson".to_owned());
        let result = handlebars.template_render("Hello, {{toml-escape name}}", &data).unwrap();
        assert_eq!(result, "Hello, \"\\\"Iron\\\" Mike Tyson\"");
    }
}
