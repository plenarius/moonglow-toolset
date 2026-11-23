//! JSON for plugins (API 0.10): `mg.json` (`encode`, `decode`, `raw`,
//! `null`), `ctx.module:json` and `ctx.edit:write_json`. Lossless: a
//! whole number a Luau number cannot hold exactly (a GFF's 64-bit
//! fields) decodes as a raw value, which encodes back as its digits.

use mlua::{Lua, Table, UserData, UserDataMethods, Value as LuaValue};
use serde_json::Value as Json;

use crate::runtime::fail;

/// The names of `mg.json`, as the reference writes them.
pub(crate) const NAMES: [&str; 4] =
    ["mg.json.encode", "mg.json.decode", "mg.json.raw", "mg.json.null"];

/// The deepest JSON nests (as decoded, and as encoded).
const MAX_DEPTH: usize = 128;

/// Integers past this are not exact as Luau numbers.
const EXACT: f64 = 9_007_199_254_740_992.0;

/// JSON text kept as it is: `mg.json.raw(text)`, and a number too large
/// for a Luau number that `decode` read.
#[derive(Debug, Clone)]
pub(crate) struct Raw(pub(crate) String);

impl UserData for Raw {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_method(mlua::MetaMethod::ToString, |_, r, ()| Ok(r.0.clone()));
        m.add_meta_method(mlua::MetaMethod::Eq, |_, r, o: mlua::AnyUserData| {
            Ok(o.borrow::<Raw>().is_ok_and(|o| o.0 == r.0))
        });
    }
}

/// A value of the plugin's as JSON text: tables with keys 1..n (or that
/// `decode` made of an array) as arrays, other tables as objects (their
/// keys sorted), `mg.json.null` as null, raw values as their text.
pub(crate) fn encode(v: &LuaValue, pretty: bool) -> mlua::Result<String> {
    let mut out = String::new();
    write(v, &mut out, pretty, 0)?;
    Ok(out)
}

/// The keys of an object `decode` made, in the text's order.
fn key_order(t: &Table) -> mlua::Result<Option<Table>> {
    match t.metatable() {
        Some(meta) => meta.raw_get::<Option<Table>>("__mg_json_keys"),
        None => Ok(None),
    }
}

fn is_array(t: &Table) -> mlua::Result<Option<usize>> {
    if let Some(meta) = t.metatable()
        && meta.raw_get::<bool>("__mg_json_array")?
    {
        return Ok(Some(t.raw_len()));
    }
    let n = t.raw_len();
    let mut count = 0usize;
    for pair in t.pairs::<LuaValue, LuaValue>() {
        let (k, _) = pair?;
        count += 1;
        let at = match k {
            LuaValue::Integer(i) => i as f64,
            LuaValue::Number(f) => f,
            _ => return Ok(None),
        };
        if at.fract() != 0.0 || at < 1.0 || at > n as f64 {
            return Ok(None);
        }
    }
    Ok((count == n && n > 0).then_some(n))
}

fn indent(out: &mut String, pretty: bool, depth: usize) {
    if pretty {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', depth * 2));
    }
}

fn number(f: f64, out: &mut String) -> mlua::Result<()> {
    if !f.is_finite() {
        return fail("JSON has no NaN or infinity");
    }
    if f.fract() == 0.0 && f.abs() <= EXACT {
        out.push_str(&(f as i64).to_string());
    } else {
        out.push_str(&serde_json::Number::from_f64(f).map(|n| n.to_string()).unwrap_or_default());
    }
    Ok(())
}

fn write(v: &LuaValue, out: &mut String, pretty: bool, depth: usize) -> mlua::Result<()> {
    if depth > MAX_DEPTH {
        return fail(format!("JSON nests at most {MAX_DEPTH} deep (or a table holds itself)"));
    }
    match v {
        LuaValue::Nil => out.push_str("null"),
        LuaValue::LightUserData(p) if p.0.is_null() => out.push_str("null"),
        LuaValue::Boolean(b) => out.push_str(if *b { "true" } else { "false" }),
        LuaValue::Integer(i) => out.push_str(&i.to_string()),
        LuaValue::Number(f) => number(*f, out)?,
        LuaValue::String(s) => {
            let Ok(text) = s.to_str() else { return fail("JSON text is UTF-8") };
            out.push_str(&serde_json::to_string(&*text).unwrap_or_default());
        }
        LuaValue::UserData(u) if u.is::<Raw>() => out.push_str(&u.borrow::<Raw>()?.0),
        LuaValue::Table(t) => match is_array(t)? {
            Some(n) => {
                out.push('[');
                for i in 1..=n {
                    if i > 1 {
                        out.push(',');
                    }
                    indent(out, pretty, depth + 1);
                    write(&t.raw_get::<LuaValue>(i)?, out, pretty, depth + 1)?;
                }
                if n > 0 {
                    indent(out, pretty, depth);
                }
                out.push(']');
            }
            None => {
                let mut entries = Vec::new();
                for pair in t.pairs::<LuaValue, LuaValue>() {
                    let (k, v) = pair?;
                    let key = match &k {
                        LuaValue::String(s) => match s.to_str() {
                            Ok(s) => s.to_string(),
                            Err(_) => return fail("a JSON object's key is UTF-8"),
                        },
                        LuaValue::Integer(i) => i.to_string(),
                        LuaValue::Number(f) if f.fract() == 0.0 => (*f as i64).to_string(),
                        other => {
                            return fail(format!(
                                "a JSON object's key is text, not {}",
                                other.type_name()
                            ));
                        }
                    };
                    entries.push((key, v));
                }
                // (Decoded keys in their order, a GFF's fields' order; new ones sorted after.)
                let order: Vec<String> = match key_order(t)? {
                    Some(keys) => keys.sequence_values::<String>().collect::<mlua::Result<_>>()?,
                    None => Vec::new(),
                };
                let at = |k: &str| order.iter().position(|o| o == k).unwrap_or(usize::MAX);
                entries.sort_by(|a, b| at(&a.0).cmp(&at(&b.0)).then_with(|| a.0.cmp(&b.0)));
                out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    indent(out, pretty, depth + 1);
                    out.push_str(&serde_json::to_string(k).unwrap_or_default());
                    out.push_str(if pretty { ": " } else { ":" });
                    write(v, out, pretty, depth + 1)?;
                }
                if !entries.is_empty() {
                    indent(out, pretty, depth);
                }
                out.push('}');
            }
        },
        other => return fail(format!("a {} is not JSON", other.type_name())),
    }
    Ok(())
}

