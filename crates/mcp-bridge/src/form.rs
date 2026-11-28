//! What an MCP server's form is allowed to look like, and how it is shown.
//!
//! Every text of a form is the server's own: untrusted. The limits here keep a
//! hostile server from flooding the owner's screen or the event stream, and the
//! credential check keeps it from phishing a secret through a form. Anything
//! over a limit is declined whole (the reason is a fixed sentence for the audit
//! log, never the server's text); titles and descriptions are only shortened.

use protocol::event::{ElicitationField, ElicitationFieldType, ElicitationOption};
use rmcp::model::{ElicitRequestParams, ElicitationSchema};
use serde_json::Value;

/// Longest form message, in bytes.
pub(crate) const MAX_MESSAGE_BYTES: usize = 1024;
/// Most fields in one form.
pub(crate) const MAX_FIELDS: usize = 20;
/// Most choices in one enum field.
pub(crate) const MAX_OPTIONS: usize = 20;
/// Largest `maxLength` (and `minLength`) a text field may ask for.
pub(crate) const MAX_TEXT_LENGTH: u32 = 2000;
/// Titles, option labels and descriptions are cut to this many characters.
const MAX_TITLE_CHARS: usize = 120;
const MAX_DESCRIPTION_CHARS: usize = 400;
/// Longest option value, in bytes.
const MAX_OPTION_VALUE_BYTES: usize = 200;

/// A form of a server, ready to be shown.
#[derive(Debug)]
pub(crate) struct Form {
    pub message: String,
    pub fields: Vec<ElicitationField>,
}

/// The form of a request, or the fixed reason it is declined. A request that is
/// not a form (URL mode) is declined too.
pub(crate) fn project(request: ElicitRequestParams) -> Result<Form, &'static str> {
    let ElicitRequestParams::FormElicitationParams {
        message,
        requested_schema,
        ..
    } = request
    else {
        return Err("not a form request (URL mode is not supported)");
    };
    if message.len() > MAX_MESSAGE_BYTES {
        return Err("the form's message is over 1 KiB");
    }
    let message_sensitive = looks_sensitive(&message);
    let fields = fields_of(&requested_schema, message_sensitive)?;
    Ok(Form { message, fields })
}

fn fields_of(
    schema: &ElicitationSchema,
    block_all: bool,
) -> Result<Vec<ElicitationField>, &'static str> {
    if schema.properties.len() > MAX_FIELDS {
        return Err("the form has more than 20 fields");
    }
    let required = schema.required.clone().unwrap_or_default();
    let order = schema
        .property_order
        .clone()
        .unwrap_or_else(|| schema.properties.keys().cloned().collect());
    if order.len() > MAX_FIELDS {
        return Err("the form has more than 20 fields");
    }
    order
        .into_iter()
        .map(|name| {
            let property = schema
                .properties
                .get(&name)
                .and_then(|p| serde_json::to_value(p).ok())
                .ok_or("a field is missing from the schema")?;
            let is_required = required.contains(&name);
            field(name, &property, is_required, block_all)
        })
        .collect()
}

