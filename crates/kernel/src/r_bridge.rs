//! Arrow IPC bridge for R data exchange.
//!
//! Sends DataFrames to R as Arrow IPC and receives results back.
//! This avoids JSON serialization overhead for large datasets.

use arrow::ipc;

/// Serialize a RecordBatch to Arrow IPC bytes (for sending to R).
pub fn to_arrow_ipc(batch: &arrow::array::RecordBatch) -> Result<Vec<u8>, crate::KernelError> {
    let mut buf = Vec::new();
    {
        let mut writer = ipc::writer::StreamWriter::try_new(&mut buf, &batch.schema())
            .map_err(|e| crate::KernelError::Protocol(format!("Arrow IPC write error: {e}")))?;
        writer.write(batch)
            .map_err(|e| crate::KernelError::Protocol(format!("Arrow IPC write error: {e}")))?;
        writer.finish()
            .map_err(|e| crate::KernelError::Protocol(format!("Arrow IPC finish error: {e}")))?;
    }
    Ok(buf)
}

/// Deserialize Arrow IPC bytes to a RecordBatch (received from R).
pub fn from_arrow_ipc(bytes: &[u8]) -> Result<arrow::array::RecordBatch, crate::KernelError> {
    let cursor = std::io::Cursor::new(bytes);
    let mut reader = ipc::reader::StreamReader::try_new(cursor, None)
        .map_err(|e| crate::KernelError::Protocol(format!("Arrow IPC read error: {e}")))?;

    reader.next()
        .ok_or_else(|| crate::KernelError::Protocol("No batches in Arrow IPC stream".into()))?
        .map_err(|e| crate::KernelError::Protocol(format!("Arrow IPC read error: {e}")))
}
