//! [`ConnectorRegistry`] — look up connectors by name.

use std::collections::HashMap;

use crate::{Connector, error::ConnectorError};

/// A registry mapping connector names to boxed connector instances.
pub struct ConnectorRegistry {
    connectors: HashMap<String, Box<dyn Connector>>,
}

impl ConnectorRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            connectors: HashMap::new(),
        }
    }

    /// Register a connector under a name.
    pub fn register(&mut self, name: impl Into<String>, connector: Box<dyn Connector>) {
        self.connectors.insert(name.into(), connector);
    }

    /// Look up a connector by name.
    ///
    /// # Errors
    /// Returns [`ConnectorError::UnknownConnector`] if the name is not registered.
    pub fn get(&self, name: &str) -> Result<&dyn Connector, ConnectorError> {
        self.connectors
            .get(name)
            .map(std::convert::AsRef::as_ref)
            .ok_or_else(|| ConnectorError::UnknownConnector(name.to_owned()))
    }

    /// List all registered connector names.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.connectors.keys().map(String::as_str)
    }
}

impl Default for ConnectorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ConnectorRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectorRegistry")
            .field("names", &self.connectors.keys().collect::<Vec<_>>())
            .finish()
    }
}
