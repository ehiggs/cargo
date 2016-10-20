use handlebars::{Context, Helper, Handlebars, RenderContext, RenderError, html_escape};
use toml;

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

pub fn html_escape_helper(_: &Context, 
                          h: &Helper, 
                          _: &Handlebars, 
                          rc: &mut RenderContext) -> Result<(), RenderError> {
    let param = h.param(0).unwrap();
    let rendered = html_escape(param.value().as_string().unwrap_or(""));
    try!(rc.writer.write(rendered.into_bytes().as_ref()));
    Ok(())
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
        let result = handlebars.template_render("Hello, {{#toml-escape name}}{{/toml-escape}}", &data).unwrap();
        assert_eq!(result, "Hello, \"\\\"Iron\\\" Mike Tyson\"");
    }
}
