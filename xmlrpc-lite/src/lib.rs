//! Codec XML-RPC minimal (sous-ensemble utilisé par rTorrent) : chaînes, entiers, booléens,
//! flottants, tableaux et structures. Aucune exécution, aucune entité externe (pas de DTD).
#![forbid(unsafe_code)]

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    /// Données binaires (type XML-RPC `base64`).
    Base64(Vec<u8>),
    Int(i64),
    Bool(bool),
    Double(f64),
    Array(Vec<Value>),
    Struct(Vec<(String, Value)>),
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum Error {
    #[error("XML invalide: {0}")]
    Xml(&'static str),
    #[error("réponse XML-RPC inattendue: {0}")]
    Shape(&'static str),
    #[error("fault XML-RPC {code}: {message}")]
    Fault { code: i64, message: String },
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        if let Self::Str(s) = self {
            Some(s)
        } else {
            None
        }
    }
    /// Entier (accepte aussi un booléen et une chaîne numérique : rTorrent n'est pas constant).
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            Self::Bool(b) => Some(i64::from(*b)),
            Self::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Value]> {
        if let Self::Array(a) = self {
            Some(a)
        } else {
            None
        }
    }
}

// ---------- Encodage ----------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(char::from(B64[(n >> 18) as usize & 63]));
        out.push(char::from(B64[(n >> 12) as usize & 63]));
        out.push(if chunk.len() > 1 {
            char::from(B64[(n >> 6) as usize & 63])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(B64[n as usize & 63])
        } else {
            '='
        });
    }
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn enc_value(v: &Value, out: &mut String) {
    out.push_str("<value>");
    match v {
        Value::Str(s) => out.push_str(&format!("<string>{}</string>", esc(s))),
        Value::Base64(b) => out.push_str(&format!("<base64>{}</base64>", base64(b))),
        Value::Int(i) => out.push_str(&format!("<i8>{i}</i8>")),
        Value::Bool(b) => out.push_str(&format!("<boolean>{}</boolean>", u8::from(*b))),
        Value::Double(d) => out.push_str(&format!("<double>{d}</double>")),
        Value::Array(a) => {
            out.push_str("<array><data>");
            for x in a {
                enc_value(x, out);
            }
            out.push_str("</data></array>");
        }
        Value::Struct(m) => {
            out.push_str("<struct>");
            for (k, x) in m {
                out.push_str(&format!("<member><name>{}</name>", esc(k)));
                enc_value(x, out);
                out.push_str("</member>");
            }
            out.push_str("</struct>");
        }
    }
    out.push_str("</value>");
}

pub fn encode_call(method: &str, params: &[Value]) -> String {
    let mut s = format!(
        "<?xml version=\"1.0\"?><methodCall><methodName>{}</methodName><params>",
        esc(method)
    );
    for p in params {
        s.push_str("<param>");
        enc_value(p, &mut s);
        s.push_str("</param>");
    }
    s.push_str("</params></methodCall>");
    s
}

pub fn encode_response(v: &Value) -> String {
    let mut s = String::from("<?xml version=\"1.0\"?><methodResponse><params><param>");
    enc_value(v, &mut s);
    s.push_str("</param></params></methodResponse>");
    s
}

pub fn encode_fault(code: i64, message: &str) -> String {
    let mut s = String::from("<?xml version=\"1.0\"?><methodResponse><fault>");
    enc_value(
        &Value::Struct(vec![
            ("faultCode".into(), Value::Int(code)),
            ("faultString".into(), Value::Str(message.into())),
        ]),
        &mut s,
    );
    s.push_str("</fault></methodResponse>");
    s
}

// ---------- Analyse ----------

#[derive(Debug, Default)]
struct Node {
    name: String,
    text: String,
    children: Vec<Node>,
}

