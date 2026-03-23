//! Connection file parsing for Jupyter kernels.

use serde::Deserialize;

/// Connection information from a Jupyter connection file.
#[derive(Debug, Clone, Deserialize)]
pub struct ConnectionInfo {
    pub transport: String,
    pub ip: String,
    pub shell_port: u16,
    pub iopub_port: u16,
    pub stdin_port: u16,
    pub control_port: u16,
    pub hb_port: u16,
    pub signature_scheme: String,
    pub key: String,
}

impl ConnectionInfo {
    /// Parse a connection file from a file path.
    pub fn from_file(path: &str) -> Result<Self, crate::KernelError> {
        let content = std::fs::read_to_string(path)
            .map_err(crate::KernelError::Io)?;
        let info: ConnectionInfo = serde_json::from_str(&content)?;
        Ok(info)
    }

    /// Get the ZMQ address for a given port.
    pub fn address(&self, port: u16) -> String {
        format!("{}://{}:{}", self.transport, self.ip, port)
    }

    /// Shell channel address.
    pub fn shell_addr(&self) -> String {
        self.address(self.shell_port)
    }

    /// IOPub channel address.
    pub fn iopub_addr(&self) -> String {
        self.address(self.iopub_port)
    }

    /// Control channel address.
    pub fn control_addr(&self) -> String {
        self.address(self.control_port)
    }

    /// Heartbeat channel address.
    pub fn hb_addr(&self) -> String {
        self.address(self.hb_port)
    }

    /// Stdin channel address.
    pub fn stdin_addr(&self) -> String {
        self.address(self.stdin_port)
    }
}
