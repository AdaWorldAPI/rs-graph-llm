//! ZMQ socket management for Jupyter kernel protocol.
//!
//! Handles message serialization, HMAC signing, and socket lifecycle.

use crate::connection::ConnectionInfo;
use crate::protocol::{Message, Header, DELIMITER};
use crate::KernelError;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Sign a message using HMAC-SHA256.
pub fn sign_message(key: &str, header: &str, parent: &str, metadata: &str, content: &str) -> String {
    if key.is_empty() {
        return String::new();
    }

    let mut mac = HmacSha256::new_from_slice(key.as_bytes())
        .expect("HMAC key length error");
    mac.update(header.as_bytes());
    mac.update(parent.as_bytes());
    mac.update(metadata.as_bytes());
    mac.update(content.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Serialize a Message into ZMQ multipart frames.
pub fn serialize_message(msg: &Message, key: &str) -> Vec<Vec<u8>> {
    let header = serde_json::to_string(&msg.header).unwrap();
    let parent = serde_json::to_string(&msg.parent_header).unwrap();
    let metadata = serde_json::to_string(&msg.metadata).unwrap();
    let content = serde_json::to_string(&msg.content).unwrap();

    let signature = sign_message(key, &header, &parent, &metadata, &content);

    let mut frames = Vec::new();
    frames.push(DELIMITER.to_vec());
    frames.push(signature.into_bytes());
    frames.push(header.into_bytes());
    frames.push(parent.into_bytes());
    frames.push(metadata.into_bytes());
    frames.push(content.into_bytes());

    // Append binary buffers
    for buf in &msg.buffers {
        frames.push(buf.clone());
    }

    frames
}

/// Deserialize ZMQ multipart frames into a Message.
pub fn deserialize_message(frames: &[Vec<u8>], key: &str) -> Result<Message, KernelError> {
    // Find the delimiter
    let delim_idx = frames.iter()
        .position(|f| f.as_slice() == DELIMITER)
        .ok_or_else(|| KernelError::Protocol("Missing delimiter".into()))?;

    // Frames after delimiter: signature, header, parent, metadata, content, [buffers...]
    let sig_idx = delim_idx + 1;
    if frames.len() < sig_idx + 5 {
        return Err(KernelError::Protocol("Not enough frames".into()));
    }

    let signature = String::from_utf8_lossy(&frames[sig_idx]).to_string();
    let header_str = String::from_utf8_lossy(&frames[sig_idx + 1]);
    let parent_str = String::from_utf8_lossy(&frames[sig_idx + 2]);
    let metadata_str = String::from_utf8_lossy(&frames[sig_idx + 3]);
    let content_str = String::from_utf8_lossy(&frames[sig_idx + 4]);

    // Verify HMAC if key is set
    if !key.is_empty() {
        let expected = sign_message(key, &header_str, &parent_str, &metadata_str, &content_str);
        if signature != expected {
            return Err(KernelError::Protocol("HMAC signature mismatch".into()));
        }
    }

    let header: Header = serde_json::from_str(&header_str)
        .map_err(|e| KernelError::Protocol(format!("Invalid header: {e}")))?;
    let parent_header: serde_json::Value = serde_json::from_str(&parent_str)
        .map_err(|e| KernelError::Protocol(format!("Invalid parent header: {e}")))?;
    let metadata: serde_json::Value = serde_json::from_str(&metadata_str)
        .map_err(|e| KernelError::Protocol(format!("Invalid metadata: {e}")))?;
    let content: serde_json::Value = serde_json::from_str(&content_str)
        .map_err(|e| KernelError::Protocol(format!("Invalid content: {e}")))?;

    let buffers = if frames.len() > sig_idx + 5 {
        frames[sig_idx + 5..].to_vec()
    } else {
        Vec::new()
    };

    Ok(Message {
        header,
        parent_header,
        metadata,
        content,
        buffers,
    })
}
