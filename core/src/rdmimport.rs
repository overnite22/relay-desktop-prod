//! Remote Desktop Manager export (.rdm XML) analysis and import for local
//! sources: a port of the Relay server's `internal/rdmimport`, so that both
//! modes produce the same plan.

use crate::admin::*;
use crate::model::*;
use crate::source::{DataSource, Error, Result};
use std::collections::{HashMap, HashSet};

pub const MAX_IMPORT: usize = 32 << 20;

struct Node {
    name: String,
    text: String,
    children: Vec<Node>,
}

const SKIP_BLOCKS: &[&str] = &[
    "metainformation",
    "events",
    "subconnections",
    "attachments",
    "customfields",
    "documentation",
    "vpn",
    "gateway",
    "sshgateway",
    "proxy",
    "beforeopen",
    "afteropen",
    "beforeclose",
    "afterclose",
];

impl Node {
    fn from(n: roxmltree::Node, depth: usize) -> Result<Node> {
        if depth > 64 {
            return Err(Error::Invalid("XML trop profond".into()));
        }
        let mut out = Node {
            name: n.tag_name().name().to_owned(),
            text: String::new(),
            children: Vec::new(),
        };
        for c in n.children() {
            if c.is_element() {
                out.children.push(Node::from(c, depth + 1)?);
            } else if c.is_text() {
                out.text.push_str(c.text().unwrap_or(""));
            }
        }
        Ok(out)
    }

    fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name.eq_ignore_ascii_case(name))
    }

    fn text(&self, name: &str) -> String {
        self.child(name).map(|c| c.text.trim().to_owned()).unwrap_or_default()
    }

    /// First non-empty leaf among `names`, searched breadth first on this
    /// level then in sub-blocks (except event, gateway… blocks).
    fn find(&self, names: &[&str]) -> String {
        for c in &self.children {
            if c.children.is_empty() && !c.text.trim().is_empty() && names.iter().any(|n| c.name.eq_ignore_ascii_case(n)) {
                return c.text.trim().to_owned();
            }
        }
        for c in &self.children {
            if c.children.is_empty() || SKIP_BLOCKS.contains(&c.name.to_lowercase().as_str()) {
                continue;
            }
            let v = c.find(names);
            if !v.is_empty() {
                return v;
            }
        }
        String::new()
    }
}

fn first(v: &[String]) -> String {
    v.iter().map(|s| s.trim()).find(|s| !s.is_empty()).unwrap_or("").to_owned()
}

fn kind_of(t: &str) -> Option<&'static str> {
    Some(match t {
        "rdpconfigured" | "rdpfilename" => "rdp",
        "sshshell" | "putty" | "terminalconsole" | "unixterminal" => "ssh",
        "vnc" | "appleremotedesktop" => "vnc",
        "webbrowser" | "idrac" | "ilo" | "dellremoteaccesscontroller" | "hpintegratedlightsout" => "web",
        _ => return None,
    })
}

