//! Assembly manifest loader (Isotope spec 02/08, pre-runtime host).
//!
//! The manifest — `gateway.assembly.yaml` — declares the Blocks, the
//! public Block, and the wiring that constructs each Block's namespace.
//! This module makes the manifest load-bearing: the Block backings route
//! every guest read/write through a [`WiringTable`] derived here, so a
//! path with no wiring entry is refused at the namespace boundary rather
//! than silently reaching the substrate. Remove a wiring line and the
//! Block genuinely loses that capability.
//!
//! Imports are bound by the native host (main.rs plays the parent
//! Assembly): each `$import` target maps to a substrate mount prefix via
//! [`standard_bindings`].

use std::collections::BTreeMap;
use std::sync::Arc;

/// Embedded default manifest, compiled into the binary so the gateway is
/// runnable with no files on disk. `OX_GATEWAY_ASSEMBLY=<path>` overrides.
const EMBEDDED: &str = include_str!("../gateway.assembly.yaml");

#[derive(Debug, serde::Deserialize)]
pub struct Manifest {
    pub assembly: String,
    pub version: String,
    #[serde(default)]
    pub imports: BTreeMap<String, String>,
    pub blocks: BTreeMap<String, String>,
    pub public: String,
    #[serde(default)]
    pub wiring: Vec<String>,
    #[serde(default)]
    pub config: BTreeMap<String, serde_json::Value>,
}

/// One parsed wiring entry: `<block>:/<path> -> <target>`.
#[derive(Debug, PartialEq)]
pub struct Wire {
    pub block: String,
    /// Guest-namespace prefix, no leading slash (`gateway/completions`).
    pub prefix: String,
    pub target: Target,
}

#[derive(Debug, PartialEq)]
pub enum Target {
    /// `$name` — a service the parent Assembly wires in.
    Import(String),
    /// Another Block in this Assembly.
    Block(String),
}