/// A field name is the key of the answer: ASCII letters, digits, `_` and `-`,
/// 1 to 64 of them (`^[A-Za-z0-9_-]{1,64}$`).
pub(crate) fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn field(
    name: String,
    p: &Value,
    required: bool,
    block_all: bool,
) -> Result<ElicitationField, &'static str> {
    if !valid_name(&name) {
        return Err("a field name is not 1 to 64 letters, digits, `_` or `-`");
    }
    let text = |key: &str| p.get(key).and_then(Value::as_str);
    let length = |key: &str| {
        p.get(key)
            .and_then(Value::as_u64)
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
    };
    let kind = match p.get("type").and_then(Value::as_str) {
        Some("string") => ElicitationFieldType::String,
        Some("number") => ElicitationFieldType::Number,
        Some("integer") => ElicitationFieldType::Integer,
        Some("boolean") => ElicitationFieldType::Boolean,
        _ => return Err("a field is not a string, number, integer, boolean or single choice"),
    };
    let options = options(p)?;
    if !options.is_empty() && kind != ElicitationFieldType::String {
        return Err("a choice field is not a string");
    }
    let (min_length, max_length) = (length("minLength"), length("maxLength"));
    if max_length.is_some_and(|n| n > MAX_TEXT_LENGTH)
        || min_length.is_some_and(|n| n > MAX_TEXT_LENGTH || max_length.is_some_and(|m| n > m))
    {
        return Err("a text field asks for a length over 2000 characters");
    }
    let format = match text("format") {
        None => None,
        Some(f @ ("email" | "uri" | "date" | "date-time")) => Some(f.to_owned()),
        Some(_) => return Err("a field has a format the form does not support"),
    };
    let title = text("title").map(|t| shorten(t, MAX_TITLE_CHARS));
    let description = text("description").map(|d| shorten(d, MAX_DESCRIPTION_CHARS));
    let blocked = block_all
        || looks_sensitive(&name)
        || text("title").is_some_and(looks_sensitive)
        || text("description").is_some_and(looks_sensitive);
    // A blocked field shows its title and nothing else the server wrote.
    let default = p
        .get("default")
        .filter(|d| !blocked && default_fits(kind, d));
    Ok(ElicitationField {
        name,
        title,
        description: description.filter(|_| !blocked),
        kind,
        required,
        options: if blocked { Vec::new() } else { options },
        minimum: p.get("minimum").and_then(Value::as_f64),
        maximum: p.get("maximum").and_then(Value::as_f64),
        min_length,
        max_length,
        format,
        default: default.cloned(),
        blocked,
    })
}

/// A prefilled value must be of the field's own type and not a wall of text.
fn default_fits(kind: ElicitationFieldType, v: &Value) -> bool {
    match kind {
        ElicitationFieldType::String => v
            .as_str()
            .is_some_and(|s| s.chars().count() <= MAX_TEXT_LENGTH as usize),
        ElicitationFieldType::Number | ElicitationFieldType::Integer => v.is_number(),
        ElicitationFieldType::Boolean => v.is_boolean(),
    }
}

/// The choices of an `enum` (with optional `enumNames`) or a titled `oneOf`.
fn options(p: &Value) -> Result<Vec<ElicitationOption>, &'static str> {
    let mut out = Vec::new();
    if let Some(values) = p.get("enum").and_then(Value::as_array) {
        if values.len() > MAX_OPTIONS {
            return Err("a choice field has more than 20 options");
        }
        let names = p.get("enumNames").and_then(Value::as_array);
        for (i, v) in values.iter().enumerate() {
            let value = v.as_str().ok_or("a choice is not a string")?;
            let label = names
                .and_then(|n| n.get(i))
                .and_then(Value::as_str)
                .unwrap_or(value);
            out.push(option(value, label)?);
        }
        if values.is_empty() {
            return Err("a choice field has no options");
        }
    } else if let Some(choices) = p.get("oneOf").and_then(Value::as_array) {
        if choices.len() > MAX_OPTIONS {
            return Err("a choice field has more than 20 options");
        }
        for c in choices {
            let value = c
                .get("const")
                .and_then(Value::as_str)
                .ok_or("a choice is not a string")?;
            let label = c.get("title").and_then(Value::as_str).unwrap_or(value);
            out.push(option(value, label)?);
        }
        if choices.is_empty() {
            return Err("a choice field has no options");
        }
    }
    for (i, o) in out.iter().enumerate() {
        if out[..i].iter().any(|other| other.value == o.value) {
            return Err("a choice field repeats an option");
        }
    }
    Ok(out)
}

fn option(value: &str, label: &str) -> Result<ElicitationOption, &'static str> {
    if value.len() > MAX_OPTION_VALUE_BYTES {
        return Err("a choice value is over 200 bytes");
    }
    Ok(ElicitationOption {
        value: value.to_owned(),
        label: shorten(label, MAX_TITLE_CHARS),
    })
}