fn unesc(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Analyse un document XML simple (éléments, texte, auto-fermantes ; attributs ignorés).
fn parse_xml(src: &str) -> Result<Node, Error> {
    let b = src.as_bytes();
    let mut i = 0usize;
    let mut stack: Vec<Node> = vec![Node::default()];
    let mut text_start = 0usize;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        // Texte accumulé avant ce « < ».
        if let Some(top) = stack.last_mut() {
            top.text.push_str(&unesc(&src[text_start..i]));
        }
        let end = src[i..]
            .find('>')
            .map(|e| i + e)
            .ok_or(Error::Xml("balise non fermée"))?;
        let tag = &src[i + 1..end];
        if tag.starts_with('?') || tag.starts_with('!') {
            // Déclaration / commentaire : ignoré (DTD et entités externes jamais interprétées).
        } else if let Some(name) = tag.strip_prefix('/') {
            let node = stack.pop().ok_or(Error::Xml("pile vide"))?;
            if node.name != name.trim() {
                return Err(Error::Xml("balises mal imbriquées"));
            }
            stack
                .last_mut()
                .ok_or(Error::Xml("pile vide"))?
                .children
                .push(node);
        } else {
            let self_closing = tag.ends_with('/');
            let name = tag
                .trim_end_matches('/')
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_owned();
            let node = Node {
                name,
                ..Node::default()
            };
            if self_closing {
                stack
                    .last_mut()
                    .ok_or(Error::Xml("pile vide"))?
                    .children
                    .push(node);
            } else {
                stack.push(node);
            }
        }
        i = end + 1;
        text_start = i;
        if stack.len() > 64 {
            return Err(Error::Xml("imbrication excessive"));
        }
    }
    if stack.len() != 1 {
        return Err(Error::Xml("document incomplet"));
    }
    stack
        .pop()
        .and_then(|mut root| root.children.pop())
        .ok_or(Error::Xml("document vide"))
}

fn value_of(n: &Node) -> Result<Value, Error> {
    if n.name != "value" {
        return Err(Error::Shape("<value> attendu"));
    }
    let Some(c) = n.children.first() else {
        return Ok(Value::Str(n.text.clone())); // <value>texte</value> = chaîne
    };
    Ok(match c.name.as_str() {
        "string" | "dateTime.iso8601" => Value::Str(c.text.clone()),
        "base64" => Value::Str(c.text.trim().to_owned()),
        "i4" | "i8" | "int" | "i2" => Value::Int(
            c.text
                .trim()
                .parse()
                .map_err(|_| Error::Shape("entier invalide"))?,
        ),
        "boolean" => Value::Bool(c.text.trim() == "1"),
        "double" => Value::Double(
            c.text
                .trim()
                .parse()
                .map_err(|_| Error::Shape("flottant invalide"))?,
        ),
        "array" => {
            let data = c
                .children
                .iter()
                .find(|x| x.name == "data")
                .ok_or(Error::Shape("<data> attendu"))?;
            Value::Array(
                data.children
                    .iter()
                    .map(value_of)
                    .collect::<Result<_, _>>()?,
            )
        }
        "struct" => {
            let mut m = Vec::new();
            for mem in &c.children {
                let name = mem
                    .children
                    .iter()
                    .find(|x| x.name == "name")
                    .map(|x| x.text.clone());
                let val = mem.children.iter().find(|x| x.name == "value");
                if let (Some(k), Some(v)) = (name, val) {
                    m.push((k, value_of(v)?));
                }
            }
            Value::Struct(m)
        }
        _ => return Err(Error::Shape("type non géré")),
    })
}

/// Valeur de retour d'une réponse ; `Err(Fault)` si le serveur renvoie un `<fault>`.
pub fn parse_response(xml: &str) -> Result<Value, Error> {
    let root = parse_xml(xml)?;
    if root.name != "methodResponse" {
        return Err(Error::Shape("methodResponse attendu"));
    }
    if let Some(f) = root.children.iter().find(|c| c.name == "fault") {
        let v = value_of(f.children.first().ok_or(Error::Shape("fault vide"))?)?;
        if let Value::Struct(m) = v {
            let get = |k: &str| m.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
            return Err(Error::Fault {
                code: get("faultCode").and_then(|v| v.as_i64()).unwrap_or(0),
                message: get("faultString")
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default(),
            });
        }
        return Err(Error::Shape("fault invalide"));
    }
    let params = root
        .children
        .iter()
        .find(|c| c.name == "params")
        .ok_or(Error::Shape("params attendu"))?;
    let p = params
        .children
        .first()
        .ok_or(Error::Shape("param attendu"))?;
    value_of(p.children.first().ok_or(Error::Shape("value attendu"))?)
}