fn split_path(g: &str) -> Vec<String> {
    g.replace('/', "\\")
        .split('\\')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

fn split_host_port(s: &str) -> (String, u16) {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('[') {
        if let Some((h, p)) = rest.split_once("]:") {
            if let Ok(n) = p.parse() {
                return (h.to_owned(), n);
            }
        }
    } else if let Some((h, p)) = s.rsplit_once(':') {
        if !h.contains(':') {
            if let Ok(n) = p.parse() {
                return (h.to_owned(), n);
            }
        }
    }
    (s.trim_matches(['[', ']']).to_owned(), 0)
}

fn special_ref(r: &str) -> bool {
    [
        "245a4245-",
        "b87b29d9-",
        "0c0c8d0a-",
        "9f3c3bcf-",
        "88e4be76-",
        "1310cf82-",
        "e2cc9029-",
    ]
    .iter()
    .any(|p| r.starts_with(p))
}

fn find_list(n: &Node) -> Option<&Node> {
    for c in &n.children {
        if c.children.first().is_some_and(|f| f.name.eq_ignore_ascii_case("Connection")) {
            return Some(c);
        }
        if let Some(f) = find_list(c) {
            return Some(f);
        }
    }
    None
}

pub fn parse(data: &[u8]) -> Result<ImportPlan> {
    if data.len() > MAX_IMPORT {
        return Err(Error::Invalid("fichier trop volumineux (32 Mo maximum)".into()));
    }
    let text = String::from_utf8_lossy(data);
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Err(Error::Invalid("fichier vide".into()));
    }
    let opts = roxmltree::ParsingOptions {
        allow_dtd: false,
        ..Default::default()
    };
    let doc = roxmltree::Document::parse_with_options(text, opts).map_err(|e| Error::Invalid(format!("XML invalide : {e}")))?;
    let root = Node::from(doc.root_element(), 0)?;
    let conns: &[Node] = if root.name.eq_ignore_ascii_case("ArrayOfConnection") {
        &root.children
    } else {
        match find_list(&root) {
            Some(c) => &c.children,
            None => {
                return Err(Error::Invalid(
                    "format non reconnu : un export Remote Desktop Manager (.rdm) contient une liste <ArrayOfConnection>".into(),
                ))
            }
        }
    };
    let mut p = ImportPlan::default();
    let mut seen = HashSet::new();
    let mut add_folder = |p: &mut ImportPlan, path: &[String]| {
        for i in 1..=path.len() {
            if seen.insert(path[..i].join("\0")) {
                p.folders.push(ImportFolder { path: path[..i].to_vec() });
            }
        }
    };
    let is_conn = |c: &&Node| c.name.eq_ignore_ascii_case("Connection");
    // RDM "Host" records: their sessions inherit the address.
    let mut hosts: HashMap<String, (String, String)> = HashMap::new();
    for c in conns.iter().filter(is_conn) {
        if c.text("ConnectionType").eq_ignore_ascii_case("Host") {
            let blk = c.child("HostDetails").unwrap_or(c);
            hosts.insert(
                c.text("ID").to_lowercase(),
                (blk.find(&["Host"]), blk.find(&["UserName", "Username"])),
            );
        }
    }
    let mut cred_by_id = HashMap::new();
    let mut pending = Vec::new();
    for c in conns.iter().filter(is_conn) {
        let typ = c.text("ConnectionType");
        let lt = typ.to_lowercase();
        let mut name = c.text("Name");
        if name.is_empty() {
            name = "(sans nom)".into();
        }
        let id = c.text("ID");
        let mut path = split_path(&c.text("Group"));
        let skip = |p: &mut ImportPlan, reason: &str| {
            p.skipped.push(ImportSkipped {
                name: name.clone(),
                kind: typ.clone(),
                reason: reason.into(),
            })
        };
        match lt.as_str() {
            "group" | "templategroup" => {
                if path.is_empty() {
                    path = vec![name.clone()];
                }
                add_folder(&mut p, &path);
                continue;
            }
            "credential" => {
                let blk = c.child("Credentials").unwrap_or(c);
                let mut cr = ImportCredential {
                    source_id: id.clone(),
                    name: name.clone(),
                    username: first(&[
                        blk.find(&["UserName", "Username", "PrivateKeyOverrideUsername"]),
                        c.text("UserName"),
                    ]),
                    domain: first(&[blk.find(&["Domain"]), c.text("Domain")]),
                    password: first(&[blk.find(&["Password"]), c.text("Password")]),
                    ..Default::default()
                };
                cr.has_password = !cr.password.is_empty();
                let key = blk.find(&["PrivateKeyData"]);
                if key.contains("PRIVATE KEY") {
                    (cr.private_key, cr.has_private_key) = (key, true);
                }
                cr.encrypted_secret = (!cr.has_password
                    && !first(&[blk.find(&["SafePassword", "SafePasswordAndPassphrase"]), c.text("SafePassword")]).is_empty())
                    || (!cr.has_private_key && !blk.find(&["SafePrivateKeyData"]).is_empty());
                cred_by_id.insert(id.to_lowercase(), p.credentials.len());
                p.credentials.push(cr);
                continue;
            }
            "host" => {
                skip(&mut p, "fiche hôte RDM : ses sessions sont importées avec son adresse");
                continue;
            }
            "proxmoxconsole" => {
                skip(&mut p, "utilisez la synchronisation Proxmox de Relay");
                continue;
            }
            _ => {}
        }
        let Some(kind) = kind_of(&lt) else {
            skip(&mut p, "type non pris en charge par Relay");
            continue;
        };
        let hi = hosts.get(&c.text("HostConnectionID").to_lowercase()).cloned().unwrap_or_default();
        let mut e = ImportEntry {
            source_id: id,
            name: name.clone(),
            kind: kind.into(),
            path: path.clone(),
            notes: c.text("Description"),
            rdm_type: typ.clone(),
            credential_ref: c.text("CredentialConnectionID").to_lowercase(),
            ..Default::default()
        };
        if kind == "web" {
            e.url = first(&[c.find(&["WebBrowserUrl"]), c.text("Url"), c.find(&["Url", "Host"]), hi.0.clone()]);
            if !e.url.is_empty() && !e.url.contains("://") {
                e.url = format!("https://{}", e.url);
            }
        } else {
            let raw = first(&[c.text("Url"), c.find(&["Host", "HostName", "Url", "Server"]), hi.0.clone()]);
            (e.host, e.port) = split_host_port(&raw);
            if e.port == 0 {
                e.port = first(&[c.find(&["HostPort", "Port"]), c.text("Port")]).parse().unwrap_or(0);
            }
            if e.port == 0 {
                e.port = match kind {
                    "rdp" => 3389,
                    "ssh" => 22,
                    _ => 5900,
                };
            }
        }
        e.username = first(&[c.text("UserName"), c.find(&["UserName", "Username"]), hi.1]);
        e.inline_secret = !first(&[c.text("SafePassword"), c.find(&["SafePassword", "SafePrivateKeyData"])]).is_empty();
        if (kind != "web" && e.host.is_empty()) || (kind == "web" && e.url.is_empty()) {
            skip(&mut p, "hôte ou URL introuvable");
            continue;
        }
        add_folder(&mut p, &path);
        pending.push(e);
    }
    let (mut unresolved, mut inline) = (0, 0);
    for mut e in pending {
        if !e.credential_ref.is_empty() && !special_ref(&e.credential_ref) {
            match cred_by_id.get(&e.credential_ref) {
                Some(&i) => e.credential = p.credentials[i].name.clone(),
                None => {
                    unresolved += 1;
                    e.credential_ref.clear();
                }
            }
        } else {
            e.credential_ref.clear();
        }
        e.inline_secret = e.inline_secret && e.credential.is_empty();
        inline += e.inline_secret as usize;
        p.entries.push(e);
    }
    let enc = p.credentials.iter().filter(|c| c.encrypted_secret).count();
    if enc > 0 {
        p.warnings.push(format!("{enc} identifiant(s) ont un mot de passe ou une clé chiffrés par RDM : ils seront créés sans secret, à ressaisir dans le coffre."));
    }
    if inline > 0 {
        p.warnings.push(format!("{inline} entrée(s) ont un mot de passe ou une clé propres (non liés à un identifiant) : ils ne sont pas importés. Choisissez ci-dessous un identifiant par défaut si besoin."));
    }
    if unresolved > 0 {
        p.warnings.push(format!(
            "{unresolved} entrée(s) utilisent un identifiant absent de l'export (autre coffre) : elles seront importées sans identifiant."
        ));
    }
    Ok(p)
}