/// At most `max` characters, with an ellipsis when something was cut.
fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// English and Turkish spellings of a secret, with Turkish letters folded to
/// ASCII (`şifre`, `sifre`), as one lower-case word each. Long terms are looked
/// for inside a name (`apiKey`, `user_password`); the short ones only count as a
/// whole word (`otp` is not `footprint`).
const SECRET_TERMS: &[&str] = &[
    "password",
    "passwd",
    "passcode",
    "passphrase",
    "pincode",
    "secret",
    "apikey",
    "accesskey",
    "privatekey",
    "securitykey",
    "token",
    "bearer",
    "credential",
    "sessioncookie",
    "sessionid",
    "authcode",
    "verificationcode",
    "securitycode",
    "recoverycode",
    "onetimecode",
    "onetimepass",
    "cardnumber",
    "cardnum",
    "creditcard",
    "debitcard",
    "parola",
    "sifre",
    "apianahtar",
    "erisimanahtar",
    "gizlianahtar",
    "girisbilgi",
    "dogrulamakod",
    "guvenlikkod",
    "tekkullanimlik",
    "kartnumara",
    "kredikarti",
    "bankakarti",
    "kartno",
];
const SECRET_WORDS: &[&str] = &[
    "otp", "2fa", "mfa", "tan", "pin", "pwd", "cvv", "cvc", "cvv2", "ssn", "tckn", "card", "kart",
];

fn fold(c: char) -> char {
    match c {
        '\u{15f}' | '\u{15e}' => 's',
        '\u{11f}' | '\u{11e}' => 'g',
        '\u{fc}' | '\u{dc}' => 'u',
        '\u{f6}' | '\u{d6}' => 'o',
        '\u{e7}' | '\u{c7}' => 'c',
        '\u{131}' | '\u{130}' => 'i',
        c => c,
    }
}