impl Manifest {
    pub fn embedded() -> Result<Self, String> {
        Self::parse(EMBEDDED)
    }

    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let manifest: Manifest =
            serde_yaml::from_str(text).map_err(|e| format!("assembly manifest: {e}"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Structural validation: every name a section mentions must resolve.
    fn validate(&self) -> Result<(), String> {
        if !self.blocks.contains_key(&self.public) {
            return Err(format!("public block '{}' is not in blocks", self.public));
        }
        for name in self.config.keys() {
            if !self.blocks.contains_key(name) {
                return Err(format!("config section for unknown block '{name}'"));
            }
        }
        for entry in self.wires()? {
            if !self.blocks.contains_key(&entry.block) {
                return Err(format!("wiring for unknown block '{}'", entry.block));
            }
            match &entry.target {
                Target::Import(name) if !self.imports.contains_key(name) => {
                    return Err(format!("wiring targets undeclared import '${name}'"));
                }
                Target::Block(name) if !self.blocks.contains_key(name) => {
                    return Err(format!("wiring targets unknown block '{name}'"));
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn wires(&self) -> Result<Vec<Wire>, String> {
        self.wiring.iter().map(|line| parse_wire(line)).collect()
    }

    /// The namespace for one Block, resolved against the host's import
    /// bindings. Block-to-block targets resolve through the target Block's
    /// own binding under its name (the pre-Isotope stand-in for "the
    /// target Block's store").
    pub fn wiring_for(
        &self,
        block: &str,
        bindings: &BTreeMap<String, String>,
    ) -> Result<WiringTable, String> {
        if !self.blocks.contains_key(block) {
            return Err(format!("no block named '{block}' in assembly"));
        }
        let mut entries = Vec::new();
        for wire in self.wires()? {
            if wire.block != block {
                continue;
            }
            let substrate = match &wire.target {
                Target::Import(name) => bindings
                    .get(name)
                    .ok_or_else(|| format!("host provides no binding for import '${name}'"))?
                    .clone(),
                Target::Block(name) => bindings
                    .get(name)
                    .ok_or_else(|| format!("host provides no binding for block '{name}'"))?
                    .clone(),
            };
            entries.push((wire.prefix, substrate));
        }
        // Longest prefix first so nested declarations shadow correctly.
        entries.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        Ok(WiringTable(Arc::new(entries)))
    }
}

fn parse_wire(line: &str) -> Result<Wire, String> {
    let (lhs, rhs) = line
        .split_once("->")
        .ok_or_else(|| format!("wiring entry missing '->': {line}"))?;
    let (block, path) = lhs
        .trim()
        .split_once(":/")
        .ok_or_else(|| format!("wiring entry missing '<block>:/': {line}"))?;
    let prefix = path.trim().trim_matches('/').to_string();
    if prefix.is_empty() {
        return Err(format!("wiring entry has empty path: {line}"));
    }
    let rhs = rhs.trim();
    let target = match rhs.strip_prefix('$') {
        Some(import) => Target::Import(import.to_string()),
        None => Target::Block(rhs.to_string()),
    };
    Ok(Wire {
        block: block.trim().to_string(),
        prefix,
        target,
    })
}

/// A Block's namespace as (guest prefix → substrate prefix) rewrites,
/// longest prefix first. Paths outside every entry do not resolve.
#[derive(Clone)]
pub struct WiringTable(Arc<Vec<(String, String)>>);

impl WiringTable {
    /// Guest path → substrate path, or None if the path is not wired.
    pub fn resolve(&self, guest_path: &str) -> Option<String> {
        for (guest, substrate) in self.0.iter() {
            if let Some(rest) = strip_prefix(guest_path, guest) {
                return Some(join(substrate, rest));
            }
        }
        None
    }

    /// Substrate result path → guest namespace, for returning write
    /// results. Falls back to the input when no entry covers it (the
    /// identity-mapping case never hits the fallback).
    pub fn unresolve(&self, substrate_path: &str) -> String {
        for (guest, substrate) in self.0.iter() {
            if let Some(rest) = strip_prefix(substrate_path, substrate) {
                return join(guest, rest);
            }
        }
        substrate_path.to_string()
    }
}

/// Component-wise prefix strip: "gate" covers "gate/x" and "gate", not
/// "gateway".
fn strip_prefix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(prefix)?;
    if rest.is_empty() {
        Some("")
    } else {
        rest.strip_prefix('/')
    }
}

fn join(prefix: &str, rest: &str) -> String {
    if rest.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}/{rest}")
    }
}

/// The native host's import bindings: manifest import name → substrate
/// mount prefix. This is main.rs's half of the wiring contract; tests use
/// the same map so the parity suites exercise the manifest end-to-end.
pub fn standard_bindings() -> BTreeMap<String, String> {
    [
        ("gate", "gate"),
        ("secret", "secret"),
        ("completions", "gateway/completions"),
        ("usage", "gateway/usage"),
        ("traffic", "gateway/traffic"),
        ("http-out", "upstream"),
        ("wire-handles", "wire"),
        ("telemetry", "gateway/telemetry"),
        ("sys", "sys"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_manifest_parses_and_validates() {
        let m = Manifest::embedded().expect("embedded manifest must be valid");
        assert_eq!(m.assembly, "ox-gateway");
        assert_eq!(m.public, "wire");
        assert!(m.blocks.contains_key("broker"));
        let wires = m.wires().unwrap();
        assert!(wires.iter().any(|w| w.block == "broker"
            && w.prefix == "gate"
            && w.target == Target::Import("gate".into())));
    }

    #[test]
    fn embedded_wiring_resolves_against_standard_bindings() {
        let m = Manifest::embedded().unwrap();
        let bindings = standard_bindings();
        let broker = m.wiring_for("broker", &bindings).unwrap();
        assert_eq!(
            broker.resolve("gate/accounts/anthropic").as_deref(),
            Some("gate/accounts/anthropic")
        );
        assert_eq!(
            broker.resolve("upstream/outstanding/0/events/from/2").as_deref(),
            Some("upstream/outstanding/0/events/from/2")
        );
        // Undeclared namespace: the wire mount is not in broker's wiring.
        assert_eq!(broker.resolve("wire/outstanding/0/head"), None);
        assert_eq!(broker.resolve("config/gate/accounts"), None);

        let wire = m.wiring_for("wire", &bindings).unwrap();
        assert_eq!(
            wire.resolve("gateway/completions").as_deref(),
            Some("gateway/completions")
        );
        // The wire Block cannot reach keys or the upstream socket.
        assert_eq!(wire.resolve("secret/keys/anthropic"), None);
        assert_eq!(wire.resolve("upstream"), None);

        // The stats Block reads ledger + in-flight and writes summaries;
        // it cannot reach keys, wire handles, or the upstream socket.
        let stats = m.wiring_for("stats", &bindings).unwrap();
        assert_eq!(
            stats.resolve("gateway/telemetry/outstanding/0/summary").as_deref(),
            Some("gateway/telemetry/outstanding/0/summary")
        );
        assert_eq!(stats.resolve("gateway/usage").as_deref(), Some("gateway/usage"));
        assert_eq!(stats.resolve("secret/keys/anthropic"), None);
        assert_eq!(stats.resolve("wire/outstanding/0"), None);
        assert_eq!(stats.resolve("upstream"), None);
    }

    #[test]
    fn aliased_binding_rewrites_both_directions() {
        let m = Manifest::parse(
            r#"
assembly: t
version: 0.0.0
imports: {svc: "x"}
blocks: {a: ./a.wasm}
public: a
wiring: ["a:/services/db -> $svc"]
"#,
        )
        .unwrap();
        let bindings: BTreeMap<_, _> =
            [("svc".to_string(), "backend/postgres".to_string())].into();
        let t = m.wiring_for("a", &bindings).unwrap();
        assert_eq!(
            t.resolve("services/db/users/123").as_deref(),
            Some("backend/postgres/users/123")
        );
        assert_eq!(t.resolve("services/db").as_deref(), Some("backend/postgres"));
        assert_eq!(
            t.unresolve("backend/postgres/outstanding/7"),
            "services/db/outstanding/7"
        );
        // Component-wise: "services/dbx" is not under "services/db".
        assert_eq!(t.resolve("services/dbx"), None);
    }

    #[test]
    fn longest_prefix_wins() {
        let m = Manifest::parse(
            r#"
assembly: t
version: 0.0.0
imports: {broad: "x", narrow: "y"}
blocks: {a: ./a.wasm}
public: a
wiring: ["a:/gateway -> $broad", "a:/gateway/usage -> $narrow"]
"#,
        )
        .unwrap();
        let bindings: BTreeMap<_, _> = [
            ("broad".to_string(), "wide".to_string()),
            ("narrow".to_string(), "tight".to_string()),
        ]
        .into();
        let t = m.wiring_for("a", &bindings).unwrap();
        assert_eq!(t.resolve("gateway/usage/append").as_deref(), Some("tight/append"));
        assert_eq!(t.resolve("gateway/other").as_deref(), Some("wide/other"));
    }

    #[test]
    fn validation_rejects_bad_references() {
        let base = |wiring: &str, public: &str| {
            format!(
                r#"
assembly: t
version: 0.0.0
imports: {{svc: "x"}}
blocks: {{a: ./a.wasm}}
public: {public}
wiring: [{wiring}]
"#
            )
        };
        assert!(Manifest::parse(&base("\"a:/p -> $nope\"", "a"))
            .unwrap_err()
            .contains("undeclared import"));
        assert!(Manifest::parse(&base("\"ghost:/p -> $svc\"", "a"))
            .unwrap_err()
            .contains("unknown block"));
        assert!(Manifest::parse(&base("\"a:/p -> $svc\"", "ghost"))
            .unwrap_err()
            .contains("public block"));
        assert!(Manifest::parse(&base("\"a:/p, $svc\"", "a"))
            .unwrap_err()
            .contains("missing '->'"));
    }
}