/// Creates the plan under a new root folder (same rules as the server).
pub fn apply(src: &dyn DataSource, p: &ImportPlan, opt: &ImportOptions) -> Result<ImportResult> {
    let root_name = if opt.root.trim().is_empty() {
        "Import RDM"
    } else {
        opt.root.trim()
    };
    let root = src.save_folder(&Folder {
        name: root_name.into(),
        ..Default::default()
    })?;
    let mut res = ImportResult {
        root_id: root.id.clone(),
        ..Default::default()
    };
    let mut folders: HashMap<String, String> = HashMap::new();
    let mut ensure = |res: &mut ImportResult, path: &[String]| -> Result<String> {
        let (mut parent, mut key) = (root.id.clone(), String::new());
        for part in path {
            key.push('\0');
            key.push_str(part);
            if let Some(id) = folders.get(&key) {
                parent = id.clone();
                continue;
            }
            let f = src.save_folder(&Folder {
                name: part.clone(),
                parent_id: Some(parent),
                ..Default::default()
            })?;
            folders.insert(key.clone(), f.id.clone());
            parent = f.id;
            res.folders += 1;
        }
        Ok(parent)
    };
    for f in &p.folders {
        ensure(&mut res, &f.path)?;
    }
    let mut cred_ids: HashMap<String, String> = HashMap::new();
    for c in &p.credentials {
        let input = CredentialInput {
            name: c.name.clone(),
            username: c.username.clone(),
            domain: c.domain.clone(),
            notes: if c.encrypted_secret {
                "Importé depuis RDM : mot de passe à ressaisir.".into()
            } else {
                String::new()
            },
            password: Some(c.password.clone()).filter(|s| !s.is_empty()),
            private_key: Some(c.private_key.clone()).filter(|s| !s.is_empty()),
            ..Default::default()
        };
        let saved = src
            .save_credential(None, &input)
            .map_err(|e| Error::Invalid(format!("identifiant {} : {e}", c.name)))?;
        cred_ids.insert(c.source_id.to_lowercase(), saved.id.clone());
        if c.name == opt.ssh_default {
            cred_ids.insert("\0ssh".into(), saved.id.clone());
        }
        if c.name == opt.rdp_default {
            cred_ids.insert("\0rdp".into(), saved.id.clone());
        }
        res.credentials += 1;
    }
    for e in &p.entries {
        let folder = ensure(&mut res, &e.path)?;
        let kind: Kind = serde_json::from_value(serde_json::Value::String(e.kind.clone())).map_err(|x| Error::Invalid(x.to_string()))?;
        let mut en = Entry::new(kind);
        en.folder_id = Some(folder);
        en.name = e.name.clone();
        en.host = e.host.clone();
        en.port = e.port;
        en.url = e.url.clone();
        en.username = e.username.clone();
        en.notes = e.notes.clone();
        en.credential_id = (!e.credential_ref.is_empty())
            .then(|| cred_ids.get(&e.credential_ref).cloned())
            .flatten()
            .or_else(|| cred_ids.get(&format!("\0{}", e.kind)).cloned());
        src.save_entry(&en)
            .map_err(|x| Error::Invalid(format!("entrée {} : {x}", e.name)))?;
        res.entries += 1;
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = include_bytes!("../tests/data/sample.rdm");

    #[test]
    fn parse_sample_like_the_server() {
        let p = parse(SAMPLE).unwrap();
        assert_eq!(p.entries.len(), 4);
        let by = |n: &str| p.entries.iter().find(|e| e.name == n).unwrap();
        let e = by("srv-lab-ansible");
        assert_eq!(
            (e.kind.as_str(), e.host.as_str(), e.port, e.username.as_str(), e.inline_secret),
            ("ssh", "10.20.0.61", 2222, "root", true)
        );
        let e = by("srv-lab-dc1");
        assert_eq!((e.kind.as_str(), e.host.as_str(), e.port), ("rdp", "10.20.0.20", 3390));
        assert_eq!(
            (e.credential.as_str(), e.notes.as_str()),
            ("Administrator OverNite Industries", "Contrôleur de domaine")
        );
        let e = by("srv-lab-apps");
        assert_eq!(
            (e.port, e.path.join("/"), e.credential_ref.as_str()),
            (3389, "WINDOWS/Sous-dossier".into(), "")
        );
        assert_eq!(by("Omada").url, "https://10.20.0.249:8043");
        assert_eq!(p.credentials.len(), 1);
        let c = &p.credentials[0];
        assert!(c.username == "Administrator" && c.domain == "OVERNITE" && c.encrypted_secret);
        assert_eq!((p.skipped.len(), p.warnings.len()), (2, 3));
        let paths: Vec<String> = p.folders.iter().map(|f| f.path.join("/")).collect();
        assert_eq!(paths.join(","), "LINUX LXC,WINDOWS,WINDOWS/Sous-dossier,RESEAU");
        assert!(parse(b"<autre><x/></autre>").is_err());
        assert!(parse(b"pas du xml").is_err());
    }

    #[test]
    fn apply_to_local_source() {
        let dir = tempfile::tempdir().unwrap();
        let src = crate::LocalSource::create(&dir.path().join("i.rdb"), "mot de passe maître").unwrap();
        let p = parse(SAMPLE).unwrap();
        let opt = ImportOptions {
            root: "RDM".into(),
            ssh_default: "Administrator OverNite Industries".into(),
            ..Default::default()
        };
        let res = apply(&src, &p, &opt).unwrap();
        assert_eq!((res.entries, res.credentials, res.folders), (4, 1, 4));
        let tree = src.tree().unwrap();
        let cred = src.credentials().unwrap()[0].id.clone();
        let ssh = tree.entries.iter().find(|e| e.name == "srv-lab-ansible").unwrap();
        assert_eq!(ssh.credential_id.as_deref(), Some(cred.as_str()));
        let rapps = tree.entries.iter().find(|e| e.name == "srv-lab-apps").unwrap();
        assert!(rapps.credential_id.is_none());
        assert_eq!(
            tree.folders
                .iter()
                .filter(|f| f.parent_id.as_deref() == Some(res.root_id.as_str()))
                .count(),
            3
        );
    }
}
