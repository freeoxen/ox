use std::collections::HashMap;

/// Bidirectional mapping between model wire names (flat strings like "read_file")
/// and internal StructFS paths (hierarchical like "fs/read").
///
/// Models can't use slashes in tool names, so this translation layer bridges
/// the flat wire namespace to the hierarchical internal namespace.
pub struct NameMap {
    wire_to_internal: HashMap<String, String>,
    internal_to_wire: HashMap<String, String>,
}

impl NameMap {
    pub fn new() -> Self {
        Self {
            wire_to_internal: HashMap::new(),
            internal_to_wire: HashMap::new(),
        }
    }

    pub fn register(&mut self, wire_name: &str, internal_path: &str) {
        self.wire_to_internal
            .insert(wire_name.to_string(), internal_path.to_string());
        self.internal_to_wire
            .insert(internal_path.to_string(), wire_name.to_string());
    }

    pub fn to_internal(&self, wire_name: &str) -> Option<&str> {
        self.wire_to_internal.get(wire_name).map(String::as_str)
    }

    pub fn to_wire(&self, internal_path: &str) -> Option<&str> {
        self.internal_to_wire.get(internal_path).map(String::as_str)
    }
}

impl Default for NameMap {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_wire_to_internal() {
        let mut map = NameMap::new();
        map.register("read_file", "fs/read");
        map.register("shell", "os/shell");

        assert_eq!(map.to_internal("read_file"), Some("fs/read"));
        assert_eq!(map.to_wire("fs/read"), Some("read_file"));
        assert_eq!(map.to_internal("unknown"), None);
    }
}
