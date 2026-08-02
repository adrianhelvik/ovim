use crate::language_catalog::{
    DynamicLanguageSpec, DynamicLspSpec, DynamicParserSpec, LanguageCatalog, RegistrationOwner,
};
use anyhow::Result;
use mlua::{Lua, Table, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub type LuaSourceContext = Arc<Mutex<Option<PathBuf>>>;

pub fn setup_ovim_api(
    lua: &Lua,
    catalog: Arc<LanguageCatalog>,
    source: LuaSourceContext,
) -> Result<()> {
    let ovim = lua.create_table()?;
    let languages = lua.create_table()?;
    let register = lua.create_function(move |_lua, table: Table| {
        reject_unknown(
            &table,
            "language",
            &["id", "name", "files", "syntax", "lsp"],
        )?;
        let id = required_string(&table, "id", "language.id")?;
        let name = required_string(&table, "name", "language.name")?;

        let files: Table = table
            .get("files")
            .map_err(|_| mlua::Error::external("language.files must be a table"))?;
        reject_unknown(&files, "language.files", &["extensions"])?;
        let extensions = required_strings(&files, "extensions", "language.files.extensions")?;

        let parser = match table.get::<_, Value>("syntax")? {
            Value::Nil => None,
            Value::Table(syntax) => {
                reject_unknown(&syntax, "language.syntax", &["parser", "highlights"])?;
                let parser: Table = syntax
                    .get("parser")
                    .map_err(|_| mlua::Error::external("language.syntax.parser must be a table"))?;
                reject_unknown(&parser, "language.syntax.parser", &["path", "symbol"])?;
                Some(DynamicParserSpec {
                    path: PathBuf::from(required_string(
                        &parser,
                        "path",
                        "language.syntax.parser.path",
                    )?),
                    symbol: required_string(&parser, "symbol", "language.syntax.parser.symbol")?,
                    highlights: PathBuf::from(required_string(
                        &syntax,
                        "highlights",
                        "language.syntax.highlights",
                    )?),
                })
            }
            _ => return Err(mlua::Error::external("language.syntax must be a table")),
        };

        let lsp = match table.get::<_, Value>("lsp")? {
            Value::Nil => None,
            Value::Table(lsp) => {
                reject_unknown(
                    &lsp,
                    "language.lsp",
                    &["cmd", "language_id", "root_markers"],
                )?;
                let command = required_strings(&lsp, "cmd", "language.lsp.cmd")?;
                let language_id = lsp
                    .get::<_, Option<String>>("language_id")?
                    .unwrap_or_else(|| id.clone());
                let root_markers = match lsp.get::<_, Value>("root_markers")? {
                    Value::Nil => vec![".git".to_string()],
                    Value::Table(_) => {
                        required_strings(&lsp, "root_markers", "language.lsp.root_markers")?
                    }
                    _ => {
                        return Err(mlua::Error::external(
                            "language.lsp.root_markers must be an array",
                        ))
                    }
                };
                Some(DynamicLspSpec {
                    command,
                    language_id,
                    root_markers,
                })
            }
            _ => return Err(mlua::Error::external("language.lsp must be a table")),
        };

        let source_file = source
            .lock()
            .map_err(|_| mlua::Error::external("Lua source context is poisoned"))?
            .clone()
            .ok_or_else(|| {
                mlua::Error::external(
                    "ovim.languages.register must be called while loading a config or plugin file",
                )
            })?;
        let source_dir = source_file
            .parent()
            .ok_or_else(|| mlua::Error::external("declaration source has no parent directory"))?;
        let owner = plugin_owner(&source_file).unwrap_or_else(|| RegistrationOwner::UserConfig {
            source: source_file.clone(),
        });

        catalog
            .register_dynamic(
                DynamicLanguageSpec {
                    id,
                    name,
                    extensions,
                    parser,
                    lsp,
                },
                owner,
                source_dir,
            )
            .map_err(mlua::Error::external)
    })?;
    languages.set("register", register)?;
    ovim.set("languages", languages)?;
    lua.globals().set("ovim", ovim)?;
    Ok(())
}

fn plugin_owner(source: &std::path::Path) -> Option<RegistrationOwner> {
    let root = source.parent()?.to_path_buf();
    if root.parent()?.file_name()?.to_str()? != "plugins" {
        return None;
    }
    Some(RegistrationOwner::Plugin {
        name: root.file_name()?.to_string_lossy().to_string(),
        root,
    })
}

fn reject_unknown(table: &Table, path: &str, allowed: &[&str]) -> mlua::Result<()> {
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, _) = pair?;
        let Value::String(key) = key else {
            return Err(mlua::Error::external(format!(
                "{path} contains a non-string field"
            )));
        };
        let key = key.to_str()?;
        if !allowed.contains(&key) {
            return Err(mlua::Error::external(format!("unknown field {path}.{key}")));
        }
    }
    Ok(())
}

fn required_string(table: &Table, key: &str, path: &str) -> mlua::Result<String> {
    match table.get::<_, Value>(key)? {
        Value::String(value) => Ok(value.to_str()?.to_string()),
        _ => Err(mlua::Error::external(format!("{path} must be a string"))),
    }
}

fn required_strings(table: &Table, key: &str, path: &str) -> mlua::Result<Vec<String>> {
    let value: Table = table
        .get(key)
        .map_err(|_| mlua::Error::external(format!("{path} must be an array")))?;
    let values = value
        .sequence_values::<String>()
        .collect::<mlua::Result<Vec<_>>>()?;
    if values.is_empty() {
        return Err(mlua::Error::external(format!("{path} must not be empty")));
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_uses_the_declaring_file_and_rejects_unknown_fields() {
        let temp = tempfile::tempdir().unwrap();
        let init = temp.path().join("init.lua");
        std::fs::write(
            &init,
            r#"
                ovim.languages.register({
                  id = "lua-test-language",
                  name = "Lua test",
                  files = { extensions = { "ltest" } },
                  lsp = { cmd = { "test-server", "--stdio" } },
                })
            "#,
        )
        .unwrap();

        let catalog = LanguageCatalog::built_in();
        let mut context = crate::lua::LuaContext::new().unwrap();
        setup_ovim_api(context.lua(), catalog.clone(), context.source_context()).unwrap();
        context.execute_file(&init).unwrap();

        let language = catalog.detect("sample.ltest").unwrap();
        assert_eq!(language.source, temp.path().canonicalize().unwrap());
        assert_eq!(language.lsp().unwrap().args, ["--stdio"]);

        let bad = temp.path().join("bad.lua");
        std::fs::write(
            &bad,
            r#"
                ovim.languages.register({
                  id = "bad-language",
                  name = "Bad",
                  files = { extensions = { "bad" }, extension = "bad" },
                  lsp = { cmd = { "bad-server" } },
                })
            "#,
        )
        .unwrap();
        let error = context.execute_file(&bad).unwrap_err().to_string();
        assert!(error.contains("unknown field language.files.extension"));
        assert!(catalog.detect("sample.bad").is_none());
    }
}