/// JSON text as the plugin's values: arrays as tables marked so (an
/// empty one encodes as `[]` again), null as `mg.json.null`.
pub(crate) fn decode(lua: &Lua, text: &str) -> mlua::Result<LuaValue> {
    let json: Json = serde_json::from_str(text).or_else(|e| fail(format!("not JSON: {e}")))?;
    value(lua, &json, 0)
}

fn array_meta(lua: &Lua) -> mlua::Result<Table> {
    if let Ok(Some(t)) = lua.named_registry_value::<Option<Table>>("mg.json.array") {
        return Ok(t);
    }
    let t = lua.create_table()?;
    t.raw_set("__mg_json_array", true)?;
    lua.set_named_registry_value("mg.json.array", &t)?;
    Ok(t)
}

fn value(lua: &Lua, j: &Json, depth: usize) -> mlua::Result<LuaValue> {
    if depth > MAX_DEPTH {
        return fail(format!("JSON nests at most {MAX_DEPTH} deep"));
    }
    Ok(match j {
        Json::Null => LuaValue::LightUserData(mlua::LightUserData(std::ptr::null_mut())),
        Json::Bool(b) => LuaValue::Boolean(*b),
        Json::Number(n) => {
            let exact = n.as_i64().map(|i| i as f64).or_else(|| n.as_u64().map(|u| u as f64));
            match exact {
                Some(f) if f.abs() <= EXACT => LuaValue::Number(f),
                Some(_) => LuaValue::UserData(lua.create_userdata(Raw(n.to_string()))?),
                None => LuaValue::Number(n.as_f64().unwrap_or(f64::NAN)),
            }
        }
        Json::String(s) => LuaValue::String(lua.create_string(s)?),
        Json::Array(items) => {
            let t = lua.create_table_with_capacity(items.len(), 0)?;
            for (i, item) in items.iter().enumerate() {
                t.raw_set(i + 1, value(lua, item, depth + 1)?)?;
            }
            t.set_metatable(Some(array_meta(lua)?))?;
            LuaValue::Table(t)
        }
        Json::Object(map) => {
            let t = lua.create_table_with_capacity(0, map.len())?;
            let keys = lua.create_table_with_capacity(map.len(), 0)?;
            for (i, (k, v)) in map.iter().enumerate() {
                t.raw_set(k.as_str(), value(lua, v, depth + 1)?)?;
                keys.raw_set(i + 1, k.as_str())?;
            }
            let meta = lua.create_table()?;
            meta.raw_set("__mg_json_keys", keys)?;
            t.set_metatable(Some(meta))?;
            LuaValue::Table(t)
        }
    })
}

/// `mg.json`.
pub(crate) fn json_api(lua: &Lua) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set(
        "encode",
        lua.create_function(|_, (v, options): (LuaValue, Option<Table>)| {
            let pretty = match options {
                Some(o) => o.get::<Option<bool>>("pretty")?.unwrap_or(false),
                None => false,
            };
            encode(&v, pretty)
        })?,
    )?;
    t.set(
        "decode",
        lua.create_function(|lua, text: mlua::LuaString| {
            let Ok(text) = text.to_str() else { return fail("JSON text is UTF-8") };
            decode(lua, &text)
        })?,
    )?;
    t.set(
        "raw",
        lua.create_function(|lua, text: mlua::LuaString| {
            let Ok(text) = text.to_str() else { return fail("JSON text is UTF-8") };
            if let Err(e) = serde_json::from_str::<serde::de::IgnoredAny>(&text) {
                return fail(format!("mg.json.raw: not JSON: {e}"));
            }
            lua.create_userdata(Raw(text.trim().to_string()))
        })?,
    )?;
    t.set("null", LuaValue::LightUserData(mlua::LightUserData(std::ptr::null_mut())))?;
    Ok(t)
}

/// A GFF's text as `mg gff` writes it (nwn_gff's JSON).
pub(crate) fn gff_text(gff: &mg_gff::Gff, what: &str) -> mlua::Result<String> {
    let json = mg_gff::to_json(gff, crate::runtime::codepage())
        .or_else(|e| fail(format!("{what}: {e}")))?;
    Ok(mg_gff::to_json_text(&json, mg_gff::TextStyle::default()))
}

/// A GFF from nwn_gff's JSON: given as text, or as a value to encode.
pub(crate) fn gff_of(v: &LuaValue, what: &str) -> mlua::Result<mg_gff::Gff> {
    let text = match v {
        LuaValue::String(s) => match s.to_str() {
            Ok(t) => t.to_string(),
            Err(_) => return fail("JSON text is UTF-8"),
        },
        other => encode(other, false)?,
    };
    let json: Json =
        serde_json::from_str(&text).or_else(|e| fail(format!("{what}: not JSON: {e}")))?;
    mg_gff::from_json(&json, crate::runtime::codepage()).or_else(|e| fail(format!("{what}: {e}")))
}
