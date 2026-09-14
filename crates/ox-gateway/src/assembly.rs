//! Gateway validation and import bindings around Featherweight assemblies.
use featherweight_runtime::{AssemblyDef, WireTarget};
use std::collections::BTreeMap;
use std::sync::Arc;
use structfs_core_store::Path;
const EMBEDDED: &str = include_str!("../gateway.assembly.yaml");

#[derive(Debug)]
pub struct Manifest {
    pub assembly: String,
    pub version: String,
    pub public: String,
    pub blocks: BTreeMap<String, featherweight_runtime::BlockDef>,
    definition: AssemblyDef,
}
pub use featherweight_runtime::WireTarget as Target;
pub struct Wire {
    pub block: String,
    pub prefix: String,
    pub target: Target,
}
impl Manifest {
    pub fn embedded() -> Result<Self, String> {
        Self::parse(EMBEDDED)
    }
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        Self::parse(
            &std::fs::read_to_string(path)
                .map_err(|e| format!("reading {}: {e}", path.display()))?,
        )
    }
    pub fn parse(text: &str) -> Result<Self, String> {
        let definition = AssemblyDef::from_str(text).map_err(|e| e.to_string())?;
        Ok(Self {
            assembly: definition.name.clone(),
            version: definition.version.clone().unwrap_or_default(),
            public: definition.public.clone(),
            blocks: definition.blocks.clone(),
            definition,
        })
    }
    pub fn wires(&self) -> Result<Vec<Wire>, String> {
        Ok(self
            .definition
            .wiring
            .iter()
            .map(|w| Wire {
                block: w.block.clone(),
                prefix: w.prefix.to_string(),
                target: w.target.clone(),
            })
            .collect())
    }
    pub fn wiring_for(
        &self,
        block: &str,
        bindings: &BTreeMap<String, String>,
    ) -> Result<WiringTable, String> {
        if !self.blocks.contains_key(block) {
            return Err(format!("no block named '{block}' in assembly"));
        }
        let mut entries = Vec::new();
        for wire in self.definition.wiring.iter().filter(|w| w.block == block) {
            let name = match &wire.target {
                WireTarget::Import(n) | WireTarget::Block(n) => n,
            };
            let base = bindings
                .get(name)
                .ok_or_else(|| format!("host provides no binding for '{name}'"))?;
            entries.push((
                wire.prefix.clone(),
                Path::parse(base).map_err(|e| e.to_string())?,
            ));
        }
        Ok(WiringTable {
            entries: Arc::new(entries),
            config: self.definition.config.get(block).cloned(),
        })
    }
}

/// Resolved import bindings. Routing itself belongs to the runtime namespace.
#[derive(Clone)]
pub struct WiringTable {
    pub(crate) entries: Arc<Vec<(Path, Path)>>,
    pub(crate) config: Option<structfs_core_store::Value>,
}
impl WiringTable {
    pub fn resolve(&self, path: &str) -> Option<String> {
        let routes = structfs_service::RouteTable::new(self.entries.as_ref().clone());
        let (_, suffix, _) = routes.resolve(&Path::parse(path).ok()?)?;
        let path = Path::parse(path).ok()?;
        let (base, _, _) = routes.resolve(&path)?;
        Some(base.join(&suffix).to_string())
    }
    pub fn unresolve(&self, path: &str) -> String {
        let routes = structfs_service::RouteTable::new(
            self.entries
                .iter()
                .map(|(guest, base)| (base.clone(), guest.clone()))
                .collect(),
        );
        let Ok(parsed) = Path::parse(path) else {
            return path.to_string();
        };
        routes
            .resolve(&parsed)
            .map(|(guest, suffix, _)| guest.join(&suffix).to_string())
            .unwrap_or_else(|| path.to_string())
    }
}

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
            broker
                .resolve("upstream/outstanding/0/events/from/2")
                .as_deref(),
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
            stats
                .resolve("gateway/telemetry/outstanding/0/summary")
                .as_deref(),
            Some("gateway/telemetry/outstanding/0/summary")
        );
        assert_eq!(
            stats.resolve("gateway/usage").as_deref(),
            Some("gateway/usage")
        );
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
        let bindings: BTreeMap<_, _> = [("svc".to_string(), "backend/postgres".to_string())].into();
        let t = m.wiring_for("a", &bindings).unwrap();
        assert_eq!(
            t.resolve("services/db/users/123").as_deref(),
            Some("backend/postgres/users/123")
        );
        assert_eq!(
            t.resolve("services/db").as_deref(),
            Some("backend/postgres")
        );
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
        assert_eq!(
            t.resolve("gateway/usage/append").as_deref(),
            Some("tight/append")
        );
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
        assert!(
            Manifest::parse(&base("\"a:/p -> $nope\"", "a"))
                .unwrap_err()
                .contains("undeclared import")
        );
        assert!(
            Manifest::parse(&base("\"ghost:/p -> $svc\"", "a"))
                .unwrap_err()
                .contains("unknown block")
        );
        assert!(
            Manifest::parse(&base("\"a:/p -> $svc\"", "ghost"))
                .unwrap_err()
                .contains("public block")
        );
        assert!(
            Manifest::parse(&base("\"a:/p, $svc\"", "a"))
                .unwrap_err()
                .contains("missing '->'")
        );
    }
}

#[cfg(test)]
mod validation_regressions {
    use super::*;
    #[test]
    fn upstream_and_gateway_reject_malformed_sections_and_unknown_references() {
        for section in [
            "imports: false",
            "config: false",
            "failure: false",
            "wiring: false",
            "version: false",
            "config: {ghost: {}}",
            "failure: {ghost: isolate}",
            "wiring: ['a:/p -> $ghost']",
            "wiring: ['a:/p -> ghost']",
            "wiring: ['ghost:/p -> a']",
            "improts: {}",
        ] {
            let text = format!("assembly: t\nblocks: {{a: 'embedded:a'}}\npublic: a\n{section}\n");
            assert!(AssemblyDef::from_str(&text).is_err(), "{section}");
            assert!(Manifest::parse(&text).is_err(), "{section}");
        }
        for field in ["env: false", "args: false", "spawn: []", "spwan: true"] {
            let text = format!(
                "assembly: t\nblocks: {{a: {{artifact: 'embedded:a', {field}}}}}\npublic: a\n"
            );
            assert!(AssemblyDef::from_str(&text).is_err(), "{field}");
            assert!(Manifest::parse(&text).is_err(), "{field}");
        }
    }

    #[test]
    fn upstream_and_gateway_accept_namespaced_extensions_and_arbitrary_config() {
        let text = r#"
assembly: t
x-owner: gateway
blocks:
  a:
    artifact: embedded:a
    x-build: {revision: test}
public: a
config:
  a: {application_field: [true, 42]}
"#;
        let upstream = AssemblyDef::from_str(text).unwrap();
        let gateway = Manifest::parse(text).unwrap();
        assert_eq!(gateway.definition.config, upstream.config);
        assert!(gateway.definition.config.contains_key("a"));
    }
}