/// Lower-case words of `text`: split at anything that is not a letter or digit
/// and at a camelCase step.
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in text.chars() {
        let c = fold(c);
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase();
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Whether a name, title, description or message looks like it asks for a
/// credential: a password, token, one-time code, card number or API key (in
/// English or Turkish), or contains something that is itself a secret.
pub(crate) fn looks_sensitive(text: &str) -> bool {
    let words = words(text);
    let joined = words.concat();
    SECRET_TERMS.iter().any(|t| joined.contains(t))
        || words.iter().any(|w| SECRET_WORDS.contains(&w.as_str()))
        || !secret_store::detect_secrets(text).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[allow(clippy::needless_pass_by_value)] // call sites pass a `json!` literal
    fn request(message: &str, properties: Value) -> ElicitRequestParams {
        serde_json::from_value(json!({
            "message": message,
            "requestedSchema": {"type": "object", "properties": properties}
        }))
        .unwrap()
    }

    fn fields(properties: Value) -> Result<Vec<ElicitationField>, &'static str> {
        project(request("Which one?", properties)).map(|f| f.fields)
    }

    #[test]
    fn secret_looking_names_titles_and_descriptions_are_blocked_in_english_and_turkish() {
        for (name, title, description) in [
            ("password", None, None),
            ("user_password", None, None),
            ("apiKey", None, None),
            ("api-key", None, None),
            ("auth_token", None, None),
            ("otp", None, None),
            ("code", Some("One-time code"), None),
            ("value", Some("Card number"), None),
            ("cvv", None, None),
            ("pin", None, None),
            ("x", Some("Şifre"), None),
            ("x", Some("SIFRE"), None),
            ("x", Some("Parolanız"), None),
            ("x", Some("API anahtarı"), None),
            ("x", Some("Kart numarası"), None),
            ("x", Some("Doğrulama kodu"), None),
            ("x", Some("Tek kullanımlık kod"), None),
            ("x", None, Some("Paste your access token here")),
            ("x", None, Some("Telefonunuza gelen OTP kodu")),
        ] {
            let mut p = json!({"type": "string"});
            if let Some(t) = title {
                p["title"] = json!(t);
            }
            if let Some(d) = description {
                p["description"] = json!(d);
            }
            let f = fields(json!({ name: p })).unwrap();
            assert!(f[0].blocked, "{name} {title:?} {description:?}");
        }
    }

    #[test]
    fn ordinary_fields_are_not_blocked_and_short_terms_need_a_whole_word() {
        for (name, title) in [
            ("email", "Your email"),
            ("footprint", "Carbon footprint"),
            ("pinned", "Pinned item"),
            ("city", "City"),
            ("age", "Age"),
        ] {
            let f = fields(json!({ name: {"type": "string", "title": title} })).unwrap();
            assert!(!f[0].blocked, "{name}");
        }
    }

    #[test]
    fn a_sensitive_message_blocks_every_field_and_drops_what_the_server_wrote() {
        let f = project(request(
            "Enter the password for your account",
            json!({"value": {"type": "string", "description": "x", "default": "guess"}}),
        ))
        .unwrap()
        .fields;
        assert!(f[0].blocked);
        assert!(f[0].description.is_none() && f[0].default.is_none());
    }

    #[test]
    fn a_blocked_choice_field_shows_no_options() {
        let f = fields(json!({"token": {"type": "string", "enum": ["a", "b"]}})).unwrap();
        assert!(f[0].blocked && f[0].options.is_empty());
    }

    #[test]
    fn field_names_must_be_plain() {
        for bad in [
            "",
            "a b",
            "password-for-praxi\u{200b}",
            "naïve",
            &"a".repeat(65),
        ] {
            assert!(
                fields(json!({ bad: {"type": "string"} })).is_err(),
                "{bad:?}"
            );
        }
        assert!(valid_name("a_B-9") && valid_name(&"a".repeat(64)));
    }

    #[test]
    fn size_limits_decline_the_whole_form() {
        let many = |n: usize| {
            let mut props = serde_json::Map::new();
            for i in 0..n {
                props.insert(format!("f{i}"), json!({"type": "string"}));
            }
            Value::Object(props)
        };
        assert!(fields(many(MAX_FIELDS)).is_ok());
        assert!(fields(many(MAX_FIELDS + 1)).is_err());

        let options = |n: usize| {
            let values: Vec<String> = (0..n).map(|i| format!("v{i}")).collect();
            json!({"c": {"type": "string", "enum": values}})
        };
        assert!(fields(options(MAX_OPTIONS)).is_ok());
        assert!(fields(options(MAX_OPTIONS + 1)).is_err());
        assert!(fields(json!({"c": {"type": "string", "enum": []}})).is_err());

        assert!(fields(json!({"t": {"type": "string", "maxLength": 2000}})).is_ok());
        assert!(fields(json!({"t": {"type": "string", "maxLength": 2001}})).is_err());
        assert!(fields(json!({"t": {"type": "string", "minLength": 2001}})).is_err());

        let message = |n: usize| project(request(&"m".repeat(n), json!({})));
        assert!(message(MAX_MESSAGE_BYTES).is_ok());
        assert!(message(MAX_MESSAGE_BYTES + 1).is_err());
        let long_option = json!({"c": {"type": "string", "enum": ["x".repeat(201)]}});
        assert!(fields(long_option).is_err());
    }

    #[test]
    fn titles_descriptions_and_labels_are_truncated_not_declined() {
        let f = fields(json!({"c": {
            "type": "string",
            "title": "t".repeat(500),
            "description": "d".repeat(5000),
            "oneOf": [{"const": "a", "title": "l".repeat(500)}]
        }}))
        .unwrap();
        assert_eq!(
            f[0].title.as_deref().unwrap().chars().count(),
            MAX_TITLE_CHARS
        );
        assert!(f[0].title.as_deref().unwrap().ends_with('\u{2026}'));
        assert_eq!(
            f[0].description.as_deref().unwrap().chars().count(),
            MAX_DESCRIPTION_CHARS
        );
        assert_eq!(f[0].options[0].label.chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn a_flat_schema_keeps_its_shape_and_url_mode_is_declined() {
        let f = fields(json!({
            "name": {"type": "string", "title": "Name", "maxLength": 20},
            "age": {"type": "integer", "minimum": 0, "maximum": 150},
            "ok": {"type": "boolean", "default": true},
            "size": {"type": "string", "enum": ["s", "m"], "enumNames": ["Small", "Medium"]}
        }))
        .unwrap();
        let by = |n: &str| f.iter().find(|f| f.name == n).unwrap();
        assert_eq!(f.len(), 4);
        assert_eq!(by("name").max_length, Some(20));
        assert_eq!(by("age").maximum, Some(150.0));
        assert_eq!(by("ok").default, Some(Value::Bool(true)));
        assert_eq!(by("size").options[1].label, "Medium");
        assert!(f.iter().all(|f| !f.blocked));

        let url = ElicitRequestParams::UrlElicitationParams {
            meta: None,
            message: "open".into(),
            url: "https://example.com/".into(),
            elicitation_id: "x".into(),
        };
        assert!(project(url).is_err());

        let multi = fields(json!({
            "tags": {"type": "array", "items": {"type": "string", "enum": ["a", "b"]}}
        }));
        assert!(multi.is_err());
    }
}