/// Analyse un appel (utile aux faux serveurs de test).
pub fn parse_call(xml: &str) -> Result<(String, Vec<Value>), Error> {
    let root = parse_xml(xml)?;
    if root.name != "methodCall" {
        return Err(Error::Shape("methodCall attendu"));
    }
    let name = root
        .children
        .iter()
        .find(|c| c.name == "methodName")
        .map(|c| c.text.trim().to_owned())
        .ok_or(Error::Shape("methodName attendu"))?;
    let mut params = Vec::new();
    if let Some(ps) = root.children.iter().find(|c| c.name == "params") {
        for p in &ps.children {
            params.push(value_of(
                p.children.first().ok_or(Error::Shape("value attendu"))?,
            )?);
        }
    }
    Ok((name, params))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_roundtrip_with_escaping() {
        let params = vec![
            Value::Str(String::new()),
            Value::Str("a<b>&c".into()),
            Value::Int(-5),
            Value::Bool(true),
        ];
        let xml = encode_call("load.start", &params);
        assert_eq!(parse_call(&xml).unwrap(), ("load.start".to_owned(), params));
    }

    #[test]
    fn parses_real_rtorrent_shapes() {
        // d.multicall2 : tableau de tableaux ; <data/> vide ; entiers i4/i8 ; chaîne sans balise.
        let xml = r#"<?xml version="1.0"?><methodResponse><params><param><value><array><data>
            <value><array><data><value><string>ABCDEF</string></value><value><i8>12345</i8></value><value><i4>1</i4></value><value>nu</value></data></array></value>
            </data></array></value></param></params></methodResponse>"#;
        let v = parse_response(xml).unwrap();
        let row = &v.as_array().unwrap()[0];
        let r = row.as_array().unwrap();
        assert_eq!(r[0].as_str(), Some("ABCDEF"));
        assert_eq!(r[1].as_i64(), Some(12345));
        assert_eq!(r[2].as_i64(), Some(1));
        assert_eq!(r[3].as_str(), Some("nu"));
        let empty = parse_response("<?xml version=\"1.0\"?><methodResponse><params><param><value><array><data/></array></value></param></params></methodResponse>").unwrap();
        assert_eq!(empty, Value::Array(vec![]));
    }

    #[test]
    fn base64_values_are_encoded_for_binary_payloads() {
        let x = encode_call(
            "load.raw",
            &[
                Value::Base64(b"Man".to_vec()),
                Value::Base64(vec![0xff, 0x00]),
            ],
        );
        assert!(x.contains("<base64>TWFu</base64>"), "{x}");
        assert!(x.contains("<base64>/wA=</base64>"), "{x}"); // remplissage « = »
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"Ma"), "TWE=");
    }

    #[test]
    fn faults_become_errors() {
        let xml = encode_fault(-506, "Could not find info-hash.");
        assert_eq!(
            parse_response(&xml),
            Err(Error::Fault {
                code: -506,
                message: "Could not find info-hash.".into()
            })
        );
    }

    #[test]
    fn struct_and_entities() {
        let v = Value::Struct(vec![("k".into(), Value::Str("x&y".into()))]);
        assert_eq!(parse_response(&encode_response(&v)).unwrap(), v);
    }

    #[test]
    fn malformed_input_is_rejected_not_panicking() {
        for bad in [
            "",
            "<a>",
            "<a></b>",
            "<methodResponse><params></methodResponse>",
            "no xml",
            &"<a>".repeat(200),
        ] {
            assert!(parse_response(bad).is_err(), "{bad:?}");
        }
    }
}
